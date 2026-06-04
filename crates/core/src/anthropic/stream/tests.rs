use super::*;
use crate::openai::stream::ResponseStreamEvent;
use axum::{
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use futures_util::stream;
use std::{convert::Infallible, time::Duration};
use tokio::net::TcpListener;

#[tokio::test]
async fn message_stop_emits_completed_once() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut completed = 0;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if matches!(event.unwrap(), ResponseStreamEvent::Completed { .. }) {
            completed += 1;
        }
    }
    server.abort();

    assert_eq!(completed, 1);
}

#[tokio::test]
async fn message_delta_usage_folds_all_fields_over_message_start() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20,"cache_read_input_tokens":7}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut usage = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::Completed { response } = event.unwrap() {
            usage = response.get("usage").cloned();
        }
    }
    server.abort();

    let usage = usage.expect("a completed event carrying usage");
    // input_tokens appears only in message_start — it must survive the merge.
    assert_eq!(usage["input_tokens"], 10);
    // output_tokens and the cache class are updated by message_delta (the old
    // code merged only output_tokens, dropping the corrected cache count).
    assert_eq!(usage["output_tokens"], 20);
    assert_eq!(usage["cache_read_input_tokens"], 7);
}

#[tokio::test]
async fn eof_before_message_stop_reports_error() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![Ok::<Event, Infallible>(Event::default().data(
                r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
            ))];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let err = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    server.abort();

    assert!(err.to_string().contains("message_stop"), "got: {err}");
}

#[tokio::test]
async fn content_block_start_keeps_non_empty_tool_input() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{"path":"Cargo.toml"}}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"content_block_stop","index":0}"#)),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut inline_args = None;
    let mut done_args = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match event.unwrap() {
            ResponseStreamEvent::OutputItemAdded { item, .. } => {
                inline_args = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => {
                done_args = Some(arguments);
            }
            _ => {}
        }
    }
    server.abort();

    assert_eq!(inline_args.as_deref(), Some(r#"{"path":"Cargo.toml"}"#));
    assert_eq!(done_args.as_deref(), Some(r#"{"path":"Cargo.toml"}"#));
}

#[tokio::test]
async fn redacted_thinking_block_forwards_its_data() {
    // A redacted_thinking block carries its encrypted payload whole in
    // `data` at content_block_start (no deltas) — it must be forwarded as a
    // RedactedThinking event, not silently dropped, so it can be replayed.
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"enc-xyz"}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"content_block_stop","index":0}"#)),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut redacted = None;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::RedactedThinking { data } = event.unwrap() {
            redacted = Some(data);
        }
    }
    server.abort();

    assert_eq!(redacted.as_deref(), Some("enc-xyz"));
}

#[tokio::test]
async fn refusal_stop_reason_surfaces_as_error() {
    // A `refusal` stop means a safety classifier blocked the output. It must
    // surface as an error, not a silent successful turn.
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","explanation":"policy"}},"usage":{"output_tokens":3}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut saw_error = false;
    let mut saw_completed = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match event {
            Err(e) => {
                assert!(e.to_string().contains("refusal"), "got: {e}");
                saw_error = true;
            }
            Ok(ResponseStreamEvent::Completed { .. }) => saw_completed = true,
            _ => {}
        }
    }
    server.abort();

    assert!(saw_error, "refusal should emit an error");
    assert!(!saw_completed, "refusal must not also complete");
}

#[tokio::test]
async fn content_block_delta_accepts_camel_case_partial_json() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partialJson":"{\"path\":\"Cargo.toml\"}"}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"content_block_stop","index":0}"#)),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut delta_args = String::new();
    let mut done_args = String::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match event.unwrap() {
            ResponseStreamEvent::FunctionCallArgsDelta { delta, .. } => {
                delta_args.push_str(&delta);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => {
                done_args = arguments;
            }
            _ => {}
        }
    }
    server.abort();

    assert_eq!(delta_args, r#"{"path":"Cargo.toml"}"#);
    assert_eq!(done_args, r#"{"path":"Cargo.toml"}"#);
}

#[tokio::test]
async fn content_block_delta_accepts_text_without_delta_type() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_delta","index":0,"delta":{"text":"hello"}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"content_block_stop","index":0}"#)),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut streamed_text = String::new();
    let mut done_text = String::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match event.unwrap() {
            ResponseStreamEvent::OutputTextDelta { delta, .. } => {
                streamed_text.push_str(&delta);
            }
            ResponseStreamEvent::OutputTextDone { text, .. } => {
                done_text = text;
            }
            _ => {}
        }
    }
    server.abort();

    assert_eq!(streamed_text, "hello");
    assert_eq!(done_text, "hello");
}

#[tokio::test]
async fn content_block_delta_accepts_tool_args_without_delta_type() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_delta","index":0,"delta":{"partial_json":"{\"path\":\"Cargo.toml\"}"}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"content_block_stop","index":0}"#)),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut delta_args = String::new();
    let mut done_args = String::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match event.unwrap() {
            ResponseStreamEvent::FunctionCallArgsDelta { delta, .. } => {
                delta_args.push_str(&delta);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => {
                done_args = arguments;
            }
            _ => {}
        }
    }
    server.abort();

    assert_eq!(delta_args, r#"{"path":"Cargo.toml"}"#);
    assert_eq!(done_args, r#"{"path":"Cargo.toml"}"#);
}

#[tokio::test]
async fn content_block_delta_ignores_empty_arg_placeholder_before_real_args() {
    let app = Router::new().route(
        "/",
        get(|| async {
            let events = vec![
                Ok::<Event, Infallible>(Event::default().data(
                    r#"{"type":"message_start","message":{"id":"msg_1","model":"claude","usage":{"input_tokens":1}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partialJson":"{}"}}"#,
                )),
                Ok(Event::default().data(
                    r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partialJson":"{\"path\":\"Cargo.toml\"}"}}"#,
                )),
                Ok(Event::default().data(r#"{"type":"content_block_stop","index":0}"#)),
                Ok(Event::default().data(r#"{"type":"message_stop"}"#)),
            ];
            Sse::new(stream::iter(events))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_from_response(resp);
    let mut delta_args = String::new();
    let mut done_args = String::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match event.unwrap() {
            ResponseStreamEvent::FunctionCallArgsDelta { delta, .. } => {
                delta_args.push_str(&delta);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => {
                done_args = arguments;
            }
            _ => {}
        }
    }
    server.abort();

    assert_eq!(delta_args, r#"{"path":"Cargo.toml"}"#);
    assert_eq!(done_args, r#"{"path":"Cargo.toml"}"#);
}

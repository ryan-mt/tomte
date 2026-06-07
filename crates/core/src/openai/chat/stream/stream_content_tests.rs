//! Chat Completions streaming tests: content, tool-call, and usage mapping.

use axum::{
    response::sse::{Event, Sse},
    routing::get,
    Router,
};
use futures_util::stream;
use std::{convert::Infallible, time::Duration};
use tokio::net::TcpListener;

use super::*;

#[tokio::test]
async fn stream_maps_content_tool_calls_and_usage() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","model":"m","choices":[{"index":0,"delta":{"content":"Hi"}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"x\"}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut text = String::new();
    let mut added_name = None;
    let mut args = String::new();
    let mut completed_usage = None;
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev.unwrap() {
            ResponseStreamEvent::OutputTextDelta { delta, .. } => text.push_str(&delta),
            ResponseStreamEvent::OutputItemAdded { item, .. } => {
                added_name = item.get("name").and_then(|v| v.as_str()).map(String::from);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
            ResponseStreamEvent::Completed { response } => {
                completed_usage = response.get("usage").cloned();
            }
            _ => {}
        }
    }
    server.abort();

    assert_eq!(text, "Hi");
    assert_eq!(added_name.as_deref(), Some("read_file"));
    assert_eq!(args, "{\"path\":\"x\"}");
    let usage = completed_usage.unwrap();
    assert_eq!(usage["input_tokens"], 10);
    assert_eq!(usage["output_tokens"], 5);
    assert_eq!(usage["total_tokens"], 15);
}

#[tokio::test]
async fn stream_waits_for_tool_name_split_across_deltas() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"arguments":""}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"x\"}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut added_name = None;
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev.unwrap() {
            ResponseStreamEvent::OutputItemAdded { item, .. } => {
                added_name = item.get("name").and_then(|v| v.as_str()).map(String::from);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
            _ => {}
        }
    }
    server.abort();

    assert_eq!(added_name.as_deref(), Some("read_file"));
    assert_eq!(args, "{\"path\":\"x\"}");
}

#[tokio::test]
async fn stream_maps_legacy_function_call_deltas() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"function_call":{"name":"read_file","arguments":""}}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"function_call":{"arguments":"{\"path\":\"x\"}"}}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"function_call"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut added_name = None;
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev.unwrap() {
            ResponseStreamEvent::OutputItemAdded { item, .. } => {
                added_name = item.get("name").and_then(|v| v.as_str()).map(String::from);
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
            _ => {}
        }
    }
    server.abort();

    assert_eq!(added_name.as_deref(), Some("read_file"));
    assert_eq!(args, "{\"path\":\"x\"}");
}

#[tokio::test]
async fn stream_accepts_tool_call_object_and_object_arguments() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":{"path":"x"}}}}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } = ev.unwrap() {
            args = arguments;
        }
    }
    server.abort();

    assert_eq!(args, r#"{"path":"x"}"#);
}

#[tokio::test]
async fn stream_accepts_parameters_as_tool_arguments() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","parameters":{"path":"Cargo.toml"}}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } = ev.unwrap() {
            args = arguments;
        }
    }
    server.abort();

    assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
}

// Regression: a content_filter stop must surface as an error, not a clean
// (empty) Completed that the agent mistakes for a successful turn.
#[tokio::test]
async fn stream_content_filter_surfaces_as_error() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut saw_error = false;
    let mut saw_completed = false;
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev {
            Err(e) => {
                assert!(e.to_string().contains("content filter"), "got: {e}");
                saw_error = true;
            }
            Ok(ResponseStreamEvent::Completed { .. }) => saw_completed = true,
            _ => {}
        }
    }
    server.abort();

    assert!(saw_error, "content_filter should emit an error");
    assert!(
        !saw_completed,
        "must not also report a successful completion"
    );
}

#[tokio::test]
async fn stream_accepts_message_shape_chunks() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"message":{"content":"Hi","tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":{"path":"Cargo.toml"}}}]},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = handle_chat_response(resp);
    let mut text = String::new();
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev.unwrap() {
            ResponseStreamEvent::OutputTextDelta { delta, .. } => text.push_str(&delta),
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
            _ => {}
        }
    }
    server.abort();

    assert_eq!(text, "Hi");
    assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
}

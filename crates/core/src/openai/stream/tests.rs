use super::*;
use axum::{routing::get, Router};
use std::time::Duration;

#[test]
fn parse_sse_error_redacts_and_caps_event_data() {
    let data = format!(
        "{{\"error\":\"bad key sk-proj-secret and Bearer oauth-secret\",\"padding\":\"{}\"",
        "x".repeat(512)
    );

    let err = parse_sse_error("expected value", &data);
    let message = err.to_string();

    assert!(!message.contains("sk-proj-secret"), "{message}");
    assert!(!message.contains("oauth-secret"), "{message}");
    assert!(!message.contains(&"x".repeat(256)), "{message}");
    assert!(message.contains("<redacted>"), "{message}");
    assert!(message.contains("truncated"), "{message}");
    assert!(message.len() < 340, "{message}");
}

#[tokio::test]
async fn stream_reports_eof_before_terminal_event() {
    let app = Router::new().route(
        "/",
        get(|| async {
            (
                [("content-type", "text/event-stream")],
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = StreamHandle::from_response(resp);
    let first = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        first,
        ResponseStreamEvent::OutputTextDelta { ref delta, .. } if delta == "hi"
    ));

    let err = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("terminal event"), "got: {err}");
}

#[tokio::test]
async fn stream_done_marker_closes_without_error() {
    let app = Router::new().route(
        "/",
        get(|| async { ([("content-type", "text/event-stream")], "data: [DONE]\n\n") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    let mut handle = StreamHandle::from_response(resp);
    let next = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap();
    assert!(next.is_none(), "got: {next:?}");
}

#[test]
fn parse_function_delta_accepts_call_id_and_arguments_delta() {
    let ev = parse_event(
        r#"{"type":"response.function_call_arguments.delta","call_id":"call_123","arguments_delta":"{\"path\""}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDelta { item_id, delta } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(delta, r#"{"path""#);
        }
        other => panic!("expected FunctionCallArgsDelta, got {other:?}"),
    }
}

#[test]
fn parse_text_delta_accepts_text_alias() {
    let ev = parse_event(r#"{"type":"response.output_text.delta","item_id":"msg_1","text":"Hi"}"#)
        .unwrap();

    match ev {
        ResponseStreamEvent::OutputTextDelta { delta, item_id } => {
            assert_eq!(item_id.as_deref(), Some("msg_1"));
            assert_eq!(delta, "Hi");
        }
        other => panic!("expected OutputTextDelta, got {other:?}"),
    }
}

#[test]
fn parse_text_delta_accepts_object_delta_text() {
    let ev =
        parse_event(r#"{"type":"response.message.delta","delta":{"type":"text","text":"Hi"}}"#)
            .unwrap();

    match ev {
        ResponseStreamEvent::OutputTextDelta { delta, .. } => {
            assert_eq!(delta, "Hi");
        }
        other => panic!("expected OutputTextDelta, got {other:?}"),
    }
}

#[test]
fn parse_reasoning_delta_accepts_content_alias() {
    let ev = parse_event(
        r#"{"type":"response.reasoning.delta","content":[{"type":"text","text":"thinking"}]}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::ReasoningDelta { delta } => {
            assert_eq!(delta, "thinking");
        }
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn parse_function_delta_accepts_camel_case_partial_json() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.delta","id":"call_123","partialJson":"{\"path\""}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDelta { item_id, delta } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(delta, r#"{"path""#);
        }
        other => panic!("expected FunctionCallArgsDelta, got {other:?}"),
    }
}

#[test]
fn parse_function_done_accepts_tool_call_id_and_object_arguments() {
    let ev = parse_event(
        r#"{"type":"response.function_call_arguments.done","tool_call_id":"call_123","arguments":{"path":"Cargo.toml"}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(arguments, r#"{"path":"Cargo.toml"}"#);
        }
        other => panic!("expected FunctionCallArgsDone, got {other:?}"),
    }
}

#[test]
fn parse_function_done_accepts_nested_tool_input() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.done","id":"call_123","tool":{"input":{"path":"Cargo.toml"}}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(arguments, r#"{"path":"Cargo.toml"}"#);
        }
        other => panic!("expected FunctionCallArgsDone, got {other:?}"),
    }
}

#[test]
fn parse_function_done_accepts_camel_case_id_and_tool_input() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.done","callId":"call_123","toolInput":{"path":"Cargo.toml"}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(arguments, r#"{"path":"Cargo.toml"}"#);
        }
        other => panic!("expected FunctionCallArgsDone, got {other:?}"),
    }
}

#[test]
fn parse_function_done_skips_empty_wrapper_arguments_for_nested_tool_input() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.done","id":"call_123","arguments":{},"tool":{"input":{"path":"Cargo.toml"}}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(arguments, r#"{"path":"Cargo.toml"}"#);
        }
        other => panic!("expected FunctionCallArgsDone, got {other:?}"),
    }
}

#[test]
fn parse_function_delta_accepts_nested_item_wrapper() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.delta","item":{"id":"call_123","function":{"partialJson":"{\"path\""}}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDelta { item_id, delta } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(delta, r#"{"path""#);
        }
        other => panic!("expected FunctionCallArgsDelta, got {other:?}"),
    }
}

#[test]
fn parse_function_done_accepts_parameters_arguments() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.done","id":"call_123","function":{"parameters":{"path":"Cargo.toml"}}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
            assert_eq!(item_id, "call_123");
            assert_eq!(arguments, r#"{"path":"Cargo.toml"}"#);
        }
        other => panic!("expected FunctionCallArgsDone, got {other:?}"),
    }
}

#[test]
fn parse_function_done_accepts_anthropic_tool_use_id_alias() {
    let ev = parse_event(
        r#"{"type":"response.tool_call.done","tool_use_id":"toolu_123","toolInput":{"path":"Cargo.toml"}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
            assert_eq!(item_id, "toolu_123");
            assert_eq!(arguments, r#"{"path":"Cargo.toml"}"#);
        }
        other => panic!("expected FunctionCallArgsDone, got {other:?}"),
    }
}

#[test]
fn parse_output_item_added_accepts_output_item_wrapper() {
    let ev = parse_event(
        r#"{"type":"response.output_item.added","output_index":2,"output_item":{"type":"tool_use","id":"call_123","name":"read_file","input":{"path":"Cargo.toml"}}}"#,
    )
    .unwrap();

    match ev {
        ResponseStreamEvent::OutputItemAdded { item, output_index } => {
            assert_eq!(output_index, 2);
            assert_eq!(item["type"], "tool_use");
            assert_eq!(item["id"], "call_123");
        }
        other => panic!("expected OutputItemAdded, got {other:?}"),
    }
}

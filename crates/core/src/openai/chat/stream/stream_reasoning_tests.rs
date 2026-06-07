//! Chat Completions streaming tests: typed content parts and reasoning deltas.

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
async fn stream_accepts_typed_content_parts() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"message":{"content":[{"type":"text","text":"Hi "},{"type":"output_text","text":"there"}]},"finish_reason":"stop"}]}"#,
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
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::OutputTextDelta { delta, .. } = ev.unwrap() {
            text.push_str(&delta);
        }
    }
    server.abort();

    assert_eq!(text, "Hi there");
}

#[tokio::test]
async fn stream_maps_reasoning_content_delta() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"thinking"},"finish_reason":"stop"}]}"#,
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
    let mut reasoning = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::ReasoningDelta { delta } = ev.unwrap() {
            reasoning.push_str(&delta);
        }
    }
    server.abort();

    assert_eq!(reasoning, "thinking");
}

#[tokio::test]
async fn stream_accepts_provider_tool_name_and_partial_json_aliases() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"recipient_name":"functions.Read","partialJson":"{\"path\":\"Cargo.toml\"}"}}]}}]}"#,
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
    let mut name = String::new();
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev.unwrap() {
            ResponseStreamEvent::OutputItemAdded { item, .. } => {
                name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
            _ => {}
        }
    }
    server.abort();

    assert_eq!(name, "functions.Read");
    assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
}

#[tokio::test]
async fn stream_ignores_empty_arg_placeholder_before_real_tool_arguments() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":"{}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"Cargo.toml\"}"}}]}}]}"#,
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

#[tokio::test]
async fn stream_keeps_multiple_unindexed_tool_calls_separate() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}},{"id":"call_b","type":"function","function":{"name":"grep","arguments":""}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{\"path\":\"a\"}"}},{"function":{"arguments":"{\"pattern\":\"x\"}"}}]}}]}"#,
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
    let mut args_by_id = BTreeMap::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        if let ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } = ev.unwrap() {
            args_by_id.insert(item_id, arguments);
        }
    }
    server.abort();

    assert_eq!(
        args_by_id.get("call_a").map(String::as_str),
        Some(r#"{"path":"a"}"#)
    );
    assert_eq!(
        args_by_id.get("call_b").map(String::as_str),
        Some(r#"{"pattern":"x"}"#)
    );
}

#[tokio::test]
async fn stream_accepts_singular_tool_call_shape() {
    let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_call":{"id":"call_x","type":"function","function":{"name":"read_file","arguments":{"path":"Cargo.toml"}}}}}]}"#,
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
    let mut name = String::new();
    let mut args = String::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
        .await
        .unwrap()
    {
        match ev.unwrap() {
            ResponseStreamEvent::OutputItemAdded { item, .. } => {
                name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
            }
            ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
            _ => {}
        }
    }
    server.abort();

    assert_eq!(name, "read_file");
    assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
}

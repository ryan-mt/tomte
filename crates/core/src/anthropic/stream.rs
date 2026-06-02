//! Translate Anthropic SSE events into the shared
//! [`ResponseStreamEvent`](crate::openai::stream::ResponseStreamEvent) shape
//! so the agent loop doesn't need a provider-specific code path.
//!
//! Anthropic streams events of the form:
//!   - `message_start` — opening envelope (id, model, initial usage)
//!   - `content_block_start` — a new content block opens at `index`
//!   - `content_block_delta` — `text_delta` or `input_json_delta`
//!   - `content_block_stop` — closes the block at `index`
//!   - `message_delta` — top-level Message updates (stop_reason, output usage)
//!   - `message_stop` — terminator
//!   - `ping` / `error`
//!
//! We map text blocks to `OutputTextDelta`/`OutputTextDone`, tool_use blocks
//! to OpenAI-style `function_call` items via `OutputItemAdded` +
//! `FunctionCallArgsDelta` + `FunctionCallArgsDone`. The final usage from
//! `message_delta` is rolled into a `Completed` event so usage accounting
//! works uniformly.

use std::collections::HashMap;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::openai::stream::{ResponseStreamEvent, StreamHandle};
use crate::tool_args::accumulate_argument_fragment;

/// Cap on distinct content blocks tracked during one response. A real Anthropic
/// response opens only a handful; the cap stops a malformed stream of unique
/// block indices from growing the pending-block map without bound.
const MAX_PENDING_BLOCKS: usize = 1024;

struct PendingBlock {
    kind: String,
    item_id: String,
    args_buf: String,
    args_truncated: bool,
    text_buf: String,
    /// Accumulated `signature_delta` for a thinking block — the encrypted
    /// reasoning the API needs back when this turn's tool_use is replayed.
    signature_buf: String,
    /// Opaque `data` of a `redacted_thinking` block, captured whole at
    /// `content_block_start` (it streams no deltas). `None` for every other
    /// block. Replayed verbatim so a redacted-thinking-then-tool turn isn't
    /// rejected on the next request.
    redacted_data: Option<String>,
}

const ANTHROPIC_TOOL_ARGUMENT_MAX_BYTES: usize = 2 * 1024 * 1024;
const ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES: usize = ANTHROPIC_TOOL_ARGUMENT_MAX_BYTES + 1;

fn append_tool_args_capped(
    buf: &mut String,
    truncated: &mut bool,
    fragment: &str,
) -> Option<String> {
    // Drop a leading empty-args placeholder, keep mid-object fragments verbatim
    // (so a bare `"limit": null` survives) — shared rule, see
    // `accumulate_argument_fragment`.
    let fragment = accumulate_argument_fragment(buf.is_empty(), fragment)?;
    if *truncated {
        return None;
    }
    let remaining = ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES.saturating_sub(buf.len());
    if remaining == 0 {
        *truncated = true;
        return None;
    }
    if fragment.len() <= remaining {
        buf.push_str(fragment);
        return Some(fragment.to_string());
    }

    let mut cut = remaining;
    while cut > 0 && !fragment.is_char_boundary(cut) {
        cut -= 1;
    }
    let appended = if cut == 0 {
        " ".repeat(remaining)
    } else {
        fragment[..cut].to_string()
    };
    buf.push_str(&appended);
    *truncated = true;
    Some(appended)
}

fn non_empty_tool_input(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        // Raw JSON-text fragment: keep it verbatim (see `append_tool_args_capped`).
        // Normalizing here would drop a bare `null`/`{}`/`[]` chunk mid-stream.
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string(other).ok(),
    }
}

fn parse_anthropic_sse_error(error: impl std::fmt::Display, data: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "parse Anthropic SSE: {error}; data: {}",
        crate::sensitive::error_excerpt(data)
    )
}

fn partial_json_delta(delta: Option<&Value>) -> String {
    const KEYS: &[&str] = &[
        "partial_json",
        "partialJson",
        "input_json_delta",
        "inputJsonDelta",
        "arguments_delta",
        "argumentsDelta",
        "arguments",
    ];
    let Some(delta) = delta else {
        return String::new();
    };
    KEYS.iter()
        .find_map(|key| non_empty_tool_input(delta.get(*key)?))
        .unwrap_or_default()
}

fn string_delta_field(delta: Option<&Value>, keys: &[&str]) -> String {
    let Some(delta) = delta else {
        return String::new();
    };
    keys.iter()
        .find_map(|key| delta.get(*key)?.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn text_delta(delta: Option<&Value>) -> String {
    string_delta_field(delta, &["text", "content", "delta"])
}

fn thinking_delta(delta: Option<&Value>) -> String {
    string_delta_field(
        delta,
        &[
            "thinking",
            "reasoning",
            "reasoning_text",
            "reasoningText",
            "text",
            "content",
        ],
    )
}

fn signature_delta(delta: Option<&Value>) -> String {
    string_delta_field(delta, &["signature", "signature_delta", "signatureDelta"])
}

fn content_block_delta_type(delta: Option<&Value>, block_kind: Option<&str>) -> &'static str {
    let raw_type = delta
        .and_then(|d| d.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match raw_type {
        "text_delta" | "text" | "content_delta" => return "text_delta",
        "input_json_delta" | "tool_input_delta" | "arguments_delta" => {
            return "input_json_delta";
        }
        "thinking_delta" | "reasoning_delta" => return "thinking_delta",
        "signature_delta" => return "signature_delta",
        _ => {}
    }

    if !partial_json_delta(delta).is_empty() {
        return "input_json_delta";
    }
    if !signature_delta(delta).is_empty() {
        return "signature_delta";
    }
    if (block_kind == Some("thinking") || block_kind == Some("redacted_thinking"))
        && !thinking_delta(delta).is_empty()
    {
        return "thinking_delta";
    }
    if !text_delta(delta).is_empty() {
        return "text_delta";
    }
    "unknown"
}

/// Bridge an Anthropic SSE response onto a `StreamHandle` carrying
/// `ResponseStreamEvent`. The returned handle is consumed by the agent loop
/// just like an OpenAI stream.
pub fn handle_from_response(resp: reqwest::Response) -> StreamHandle {
    let (tx, rx) = mpsc::channel::<anyhow::Result<ResponseStreamEvent>>(256);
    let tx_supervisor = tx.clone();
    tokio::spawn(async move {
        let pump = tokio::spawn(async move {
            let mut stream = resp.bytes_stream().eventsource();
            let mut blocks: HashMap<u32, PendingBlock> = HashMap::new();
            let mut last_usage: Option<Value> = None;
            let mut response_id: Option<String> = None;
            let mut response_model: Option<String> = None;
            // The terminal `stop_reason` from `message_delta`. A `refusal` means
            // the safety classifier blocked the output; surface it as an error
            // rather than a silent (often empty) successful turn.
            let mut stop_reason: Option<String> = None;
            let mut stop_explanation: Option<String> = None;
            let mut had_error = false;
            let mut completed = false;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(ev) => {
                        if ev.data == "[DONE]" {
                            completed = true;
                            break;
                        }
                        let parsed: Value = match serde_json::from_str(&ev.data) {
                            Ok(v) => v,
                            Err(e) => {
                                let _ = tx.send(Err(parse_anthropic_sse_error(e, &ev.data))).await;
                                continue;
                            }
                        };
                        let kind = parsed
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        match kind.as_str() {
                            "message_start" => {
                                if let Some(msg) = parsed.get("message") {
                                    response_id = msg
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    response_model = msg
                                        .get("model")
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                    if let Some(u) = msg.get("usage") {
                                        last_usage = Some(u.clone());
                                    }
                                }
                            }
                            "content_block_start" => {
                                let index =
                                    parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;
                                // A real response opens only a handful of blocks;
                                // refuse to track unboundedly many distinct
                                // indices from a malformed stream.
                                if !blocks.contains_key(&index)
                                    && blocks.len() >= MAX_PENDING_BLOCKS
                                {
                                    continue;
                                }
                                let block = parsed.get("content_block");
                                let btype = block
                                    .and_then(|b| b.get("type"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if btype == "tool_use" {
                                    let id = block
                                        .and_then(|b| b.get("id"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let name = block
                                        .and_then(|b| b.get("name"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let initial_args = block
                                        .and_then(|b| b.get("input"))
                                        .and_then(non_empty_tool_input)
                                        .unwrap_or_default();
                                    blocks.insert(
                                        index,
                                        PendingBlock {
                                            kind: "tool_use".into(),
                                            item_id: id.clone(),
                                            args_buf: initial_args.clone(),
                                            args_truncated: false,
                                            text_buf: String::new(),
                                            signature_buf: String::new(),
                                            redacted_data: None,
                                        },
                                    );
                                    let item = json!({
                                        "type": "function_call",
                                        "id": id,
                                        "call_id": id,
                                        "name": name,
                                        "arguments": initial_args
                                    });
                                    let _ = tx
                                        .send(Ok(ResponseStreamEvent::OutputItemAdded {
                                            item,
                                            output_index: index,
                                        }))
                                        .await;
                                } else {
                                    // Store the real block type ("text",
                                    // "thinking", "redacted_thinking", …). Only a
                                    // "text" block finalizes into OutputTextDone;
                                    // a thinking block has no text_buf (its content
                                    // streams via ReasoningDelta), so labelling it
                                    // "text" made content_block_stop emit an empty
                                    // OutputTextDone that blanked the assistant
                                    // buffer mid-stream on every thinking turn.
                                    // A redacted_thinking block carries its
                                    // encrypted payload whole in `data` here (no
                                    // deltas follow), so capture it now.
                                    let redacted_data = if btype == "redacted_thinking" {
                                        block
                                            .and_then(|b| b.get("data"))
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string())
                                    } else {
                                        None
                                    };
                                    blocks.insert(
                                        index,
                                        PendingBlock {
                                            kind: btype,
                                            item_id: format!("text_{index}"),
                                            args_buf: String::new(),
                                            args_truncated: false,
                                            text_buf: String::new(),
                                            signature_buf: String::new(),
                                            redacted_data,
                                        },
                                    );
                                }
                            }
                            "content_block_delta" => {
                                let index =
                                    parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;
                                let delta = parsed.get("delta");
                                let block_kind = blocks.get(&index).map(|b| b.kind.as_str());
                                match content_block_delta_type(delta, block_kind) {
                                    "text_delta" => {
                                        let text = text_delta(delta);
                                        if !text.is_empty() {
                                            let item_id = blocks.get_mut(&index).map(|b| {
                                                b.text_buf.push_str(&text);
                                                b.item_id.clone()
                                            });
                                            let _ = tx
                                                .send(Ok(ResponseStreamEvent::OutputTextDelta {
                                                    delta: text,
                                                    item_id,
                                                }))
                                                .await;
                                        }
                                    }
                                    "input_json_delta" => {
                                        let partial = partial_json_delta(delta);
                                        if let Some(b) = blocks.get_mut(&index) {
                                            if b.kind == "tool_use" && !partial.is_empty() {
                                                if let Some(appended) = append_tool_args_capped(
                                                    &mut b.args_buf,
                                                    &mut b.args_truncated,
                                                    &partial,
                                                ) {
                                                    let _ = tx
                                                        .send(Ok(
                                                            ResponseStreamEvent::FunctionCallArgsDelta {
                                                                item_id: b.item_id.clone(),
                                                                delta: appended,
                                                            },
                                                        ))
                                                        .await;
                                                }
                                            }
                                        }
                                    }
                                    "thinking_delta" => {
                                        let text = thinking_delta(delta);
                                        if !text.is_empty() {
                                            // Buffer the plaintext too so the
                                            // block-stop can replay it alongside
                                            // the signature.
                                            if let Some(b) = blocks.get_mut(&index) {
                                                b.text_buf.push_str(&text);
                                            }
                                            let _ = tx
                                                .send(Ok(ResponseStreamEvent::ReasoningDelta {
                                                    delta: text,
                                                }))
                                                .await;
                                        }
                                    }
                                    "signature_delta" => {
                                        // Accumulate the encrypted signature; it
                                        // arrives in its own delta(s) after the
                                        // thinking text and is required to replay
                                        // the block before tool_use.
                                        let sig = signature_delta(delta);
                                        if !sig.is_empty() {
                                            if let Some(b) = blocks.get_mut(&index) {
                                                b.signature_buf.push_str(&sig);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            "content_block_stop" => {
                                let index =
                                    parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;
                                if let Some(b) = blocks.remove(&index) {
                                    if b.kind == "tool_use" {
                                        let _ = tx
                                            .send(Ok(ResponseStreamEvent::FunctionCallArgsDone {
                                                item_id: b.item_id,
                                                arguments: b.args_buf,
                                            }))
                                            .await;
                                    } else if b.kind == "text" {
                                        // Only real text blocks finalize into
                                        // OutputTextDone so the agent can finalize
                                        // final_text. thinking/redacted_thinking
                                        // blocks carry no text_buf (streamed via
                                        // ReasoningDelta); emitting a done for them
                                        // would blank the assistant buffer.
                                        let _ = tx
                                            .send(Ok(ResponseStreamEvent::OutputTextDone {
                                                text: b.text_buf,
                                                item_id: Some(b.item_id),
                                            }))
                                            .await;
                                    } else if b.kind == "redacted_thinking" {
                                        // A redacted block carries no plaintext or
                                        // signature, only its encrypted `data`;
                                        // forward it so the agent can replay it
                                        // verbatim ahead of this turn's tool_use.
                                        if let Some(data) = b.redacted_data {
                                            let _ = tx
                                                .send(Ok(ResponseStreamEvent::RedactedThinking {
                                                    data,
                                                }))
                                                .await;
                                        }
                                    } else if b.kind == "thinking" {
                                        // Finalize a thinking block: hand the agent
                                        // the plaintext + signature so it can replay
                                        // the block ahead of this turn's tool_use.
                                        if !b.signature_buf.is_empty() || !b.text_buf.is_empty() {
                                            let _ = tx
                                                .send(Ok(ResponseStreamEvent::ReasoningDone {
                                                    text: b.text_buf,
                                                    signature: Some(b.signature_buf),
                                                }))
                                                .await;
                                        }
                                    }
                                }
                            }
                            "message_delta" => {
                                if let Some(delta) = parsed.get("delta") {
                                    if let Some(sr) =
                                        delta.get("stop_reason").and_then(|v| v.as_str())
                                    {
                                        stop_reason = Some(sr.to_string());
                                    }
                                    stop_explanation = delta
                                        .get("stop_details")
                                        .and_then(|d| d.get("explanation"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string());
                                }
                                if let Some(u) = parsed.get("usage") {
                                    if let Some(existing) = last_usage.as_mut() {
                                        if let Some(out) = u.get("output_tokens").cloned() {
                                            if let Some(obj) = existing.as_object_mut() {
                                                obj.insert("output_tokens".into(), out);
                                            }
                                        }
                                    } else {
                                        last_usage = Some(u.clone());
                                    }
                                }
                            }
                            "message_stop" => {
                                // A `refusal` stop means a safety classifier blocked
                                // the output mid-stream; surface it as an error rather
                                // than a silent, often-empty "successful" turn (the
                                // OpenAI content_filter path does the same).
                                if stop_reason.as_deref() == Some("refusal") {
                                    let detail = stop_explanation
                                        .as_deref()
                                        .map(|e| format!(": {e}"))
                                        .unwrap_or_default();
                                    let _ = tx
                                        .send(Err(anyhow::anyhow!(
                                            "response blocked by the provider safety classifier (stop_reason: refusal){detail}"
                                        )))
                                        .await;
                                    had_error = true;
                                    break;
                                }
                                let response = json!({
                                    "id": response_id,
                                    "model": response_model,
                                    "usage": last_usage.clone(),
                                });
                                let _ = tx
                                    .send(Ok(ResponseStreamEvent::Completed { response }))
                                    .await;
                                completed = true;
                                break;
                            }
                            "ping" => {}
                            "error" => {
                                had_error = true;
                                let msg = parsed
                                    .get("error")
                                    .and_then(|e| e.get("message"))
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("Anthropic stream error")
                                    .to_string();
                                let _ = tx
                                    .send(Ok(ResponseStreamEvent::Error { message: msg }))
                                    .await;
                                // Terminal, like `message_stop`: stop reading so a
                                // mid-stream error can't leave the pump looping on a
                                // half-streamed tool call. `had_error` then suppresses
                                // the redundant "ended before message_stop" error.
                                break;
                            }
                            _ => {
                                let _ = tx.send(Ok(ResponseStreamEvent::Other { kind })).await;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!("Anthropic SSE transport: {e}")))
                            .await;
                        return;
                    }
                }
            }
            if !had_error && !completed {
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "Anthropic SSE stream ended before message_stop"
                    )))
                    .await;
            }
        });
        if let Err(e) = pump.await {
            if e.is_panic() {
                let _ = tx_supervisor
                    .send(Err(anyhow::anyhow!("Anthropic SSE pump panicked: {e}")))
                    .await;
            }
        }
    });
    StreamHandle { rx, quota: None }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn parse_anthropic_sse_error_redacts_and_caps_event_data() {
        let data = format!(
            "{{\"error\":\"bad key sk-ant-api03-secret and Bearer oauth-secret\",\"padding\":\"{}\"",
            "x".repeat(512)
        );

        let err = parse_anthropic_sse_error("expected value", &data);
        let message = err.to_string();

        assert!(!message.contains("sk-ant-api03-secret"), "{message}");
        assert!(!message.contains("oauth-secret"), "{message}");
        assert!(!message.contains(&"x".repeat(256)), "{message}");
        assert!(message.contains("<redacted>"), "{message}");
        assert!(message.contains("truncated"), "{message}");
        assert!(message.len() < 360, "{message}");
    }

    #[test]
    fn input_json_delta_concatenates_bare_null_verbatim() {
        // Regression: Claude streams `"limit": null` and the `null` value arrives
        // as its own input_json_delta chunk. It must be kept, not dropped as an
        // "empty placeholder", or the accumulated args become invalid JSON.
        let mut buf = String::new();
        let mut truncated = false;
        for frag in [
            r#"{"path":"sample.py","limit":"#,
            "null",
            r#","offset":null}"#,
        ] {
            append_tool_args_capped(&mut buf, &mut truncated, frag);
        }
        assert_eq!(buf, r#"{"path":"sample.py","limit":null,"offset":null}"#);
        // And it parses cleanly with limit == null.
        let v: serde_json::Value = serde_json::from_str(&buf).unwrap();
        assert!(v["limit"].is_null());
        assert_eq!(v["path"], "sample.py");
    }

    #[test]
    fn non_empty_tool_input_keeps_bare_null_string_fragment() {
        assert_eq!(
            non_empty_tool_input(&serde_json::json!("null")),
            Some("null".to_string())
        );
        // A truly empty string fragment is still skipped.
        assert_eq!(non_empty_tool_input(&serde_json::json!("")), None);
    }

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

    #[test]
    fn anthropic_tool_argument_buffer_is_capped_before_agent() {
        let mut buf = String::new();
        let mut truncated = false;

        let appended = append_tool_args_capped(
            &mut buf,
            &mut truncated,
            &"x".repeat(ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES + 8192),
        )
        .unwrap();

        assert_eq!(buf.len(), ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES);
        assert_eq!(appended.len(), ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES);
        assert!(truncated);
    }

    #[test]
    fn anthropic_tool_argument_cap_preserves_utf8_boundary() {
        let mut buf = "x".repeat(ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES - 1);
        let mut truncated = false;

        let appended = append_tool_args_capped(&mut buf, &mut truncated, "é").unwrap();

        assert_eq!(buf.len(), ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES);
        assert_eq!(appended, " ");
        assert!(buf.is_char_boundary(buf.len()));
        assert!(truncated);
    }
}

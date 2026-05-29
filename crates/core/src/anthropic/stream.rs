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

struct PendingBlock {
    kind: String,
    item_id: String,
    args_buf: String,
    text_buf: String,
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
            let mut had_error = false;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(ev) => {
                        if ev.data == "[DONE]" {
                            break;
                        }
                        let parsed: Value = match serde_json::from_str(&ev.data) {
                            Ok(v) => v,
                            Err(e) => {
                                let _ = tx
                                    .send(Err(anyhow::anyhow!(
                                        "parse Anthropic SSE: {e}: {}",
                                        ev.data
                                    )))
                                    .await;
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
                                    blocks.insert(
                                        index,
                                        PendingBlock {
                                            kind: "tool_use".into(),
                                            item_id: id.clone(),
                                            args_buf: String::new(),
                                            text_buf: String::new(),
                                        },
                                    );
                                    let item = json!({
                                        "type": "function_call",
                                        "id": id,
                                        "call_id": id,
                                        "name": name,
                                        "arguments": ""
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
                                    blocks.insert(
                                        index,
                                        PendingBlock {
                                            kind: btype,
                                            item_id: format!("text_{index}"),
                                            args_buf: String::new(),
                                            text_buf: String::new(),
                                        },
                                    );
                                }
                            }
                            "content_block_delta" => {
                                let index =
                                    parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;
                                let delta = parsed.get("delta");
                                let dtype = delta
                                    .and_then(|d| d.get("type"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                match dtype.as_str() {
                                    "text_delta" => {
                                        let text = delta
                                            .and_then(|d| d.get("text"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
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
                                        let partial = delta
                                            .and_then(|d| d.get("partial_json"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        if let Some(b) = blocks.get_mut(&index) {
                                            if b.kind == "tool_use" && !partial.is_empty() {
                                                b.args_buf.push_str(&partial);
                                                let _ = tx
                                                    .send(Ok(
                                                        ResponseStreamEvent::FunctionCallArgsDelta {
                                                            item_id: b.item_id.clone(),
                                                            delta: partial,
                                                        },
                                                    ))
                                                    .await;
                                            }
                                        }
                                    }
                                    "thinking_delta" => {
                                        let text = delta
                                            .and_then(|d| d.get("thinking"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        if !text.is_empty() {
                                            let _ = tx
                                                .send(Ok(ResponseStreamEvent::ReasoningDelta {
                                                    delta: text,
                                                }))
                                                .await;
                                        }
                                    }
                                    "signature_delta" => {}
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
                                    }
                                }
                            }
                            "message_delta" => {
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
                                let response = json!({
                                    "id": response_id,
                                    "model": response_model,
                                    "usage": last_usage.clone(),
                                });
                                let _ = tx
                                    .send(Ok(ResponseStreamEvent::Completed { response }))
                                    .await;
                                // Exit via break so the post-loop fallback does not
                                // send a second Completed event.
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
            if !had_error {
                let response = json!({
                    "id": response_id,
                    "model": response_model,
                    "usage": last_usage,
                });
                let _ = tx
                    .send(Ok(ResponseStreamEvent::Completed { response }))
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
    StreamHandle { rx }
}

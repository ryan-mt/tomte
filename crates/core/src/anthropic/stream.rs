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

mod delta;
use delta::{
    append_tool_args_capped, content_block_delta_type, non_empty_tool_input,
    parse_anthropic_sse_error, partial_json_delta, signature_delta, text_delta, thinking_delta,
};

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
mod tests;

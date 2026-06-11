//! Bridge a Chat Completions SSE response onto the shared
//! `ResponseStreamEvent` stream consumed by the agent loop. Split out of `chat`.

use std::collections::BTreeMap;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::openai::stream::{ResponseStreamEvent, StreamHandle};

use super::accumulator::{apply_tool_delta, normalize_usage, ToolAcc};
use super::extract::{
    chat_argument_value, chat_content_text, chat_reasoning_text, chat_tool_call_id,
    chat_tool_call_index, chat_tool_name, tool_call_values,
};

/// Bridge a Chat Completions SSE response onto a `StreamHandle` carrying
/// `ResponseStreamEvent`, consumed by the agent loop exactly like the other
/// providers.
pub fn handle_chat_response(resp: reqwest::Response) -> StreamHandle {
    let (tx, rx) = mpsc::channel::<anyhow::Result<ResponseStreamEvent>>(256);
    let tx_supervisor = tx.clone();
    tokio::spawn(async move {
        let pump = tokio::spawn(async move {
            let mut stream = resp.bytes_stream().eventsource();
            // Tool calls accumulate by their streamed `index`.
            let mut tools: BTreeMap<u32, ToolAcc> = BTreeMap::new();
            let mut content_buf = String::new();
            let mut usage: Option<Value> = None;
            let mut response_id: Option<String> = None;
            let mut response_model: Option<String> = None;
            let mut done = false;
            let mut saw_finish = false;
            let mut finish_reason: Option<String> = None;
            let mut had_error = false;

            while let Some(item) = stream.next().await {
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!("Chat Completions SSE transport: {e}")))
                            .await;
                        return;
                    }
                };
                if ev.data == "[DONE]" {
                    done = true;
                    break;
                }
                let chunk: Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!(
                                "parse Chat Completions SSE: {e}: {}",
                                crate::sensitive::error_excerpt(&ev.data)
                            )))
                            .await;
                        continue;
                    }
                };
                // Some Chat-compatible providers serialize `"error": null` on
                // every chunk (same shape habit as the null `finish_reason`
                // below) — only a non-null error ends the stream.
                if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
                    had_error = true;
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Chat Completions stream error")
                        .to_string();
                    let _ = tx
                        .send(Ok(ResponseStreamEvent::Error { message: msg }))
                        .await;
                    break;
                }
                if response_id.is_none() {
                    response_id = chunk.get("id").and_then(|v| v.as_str()).map(String::from);
                }
                if response_model.is_none() {
                    response_model = chunk
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if let Some(u) = chunk.get("usage") {
                    if u.is_object() {
                        usage = Some(normalize_usage(u));
                    }
                }
                let Some(choice) = chunk
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                else {
                    continue;
                };
                if let Some(fr) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                    saw_finish = true;
                    if let Some(s) = fr.as_str() {
                        finish_reason = Some(s.to_string());
                    }
                }
                let message_part = choice.get("delta").or_else(|| choice.get("message"));
                if let Some(reasoning) = message_part.and_then(chat_reasoning_text) {
                    let _ = tx
                        .send(Ok(ResponseStreamEvent::ReasoningDelta { delta: reasoning }))
                        .await;
                }
                if let Some(text) = chat_content_text(message_part.and_then(|d| d.get("content"))) {
                    if !text.is_empty() {
                        content_buf.push_str(&text);
                        let _ = tx
                            .send(Ok(ResponseStreamEvent::OutputTextDelta {
                                delta: text,
                                item_id: None,
                            }))
                            .await;
                    }
                }
                for key in ["tool_calls", "tool_call"] {
                    if let Some(calls) = message_part.and_then(|d| d.get(key)) {
                        for (slot, tc) in tool_call_values(calls).into_iter().enumerate() {
                            let index = chat_tool_call_index(&tools, tc, slot);
                            let function = tc.get("function");
                            apply_tool_delta(
                                &mut tools,
                                index,
                                chat_tool_call_id(tc),
                                chat_tool_name(function, tc),
                                chat_argument_value(function, tc),
                                &tx,
                            )
                            .await;
                        }
                    }
                }
                if let Some(function_call) = message_part.and_then(|d| d.get("function_call")) {
                    apply_tool_delta(
                        &mut tools,
                        0,
                        None,
                        chat_tool_name(Some(function_call), function_call),
                        chat_argument_value(Some(function_call), function_call),
                        &tx,
                    )
                    .await;
                }
            }

            if had_error {
                return;
            }
            if !done && !saw_finish {
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "Chat Completions stream ended before completion"
                    )))
                    .await;
                return;
            }
            // A content-filter stop means the provider blocked the output. Surface
            // it as an error rather than a silent (often empty) successful turn —
            // matching how the native Responses path treats `response.failed`.
            if finish_reason.as_deref() == Some("content_filter") {
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "response blocked by the provider content filter (finish_reason: content_filter)"
                    )))
                    .await;
                return;
            }
            // Finalize: close each tool call, then the text, then complete.
            for acc in tools.into_values() {
                if !acc.added {
                    let item = json!({
                        "type": "function_call",
                        "id": acc.id,
                        "call_id": acc.id,
                        "name": acc.name.unwrap_or_default(),
                        "arguments": "",
                    });
                    let _ = tx
                        .send(Ok(ResponseStreamEvent::OutputItemAdded {
                            item,
                            output_index: 0,
                        }))
                        .await;
                }
                let _ = tx
                    .send(Ok(ResponseStreamEvent::FunctionCallArgsDone {
                        item_id: acc.id,
                        arguments: acc.args,
                    }))
                    .await;
            }
            if !content_buf.is_empty() {
                let _ = tx
                    .send(Ok(ResponseStreamEvent::OutputTextDone {
                        text: content_buf,
                        item_id: None,
                    }))
                    .await;
            }
            let response = json!({
                "id": response_id,
                "model": response_model,
                "usage": usage,
            });
            let _ = tx
                .send(Ok(ResponseStreamEvent::Completed { response }))
                .await;
        });
        if let Err(e) = pump.await {
            if e.is_panic() {
                let _ = tx_supervisor
                    .send(Err(anyhow::anyhow!(
                        "Chat Completions SSE pump panicked: {e}"
                    )))
                    .await;
            }
        }
    });
    StreamHandle { rx, quota: None }
}

#[cfg(test)]
mod stream_content_tests;
#[cfg(test)]
mod stream_reasoning_tests;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::tool_args::normalize_argument_fragment;

/// Wrapper around a streaming SSE response from the Responses API.
pub struct StreamHandle {
    pub rx: mpsc::Receiver<anyhow::Result<ResponseStreamEvent>>,
    /// Provider quota/rate-limit snapshot captured from the response headers at
    /// stream start, when present. Consumed once by the agent and surfaced via
    /// `/usage`. `None` when the provider sent no recognizable rate-limit headers.
    pub quota: Option<crate::usage::QuotaSnapshot>,
}

/// Semantic stream events the agent layer cares about. Events the model
/// emits that we don't model explicitly are surfaced as `Other` carrying
/// their raw type name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseStreamEvent {
    Created {
        response: Value,
    },
    OutputItemAdded {
        item: Value,
        output_index: u32,
    },
    OutputItemDone {
        item: Value,
        output_index: u32,
    },
    OutputTextDelta {
        delta: String,
        item_id: Option<String>,
    },
    OutputTextDone {
        text: String,
        item_id: Option<String>,
    },
    FunctionCallArgsDelta {
        item_id: String,
        delta: String,
    },
    FunctionCallArgsDone {
        item_id: String,
        arguments: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ReasoningDone {
        text: String,
        /// Anthropic thinking-block signature, when present. `None` for OpenAI
        /// (its reasoning continuity is handled server-side / via store).
        signature: Option<String>,
    },
    /// An Anthropic `redacted_thinking` block: reasoning the safety system
    /// encrypted, carrying opaque `data` instead of plaintext/signature. It
    /// must be replayed verbatim ahead of the turn's `tool_use` or the API
    /// rejects the follow-up request. Only the Anthropic stream emits this.
    RedactedThinking {
        data: String,
    },
    Completed {
        response: Value,
    },
    Failed {
        response: Value,
    },
    /// Provider quota/rate-limit snapshot delivered in-stream (the Codex
    /// `codex.rate_limits` SSE event). Header-delivered quota rides on
    /// [`StreamHandle::quota`] instead. Non-terminal.
    RateLimits(crate::usage::QuotaSnapshot),
    Error {
        message: String,
    },
    /// Catch-all carrying the original `type` so it can be diagnosed.
    Other {
        kind: String,
    },
}

fn parse_sse_error(error: impl std::fmt::Display, data: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "parse SSE: {error}; data: {}",
        crate::sensitive::error_excerpt(data)
    )
}

impl StreamHandle {
    pub fn from_response(resp: reqwest::Response) -> Self {
        let (tx, rx) = mpsc::channel::<anyhow::Result<ResponseStreamEvent>>(256);
        // Run the SSE pump under a supervisor so a panic inside
        // eventsource_stream (e.g. malformed HTTP frame, non-UTF-8 chunk) is
        // reported as an Err on the channel instead of silently closing it.
        // Without this the agent sees `Ok(None)` and treats the truncation
        // as a clean [DONE].
        let tx_supervisor = tx.clone();
        tokio::spawn(async move {
            let pump = tokio::spawn(async move {
                let mut stream = resp.bytes_stream().eventsource();
                let mut saw_terminal = false;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(ev) => {
                            if ev.data == "[DONE]" {
                                saw_terminal = true;
                                break;
                            }
                            match parse_event(&ev.data) {
                                Ok(parsed) => {
                                    if is_terminal_event(&parsed) {
                                        saw_terminal = true;
                                    }
                                    if tx.send(Ok(parsed)).await.is_err() {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(parse_sse_error(e, &ev.data))).await;
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(anyhow::anyhow!("SSE transport: {e}"))).await;
                            return;
                        }
                    }
                }
                if !saw_terminal {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "SSE stream ended before a terminal event"
                        )))
                        .await;
                }
            });
            if let Err(e) = pump.await {
                if e.is_panic() {
                    let _ = tx_supervisor
                        .send(Err(anyhow::anyhow!("SSE pump panicked: {e}")))
                        .await;
                }
            }
        });
        Self { rx, quota: None }
    }

    /// Attach a quota snapshot parsed from the response headers. Builder form so
    /// the provider clients can capture headers (which they have, before the
    /// body is consumed) without `from_response` needing to know the provider.
    pub fn with_quota(mut self, quota: Option<crate::usage::QuotaSnapshot>) -> Self {
        self.quota = quota;
        self
    }
}

fn parse_event(data: &str) -> anyhow::Result<ResponseStreamEvent> {
    let value: Value = serde_json::from_str(data)?;
    let kind = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let opt_s = |k: &str| -> Option<String> { string_value(value.get(k)) };
    let u = |k: &str| -> u32 { value.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32 };

    let ev = match kind.as_str() {
        "response.created" => ResponseStreamEvent::Created {
            response: value.get("response").cloned().unwrap_or(Value::Null),
        },
        "response.output_item.added" => ResponseStreamEvent::OutputItemAdded {
            item: event_item(&value),
            output_index: u("output_index"),
        },
        "response.output_item.done" => ResponseStreamEvent::OutputItemDone {
            item: event_item(&value),
            output_index: u("output_index"),
        },
        // Text deltas — handle multiple naming styles that have appeared
        // across Responses API revisions and the ChatGPT backend.
        "response.output_text.delta" | "response.text.delta" | "response.message.delta" => {
            ResponseStreamEvent::OutputTextDelta {
                delta: first_text_string(
                    &value,
                    &["delta", "text", "content", "output_text", "outputText"],
                )
                .unwrap_or_default(),
                item_id: opt_s("item_id"),
            }
        }
        "response.output_text.done" | "response.text.done" | "response.message.done" => {
            ResponseStreamEvent::OutputTextDone {
                text: first_text_string(
                    &value,
                    &["text", "content", "output_text", "outputText", "delta"],
                )
                .unwrap_or_default(),
                item_id: opt_s("item_id"),
            }
        }
        // Function/tool call streaming.
        "response.function_call_arguments.delta" | "response.tool_call.delta" => {
            ResponseStreamEvent::FunctionCallArgsDelta {
                item_id: function_call_event_id(&value),
                delta: first_argument_string(
                    &value,
                    &[
                        "delta",
                        "arguments_delta",
                        "argumentsDelta",
                        "arguments",
                        "arguments_json",
                        "argumentsJson",
                        "partial_json",
                        "partialJson",
                        "input_json_delta",
                        "inputJsonDelta",
                        "input_json",
                        "inputJson",
                        "tool_input",
                        "toolInput",
                        "args",
                        "input",
                        "parameters",
                        "parameters_json",
                        "parametersJson",
                    ],
                )
                .unwrap_or_default(),
            }
        }
        "response.function_call_arguments.done" | "response.tool_call.done" => {
            // Lenient like the sibling delta handler: a `.done` without a string
            // `arguments` must not abort the whole turn — the args were already
            // accumulated from the deltas / OutputItemDone. Default to "" and let
            // the agent keep its accumulated buffer (it only overwrites when the
            // done event actually carried args).
            ResponseStreamEvent::FunctionCallArgsDone {
                item_id: function_call_event_id(&value),
                arguments: first_argument_string(
                    &value,
                    &[
                        "arguments",
                        "arguments_json",
                        "argumentsJson",
                        "delta",
                        "arguments_delta",
                        "argumentsDelta",
                        "partial_json",
                        "partialJson",
                        "input_json_delta",
                        "inputJsonDelta",
                        "input_json",
                        "inputJson",
                        "tool_input",
                        "toolInput",
                        "args",
                        "input",
                        "parameters",
                        "parameters_json",
                        "parametersJson",
                    ],
                )
                .unwrap_or_default(),
            }
        }
        // Reasoning summary — multiple shapes.
        "response.reasoning_summary_text.delta"
        | "response.reasoning_summary.delta"
        | "response.reasoning.delta"
        | "response.reasoning_text.delta" => ResponseStreamEvent::ReasoningDelta {
            delta: first_text_string(
                &value,
                &[
                    "delta",
                    "text",
                    "content",
                    "summary",
                    "reasoning",
                    "thinking",
                ],
            )
            .unwrap_or_default(),
        },
        "response.reasoning_summary_text.done"
        | "response.reasoning_summary.done"
        | "response.reasoning.done"
        | "response.reasoning_text.done" => ResponseStreamEvent::ReasoningDone {
            text: first_text_string(
                &value,
                &[
                    "text",
                    "content",
                    "summary",
                    "reasoning",
                    "thinking",
                    "delta",
                ],
            )
            .unwrap_or_default(),
            signature: None,
        },
        "response.completed" => ResponseStreamEvent::Completed {
            response: value.get("response").cloned().unwrap_or(Value::Null),
        },
        // Codex/ChatGPT-backend in-stream quota event (some routes send the
        // snapshot here instead of, or alongside, the `x-codex-*` headers).
        "codex.rate_limits" => match crate::usage::parse_codex_rate_limit_event(&value) {
            Some(snapshot) => ResponseStreamEvent::RateLimits(snapshot),
            None => ResponseStreamEvent::Other { kind: kind.clone() },
        },
        "response.failed" => ResponseStreamEvent::Failed {
            response: value.get("response").cloned().unwrap_or(Value::Null),
        },
        "error" => ResponseStreamEvent::Error {
            message: value
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| string_value(value.get("message")).unwrap_or_default()),
        },
        "" => ResponseStreamEvent::Other {
            kind: "(no type)".into(),
        },
        other => ResponseStreamEvent::Other { kind: other.into() },
    };
    Ok(ev)
}

fn event_item(value: &Value) -> Value {
    value
        .get("item")
        .or_else(|| value.get("output_item"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn function_call_event_id(value: &Value) -> String {
    let keys = &[
        "item_id",
        "itemId",
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ];
    first_string(value, keys)
        .or_else(|| value.get("item").and_then(|item| first_string(item, keys)))
        .or_else(|| {
            value
                .get("output_item")
                .and_then(|item| first_string(item, keys))
        })
        .unwrap_or_default()
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_value(value.get(*key)))
}

fn first_argument_string(value: &Value, keys: &[&str]) -> Option<String> {
    first_argument_value(value, keys)
        .or_else(|| {
            value
                .get("function")
                .and_then(|f| first_argument_value(f, keys))
        })
        .or_else(|| {
            value
                .get("tool")
                .and_then(|t| first_argument_value(t, keys))
        })
        .or_else(|| {
            value
                .get("item")
                .and_then(|item| first_argument_string(item, keys))
        })
        .or_else(|| {
            value
                .get("output_item")
                .and_then(|item| first_argument_string(item, keys))
        })
}

fn first_text_string(value: &Value, keys: &[&str]) -> Option<String> {
    first_text_value(value, keys)
        .or_else(|| {
            value
                .get("item")
                .and_then(|item| first_text_string(item, keys))
        })
        .or_else(|| {
            value
                .get("output_item")
                .and_then(|item| first_text_string(item, keys))
        })
}

fn first_text_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| text_string_value(value.get(*key)))
}

fn text_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(piece) = text_string_value(Some(part)) {
                    out.push_str(&piece);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        Value::Object(obj) => {
            for key in ["text", "content", "output_text", "outputText", "delta"] {
                if let Some(text) = text_string_value(obj.get(key)) {
                    return Some(text);
                }
            }
            None
        }
        Value::Number(_) | Value::Bool(_) => None,
    }
}

fn first_argument_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| argument_string_value(value.get(*key)))
}

fn argument_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) => normalize_argument_fragment(s).map(str::to_string),
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

fn is_terminal_event(ev: &ResponseStreamEvent) -> bool {
    matches!(
        ev,
        ResponseStreamEvent::Completed { .. }
            | ResponseStreamEvent::Failed { .. }
            | ResponseStreamEvent::Error { .. }
    )
}

#[cfg(test)]
mod tests;

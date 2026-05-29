use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

/// Wrapper around a streaming SSE response from the Responses API.
pub struct StreamHandle {
    pub rx: mpsc::Receiver<anyhow::Result<ResponseStreamEvent>>,
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
    },
    Completed {
        response: Value,
    },
    Failed {
        response: Value,
    },
    Error {
        message: String,
    },
    /// Catch-all carrying the original `type` so it can be diagnosed.
    Other {
        kind: String,
    },
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
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(ev) => {
                            if ev.data == "[DONE]" {
                                break;
                            }
                            match parse_event(&ev.data) {
                                Ok(parsed) => {
                                    if tx.send(Ok(parsed)).await.is_err() {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(Err(anyhow::anyhow!("parse SSE: {e}: {}", ev.data)))
                                        .await;
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
            });
            if let Err(e) = pump.await {
                if e.is_panic() {
                    let _ = tx_supervisor
                        .send(Err(anyhow::anyhow!("SSE pump panicked: {e}")))
                        .await;
                }
            }
        });
        Self { rx }
    }
}

fn parse_event(data: &str) -> anyhow::Result<ResponseStreamEvent> {
    let value: Value = serde_json::from_str(data)?;
    let kind = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let s = |k: &str| -> String {
        value
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let opt_s = |k: &str| -> Option<String> {
        value.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    let u = |k: &str| -> u32 { value.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32 };

    let ev = match kind.as_str() {
        "response.created" => ResponseStreamEvent::Created {
            response: value.get("response").cloned().unwrap_or(Value::Null),
        },
        "response.output_item.added" => ResponseStreamEvent::OutputItemAdded {
            item: value.get("item").cloned().unwrap_or(Value::Null),
            output_index: u("output_index"),
        },
        "response.output_item.done" => ResponseStreamEvent::OutputItemDone {
            item: value.get("item").cloned().unwrap_or(Value::Null),
            output_index: u("output_index"),
        },
        // Text deltas — handle multiple naming styles that have appeared
        // across Responses API revisions and the ChatGPT backend.
        "response.output_text.delta" | "response.text.delta" | "response.message.delta" => {
            ResponseStreamEvent::OutputTextDelta {
                delta: s("delta"),
                item_id: opt_s("item_id"),
            }
        }
        "response.output_text.done" | "response.text.done" | "response.message.done" => {
            ResponseStreamEvent::OutputTextDone {
                text: s("text"),
                item_id: opt_s("item_id"),
            }
        }
        // Function/tool call streaming.
        "response.function_call_arguments.delta" | "response.tool_call.delta" => {
            ResponseStreamEvent::FunctionCallArgsDelta {
                item_id: s("item_id"),
                delta: s("delta"),
            }
        }
        "response.function_call_arguments.done" | "response.tool_call.done" => {
            // Lenient like the sibling delta handler: a `.done` without a string
            // `arguments` must not abort the whole turn — the args were already
            // accumulated from the deltas / OutputItemDone. Default to "" and let
            // the agent keep its accumulated buffer (it only overwrites when the
            // done event actually carried args).
            ResponseStreamEvent::FunctionCallArgsDone {
                item_id: s("item_id"),
                arguments: s("arguments"),
            }
        }
        // Reasoning summary — multiple shapes.
        "response.reasoning_summary_text.delta"
        | "response.reasoning_summary.delta"
        | "response.reasoning.delta"
        | "response.reasoning_text.delta" => {
            ResponseStreamEvent::ReasoningDelta { delta: s("delta") }
        }
        "response.reasoning_summary_text.done"
        | "response.reasoning_summary.done"
        | "response.reasoning.done"
        | "response.reasoning_text.done" => ResponseStreamEvent::ReasoningDone { text: s("text") },
        "response.completed" => ResponseStreamEvent::Completed {
            response: value.get("response").cloned().unwrap_or(Value::Null),
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
                .unwrap_or_else(|| s("message")),
        },
        "" => ResponseStreamEvent::Other {
            kind: "(no type)".into(),
        },
        other => ResponseStreamEvent::Other { kind: other.into() },
    };
    Ok(ev)
}

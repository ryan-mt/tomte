//! Split out of `agent` (impl Agent block); logic unchanged.

use super::*;

impl Agent {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn apply_stream_event(
        &mut self,
        ev: ResponseStreamEvent,
        pending_calls: &mut Vec<PendingCall>,
        final_text: &mut String,
        produced_output: &mut bool,
        thinking_blocks: &mut Vec<InputItem>,
        orphan_arg_buffers: &mut std::collections::HashMap<String, ToolArgsBuffer>,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> EventFlow {
        match ev {
            ResponseStreamEvent::OutputItemAdded { item, .. } if is_function_call_item(&item) => {
                // If both ids are missing, downstream args deltas can't
                // be matched back to this call — pending_calls would
                // hold a ghost entry that any later "empty id" delta
                // would corrupt, eventually dispatching a tool with
                // bogus or empty arguments. Drop the event with a
                // warning instead of pretending it worked.
                let Some((call_id, item_id)) = function_call_refs(&item) else {
                    tracing::warn!(
                        name = %tool_name_from_item(&item),
                        "function_call event missing both call_id and id; skipping"
                    );
                    return EventFlow::Continue;
                };
                // Some models send the complete arguments inline on the
                // OutputItemAdded item; capture them as an initial buffer.
                let mut args = take_orphan_args(orphan_arg_buffers, &call_id, &item_id);
                if let Some(inline_args) = arguments_from_item(&item) {
                    args.merge_inline(&inline_args);
                }
                let name = tool_name_from_item(&item);
                // A duplicate non-empty call_id would later collapse in
                // the results HashMap, silently dropping one tool output
                // and leaving an unanswered call in history. Skip the
                // second item and log the server-side anomaly instead.
                if pending_calls.iter().any(|p| {
                    p.call_id == call_id
                        || p.call_id == item_id
                        || p.item_id == call_id
                        || p.item_id == item_id
                }) {
                    tracing::warn!(
                        call_id = %call_id,
                        item_id = %item_id,
                        name = %name,
                        "duplicate function_call id from server; ignoring second function_call item"
                    );
                    return EventFlow::Continue;
                }
                pending_calls.push(PendingCall {
                    call_id: call_id.clone(),
                    item_id,
                    name: name.clone(),
                    args,
                    args_done_emitted: false,
                });
                let _ = tx.send(AgentEvent::ToolCallStarted { name, call_id }).await;
                if let Some(pc) = pending_calls.last_mut() {
                    if !pc.args.text.is_empty() {
                        let _ = tx
                            .send(AgentEvent::ToolCallArgsDone {
                                call_id: pc.call_id.clone(),
                                arguments: pc.args.text.clone(),
                            })
                            .await;
                        pc.args_done_emitted = true;
                    }
                }
            }
            ResponseStreamEvent::OutputItemDone { item, .. } if is_function_call_item(&item) => {
                let Some((call_id, item_id)) = function_call_refs(&item) else {
                    return EventFlow::Continue;
                };
                if let Some(pc) = pending_calls
                    .iter_mut()
                    .find(|p| p.call_id == call_id || p.item_id == item_id)
                {
                    let name = tool_name_from_item(&item);
                    if !name.is_empty() {
                        pc.name = name;
                    }
                    if let Some(arguments) = arguments_from_item(&item) {
                        pc.args.replace_if_non_empty(arguments);
                        if !pc.args_done_emitted {
                            let _ = tx
                                .send(AgentEvent::ToolCallArgsDone {
                                    call_id: pc.call_id.clone(),
                                    arguments: pc.args.text.clone(),
                                })
                                .await;
                            pc.args_done_emitted = true;
                        }
                    }
                }
            }
            ResponseStreamEvent::OutputTextDelta { delta, .. } => {
                final_text.push_str(&delta);
                *produced_output = true;
                let _ = tx
                    .send(AgentEvent::AssistantTextDelta { text: delta })
                    .await;
            }
            ResponseStreamEvent::OutputTextDone { text, .. } => {
                // `text` is only THIS block's text. Anthropic emits one
                // OutputTextDone per text block, so overwriting would drop
                // earlier blocks (e.g. text before AND after a tool_use in
                // one response). `final_text` already holds the full
                // accumulation from the deltas for both providers; keep it,
                // falling back to `text` only if no deltas arrived. Emit the
                // cumulative text so the UI's "done" replace stays complete.
                if final_text.is_empty() {
                    *final_text = text;
                }
                if !final_text.is_empty() {
                    *produced_output = true;
                }
                let _ = tx
                    .send(AgentEvent::AssistantTextDone {
                        text: final_text.clone(),
                    })
                    .await;
            }
            ResponseStreamEvent::FunctionCallArgsDelta { item_id, delta } => {
                if item_id.is_empty() {
                    tracing::warn!("tool.args.delta missing item_id; dropping delta");
                    return EventFlow::Continue;
                }
                // Stream delta event references the output-item id, which
                // may differ from the function call_id. Match by either.
                if let Some(pc) = pending_calls
                    .iter_mut()
                    .find(|p| p.item_id == item_id || p.call_id == item_id)
                {
                    let call_id = pc.call_id.clone();
                    if let Some(delta) = pc.args.push(&delta) {
                        let _ = tx
                            .send(AgentEvent::ToolCallArgsDelta {
                                call_id,
                                delta: delta.to_string(),
                            })
                            .await;
                    }
                } else if orphan_args_has_room(orphan_arg_buffers, &item_id) {
                    let _ = orphan_arg_buffers.entry(item_id).or_default().push(&delta);
                }
            }
            ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
                if item_id.is_empty() {
                    tracing::warn!("tool.args.done missing item_id; dropping arguments");
                    return EventFlow::Continue;
                }
                let emit = match pending_calls
                    .iter_mut()
                    .find(|p| p.item_id == item_id || p.call_id == item_id)
                {
                    Some(pc) => {
                        // Only overwrite when the done event actually
                        // carried args; an empty/absent `arguments` must
                        // not wipe the buffer accumulated from the deltas
                        // (matches the OutputItemDone handler above).
                        if !arguments.is_empty() {
                            pc.args.replace_if_non_empty(arguments.clone());
                        }
                        // Emit at most once per call (see args_done_emitted).
                        if pc.args_done_emitted {
                            None
                        } else {
                            pc.args_done_emitted = true;
                            Some((pc.call_id.clone(), pc.args.text.clone()))
                        }
                    }
                    None => {
                        if !arguments.is_empty()
                            && orphan_args_has_room(orphan_arg_buffers, &item_id)
                        {
                            orphan_arg_buffers
                                .entry(item_id)
                                .or_default()
                                .replace_if_non_empty(arguments);
                        }
                        None
                    }
                };
                if let Some((call_id, arguments)) = emit {
                    let _ = tx
                        .send(AgentEvent::ToolCallArgsDone { call_id, arguments })
                        .await;
                }
            }
            ResponseStreamEvent::ReasoningDelta { delta } => {
                let _ = tx.send(AgentEvent::ReasoningDelta { text: delta }).await;
            }
            ResponseStreamEvent::ReasoningDone { text, signature } => {
                let _ = tx
                    .send(AgentEvent::ReasoningDone { text: text.clone() })
                    .await;
                // A thinking block is replayable only with its signature
                // (Anthropic rejects unsigned thinking on input), so keep
                // one entry per signed block — a multi-block turn then
                // replays them all in order. `None` on the OpenAI path.
                if let Some(signature) = signature {
                    thinking_blocks.push(InputItem::Reasoning {
                        id: String::new(),
                        summary: Vec::new(),
                        thinking: Some(text),
                        signature: Some(signature),
                        redacted_thinking: None,
                    });
                }
            }
            ResponseStreamEvent::RedactedThinking { data } => {
                // Encrypted reasoning — nothing to show, but it must be
                // replayed verbatim ahead of this turn's tool_use or the
                // next Anthropic request rejects the broken chain.
                thinking_blocks.push(InputItem::Reasoning {
                    id: String::new(),
                    summary: Vec::new(),
                    thinking: None,
                    signature: None,
                    redacted_thinking: Some(data),
                });
            }
            ResponseStreamEvent::Completed { response } => {
                if let Some(u) =
                    emit_usage(&response, tx, self.config.effective_context_limit()).await
                {
                    self.last_input_tokens = u.occupancy;
                    let model = self.config.model.clone();
                    self.record_cost(&model, &u);
                    let _ = tx
                        .send(AgentEvent::CostUpdate {
                            usage: self.cost_usage.clone(),
                        })
                        .await;
                }
                return EventFlow::Break;
            }
            ResponseStreamEvent::Failed { response } => {
                // Previously handled identically to Completed, which
                // masked content-filter / quota / 5xx errors as a
                // successful empty turn. Surface them instead.
                if let Some(u) =
                    emit_usage(&response, tx, self.config.effective_context_limit()).await
                {
                    self.last_input_tokens = u.occupancy;
                    let model = self.config.model.clone();
                    self.record_cost(&model, &u);
                    let _ = tx
                        .send(AgentEvent::CostUpdate {
                            usage: self.cost_usage.clone(),
                        })
                        .await;
                }
                let message = response
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("response.failed (no message)")
                    .to_string();
                // A context-overflow can surface mid-stream as a failed
                // event rather than a pre-stream 4xx; the caller recovers.
                return EventFlow::Failed(message);
            }
            ResponseStreamEvent::Error { message } => return EventFlow::Errored(message),
            ResponseStreamEvent::RateLimits(mut snapshot) => {
                // In-stream quota (Codex `codex.rate_limits`). The parser
                // can't stamp the capture time, so do it here.
                if snapshot.captured_at_epoch == 0 {
                    snapshot.captured_at_epoch = chrono::Utc::now().timestamp();
                }
                let _ = tx.send(AgentEvent::Quota { snapshot }).await;
            }
            crate::openai::stream::ResponseStreamEvent::Other { kind } => {
                tracing::debug!(event = %kind, "unknown SSE event");
            }
            _ => {}
        }
        EventFlow::Continue
    }
}

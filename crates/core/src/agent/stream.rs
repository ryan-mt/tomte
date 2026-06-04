//! Split out of `agent` (impl Agent block); logic unchanged.

use super::*;

impl Agent {
    pub(super) async fn run_turn_inner(&mut self, tx: mpsc::Sender<AgentEvent>) -> Result<()> {
        let mut steps = 0usize;
        let mut overflow_recoveries = 0usize;
        let mut stream_recoveries = 0usize;
        let mut fallback_attempts = 0usize;
        // Seeded with the active model so a fallback list that names the current
        // model can't fail over to itself.
        let mut fallback_tried: Vec<String> = vec![self.config.model.clone()];
        'turn: loop {
            steps += 1;
            if steps > MAX_AGENT_STEPS {
                return Err(anyhow::anyhow!(
                    "stopped after {MAX_AGENT_STEPS} tool-call round-trips — \
                     the model may be stuck in a loop; send another message to continue"
                ));
            }
            // Proactively shed stale tool-output bulk before building the next
            // request, so a long tool-using turn stays under the window instead
            // of ballooning into a hard overflow (or a lossy full /compact).
            self.microcompact_tool_outputs();
            let input = {
                let todos = self.session.lock().await.todos.clone();
                input_with_todo_reminder(&self.history, &todos)
            };
            // The model is about to be shown all of current history; from here
            // those items are eligible for microcompaction on a later turn.
            self.history_seen_len = self.history.len();
            let request = ResponsesRequest::new(self.config.model.clone(), input)
                .with_instructions(instructions_for_approval(
                    &self.system_prompt,
                    self.approval,
                ))
                .with_tools(self.registry.definitions())
                .with_reasoning(self.config.reasoning_effort.clone())
                .with_verbosity(self.config.verbosity.clone());
            if wire_debug_enabled() {
                eprintln!(
                    "[tomte wire] → model={} reasoning={:?} verbosity={:?}",
                    request.model, request.reasoning, request.text
                );
            }
            let mut handle = match self.client.stream(request).await {
                Ok(handle) => handle,
                Err(e) => {
                    // Auto-recover from a hard context-window overflow instead of
                    // failing the turn: shed old tool-output bulk and retry, so
                    // the user never has to manually /compact and resend. Bounded,
                    // and only when shedding actually frees space — a history that
                    // can't be shrunk still surfaces the error with the /compact
                    // hint (handled by the TUI).
                    if self.try_recover_overflow(&e.to_string(), &mut overflow_recoveries) {
                        continue;
                    }
                    // A rate-limit / overload on stream open means the request was
                    // rejected before any output was produced or billed — safe to
                    // transparently fail over to a configured fallback model and
                    // retry. Other errors (auth, bad request, refusal, overflow)
                    // fall through and surface as today.
                    if self
                        .try_fail_over(
                            &e.to_string(),
                            &mut fallback_tried,
                            &mut fallback_attempts,
                            &tx,
                        )
                        .await
                    {
                        continue;
                    }
                    return Err(e);
                }
            };

            // Surface the provider quota captured from this response's headers so
            // `/usage` reflects the latest. In-stream Codex quota events are
            // forwarded below from the recv loop.
            if let Some(snapshot) = handle.quota.take() {
                let _ = tx.send(AgentEvent::Quota { snapshot }).await;
            }

            let mut pending_calls: Vec<PendingCall> = Vec::new();
            let mut final_text = String::new();
            // Whether this attempt streamed committed assistant text. Together with
            // any *completed* tool call (see the truncation handler below) this gates
            // stream-truncation handling: with usable content an early EOF finalizes
            // what we have; without it the turn is retried. Reasoning-only output does
            // NOT count — re-running to actually answer is better than ending the turn
            // with only a thought shown.
            let mut produced_output = false;
            // Reasoning blocks captured this turn, in stream order, so they can
            // be replayed ahead of the tool_use that follows (Anthropic
            // adaptive/extended thinking). Each is a ready-to-replay
            // `InputItem::Reasoning`: a signed thinking block (plaintext empty
            // when the model's display is omitted, 4.7/4.8) or a redacted_thinking
            // block carrying only its encrypted data.
            let mut thinking_blocks: Vec<InputItem> = Vec::new();
            let mut orphan_arg_buffers: std::collections::HashMap<String, ToolArgsBuffer> =
                std::collections::HashMap::new();

            loop {
                let recv = tokio::time::timeout(STREAM_IDLE_TIMEOUT, handle.rx.recv()).await;
                let ev = match recv {
                    Err(_) => {
                        // No event for STREAM_IDLE_TIMEOUT — the upstream
                        // stream is stuck. Surface as an error and bail so
                        // the UI unsticks and the user can retry.
                        return Err(anyhow::anyhow!(
                            "stream idle for {}s — connection may be stale, try again",
                            STREAM_IDLE_TIMEOUT.as_secs()
                        ));
                    }
                    Ok(None) => break,
                    Ok(Some(Err(e))) => {
                        // A streamed error can carry an overflow rejection too;
                        // recover the same way as a pre-stream 4xx.
                        if self.try_recover_overflow(&e.to_string(), &mut overflow_recoveries) {
                            continue 'turn;
                        }
                        let msg = e.to_string();
                        // Usable answer content = streamed assistant text, or a tool
                        // call whose arguments fully streamed (`args_done_emitted`). A
                        // call truncated mid-arguments is NOT usable — finalizing it
                        // would dispatch the tool with the empty/partial args the model
                        // never finished — so it doesn't count and the turn is retried.
                        let have_output =
                            produced_output || pending_calls.iter().any(|pc| pc.args_done_emitted);
                        // A recoverable stream failure — the SSE feed ended before
                        // its terminal event, OR a transport drop (TCP reset/decode
                        // error) — after we already received usable answer content.
                        // Finalize it (a soft completion) rather than discarding a
                        // good turn: a transport drop right after a tool call's
                        // arguments fully streamed must still execute that call, not
                        // hard-fail the turn.
                        if (is_stream_truncation_error(&msg) || is_stream_transport_error(&msg))
                            && have_output
                        {
                            // We already received usable answer content this attempt;
                            // finalize it (a soft completion) rather than discarding a
                            // good turn just because the trailing terminator was lost.
                            // Do NOT execute any tool call whose arguments never
                            // reached `args.done`/`output_item.done`: a partial call
                            // may have only a name or a JSON prefix, and treating that
                            // as `{}` would run the wrong command. Surface a skipped
                            // result in the UI and keep only complete calls for the
                            // history/tool execution path below.
                            skip_incomplete_tool_calls_after_truncation(
                                &mut pending_calls,
                                &tx,
                                &msg,
                            )
                            .await;
                            tracing::warn!(
                                error = %msg,
                                "stream ended before terminal event after partial output; finalizing turn"
                            );
                            break;
                        }
                        // Nothing usable yet (no text, no completed tool call): the
                        // connection dropped before the model committed anything, so
                        // re-sending the identical request is safe. Bounded.
                        if (is_stream_truncation_error(&msg) || is_stream_transport_error(&msg))
                            && !have_output
                            && stream_recoveries < MAX_STREAM_RECOVERIES
                        {
                            stream_recoveries += 1;
                            tracing::warn!(
                                attempt = stream_recoveries,
                                error = %msg,
                                "stream ended before any output; retrying turn"
                            );
                            continue 'turn;
                        }
                        return Err(e);
                    }
                    Ok(Some(Ok(v))) => v,
                };
                match self
                    .apply_stream_event(
                        ev,
                        &mut pending_calls,
                        &mut final_text,
                        &mut produced_output,
                        &mut thinking_blocks,
                        &mut orphan_arg_buffers,
                        &tx,
                    )
                    .await
                {
                    EventFlow::Continue => {}
                    EventFlow::Break => break,
                    EventFlow::Failed(message) => {
                        // Only shed-and-retry an overflow *before any answer text was
                        // committed*. Once text has streamed, a `continue 'turn`
                        // re-streams the whole answer and the UI shows it twice — the
                        // same "never duplicate a partial answer" invariant the
                        // fail-over below already upholds. Past that point, surface it.
                        if !produced_output
                            && self.try_recover_overflow(&message, &mut overflow_recoveries)
                        {
                            continue 'turn;
                        }
                        // An overload that surfaces mid-stream *before any answer
                        // text was committed* is as safe to fail over as a
                        // pre-stream rejection: nothing usable was shown or appended
                        // to history yet, so retrying on a fallback model can't
                        // replay output. (Reasoning-only output doesn't count — see
                        // `produced_output`.) Once text has streamed we surface the
                        // error instead, to never duplicate a partial answer.
                        if !produced_output
                            && self
                                .try_fail_over(
                                    &message,
                                    &mut fallback_tried,
                                    &mut fallback_attempts,
                                    &tx,
                                )
                                .await
                        {
                            continue 'turn;
                        }
                        return Err(anyhow::anyhow!("response.failed: {message}"));
                    }
                    EventFlow::Errored(message) => {
                        // See `Failed`: don't shed-and-retry an overflow after answer
                        // text has already streamed, or the retry duplicates it.
                        if !produced_output
                            && self.try_recover_overflow(&message, &mut overflow_recoveries)
                        {
                            continue 'turn;
                        }
                        // Same as `Failed`: a mid-stream overload error event (e.g.
                        // Anthropic's `overloaded_error`) before any committed text
                        // can transparently fail over to a fallback model.
                        if !produced_output
                            && self
                                .try_fail_over(
                                    &message,
                                    &mut fallback_tried,
                                    &mut fallback_attempts,
                                    &tx,
                                )
                                .await
                        {
                            continue 'turn;
                        }
                        return Err(anyhow::anyhow!(message));
                    }
                }
            }

            match self
                .run_tool_phase(pending_calls, final_text, thinking_blocks, &tx)
                .await
            {
                TurnFlow::Done => return Ok(()),
                TurnFlow::Continue => {}
            }
        }
    }
}

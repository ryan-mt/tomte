//! Split out of `agent` (impl Agent block); logic unchanged.

use super::*;

impl Agent {
    pub(super) async fn run_tool_phase(
        &mut self,
        pending_calls: Vec<PendingCall>,
        mut final_text: String,
        mut thinking_blocks: Vec<InputItem>,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> TurnFlow {
        // Append any function calls + their outputs to history, then loop again.
        if pending_calls.is_empty() {
            if !final_text.is_empty() {
                self.history.push(InputItem::Message {
                    role: "assistant".to_string(),
                    content: vec![MessageContent::OutputText { text: final_text }],
                });
            }
            let _ = tx.send(AgentEvent::TurnComplete).await;
            return TurnFlow::Done;
        }

        let mut ctx = ToolContext {
            cwd: self.cwd.clone(),
            approval: self.approval,
            require_approval: self.require_approval,
            auto_approve_edits: self.auto_approve_edits,
            non_interactive: self.non_interactive,
            session: self.session.clone(),
            config: self.config.clone(),
            cwd_override: Arc::new(Mutex::new(None)),
            // Hand tools the live UI channel so sub-agent dispatch can
            // forward fleet-view lifecycle events to the TUI.
            events: Some(tx.clone()),
        };

        // History pushes deferred until after outputs computed (cancel-safety).

        // Split into runnable tasks vs pre-computed errors (malformed JSON,
        // unknown tool name) so the executable set can be driven in parallel
        // and the error set can be surfaced as tool errors without blocking
        // the rest.
        let mut runnable: Vec<RunnableToolCall<'_>> = Vec::new();
        let mut precomputed: Vec<(String, String, bool)> = Vec::new();
        let mut history_args_by_call_id: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let batch_enters_plan_mode = pending_calls.iter().any(|pc| {
            self.registry
                .find(&pc.name)
                .is_some_and(|t| t.name() == "enter_plan_mode")
        });
        for pc in &pending_calls {
            let tool_call_name = pc.name.trim();
            if pc.args.too_large {
                precomputed.push((
                        pc.call_id.clone(),
                        format!(
                            "Error: tool `{}` arguments exceeded the {} byte limit; resend smaller arguments or write data in smaller chunks",
                            display_tool_name(tool_call_name),
                            MAX_TOOL_ARGUMENT_BYTES
                        ),
                        true,
                    ));
                continue;
            }
            if tool_call_name.is_empty() {
                precomputed.push((
                    pc.call_id.clone(),
                    "Error: tool call is missing a function name".to_string(),
                    true,
                ));
                continue;
            }
            let parsed: std::result::Result<Value, _> = if pc.args.text.trim().is_empty() {
                Ok(Value::Object(Default::default()))
            } else {
                serde_json::from_str(&pc.args.text)
            };
            match parsed {
                Err(e) => {
                    // Surface enough detail that the model can self-correct
                    // on the next turn: which tool, which byte offset, what
                    // the parser actually saw at the failure point.
                    let preview = if pc.args.text.len() > 200 {
                        let truncated: String = pc.args.text.chars().take(200).collect();
                        format!("{truncated}…")
                    } else {
                        pc.args.text.clone()
                    };
                    tracing::warn!(
                        tool = %tool_call_name,
                        call_id = %pc.call_id,
                        error = %e,
                        "tool.args.invalid_json"
                    );
                    let hint = schema_hint_suffix(&self.registry, tool_call_name);
                    precomputed.push((
                            pc.call_id.clone(),
                            format!(
                                "Error: tool `{}` arguments are not valid JSON ({e}). Received: {preview}{hint}",
                                tool_call_name
                            ),
                            true,
                        ));
                }
                Ok(args) if !args.is_object() => {
                    let hint = schema_hint_suffix(&self.registry, tool_call_name);
                    precomputed.push((
                            pc.call_id.clone(),
                            format!(
                                "Error: tool `{tool_call_name}` arguments must be a JSON object, got {}{hint}",
                                json_type_label(&args)
                            ),
                            true,
                        ));
                }
                Ok(args) => match self.registry.find(tool_call_name) {
                    Some(t) => {
                        let tool_name = t.name().to_string();
                        let is_effectively_read_only =
                            effective_tool_read_only(&tool_name, &args, t.is_read_only());
                        history_args_by_call_id.insert(
                            pc.call_id.clone(),
                            history_tool_arguments(&tool_name, &args),
                        );
                        let perms = crate::permissions::load(&self.cwd);
                        match preflight_tool_call(
                            &perms,
                            &tool_name,
                            &args,
                            effective_approval_for_tool(
                                self.approval,
                                batch_enters_plan_mode,
                                &tool_name,
                            ),
                            is_effectively_read_only,
                        ) {
                            ToolPreflight::Block(reason) => {
                                precomputed.push((pc.call_id.clone(), reason, true));
                            }
                            ToolPreflight::Proceed { decision } => {
                                let auto_via_accept_edits = self.auto_approve_edits
                                    && EDIT_TOOLS.contains(&tool_name.as_str());
                                let base_gate = self.require_approval
                                    && matches!(
                                        self.approval,
                                        ApprovalMode::OnRequest | ApprovalMode::Manual
                                    )
                                    && !is_effectively_read_only
                                    && !ALWAYS_AUTO_TOOLS.contains(&tool_name.as_str())
                                    && !auto_via_accept_edits;
                                // A classifier-flagged destructive command
                                // (e.g. `rm -rf`, force-push) must be seen by a
                                // human even under an allow rule or bypass, so
                                // the gate forces a prompt / refusal for it.
                                let danger = t.danger_reason(&args);
                                let approved = match approval_outcome(
                                    self.non_interactive,
                                    base_gate,
                                    decision,
                                    danger.is_some(),
                                ) {
                                    ApprovalOutcome::AutoRun => true,
                                    ApprovalOutcome::Deny => false,
                                    ApprovalOutcome::Prompt => {
                                        let diff_preview = t.compute_preview(&args, &ctx).await;
                                        request_tool_approval(
                                            &self.pending_approvals,
                                            tx,
                                            &pc.call_id,
                                            &tool_name,
                                            approval_args_json(&args),
                                            diff_preview,
                                            APPROVAL_TIMEOUT,
                                        )
                                        .await
                                    }
                                };
                                match post_approval_tool_gate(
                                    &self.hooks,
                                    &tool_name,
                                    &args,
                                    approved,
                                )
                                .await
                                {
                                    Ok(()) => runnable.push((pc.call_id.clone(), args, t)),
                                    Err(reason) => {
                                        // A non-interactive run can't prompt. For
                                        // a flagged destructive command, name the
                                        // danger (skip-permissions wouldn't help);
                                        // otherwise steer the model to a read-only
                                        // tool it CAN run rather than dead-ending.
                                        let reason = if !approved && self.non_interactive {
                                            match danger {
                                                Some(d) => format!(
                                                    "Error: refused: {d}. Destructive commands are not allowed in a non-interactive run and cannot be overridden by the model; the command was not executed."
                                                ),
                                                None => non_interactive_blocked_message(&tool_name),
                                            }
                                        } else {
                                            reason
                                        };
                                        precomputed.push((pc.call_id.clone(), reason, true));
                                    }
                                };
                            }
                        }
                    }
                    None => {
                        // The model named a tool that doesn't exist (often a
                        // typo or a guessed name). Offer the closest real
                        // tool name(s) so it can retry with a valid call.
                        let suggestions = crate::tools::suggest_tool_names(
                            tool_call_name,
                            &self.registry.tool_names(),
                        );
                        let msg = if suggestions.is_empty() {
                            format!("Error: unknown tool `{tool_call_name}`.")
                        } else {
                            let list = suggestions
                                .iter()
                                .map(|s| format!("`{s}`"))
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("Error: unknown tool `{tool_call_name}`. Did you mean: {list}?")
                        };
                        precomputed.push((pc.call_id.clone(), msg, true));
                    }
                },
            }
        }

        cap_precomputed_outputs(&mut precomputed);
        // Surface validation/permission errors before any runnable tool can
        // take a long time. History is still appended later in
        // `pending_calls` order, so provider transcripts remain stable.
        for (call_id, output, is_error) in &precomputed {
            let _ = tx
                .send(AgentEvent::ToolResult {
                    call_id: call_id.clone(),
                    output: output.clone(),
                    error: *is_error,
                })
                .await;
        }

        // Execute known-safe read/search tools in parallel, but serialize
        // every side-effecting or session-mutating tool in transcript
        // order. This keeps fast multi-read turns fast without allowing
        // `run_shell`, writes, approvals, or control tools to race each
        // other.
        let mut results: Vec<(String, String, bool)> = Vec::new();
        let mut parallel_batch: Vec<RunnableToolCall<'_>> = Vec::new();
        for (call_id, args, tool) in runnable {
            if is_parallel_safe_tool_call(tool, &args) {
                parallel_batch.push((call_id, args, tool));
                continue;
            }
            if !parallel_batch.is_empty() {
                let batch = std::mem::take(&mut parallel_batch);
                results.extend(
                    execute_parallel_tool_batch(batch, ctx.clone(), tx.clone(), self.hooks.clone())
                        .await,
                );
            }
            results.push(
                execute_builtin_tool_call(
                    call_id,
                    args,
                    tool,
                    ctx.clone(),
                    tx.clone(),
                    self.hooks.clone(),
                )
                .await,
            );
            apply_cwd_override(&mut ctx).await;
        }
        if !parallel_batch.is_empty() {
            results.extend(
                execute_parallel_tool_batch(
                    parallel_batch,
                    ctx.clone(),
                    tx.clone(),
                    self.hooks.clone(),
                )
                .await,
            );
        }

        if ctx.cwd != self.cwd {
            self.cwd = ctx.cwd.clone();
            self.refresh_system_context();
        }

        results.extend(precomputed);

        // Append outputs to history in the original call order so the
        // model sees a deterministic transcript even when completion
        // order shuffled.
        let should_stop_for_user_input_tool = pending_calls.iter().any(|pc| {
            self.registry
                .find(&pc.name)
                .is_some_and(|t| matches!(t.name(), "ask_user_question" | "exit_plan_mode"))
                && results
                    .iter()
                    .any(|(id, _, is_err)| id == &pc.call_id && !*is_err)
        });
        let should_enter_plan_mode = pending_calls.iter().any(|pc| {
            self.registry
                .find(&pc.name)
                .is_some_and(|t| t.name() == "enter_plan_mode")
                && results
                    .iter()
                    .any(|(id, _, is_err)| id == &pc.call_id && !*is_err)
        });
        if should_enter_plan_mode {
            self.approval = ApprovalMode::Plan;
        }
        let mut by_id: std::collections::HashMap<String, (String, bool)> = results
            .into_iter()
            .map(|(id, out, is_err)| (id, (out, is_err)))
            .collect();
        // Record the assistant's narration that preceded the tool calls
        // BEFORE the function-call items, so the transcript (and the next
        // turn's context, and any resumed session) keeps what the model said.
        // Without this, an "I'll read that file…" preamble vanished whenever
        // a response mixed text with tool calls (the only other push of
        // assistant text lives in the no-tool-calls branch).
        // Replay the signed thinking block ahead of the assistant text and
        // the tool_use it produced, so Anthropic keeps the reasoning chain
        // across the loop (and manual-mode models accept the tool turn).
        // Reached only on the tool-call path — the no-tool branch above
        // already finalized. Adaptive validation tolerates its absence, but
        // preserving it stops the model re-deliberating from scratch each
        // step (a real token-burn driver at high effort).
        for item in std::mem::take(&mut thinking_blocks) {
            self.history.push(item);
        }
        if !final_text.is_empty() {
            self.history.push(InputItem::Message {
                role: "assistant".to_string(),
                content: vec![MessageContent::OutputText {
                    text: std::mem::take(&mut final_text),
                }],
            });
        }
        for pc in &pending_calls {
            if let Some((output, is_error)) = by_id.remove(&pc.call_id) {
                append_tool_result_history(
                    &mut self.history,
                    &self.registry,
                    &pc.call_id,
                    &pc.name,
                    output,
                    is_error,
                    history_args_by_call_id.remove(&pc.call_id),
                );
            }
        }

        // Push a todos snapshot so the UI can render `/todos` and a
        // status-line hint without having to lock the agent itself.
        // Cheap: clones the vector (typically <20 items).
        let todos_snapshot = {
            let session = self.session.lock().await;
            session.todos.clone()
        };
        let _ = tx
            .send(AgentEvent::TodosSnapshot {
                todos: todos_snapshot,
            })
            .await;
        if should_stop_for_user_input_tool {
            let _ = tx.send(AgentEvent::TurnComplete).await;
            return TurnFlow::Done;
        }
        TurnFlow::Continue
    }
}

//! Split out of `agent` (impl Agent block); logic unchanged.

use super::*;

impl Agent {
    /// Pillar 5 (A2 Tier 2) — run the conscience self-check over the batch's edit
    /// calls *before* any runs, returning the conflicts keyed by call_id. Only
    /// fires in `conscience = "check"` mode; an edit to a file with no recorded
    /// decisions, the trail store itself, or a CLEAR verdict yields no entry.
    /// Done as a pre-pass so the model call doesn't fight the `self.registry`
    /// borrow the main tool loop holds.
    async fn conscience_precheck(
        &self,
        pending_calls: &[PendingCall],
    ) -> std::collections::HashMap<String, ConscienceConflictInfo> {
        let mut conflicts = std::collections::HashMap::new();
        if !self.config.conscience_checks() {
            return conflicts;
        }
        for pc in pending_calls {
            let name = pc.name.trim();
            if !matches!(name, "edit_file" | "write_file" | "multi_edit") {
                continue;
            }
            let Some(args) = conscience_change_args(name, &pc.args.text) else {
                continue;
            };
            let Some((file, change)) = crate::conscience::change_summary(name, &args) else {
                continue;
            };
            // Never run the conscience on the trail store itself (no recursion).
            if file.ends_with("decisions.jsonl") {
                continue;
            }
            let decisions = crate::decisions::for_file(&self.cwd, &file);
            if decisions.is_empty() {
                continue;
            }
            let crate::conscience::ConscienceVerdict::Conflict { ts, reason } =
                self.conscience_verdict(&file, &decisions, &change).await
            else {
                continue;
            };
            // Resolve the cited decision; on an unknown ts fall back to the most
            // recent one so the card still names something real.
            let prev = decisions
                .iter()
                .find(|d| d.ts == ts)
                .or_else(|| decisions.last());
            let (ts, prev_decision, prev_model) = match prev {
                Some(d) => (d.ts, d.decision.clone(), d.model.clone()),
                None => (ts, String::new(), String::new()),
            };
            conflicts.insert(
                pc.call_id.clone(),
                ConscienceConflictInfo {
                    file,
                    ts,
                    prev_decision,
                    prev_model,
                    reason,
                },
            );
        }
        conflicts
    }

    pub(super) async fn run_tool_phase(
        &mut self,
        pending_calls: Vec<PendingCall>,
        mut final_text: String,
        mut thinking_blocks: Vec<InputItem>,
        tx: &mpsc::Sender<AgentEvent>,
        // Auto-capture accumulators (Pillar 2), set across the turn's phases via
        // the caller's `&mut`: did a real file edit land, and did the model
        // already record a decision itself? The end-of-turn self-check reads them.
        turn_mutated: &mut bool,
        turn_recorded: &mut bool,
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

        // Pillar 5 (A2 Tier 2) — run the conscience self-check over this batch's
        // edits before any runs, so a conflict can interrupt the edit at the gate
        // (keyed by call_id; empty unless conscience = "check").
        let conscience_conflicts = self.conscience_precheck(&pending_calls).await;

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
            let parsed = parse_tool_call_arguments(&pc.args.text);
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
                            &self.cwd,
                        ) {
                            ToolPreflight::Block(reason) => {
                                precomputed.push((pc.call_id.clone(), reason, true));
                            }
                            ToolPreflight::Proceed { decision } => {
                                // Gate inputs, computed once for both the
                                // conscience-conflict path and the normal path below.
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
                                // A classifier-flagged destructive command (e.g.
                                // `rm -rf`, force-push) must be seen by a human even
                                // under an allow rule or bypass, so the gate forces a
                                // prompt / refusal for it.
                                let danger = t.danger_reason(&args);
                                // Pillar 5 (A2 Tier 2) — a conscience conflict for
                                // this edit takes over the gate: the human chooses
                                // abort / supersede / edit-anyway. Interactively the
                                // card IS the approval; headless it must not be more
                                // permissive than the baseline gate (see below).
                                if let Some(conflict) = conscience_conflicts.get(&pc.call_id) {
                                    let choice = if self.non_interactive {
                                        ConscienceChoice::EditAnyway
                                    } else {
                                        request_conscience_decision(
                                            &self.pending_conscience,
                                            tx,
                                            &pc.call_id,
                                            &tool_name,
                                            &conflict.file,
                                            conflict.ts,
                                            &conflict.prev_decision,
                                            &conflict.prev_model,
                                            &conflict.reason,
                                            APPROVAL_TIMEOUT,
                                        )
                                        .await
                                    };
                                    if matches!(choice, ConscienceChoice::Abort) {
                                        precomputed.push((
                                            pc.call_id.clone(),
                                            format!(
                                                "Error: aborted — this edit conflicts with recorded decision #{} ({}). Not applied; reconcile with the decision or supersede it.",
                                                conflict.ts, conflict.reason
                                            ),
                                            true,
                                        ));
                                        continue;
                                    }
                                    // Supersede / EditAnyway chosen. Interactive: the
                                    // human's choice stands in for the gate. Headless:
                                    // nobody saw the card, so fall back to the baseline
                                    // gate — the conscience may only ADD friction, never
                                    // turn a headless edit the gate would DENY into an
                                    // executed write. Either way, still honor a
                                    // PreToolUse hook.
                                    let approved = if self.non_interactive {
                                        matches!(
                                            approval_outcome(
                                                self.non_interactive,
                                                base_gate,
                                                decision,
                                                danger.is_some(),
                                            ),
                                            ApprovalOutcome::AutoRun
                                        )
                                    } else {
                                        true
                                    };
                                    match post_approval_tool_gate(
                                        &self.hooks,
                                        &tool_name,
                                        &args,
                                        approved,
                                    )
                                    .await
                                    {
                                        Ok(()) => {
                                            // Only now that the edit cleared the gate and
                                            // will run do we commit the override: record a
                                            // supersede to the trail (Supersede only) and
                                            // announce it. Deferring past the gate means a
                                            // blocked edit leaves no supersede record and
                                            // no "decision overturned" card for a write
                                            // that never happened.
                                            let superseding =
                                                matches!(choice, ConscienceChoice::Supersede);
                                            if superseding {
                                                let rec = crate::decisions::DecisionRecord {
                                                    loc: conflict.file.clone(),
                                                    decision: format!(
                                                        "override approved for the edit to {}",
                                                        conflict.file
                                                    ),
                                                    why: format!(
                                                        "human supersede of #{} (\"{}\") — {}",
                                                        conflict.ts,
                                                        conflict.prev_decision,
                                                        conflict.reason
                                                    ),
                                                    rejected: Vec::new(),
                                                    model: self.config.model.clone(),
                                                    ts: crate::session::now_ms(),
                                                    anchor: None,
                                                    supersedes: Some(conflict.ts),
                                                };
                                                if let Err(e) =
                                                    crate::decisions::append(&self.cwd, &rec)
                                                {
                                                    tracing::warn!(error = %e, "conscience: failed to append supersede record");
                                                }
                                            } else if self.non_interactive {
                                                tracing::warn!(
                                                    file = %conflict.file,
                                                    ts = conflict.ts,
                                                    "conscience: edited over a decision without an override (headless run)"
                                                );
                                            }
                                            let _ = tx
                                                .send(AgentEvent::DecisionOverturned {
                                                    file: conflict.file.clone(),
                                                    prev_decision: conflict.prev_decision.clone(),
                                                    prev_model: conflict.prev_model.clone(),
                                                    reason: conflict.reason.clone(),
                                                    recorded: superseding,
                                                })
                                                .await;
                                            runnable.push((pc.call_id.clone(), args, t));
                                        }
                                        Err(reason) => {
                                            let reason = if !approved && self.non_interactive {
                                                match danger {
                                                    Some(d) => format!(
                                                        "Error: refused: {d}. Destructive commands are not allowed in a non-interactive run and cannot be overridden by the model; the command was not executed."
                                                    ),
                                                    None => {
                                                        non_interactive_blocked_message(&tool_name)
                                                    }
                                                }
                                            } else {
                                                reason
                                            };
                                            precomputed.push((pc.call_id.clone(), reason, true));
                                        }
                                    }
                                    continue;
                                }
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
                                            approval_args_json(&tool_name, &args),
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

        // SOUL Pillar 1 — glass-box pre-flight. Before the runnable set executes,
        // announce each consequential call (a write or a shell command) so an
        // auto-approved action is legible *before* it happens, not just narrated
        // as it runs. Reads/searches warrant no card (`preflight_card` -> None).
        // Visibility only — the approval gate above is untouched.
        for (call_id, args, tool) in &runnable {
            let read_only = effective_tool_read_only(tool.name(), args, tool.is_read_only());
            if let Some(card) =
                preflight_card(tool.name(), args, read_only, tool.danger_reason(args))
            {
                // Pillar 5 (A2 Tier 1): surface the target file's recorded
                // decisions as "house rules" right as an edit is about to land —
                // recall at the moment of risk, pure surfacing (never a gate).
                // Only the file-targeting mutating tools carry a path to look up.
                let house_rules = if self.config.conscience_surfaces() {
                    match tool.name() {
                        // Look the path up under every alias the executing tool
                        // accepts — a `filePath` edit runs fine, so its house
                        // rules must not silently fail to surface.
                        "edit_file" | "write_file" | "multi_edit" => edit_path_argument(args)
                            .map(|p| crate::decisions::house_rules(&ctx.cwd, p))
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                // Pillar 3 — the Context Manifest: the first time this session
                // edits a file, show the twin's X-ray of it (pulling X because
                // <real edge> · read/not read; leaving out Y because <reason>)
                // so the context behind the edit is proven, not assumed.
                // Cache-only (never builds the twin inline) and once per file;
                // computed off the async runtime since it stats the tree.
                let context_manifest = match tool.name() {
                    "edit_file" | "write_file" | "multi_edit" => {
                        match edit_path_argument(args).map(str::to_string) {
                            Some(path) => {
                                let (first_time, read_files) = {
                                    let mut session = ctx.session.lock().await;
                                    (
                                        session.manifested_files.insert(path.clone()),
                                        session.read_files.clone(),
                                    )
                                };
                                if first_time {
                                    let cwd = ctx.cwd.clone();
                                    tokio::task::spawn_blocking(move || {
                                        crate::context_manifest::for_edit(&cwd, &path, &read_files)
                                    })
                                    .await
                                    .unwrap_or_default()
                                } else {
                                    Vec::new()
                                }
                            }
                            None => Vec::new(),
                        }
                    }
                    _ => Vec::new(),
                };
                let _ = tx
                    .send(AgentEvent::PreFlight {
                        call_id: call_id.clone(),
                        scope: card.scope,
                        leash: card.leash,
                        house_rules,
                        context_manifest,
                    })
                    .await;
            }
        }

        // Execute known-safe read/search tools in parallel, but serialize
        // every side-effecting or session-mutating tool in transcript
        // order. This keeps fast multi-read turns fast without allowing
        // `run_shell`, writes, approvals, or control tools to race each
        // other.
        let mut results: Vec<(String, String, bool, Vec<crate::openai::ToolMedia>)> = Vec::new();
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

        // Precomputed (gate refusals / errors) carry no media; widen to the
        // 4-tuple so they join `results` from the executed calls.
        results.extend(
            precomputed
                .into_iter()
                .map(|(id, out, err)| (id, out, err, Vec::new())),
        );

        // Note, for end-of-turn auto-capture (Pillar 2): did this phase land a
        // real file edit, and did the model already record a decision itself? A
        // tool counts only when its result is non-error. The flags accumulate
        // across the turn via the caller's `&mut`.
        for pc in &pending_calls {
            let landed = results
                .iter()
                .any(|(id, _, is_err, _)| id == &pc.call_id && !*is_err);
            if !landed {
                continue;
            }
            match capture_kind(pc.name.trim()) {
                Some(CaptureKind::Mutated) => *turn_mutated = true,
                Some(CaptureKind::Recorded) => *turn_recorded = true,
                None => {}
            }
        }

        // Append outputs to history in the original call order so the
        // model sees a deterministic transcript even when completion
        // order shuffled.
        let should_stop_for_user_input_tool = pending_calls.iter().any(|pc| {
            self.registry
                .find(&pc.name)
                .is_some_and(|t| matches!(t.name(), "ask_user_question" | "exit_plan_mode"))
                && results
                    .iter()
                    .any(|(id, _, is_err, _)| id == &pc.call_id && !*is_err)
        });
        let should_enter_plan_mode = pending_calls.iter().any(|pc| {
            self.registry
                .find(&pc.name)
                .is_some_and(|t| t.name() == "enter_plan_mode")
                && results
                    .iter()
                    .any(|(id, _, is_err, _)| id == &pc.call_id && !*is_err)
        });
        if should_enter_plan_mode {
            self.approval = ApprovalMode::Plan;
        }
        let mut by_id: std::collections::HashMap<
            String,
            (String, bool, Vec<crate::openai::ToolMedia>),
        > = results
            .into_iter()
            .map(|(id, out, is_err, media)| (id, (out, is_err, media)))
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
        let completed: Vec<CompletedCall> = pending_calls
            .iter()
            .filter_map(|pc| {
                let (output, is_error, media) = by_id.remove(&pc.call_id)?;
                Some(CompletedCall {
                    call_id: pc.call_id.clone(),
                    raw_name: pc.name.clone(),
                    output,
                    is_error,
                    media,
                    canonical_args: history_args_by_call_id.remove(&pc.call_id),
                })
            })
            .collect();
        append_step_history(&mut self.history, &self.registry, completed);

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

/// Parse a tool call's raw `arguments` text into a JSON value.
///
/// Provider-agnostic tolerance, so a model quirk costs zero round-trips:
/// - empty/whitespace arguments (some models send none for no-arg tools) → `{}`;
/// - a double-encoded payload — a JSON *string* whose content is itself a JSON
///   object — is unwrapped exactly one level (several providers/models emit
///   that shape when they stringify the object twice).
///
/// Anything else (including a plain string that is not an object) is returned
/// as parsed, so the caller's "arguments must be a JSON object" check still
/// rejects genuinely wrong shapes with a self-correct hint.
pub(crate) fn parse_tool_call_arguments(text: &str) -> Result<Value, serde_json::Error> {
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let v: Value = serde_json::from_str(text)?;
    if let Value::String(inner) = &v {
        if let Ok(obj @ Value::Object(_)) = serde_json::from_str::<Value>(inner) {
            return Ok(obj);
        }
    }
    Ok(v)
}

/// Resolve one edit-class call's arguments for the conscience pre-check with
/// the same tolerance the executing tool phase applies — the raw-text parse
/// (empty → `{}`, double-encoded payload unwrapped) plus the alias fold the
/// history uses (`filePath`/`file_path` → `path`, `oldString` → `old_string`,
/// …). Without it, a call the tool phase happily executes can slip past the
/// conscience purely on spelling.
fn conscience_change_args(tool_name: &str, text: &str) -> Option<Value> {
    let args = parse_tool_call_arguments(text).ok()?;
    Some(canonical_history_arguments(tool_name, &args).unwrap_or(args))
}

#[cfg(test)]
mod arg_parse_tests {
    use super::parse_tool_call_arguments;
    use serde_json::{json, Value};

    #[test]
    fn empty_and_whitespace_arguments_become_an_empty_object() {
        assert_eq!(parse_tool_call_arguments("").unwrap(), json!({}));
        assert_eq!(parse_tool_call_arguments("  \n ").unwrap(), json!({}));
    }

    #[test]
    fn double_encoded_object_is_unwrapped_one_level() {
        // arguments = "\"{\\\"path\\\":\\\"a.rs\\\"}\"" — a string holding an object.
        let outer = serde_json::to_string(&json!({"path": "a.rs"}).to_string()).unwrap();
        assert_eq!(
            parse_tool_call_arguments(&outer).unwrap(),
            json!({"path": "a.rs"})
        );
    }

    #[test]
    fn plain_object_and_wrong_shapes_pass_through_unchanged() {
        assert_eq!(
            parse_tool_call_arguments(r#"{"a":1}"#).unwrap(),
            json!({"a":1})
        );
        // A bare string that is NOT an object stays a string, so the caller's
        // "must be a JSON object" rejection (with schema hint) still fires.
        assert_eq!(
            parse_tool_call_arguments(r#""hello""#).unwrap(),
            Value::String("hello".into())
        );
        // A double-encoded ARRAY is not unwrapped — only objects are.
        let arr = serde_json::to_string(&json!([1, 2]).to_string()).unwrap();
        assert!(matches!(
            parse_tool_call_arguments(&arr).unwrap(),
            Value::String(_)
        ));
        // Truly invalid JSON still errors for the self-correct path.
        assert!(parse_tool_call_arguments("{not json").is_err());
    }

    #[test]
    fn conscience_args_get_the_same_tolerance_as_execution() {
        // Double-encoded edit arguments with alias spellings: the tool phase
        // unwraps them and the tool's serde aliases accept them, so the edit
        // RUNS — the conscience must see the same resolved object, not skip.
        let inner = json!({"filePath": "src/a.rs", "oldString": "x", "newString": "y"});
        let outer = serde_json::to_string(&inner.to_string()).unwrap();
        let args = super::conscience_change_args("edit_file", &outer).unwrap();
        assert_eq!(args["path"], "src/a.rs");
        assert_eq!(args["old_string"], "x");
        assert_eq!(args["new_string"], "y");
        // Unparseable arguments still skip (the call bounces before running).
        assert!(super::conscience_change_args("edit_file", "{nope").is_none());
    }
}

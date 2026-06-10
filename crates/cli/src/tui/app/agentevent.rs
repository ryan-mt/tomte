//! Agent-event application to UI blocks. Split out of `app`; logic unchanged.

use super::*;

pub fn apply_agent_event(app: &mut App, ev: AgentEvent) {
    // Note: we deliberately do NOT force auto_scroll=true on every event — that
    // caused the chat to snap back to the bottom whenever a new delta arrived,
    // making manual scrolling impossible while the agent was streaming. The
    // scroll behaviour is: stay where the user put it; resume auto-follow only
    // when the user scrolls back to the bottom (handled in `render_chat`), or
    // when the user sends a new message (handled in `handle_key`/the queue
    // flush in `main_loop`).
    match ev {
        AgentEvent::AssistantTextDelta { text } => {
            // First text deltas terminate the thinking phase.
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            if let Some(Block::Assistant { text: buf, .. }) =
                last_assistant_mut_open(&mut app.blocks)
            {
                buf.push_str(&text);
            }
        }
        AgentEvent::AssistantTextDone { text } => {
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            if let Some(Block::Assistant { text: buf, .. }) =
                last_assistant_mut_open(&mut app.blocks)
            {
                *buf = text;
            }
        }
        AgentEvent::ReasoningDelta { text } => {
            app.is_thinking = true;
            if let Some(Block::Assistant {
                reasoning,
                reasoning_started_at,
                ..
            }) = last_assistant_mut_open(&mut app.blocks)
            {
                if reasoning_started_at.is_none() {
                    *reasoning_started_at = Some(std::time::Instant::now());
                }
                reasoning.push_str(&text);
            }
        }
        AgentEvent::ReasoningDone { text } => {
            if let Some(Block::Assistant {
                reasoning,
                reasoning_started_at,
                ..
            }) = last_assistant_mut_open(&mut app.blocks)
            {
                if reasoning_started_at.is_none() {
                    *reasoning_started_at = Some(std::time::Instant::now());
                }
                *reasoning = text;
            }
        }
        AgentEvent::ToolCallStarted { name, call_id } => {
            app.blocks.push(Block::Tool {
                call_id,
                name,
                args: String::new(),
                output: None,
                error: false,
                preflight: None,
            });
        }
        AgentEvent::ToolCallArgsDelta { call_id, delta } => {
            if let Some(Block::Tool { args, .. }) = find_tool_mut(&mut app.blocks, &call_id) {
                args.push_str(&delta);
            }
        }
        AgentEvent::ToolCallArgsDone { call_id, arguments } => {
            if let Some(Block::Tool { args, .. }) = find_tool_mut(&mut app.blocks, &call_id) {
                if !arguments.is_empty() {
                    *args = arguments;
                }
            }
        }
        AgentEvent::ToolResult {
            call_id,
            output,
            error,
        } => {
            let mut ask_prompt = None;
            let mut did_ask_user = false;
            if let Some(Block::Tool {
                name,
                output: o,
                error: e,
                ..
            }) = find_tool_mut(&mut app.blocks, &call_id)
            {
                *o = Some(output);
                *e = error;
                if name == "ask_user_question" && !error {
                    did_ask_user = true;
                    ask_prompt = o
                        .as_deref()
                        .and_then(tomte_core::tools::ask::render_ask_envelope);
                }
            }
            if let Some(prompt) = ask_prompt {
                app.blocks.push(Block::System(prompt));
            }
            if did_ask_user {
                if let Some(goal) = app.active_goal.as_mut() {
                    goal.waiting_for_user = true;
                    goal.last_summary = Some("waiting for user input".into());
                    app.pending_session_save = true;
                }
            }
            // Tool result terminates the reasoning phase too.
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            // Rotate the open assistant block: close (or remove if empty) any
            // currently-open assistant block, then push a fresh one so that
            // subsequent reasoning/text renders BELOW this tool result. Without
            // this, empty Assistant blocks accumulated between back-to-back
            // tool calls.
            rotate_assistant_block(&mut app.blocks);
            // No render-cache work: the mutated tool block lives in the live
            // tail (the current turn), which `render_chat` re-wraps every frame.
        }
        AgentEvent::PreFlight {
            call_id,
            scope,
            leash,
            house_rules,
            context_manifest,
        } => {
            // SOUL Pillar 1: attach the glass-box card to its tool block so the
            // next frame shows WHAT the call will do and HOW FAR it reaches,
            // before its result lands. The block lives in the live tail, which
            // re-wraps every frame, so no render-cache work is needed.
            if let Some(Block::Tool { preflight, .. }) = find_tool_mut(&mut app.blocks, &call_id) {
                *preflight = Some(PreFlight {
                    scope,
                    leash,
                    house_rules,
                    context_manifest,
                });
            }
        }
        AgentEvent::CwdChanged { cwd } => {
            app.cwd = std::path::PathBuf::from(&cwd);
            app.blocks.push(Block::System(format!("cwd → {cwd}")));
            app.auto_scroll = true;
        }
        AgentEvent::TurnComplete => {
            collapse_reasoning_into_thought(app);
            app.is_thinking = false;
            finish_open_assistant_block(&mut app.blocks);
            // SOUL Pillar 4: leave a tidy "left in order" receipt of what this
            // turn changed (files / tests / why), pushed as the turn's last block.
            // A pure Q&A turn that changed nothing produces no receipt.
            if let Some(receipt) = build_turn_summary(&app.blocks) {
                app.blocks.push(receipt);
            }
            // `busy` flipping false moves the stable boundary to the end of the
            // transcript; the next frame wraps the settled turn once, appends it
            // to the render cache, and idle frames after that are pure hits.
            app.busy = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;
            // The fleet view is per-turn: clear finished sub-agents now that the
            // turn is over so the panel collapses away.
            app.subagents.clear();
            schedule_goal_continuation(app);

            // Drain the message queue — send EVERYTHING as one combined prompt.
            if !app.message_queue.is_empty() {
                // Defer to handler in main_loop via a tick: rebuild a fake key event isn't ideal,
                // so we expose a helper to launch directly. Stash the merged text on app.
                app.status_line = "(flushing queued messages…)".into();
            }
        }
        AgentEvent::GoalStatusUpdated { status, summary } => {
            let status = status.trim().to_ascii_lowercase();
            match status.as_str() {
                "complete" => {
                    if let Some(goal) = app.active_goal.take() {
                        app.pending_goal_replacement = None;
                        remove_pending_goal_continuations(&mut app.message_queue);
                        app.pending_session_save = true;
                        app.blocks.push(Block::System(format!(
                            "Goal complete: {}\n{summary}",
                            goal.objective
                        )));
                    }
                }
                "blocked" => {
                    if let Some(goal) = app.active_goal.take() {
                        app.pending_goal_replacement = None;
                        remove_pending_goal_continuations(&mut app.message_queue);
                        app.pending_session_save = true;
                        app.blocks.push(Block::System(format!(
                            "Goal blocked: {}\n{summary}",
                            goal.objective
                        )));
                    }
                }
                "in_progress" => {
                    if let Some(goal) = app.active_goal.as_mut() {
                        goal.last_summary = Some(summary);
                        app.pending_session_save = true;
                    }
                }
                _ => {
                    if let Some(goal) = app.active_goal.as_mut() {
                        goal.last_summary = Some(format!("unknown goal status `{status}`"));
                        app.pending_session_save = true;
                    }
                }
            }
        }
        AgentEvent::PlanExitRequested { plan } => {
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = true;
                goal.last_summary = Some("waiting for plan approval".into());
                app.pending_session_save = true;
            }
            app.pending_plan_exit = Some(PendingPlanExit { plan: plan.clone() });
            app.blocks.push(Block::System(format!(
                "Plan ready for approval:\n{plan}\n\nPress Y to approve and leave plan mode, or N/Esc to keep planning."
            )));
            app.auto_scroll = true;
        }
        AgentEvent::PlanModeRequested => {
            app.set_permission_mode(PermissionMode::Plan);
            app.pending_plan_exit = None;
            if let Some(goal) = app.active_goal.as_mut() {
                goal.last_summary = Some("entered plan mode".into());
                app.pending_session_save = true;
            }
            app.blocks.push(Block::System(
                "plan mode → on (read-only tools only; write/edit/shell will be blocked)".into(),
            ));
            app.auto_scroll = true;
        }
        AgentEvent::Usage {
            input_tokens,
            output_tokens: _,
            total_tokens: _,
        } => {
            // Current context occupancy, NOT a running total. Each agentic step
            // resends the whole history, so this is the live window size of the
            // last request (cache read/creation already folded in). Cumulative
            // billing is tracked per-model by the agent and arrives separately
            // via CostUpdate, split by billing class for an accurate /cost.
            app.tokens_used = input_tokens;
        }
        AgentEvent::CostUpdate { usage } => {
            app.usage_by_model = usage;
        }
        AgentEvent::Quota { snapshot } => {
            // Latest real provider quota; rendered on demand by `/usage`.
            app.last_quota = Some(snapshot);
        }
        AgentEvent::TodosSnapshot { todos } => {
            if todos.is_empty() && should_keep_recent_completed_todos_for_empty_snapshot(app) {
                return;
            }
            let was_empty = app.session_todos.is_empty();
            update_todo_completion_timestamps(app, &todos);
            app.session_todos = todos;
            if was_empty && !app.session_todos.is_empty() {
                app.show_todos = true;
            }
        }
        AgentEvent::Error { message } => {
            collapse_reasoning_into_thought(app);
            finish_open_assistant_block(&mut app.blocks);
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = true;
                goal.last_summary = Some(format!("paused after turn error: {message}"));
                app.pending_session_save = true;
            }
            if is_context_overflow(&message) {
                // The provider rejected the request because the conversation
                // outgrew its real context window before compaction could free
                // space — usually because a custom provider's true window is
                // smaller than the catalog's guess-by-model-name. Surface an
                // actionable recovery path instead of a raw API error.
                let mut msg = String::from(
                    "⚠ context overflow — this conversation is larger than the model's context window.\n   \
                     Run /compact to summarize older history, then resend your message.",
                );
                if app.config.model.split_once('/').is_some() {
                    msg.push_str(&format!(
                        "\n   tomte assumed a {}-token window for this provider; set its real `context_limit` in config.json so it compacts in time.",
                        app.config.effective_context_limit()
                    ));
                }
                app.blocks.push(Block::System(msg));
            } else {
                app.blocks.push(Block::System(format!("error: {message}")));
            }
            app.busy = false;
            app.is_thinking = false;
            app.turn_started_at = None;
            app.status_line.clear();
            app.current_turn = None;
            app.subagents.clear();
        }
        AgentEvent::FallbackSwitched { from, to, .. } => {
            // Adopt the fallback as the session's model so the status bar is
            // accurate and the next turn (which rebuilds the agent from
            // `app.config`) stays on it. Not persisted to disk: this is a
            // transient reaction to a rate-limit, not a user-chosen `/model`.
            app.config.model = to.clone();
            app.blocks.push(Block::System(format!(
                "⚠ {from} is rate-limited/overloaded — switched to fallback model {to}"
            )));
        }
        AgentEvent::ContextWarning { used, limit } => {
            let pct = (used as f64 / limit.max(1) as f64 * 100.0) as u64;
            app.blocks.push(Block::System(format!(
                "context {used}/{limit} tokens ({pct}%) - consider /compact"
            )));
        }
        AgentEvent::AutoCompactSuggested { used, limit } => {
            let pct = (used as f64 / limit.max(1) as f64 * 100.0) as u64;
            // Stronger signal than ContextWarning — at 85% we are one or two
            // turns away from a hard 1xx context-window failure on the next
            // request. Block::System makes the message persistent in the
            // scrollback so the user can't miss it while scrolling.
            // With auto_compact on, also schedule a real compaction (once per
            // over-threshold window — the guard clears after it succeeds) so a
            // long session never hits a hard context overflow unattended.
            if app.config.auto_compact && !app.auto_compact_done_this_window {
                app.pending_compact = true;
                app.auto_compact_done_this_window = true;
                app.blocks.push(Block::System(format!(
                    "⚠ context {used}/{limit} tokens ({pct}%) — auto-compacting to free space…"
                )));
            } else {
                app.blocks.push(Block::System(format!(
                    "⚠ context {used}/{limit} tokens ({pct}%) — run /compact now to avoid a context overflow on the next turn"
                )));
            }
        }
        AgentEvent::CompactDone {
            original_len,
            error,
        } => match error {
            // Success: snap the bar to 100% and let main_loop hold it briefly
            // before swapping in the result line. Reclaiming context lets auto-
            // compaction fire again in a future over-threshold window.
            None => {
                app.compact_done_at = Some(std::time::Instant::now());
                app.compact_result_msg = Some(format!(
                    "✓ compacted: {original_len} items → 1 summary. Earlier history is now summarized."
                ));
                app.auto_compact_done_this_window = false;
            }
            // Failure / no-op: tear the bar down immediately (no celebratory
            // 100%) and report why.
            Some(e) => {
                app.compacting = false;
                app.compact_started_at = None;
                app.compact_done_at = None;
                app.compact_result_msg = None;
                app.blocks
                    .push(Block::System(format!("compact skipped: {e}")));
                app.auto_scroll = true;
                // Re-arm auto-compaction: the success arm clears this flag, but a
                // failed summary (e.g. the summary request itself overflowed) must
                // not leave auto-compaction disarmed for the rest of the
                // over-threshold window — otherwise the session silently drifts
                // into a hard context overflow. A later 85% tick can retry once
                // the real turn's emergency shed has freed space.
                app.auto_compact_done_this_window = false;
            }
        },
        AgentEvent::ApprovalRequest {
            call_id,
            tool_name,
            args_json,
            diff_preview,
        } => {
            app.pending_approval = Some(PendingApproval {
                call_id,
                tool_name,
                args_json,
                diff_preview,
                selected: 0,
            });
        }
        AgentEvent::ApprovalGranted { call_id } => {
            if app
                .pending_approval
                .as_ref()
                .is_some_and(|p| p.call_id == call_id)
            {
                app.pending_approval = None;
            }
        }
        AgentEvent::ApprovalDenied { call_id } => {
            if app
                .pending_approval
                .as_ref()
                .is_some_and(|p| p.call_id == call_id)
            {
                app.pending_approval = None;
            }
        }
        AgentEvent::ConscienceConflict {
            call_id,
            tool_name: _,
            file,
            ts,
            prev_decision,
            prev_model,
            reason,
        } => {
            app.pending_conscience = Some(PendingConscience {
                call_id,
                file,
                ts,
                prev_decision,
                prev_model,
                reason,
                selected: 0,
            });
        }
        AgentEvent::DecisionOverturned {
            file,
            prev_decision,
            prev_model,
            reason,
            recorded,
        } => {
            // Pillar 5 (A3 — On the Record): surface the override as an audit line.
            let tag = if recorded { "superseded" } else { "overridden" };
            app.blocks.push(Block::System(format!(
                "↩ {tag} decision in {file} — {prev_model}'s \"{prev_decision}\" — {reason}"
            )));
        }
        AgentEvent::SubagentStarted {
            id,
            subagent_type,
            prompt,
        } => {
            app.subagents.push(SubagentView {
                id,
                kind: subagent_type,
                prompt,
                activity: "starting".into(),
                tokens: 0,
                started_at: std::time::Instant::now(),
                done: None,
                expanded: false,
            });
        }
        AgentEvent::SubagentActivity { id, summary } => {
            if let Some(s) = app.subagents.iter_mut().find(|s| s.id == id) {
                s.activity = summary;
            }
        }
        AgentEvent::SubagentTokens { id, output_tokens } => {
            if let Some(s) = app.subagents.iter_mut().find(|s| s.id == id) {
                s.tokens = output_tokens;
            }
        }
        AgentEvent::SubagentDone { id, ok } => {
            if let Some(s) = app.subagents.iter_mut().find(|s| s.id == id) {
                s.done = Some(ok);
                s.activity = if ok { "done".into() } else { "failed".into() };
            }
        }
    }
}

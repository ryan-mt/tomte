//! Resume, compaction, and history-to-blocks rebuild. Split out of `app`; logic unchanged.

use super::*;

/// Carry out a resume after the picker has set `pending_resume_id`. Loads the
/// session from disk, rebuilds visible blocks from the persisted history, and
/// replaces the agent's in-memory state in place.
pub async fn apply_resume(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    id: &str,
) {
    let record = match tomte_core::session::load(&app.cwd, id) {
        Ok(r) => r,
        Err(e) => {
            app.blocks
                .push(Block::System(format!("resume failed: {e}")));
            return;
        }
    };
    let preview = record.meta.preview.clone();
    let msg_count = record.meta.message_count;
    let restored_todos = record.state.todos.clone();
    let restored_goal = record.state.active_goal.clone();
    let restored_usage = record.state.usage.clone();

    // Rebuild visible blocks BEFORE we hand the history off to the agent so
    // we still own the record's contents.
    let rebuilt = rebuild_blocks_from_history(&record.history);

    {
        let mut guard = agent.lock().await;
        if guard.is_none() {
            // Lazily construct an Agent so we can stash the restored state
            // on it. The client will be created on the next turn via
            // launch_turn, which rebuilds it from the active credential.
            let client = match LlmClient::for_config(&app.config).await {
                Ok(c) => c,
                Err(e) => {
                    // The id was already taken by main_loop, so the resume intent
                    // would otherwise be silently dropped. Reopen the picker so the
                    // user can retry — user-driven, so no auto-retry loop.
                    app.blocks.push(Block::System(format!(
                        "resume client error: {e} — pick a session to retry"
                    )));
                    app.open_overlay(OverlayKind::ResumePicker);
                    return;
                }
            };
            let mut a = Agent::new(client, app.config.clone());
            a.require_approval = app.require_approval;
            a.auto_approve_edits = app.auto_approve_edits;
            a.cwd = app.cwd.clone();
            a.approval = app.approval;
            a.apply_environment();
            a.apply_project_memory();
            a.apply_memory_store();
            a.apply_decision_trail();
            a.apply_skill_manifest();
            a.load_mcp().await.ok();
            a.restore_from(record);
            *guard = Some(a);
        } else if let Some(a) = guard.as_mut() {
            a.restore_from(record);
        }
    }

    app.blocks.clear();
    // Inline mode: the restored transcript re-commits to scrollback from the top.
    app.committed_blocks = 0;
    app.blocks.push(Block::Welcome);
    app.blocks.extend(rebuilt);
    app.blocks.push(Block::System(format!(
        "↻ resumed: {preview} ({msg_count} messages)"
    )));
    app.session_todos = restored_todos;
    app.todo_completed_at.clear();
    app.usage_by_model = restored_usage;
    app.active_goal = restored_goal.map(ActiveGoal::from_session_snapshot);
    app.pending_goal_replacement = None;
    app.pending_plan_exit = None;
    if !app.session_todos.is_empty() {
        app.show_todos = true;
    }
    if let Some(goal) = &app.active_goal {
        if goal.waiting_for_user {
            app.status_line = "(goal paused for user input)".into();
        } else {
            queue_goal_continuation(app);
        }
    }
    app.auto_scroll = true;
}

/// Carry out a rewind after the picker set `pending_rewind_ordinal`: run the
/// agent's `rewind_to` (revert file edits + truncate the conversation), then
/// rebuild the visible transcript from the truncated history and print the calm
/// end-of-rewind summary. Mirrors `apply_resume`'s block-rebuild + scroll reset.
pub async fn apply_rewind(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    ordinal: usize,
) {
    let (outcome, history) = {
        let mut guard = agent.lock().await;
        match guard.as_mut() {
            Some(a) => match a.rewind_to(ordinal).await {
                Ok(out) => (out, a.history.clone()),
                Err(e) => {
                    app.blocks
                        .push(Block::System(format!("rewind failed: {e}")));
                    return;
                }
            },
            None => {
                app.blocks
                    .push(Block::System("nothing to rewind yet".to_string()));
                return;
            }
        }
    };

    let rebuilt = rebuild_blocks_from_history(&history);
    app.blocks.clear();
    // Inline mode: re-commit the truncated transcript to scrollback from the top
    // (the already-emitted rewound turns stay in native scrollback above, exactly
    // as `/resume` behaves — we can't retract them, but the live view is correct).
    app.committed_blocks = 0;
    app.blocks.push(Block::Welcome);
    app.blocks.extend(rebuilt);
    app.blocks.push(Block::System(rewind_summary(&outcome)));
    app.auto_scroll = true;
    // Persist the rewound history so a later `/resume` picks up the rewound state,
    // not the pre-rewind conversation still on disk.
    app.pending_session_save = true;
}

/// The calm, one-glance end-of-rewind summary (Pillar 4): what it restored, and
/// honestly, what it could not (externally-changed files, shell side effects).
fn rewind_summary(o: &tomte_core::tools::RewindOutcome) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut s = format!("↩ rewound to: {}", o.label);
    s.push_str(&format!(
        "\n  · dropped {} turn{} from the conversation",
        o.turns_dropped,
        plural(o.turns_dropped)
    ));
    s.push_str(&format!(
        "\n  · reverted {} file{}",
        o.files_reverted,
        plural(o.files_reverted)
    ));
    if !o.files_skipped.is_empty() {
        let names = o
            .files_skipped
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!(
            "\n  · left {} file{} changed outside tomte as-is: {names}",
            o.files_skipped.len(),
            plural(o.files_skipped.len())
        ));
    }
    if o.shell_effects > 0 {
        s.push_str(&format!(
            "\n  · {} shell side effect{} since then could NOT be undone",
            o.shell_effects,
            plural(o.shell_effects)
        ));
    }
    s
}

/// Kick off real compaction in the BACKGROUND (mirrors `launch_turn`'s spawn):
/// a task locks the agent, summarizes the history and REPLACES it with the
/// summary, persists, then reports back via `AgentEvent::CompactDone`. Running
/// off the main loop is what keeps the UI responsive and the progress bar
/// animating instead of freezing on the model call. Returns immediately.
/// Match a provider's "input too large for the context window" rejection so the
/// raw API error becomes an actionable /compact hint. Delegates to the core
/// heuristic the agent's auto-recovery also uses, keeping one source of truth.
pub fn is_context_overflow(message: &str) -> bool {
    tomte_core::agent::is_context_overflow_message(message)
}

pub fn start_compaction(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
) {
    app.compacting = true;
    app.compact_started_at = Some(std::time::Instant::now());
    app.compact_done_at = None;
    app.compact_result_msg = None;
    let goal_snapshot = app
        .active_goal
        .as_ref()
        .map(ActiveGoal::to_session_snapshot);
    if goal_snapshot.is_some() {
        app.pending_session_save = true;
    }
    // Consume any `/compact <focus>` steer for this run (auto-compaction leaves
    // it None); taking it here clears it so the next compaction starts fresh.
    let focus = app.compact_focus.take();
    let agent = agent.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = {
            let mut guard = agent.lock().await;
            match guard.as_mut() {
                Some(a) => {
                    let r = a.compact_history(focus.as_deref()).await;
                    // Persist the compacted history so /resume picks up the
                    // smaller baseline (compaction runs outside the per-turn
                    // save path).
                    if r.is_ok() {
                        let mut record = a.to_session_record().await;
                        record.state.active_goal = goal_snapshot.clone();
                        if let Err(e) = tomte_core::session::save(&record) {
                            tracing::debug!(error = %e, "session save after compact failed");
                        }
                    }
                    r
                }
                None => Err(anyhow::anyhow!("no agent yet — nothing to compact")),
            }
        };
        let ev = match result {
            Ok(original_len) => AgentEvent::CompactDone {
                original_len: original_len as u64,
                error: None,
            },
            Err(e) => AgentEvent::CompactDone {
                original_len: 0,
                error: Some(e.to_string()),
            },
        };
        let _ = tx.send(ev).await;
    });
}

/// Reconstruct chat-visible blocks from a persisted history. The Responses
/// API history is a flat list of `InputItem`s; we group function_call +
/// function_call_output by call_id and drop the reasoning items (they were
/// only ever streamed deltas).
pub fn rebuild_blocks_from_history(history: &[tomte_core::openai::InputItem]) -> Vec<Block> {
    use std::collections::HashMap;
    use tomte_core::openai::{InputItem, MessageContent};

    let mut outputs: HashMap<String, (String, bool)> = HashMap::new();
    for item in history {
        if let InputItem::FunctionCallOutput {
            call_id,
            output,
            error,
            ..
        } = item
        {
            outputs.insert(call_id.clone(), (output.clone(), *error));
        }
    }

    let mut blocks: Vec<Block> = Vec::new();
    for item in history {
        match item {
            InputItem::Message { role, content } => {
                let mut text = String::new();
                for c in content {
                    match c {
                        MessageContent::InputText { text: t } => text.push_str(t),
                        MessageContent::OutputText { text: t } => text.push_str(t),
                        MessageContent::InputImage { .. } => text.push_str("[image]"),
                    }
                }
                if role == "user" {
                    blocks.push(Block::User(text));
                } else if role == "assistant" {
                    blocks.push(Block::Assistant {
                        text,
                        reasoning: String::new(),
                        done: true,
                        thought_for_secs: None,
                        reasoning_started_at: None,
                        thinking_expanded: false,
                    });
                }
            }
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let (output, error) = outputs
                    .get(call_id)
                    .cloned()
                    .map(|(o, e)| (Some(o), e))
                    .unwrap_or((None, false));
                blocks.push(Block::Tool {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    args: arguments.clone(),
                    output,
                    error,
                    // A resumed session replays only the recorded transcript; the
                    // pre-flight is a live, in-the-moment card, so it stays None.
                    preflight: None,
                });
            }
            InputItem::FunctionCallOutput { .. } => { /* attached above */ }
            InputItem::Reasoning { .. } => { /* not persisted visually */ }
        }
    }
    blocks
}

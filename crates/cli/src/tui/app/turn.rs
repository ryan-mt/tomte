//! Turn launch/cancel and mouse click handling. Split out of `app`; logic unchanged.

use super::*;

pub async fn launch_turn(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
    text: String,
) {
    // UserPromptSubmit hook: may BLOCK the prompt (exit 2). Load hooks fresh
    // (cheap) so it works even on the first turn before the agent exists.
    if let tomte_core::hooks::HookDecision::Block(reason) = tomte_core::hooks::load()
        .fire_user_prompt_submit(&text)
        .await
    {
        app.blocks
            .push(Block::System(format!("⛔ prompt blocked: {reason}")));
        return;
    }
    // Fold in any composer-prefix context the user staged before this prompt:
    // `!` shell output (prepended) and `@file` references (file contents appended).
    // The hook above and the visible Block::User both see only the raw prompt;
    // this enrichment is invisible plumbing sent to the model.
    //
    // `@`-expansion runs over the user's prompt ONLY — not the prepended shell
    // output — so a `@token` that merely appears in a command's stdout doesn't
    // get its file attached unexpectedly.
    let text = {
        let attached = composer::expand_at_mentions(&text, &app.cwd);
        let mut t = text;
        if !app.pending_shell_context.is_empty() {
            let ctx = std::mem::take(&mut app.pending_shell_context).join("\n\n");
            t = format!("{ctx}\n\n{t}");
        }
        if let Some(attached) = attached {
            t = format!("{t}\n\n{attached}");
        }
        t
    };
    if let Some(goal) = app.active_goal.as_mut() {
        goal.waiting_for_user = false;
        app.pending_session_save = true;
    }
    let should_defer_session_save_to_host = app.active_goal.is_some();
    let client = match LlmClient::for_config(&app.config).await {
        Ok(c) => c,
        Err(e) => {
            app.blocks.push(Block::System(format!("Client error: {e}")));
            return;
        }
    };
    {
        let mut guard = agent.lock().await;
        if guard.is_none() {
            let mut a = Agent::new(client, app.config.clone());
            a.require_approval = app.require_approval;
            a.auto_approve_edits = app.auto_approve_edits;
            a.cwd = app.cwd.clone();
            a.approval = app.approval;
            a.apply_project_memory();
            a.apply_memory_store();
            a.apply_decision_trail();
            a.apply_skill_manifest();
            a.load_mcp().await.ok();
            *guard = Some(a);
        } else {
            // Update mutable config every turn so /model, /effort, /plan take effect.
            if let Some(a) = guard.as_mut() {
                let cwd_changed = a.cwd != app.cwd;
                a.client = client;
                a.config = app.config.clone();
                a.cwd = app.cwd.clone();
                a.approval = app.approval;
                a.require_approval = app.require_approval;
                a.auto_approve_edits = app.auto_approve_edits;
                if cwd_changed {
                    a.refresh_system_context();
                }
            }
        }
        if let Some(a) = guard.as_mut() {
            if app.pending_images.is_empty() {
                a.push_user_message(text);
            } else {
                let imgs = std::mem::take(&mut app.pending_images);
                a.push_user_message_with_images(text, &imgs);
            }
        }
    }
    {
        // Snapshot the agent's pending_approvals Arc so Y/N keystrokes don't
        // need to grab the outer mutex (which run_turn holds for the duration).
        let guard = agent.lock().await;
        if let Some(a) = guard.as_ref() {
            app.approval_handle = Some(a.pending_approvals.clone());
            app.conscience_handle = Some(a.pending_conscience.clone());
        }
    }
    if should_defer_session_save_to_host {
        // Persist the just-queued user/goal prompt before the long-running
        // model turn starts. The post-turn save is still deferred to the host
        // so `goal_update complete|blocked` cannot be overwritten by the
        // launch-time goal snapshot.
        save_current_session_record(app, agent).await;
    }
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());
    app.spinner_seed = pick_spinner_seed();
    app.turn_count = app.turn_count.saturating_add(1);
    app.status_line.clear();
    app.blocks.push(Block::Assistant {
        text: String::new(),
        reasoning: String::new(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
    });
    let agent_clone = agent.clone();
    let tx_clone = tx.clone();
    let handle = tokio::spawn(async move {
        let mut guard = agent_clone.lock().await;
        if let Some(a) = guard.as_mut() {
            if let Err(e) = a.run_turn(tx_clone.clone()).await {
                tracing::debug!(error = %e, "agent turn failed");
            }
            // Persist the conversation after every turn so /resume can pick
            // it up later. Failure to save is logged at debug level only —
            // we don't want to spam the chat with disk-error popups, and the
            // session is still safe in memory for the remainder of the run.
            //
            // When `/goal` is active, host-owned state may change DURING the
            // turn (for example `goal_update complete`). The TUI saves the
            // latest host state immediately after the turn finishes, so avoid
            // writing a stale launch-time goal snapshot here.
            if !should_defer_session_save_to_host {
                let record = a.to_session_record().await;
                if let Err(e) = tomte_core::session::save(&record) {
                    tracing::debug!(error = %e, "session save failed");
                }
            }
        }
    });
    // Remember the task so Esc can abort it while it's still running.
    if let Some(prev) = app.current_turn.replace(handle) {
        prev.abort();
    }
}

/// Abort the in-flight turn (if any) and reset transient UI state. Used by the
/// Esc handler when a turn appears stuck (e.g. SSE stalled, model thinking
/// forever) so the user can recover without killing the app.
pub async fn cancel_current_turn(app: &mut App) {
    if let Some(handle) = app.current_turn.take() {
        handle.abort();
    }
    app.busy = false;
    app.is_thinking = false;
    app.turn_started_at = None;
    app.status_line.clear();
    // The aborted turn won't emit TurnComplete, so collapse the fleet view here.
    app.subagents.clear();
    if let Some(goal) = app.active_goal.take() {
        app.pending_goal_replacement = None;
        remove_pending_goal_continuations(&mut app.message_queue);
        app.blocks
            .push(Block::System(format!("Stopped goal: {}", goal.objective)));
        app.pending_session_save = true;
    }
    // Drop any in-flight approval modal: leaving pending_approval=Some after
    // a cancel makes handle_key intercept every keystroke, locking the user
    // out of the input box with no way to recover short of restarting.
    if let Some(pending) = app.pending_approval.take() {
        if let Some(handle) = app.approval_handle.clone() {
            let sender = {
                let mut map = handle.lock().await;
                map.remove(&pending.call_id)
            };
            if let Some(sender) = sender {
                let _ = sender.send(false);
            }
        }
    }
    app.approval_handle = None;
    // Same for an in-flight conscience-conflict card: resolve it as Abort so a
    // cancelled turn never silently overwrites a recorded decision, and the
    // keystroke interceptor is released.
    if let Some(pending) = app.pending_conscience.take() {
        if let Some(handle) = app.conscience_handle.clone() {
            let sender = {
                let mut map = handle.lock().await;
                map.remove(&pending.call_id)
            };
            if let Some(sender) = sender {
                let _ = sender.send(tomte_core::agent::ConscienceChoice::Abort);
            }
        }
    }
    app.conscience_handle = None;
    // Close the open assistant block, then surface a small note so the user
    // can see the cancel happened.
    if let Some(Block::Assistant { done, .. }) = last_assistant_mut_open(&mut app.blocks) {
        *done = true;
    }
    app.blocks
        .push(Block::System("(cancelled — Esc)".to_string()));
}

/// True when the (col, row) terminal cell falls inside `r`. Mouse hit-test for
/// the clickable jump-to-bottom bar and fleet-view rows.
pub fn point_in(r: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Act on a plain left-click (no drag): click the jump-to-bottom bar to resume
/// tail-following, or click a fleet row to toggle its detail. Mirrors the
/// click targets the mouse-down handler used before drag selection existed.
pub fn handle_left_click(app: &mut App, col: u16, row: u16) {
    if app
        .jump_to_bottom_hint
        .is_some_and(|r| point_in(r, col, row))
    {
        app.auto_scroll = true;
    } else if let Some(id) = app
        .subagent_rows
        .iter()
        .find(|(r, _)| point_in(*r, col, row))
        .map(|(_, id)| id.clone())
    {
        if let Some(s) = app.subagents.iter_mut().find(|s| s.id == id) {
            s.expanded = !s.expanded;
        }
    }
}

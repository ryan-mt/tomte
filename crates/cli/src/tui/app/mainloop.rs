//! The main event loop. Split out of `app`; logic unchanged.

use super::*;

pub async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    start_with_resume_picker: bool,
    plan_mode_required: bool,
) -> Result<()> {
    let mut app = App::new();
    if plan_mode_required {
        apply_plan_mode_required(&mut app);
    }
    if start_with_resume_picker && app.screen == Screen::Chat {
        app.start_with_resume_picker = true;
    }
    let mut events = EventStream::new();
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(256);
    // Background `!`-command output flows back here so it lands on the UI thread
    // (the command itself runs off the event loop — see `handle_bang_shell`).
    let (bang_tx, mut bang_rx) = mpsc::channel::<BangResult>(8);
    // Persistent agent kept across turns to preserve history.
    let agent: std::sync::Arc<tokio::sync::Mutex<Option<Agent>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));

    // Cap redraws to a frame budget while a turn is streaming. The agent emits
    // many token deltas per frame interval and each draw re-wraps the whole
    // transcript; without the cap that runs hundreds of times/sec and shows up
    // as visible jank. When idle (`!busy`) we draw immediately so keystrokes
    // still echo without latency.
    let frame_budget = Duration::from_millis(16);
    let mut last_draw = std::time::Instant::now()
        .checked_sub(frame_budget)
        .unwrap_or_else(std::time::Instant::now);

    loop {
        if app.should_exit {
            break;
        }
        // Open the resume picker once on first frame when launched via
        // `opencli resume`. Guarded so re-entry (eg. after Esc) doesn't pop
        // the picker back open unexpectedly.
        if app.start_with_resume_picker && app.screen == Screen::Chat && app.overlay.is_none() {
            app.start_with_resume_picker = false;
            app.open_overlay(OverlayKind::ResumePicker);
        }
        // Resume picker leaves the chosen session id here; perform the load
        // out-of-band so handle_overlay_select doesn't need the agent Arc.
        // Deferred while compacting OR busy: a background compaction/turn holds
        // the agent mutex, so `apply_resume` locking it here would stall the main
        // loop — and a streaming turn that then fills the 256-cap agent_rx would
        // block forever on `tx.send`, a hard deadlock (the resume picker can be
        // opened mid-turn). `&&` short-circuits before `.take()`, so the id is
        // kept until the turn finishes.
        if app.can_run_deferred_agent_op() {
            if let Some(id) = app.pending_resume_id.take() {
                apply_resume(&mut app, &agent, &id).await;
            }
        }
        // `/undo` sets this so the agent Arc can stay out of handle_slash.
        // Deferred while compacting OR busy for the same agent-mutex reason as
        // the resume load above (left side of `&&` short-circuits, so the flag
        // survives until the turn/compaction finishes).
        if app.can_run_deferred_agent_op() && std::mem::take(&mut app.pending_undo) {
            let result = {
                let mut g = agent.lock().await;
                match g.as_mut() {
                    Some(a) => a.undo_last_edit().await,
                    None => Err(anyhow::anyhow!("no agent yet — nothing to undo")),
                }
            };
            let msg = match result {
                Ok(s) => s,
                Err(e) => format!("undo: {e}"),
            };
            app.blocks.push(Block::System(msg));
        }
        // `/compact` and the auto-compact trigger set this so the agent Arc can
        // stay out of the slash/event handlers. Gated on `!busy` so it never
        // runs mid-turn (it locks the same mutex `run_turn` holds). Don't use
        // `mem::take` here: the auto trigger fires while `busy` is still true
        // (AutoCompactSuggested precedes TurnComplete), and consuming the flag
        // before the `!busy` check would silently drop that compaction. Clear
        // it only when we actually start. Runs in the BACKGROUND (a spawned
        // task) so the main loop keeps ticking and animates the progress bar.
        if app.pending_compact && !app.busy && !app.compacting && app.screen == Screen::Chat {
            app.pending_compact = false;
            start_compaction(&mut app, &agent, &agent_tx);
        }
        if app.pending_session_save && !app.busy && !app.compacting && app.screen == Screen::Chat {
            save_current_session_record(&mut app, &agent).await;
        }
        // After a successful compaction the bar holds at 100% briefly, then we
        // swap in the result line and tear the bar down. The 80ms select tick
        // keeps redrawing during the hold.
        if let Some(done_at) = app.compact_done_at {
            if done_at.elapsed() >= Duration::from_millis(450) {
                app.compacting = false;
                app.compact_started_at = None;
                app.compact_done_at = None;
                if let Some(msg) = app.compact_result_msg.take() {
                    app.blocks.push(Block::System(msg));
                }
                app.auto_scroll = true;
            }
        }
        // Safety net: a compaction task that died WITHOUT reporting (e.g. an
        // unexpected panic) would otherwise pin `compacting` true forever and
        // wedge the queue/undo/resume gates. A panic unwinds and drops the
        // agent MutexGuard, so the lock is free again — force-clearing here is
        // safe. A live compaction is bounded by STREAM_IDLE_TIMEOUT (120s) and
        // a short summary, so it never reaches this 150s backstop.
        if app.compacting && app.compact_done_at.is_none() {
            if let Some(started) = app.compact_started_at {
                if started.elapsed() >= Duration::from_secs(150) {
                    app.compacting = false;
                    app.compact_started_at = None;
                    app.blocks
                        .push(Block::System("compact timed out — try again".into()));
                    app.auto_scroll = true;
                }
            }
        }
        // Flush the message queue after a turn completes. Deferred while
        // compacting: launch_turn would block on the agent mutex the
        // compaction task holds, re-freezing the UI.
        //
        // Also deferred while a Y/N decision is pending (plan approval or goal
        // replacement): a turn finishing with `exit_plan_mode` leaves `!busy`
        // with the approval prompt up, but type-ahead messages the user queued
        // while the agent was planning would otherwise flush here and relaunch a
        // turn — flipping `busy` back to true. With `pending_plan_exit` set AND
        // `busy`, the key router swallows the Y keystroke (see handle_key), so
        // the user can never approve the plan and the UI looks frozen. Hold the
        // queue until the decision is resolved; approving/rejecting clears the
        // pending state and queues its own follow-up, which then flushes.
        if !app.busy
            && !app.compacting
            && !app.message_queue.is_empty()
            && app.screen == Screen::Chat
            && !turn_launch_blocked_by_pending_decision(&app)
        {
            let queued = std::mem::take(&mut app.message_queue);
            app.status_line.clear();
            // Process items individually: dispatch slash commands in order and
            // accumulate normal messages for a single launch_turn. Flushing
            // pending normal messages before each slash command preserves order
            // and avoids sending a raw `/command` string to the model.
            //
            // Crucially, AT MOST ONE turn may be launched per flush: launch_turn
            // sets `busy` and spawns a task that holds the agent mutex for the
            // whole turn. A second launch_turn would block on that mutex while
            // the main loop is stalled here (so `select!` never drains the
            // 256-cap agent_rx), and the running turn would in turn block on a
            // full channel — a hard deadlock. So when we must launch a turn with
            // items still pending, re-queue the remainder and stop; the next
            // flush (after the turn completes) picks them up.
            let mut normal: Vec<String> = Vec::new();
            let mut iter = queued.into_iter();
            let mut launched = false;
            while let Some(item) = iter.next() {
                // Composer-prefix items (`/`, `!`, `#`) are commands, not model
                // input — dispatch them in order instead of sending them to the
                // model verbatim. To preserve ordering, any pending normal text
                // is flushed as a turn first (then we requeue and stop, since
                // launching a turn must be the last action — see the deadlock
                // note above).
                let is_command =
                    item.starts_with('/') || item.starts_with('!') || item.starts_with('#');
                if is_command {
                    if normal.is_empty() {
                        if let Some(rest) = item.strip_prefix('/') {
                            handle_slash(&mut app, rest.trim()).await;
                        } else if let Some(cmd) = item.strip_prefix('!') {
                            composer::handle_bang_shell(&mut app, &bang_tx, cmd.trim());
                        } else if let Some(note) = item.strip_prefix('#') {
                            composer::handle_hash_memory(&mut app, &agent, note.trim()).await;
                        }
                    } else {
                        let combined = std::mem::take(&mut normal).join("\n\n");
                        let mut requeue = vec![item.clone()];
                        requeue.extend(iter.by_ref());
                        app.message_queue = requeue;
                        push_visible_user_block(&mut app, &combined);
                        app.auto_scroll = true;
                        launch_turn(&mut app, &agent, &agent_tx, combined).await;
                        launched = true;
                        break;
                    }
                } else {
                    normal.push(item);
                }
            }
            if !launched && !normal.is_empty() {
                let combined = normal.join("\n\n");
                push_visible_user_block(&mut app, &combined);
                app.auto_scroll = true;
                launch_turn(&mut app, &agent, &agent_tx, combined).await;
            }
        }

        // Poll login completion (the spawned OAuth flow writes auth.json).
        // Snapshot stage + error exactly once per frame so the transition
        // check and render see a consistent view (the OAuth task can mutate
        // the shared mutex between two reads otherwise).
        let mut login_render: Option<(LoginStage, Option<String>)> = None;
        if app.screen == Screen::Login {
            let stage = app.login.stage().await;
            let login_err = app.login.error_text().await;
            match &stage {
                LoginStage::Success(mode) => {
                    app.auth_mode = *mode;
                    app.screen = Screen::Chat;
                    app.login = LoginScreen::new();
                }
                LoginStage::Cancelled => break,
                _ => {}
            }
            // Only render login if still on login screen (transition may have
            // just fired) — avoids passing a Success snapshot to render.
            if app.screen == Screen::Login {
                login_render = Some((stage, login_err));
            }
        }

        prune_expired_completed_todos(&mut app);

        // Finish the hatch animation once it has run its course, adopting the
        // companion (the loop keeps redrawing it via the idle tick below).
        if app.hatch.as_ref().is_some_and(|h| {
            h.started.elapsed() >= Duration::from_millis(crate::tui::buddy::HATCH_MS)
        }) {
            finish_hatch(&mut app);
        }

        if !app.busy || last_draw.elapsed() >= frame_budget {
            let completed = terminal.draw(|f| {
                app.last_width = f.area().width;
                app.last_height = f.area().height;
                match app.screen {
                    Screen::Login => {
                        if let Some((stage, login_err)) = login_render.as_ref() {
                            login::render(f, f.area(), &app.login, stage, login_err.as_deref());
                        }
                    }
                    Screen::Chat => ui::render(f, &mut app),
                }
            })?;
            // While a drag is active, keep a copy of the rendered frame so the
            // selected text can be read back on release (cheap no-op otherwise).
            if app.selection.is_some() {
                app.last_buffer = Some(completed.buffer.clone());
            }
            last_draw = std::time::Instant::now();
        }

        tokio::select! {
            biased;
            maybe_ev = events.next() => {
                let Some(ev) = maybe_ev else { break; };
                match ev {
                    Ok(Event::Key(key)) => {
                        if key.kind != KeyEventKind::Press { continue; }
                        match app.screen {
                            Screen::Login => {
                                if app.login.handle_key(key).await? {
                                    let stage = app.login.stage().await;
                                    if let LoginStage::Success(mode) = stage {
                                        app.auth_mode = mode;
                                        app.screen = Screen::Chat;
                                        app.login = LoginScreen::new();
                                    } else if matches!(stage, LoginStage::Cancelled) {
                                        break;
                                    }
                                }
                            }
                            Screen::Chat => {
                                if handle_key(&mut app, key, &agent, &agent_tx, &bang_tx).await? { break; }
                            }
                        }
                    }
                    Ok(Event::Resize(_, _)) => {}
                    // Bracketed paste: the whole clipboard arrives as one event,
                    // so multi-line text lands in the composer for editing
                    // instead of submitting on the first newline.
                    Ok(Event::Paste(text))
                        if app.screen == Screen::Chat && app.pending_approval.is_none() =>
                    {
                        // Insert in one shift (not char-by-char) so a large paste
                        // isn't O(n²); strip CR so CRLF clipboards don't double up.
                        let cleaned: String = text.chars().filter(|&c| c != '\r').collect();
                        app.input.insert_str(&cleaned);
                        app.history_pos = None;
                    }
                    Ok(Event::Mouse(m)) => {
                        use crossterm::event::{MouseButton, MouseEventKind};
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                app.scroll = app.scroll.saturating_sub(3);
                                app.auto_scroll = false;
                                // Content moves under a screen-coordinate
                                // selection, so drop it rather than mislead.
                                app.clear_selection();
                            }
                            MouseEventKind::ScrollDown => {
                                app.scroll = app.scroll.saturating_add(3);
                                app.clear_selection();
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                // Begin a selection. The click target (jump /
                                // fleet toggle) only fires on release if no drag
                                // happened — see Up(Left) below.
                                app.copy_notice = None;
                                app.selection =
                                    Some(selection::Selection::new(m.column, m.row));
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                if let Some(sel) = app.selection.as_mut() {
                                    sel.cursor = (m.column, m.row);
                                }
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                if let Some(mut sel) = app.selection.take() {
                                    sel.cursor = (m.column, m.row);
                                    if sel.is_dragged() {
                                        app.finish_selection(sel);
                                    } else {
                                        // A plain click → act on the target.
                                        handle_left_click(&mut app, m.column, m.row);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Some(ev) = agent_rx.recv() => {
                apply_agent_event(&mut app, ev);
                // Drain the rest of the burst so a fast token stream produces
                // one redraw per frame, not one redraw per token.
                while let Ok(ev) = agent_rx.try_recv() {
                    apply_agent_event(&mut app, ev);
                }
                // Tail-following streamed output scrolls the chat under a
                // screen-coordinate selection highlight; drop it so the
                // highlight doesn't slide onto unrelated text. When the user is
                // parked above the tail (auto_scroll off) the view is stable, so
                // a selection of history stays put.
                if app.auto_scroll && app.selection.is_some() {
                    app.clear_selection();
                }
            }
            Some(r) = bang_rx.recv() => {
                // A background `!`-command finished; show its output and stage it
                // as context for the next message.
                app.blocks.push(Block::System(r.display));
                app.pending_shell_context.push(r.context);
                app.auto_scroll = true;
            }
            _ = tokio::time::sleep(Duration::from_millis(if app.hatch.is_some() { 45 } else { 80 })) => {
                // Wake periodically so the spinner redraws while a turn runs and
                // no agent events arrive; the glyph itself advances on elapsed
                // wall-clock time in render_spinner, not on a counter.
            }
        }
    }
    Ok(())
}

/// Finish the hatch animation: adopt the pet (locked to the account) and let
/// the small corner companion take over.
pub fn finish_hatch(app: &mut App) {
    if let Some(h) = app.hatch.take() {
        app.buddy_pet = Some(h.pet);
        app.buddy_hidden = false;
    }
}

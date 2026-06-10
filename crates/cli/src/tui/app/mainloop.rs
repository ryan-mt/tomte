//! The main event loop. Split out of `app`; logic unchanged.

use super::*;

/// Upper bound on how many already-buffered input events we coalesce into one
/// batch per loop turn. A paste arrives as a burst the OS delivers at once;
/// draining it together lets us tell a pasted newline from a deliberate Enter
/// and insert the whole paste in a single redraw. The cap only bounds a
/// pathological flood — the drain otherwise ends the instant input pauses for
/// longer than `PASTE_COALESCE_GAP`.
const PASTE_BURST_MAX: usize = 200_000;

/// Once a burst is in progress, how long to wait for the next event before
/// declaring the paste over. The EventStream reader thread parses pasted bytes
/// in chunks and briefly leaves the channel empty between them; a poll that
/// gives up the instant the channel is empty (`now_or_never`) split one paste
/// across loop turns, and a newline stranded in a resulting 1-event batch then
/// submitted mid-paste — the "long paste fires off partial messages" bug.
/// Waiting this long bridges those sub-millisecond gaps, yet stays well under a
/// human inter-keystroke interval so deliberate typing never coalesces.
const PASTE_COALESCE_GAP: Duration = Duration::from_millis(15);

pub async fn main_loop(
    terminal: &mut Tty,
    start_with_resume_picker: bool,
    resume_latest: bool,
    plan_mode_required: bool,
) -> Result<()> {
    let mut app = App::new();
    if plan_mode_required {
        apply_plan_mode_required(&mut app);
    }
    if start_with_resume_picker && app.screen == Screen::Chat {
        app.start_with_resume_picker = true;
    }
    // `tomte --continue`: seed the most recent session id so the deferred resume
    // path (further down the loop) loads it on the first eligible frame — the
    // same mechanism the picker uses, minus the picker. A directory with no
    // saved session starts fresh with a one-line note.
    if resume_latest && app.screen == Screen::Chat {
        match tomte_core::session::latest_id(&app.cwd) {
            Some(id) => app.pending_resume_id = Some(id),
            None => app.blocks.push(Block::System(
                "No previous session in this directory to continue — starting fresh.".into(),
            )),
        }
    }
    // First-run setup card: if an important external tool (git) is missing, drop
    // a one-time block with the OS-specific install command. We only show the
    // command — never run an installer. A no-op on a ready machine (the usual
    // case), so it never nags.
    seed_setup_card(&mut app);
    let mut events = EventStream::new();
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentEvent>(256);
    // Background `!`-command output flows back here so it lands on the UI thread
    // (the command itself runs off the event loop — see `handle_bang_shell`).
    let (bang_tx, mut bang_rx) = mpsc::channel::<BangResult>(8);
    // A background `/prove` collection sends its rendered capsule back here so it
    // lands on the UI thread (the test/build commands run off the event loop).
    let (prove_tx, mut prove_rx) = mpsc::channel::<String>(4);
    // Persistent agent kept across turns to preserve history.
    let agent: std::sync::Arc<tokio::sync::Mutex<Option<Agent>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));

    // Cap redraws to a frame budget while a turn is streaming. The agent emits
    // many token deltas per frame interval; without the cap the redraw runs
    // hundreds of times/sec and shows up as visible jank. When idle (`!busy`)
    // we draw immediately so keystrokes still echo without latency.
    let frame_budget = Duration::from_millis(16);
    let mut last_draw = std::time::Instant::now()
        .checked_sub(frame_budget)
        .unwrap_or_else(std::time::Instant::now);
    // State changed since the last draw (set by every select! arm that mutates
    // what's on screen). While the budget skips a draw, this shortens the idle
    // tick to the budget's remainder so the deferred frame paints on cadence —
    // not up to 80ms late when the stream happens to go quiet.
    let mut dirty = false;
    // The just-processed events included user input: draw NOW, budget or not.
    // A keystroke that waits behind a token burst's frame budget reads as a
    // laggy composer; input echo always wins.
    let mut input_dirty = false;

    loop {
        if app.should_exit {
            break;
        }
        // Open the resume picker once on first frame when launched via
        // `tomte resume`. Guarded so re-entry (eg. after Esc) doesn't pop
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
        // `/rewind` opens a picker of this session's checkpoints. main_loop has the
        // agent Arc, so it snapshots them here (open_overlay can't reach the agent).
        if app.can_run_deferred_agent_op() && std::mem::take(&mut app.pending_rewind_open) {
            let points = {
                let g = agent.lock().await;
                match g.as_ref() {
                    Some(a) => a.rewind_preview().await,
                    None => Vec::new(),
                }
            };
            if points.is_empty() {
                app.blocks.push(Block::System(
                    "Nothing to rewind to yet — start a turn first.".to_string(),
                ));
            } else {
                app.rewind_points = points;
                app.open_overlay(OverlayKind::RewindPicker);
            }
        }
        // The rewind picker set this ordinal; run the rewind + rebuild the
        // transcript now (same deferral as resume/undo above).
        if app.can_run_deferred_agent_op() {
            if let Some(ordinal) = app.pending_rewind_ordinal.take() {
                apply_rewind(&mut app, &agent, ordinal).await;
            }
        }
        // `/clear` clears the transcript UI in its handler and sets this so
        // main_loop can also reset the agent's history (the model otherwise keeps
        // the full conversation). Deferred while compacting OR busy for the same
        // agent-mutex reason as undo/resume above; `&&` short-circuits so the
        // flag survives until the turn/compaction finishes.
        if app.can_run_deferred_agent_op() && std::mem::take(&mut app.pending_clear) {
            let mut g = agent.lock().await;
            if let Some(a) = g.as_mut() {
                a.clear_history();
            }
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
        // `/prove` runs in the BACKGROUND (it can shell out to a full build/test
        // suite) so the main loop keeps ticking; the rendered capsule comes back
        // over `prove_rx`. `proving` guards against a duplicate spawn.
        if std::mem::take(&mut app.pending_prove) && !app.proving {
            app.proving = true;
            app.blocks.push(Block::System(
                "🔍 Collecting proof — running the project's test / typecheck / lint / build…"
                    .into(),
            ));
            app.auto_scroll = true;
            let cwd = app.cwd.clone();
            let tx = prove_tx.clone();
            tokio::spawn(async move {
                let capsule = tomte_core::proof::collect(&cwd).await;
                let _ = tx.send(capsule.render()).await;
            });
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

        // Paint only when the frame budget (or idle/input latency) says so.
        // The loop itself can iterate hundreds of times a second under an
        // agent-event storm (each batch wakes it), so everything that writes
        // to the terminal — the synchronized-update markers, the scrollback
        // commit, the frame diff — must live inside this gate. Emitting the
        // Begin/End markers on every iteration flooded the terminal with
        // escape writes between draws, which read as whole-app lag (and made
        // native scrollback unusable) during multi-agent turns.
        let will_draw = !app.busy || input_dirty || last_draw.elapsed() >= frame_budget;
        let mut draw_result = Ok(());
        if will_draw {
            // Batch this frame's terminal writes — the scrollback commit and
            // the frame diff — into one synchronized update (DECSET 2026) so
            // the terminal paints them atomically. Without it a fast stream's
            // big diffs paint mid-write as tearing/shimmer. Best-effort:
            // terminals without the mode ignore the markers, and errors are
            // swallowed so a legacy console can't wedge the loop.
            let _ = execute!(terminal.backend_mut(), BeginSynchronizedUpdate);

            // Inline mode (Pillar 4): push finished turns into the terminal's
            // native scrollback before drawing, so the slim live viewport only
            // ever holds the active turn. A no-op in alt-screen mode.
            if app.render_mode == RenderMode::Inline && app.screen == Screen::Chat {
                commit_finished_blocks(&mut app, terminal);
            }

            draw_result = terminal
                .draw(|f| {
                    app.last_width = f.area().width;
                    app.last_height = f.area().height;
                    match app.screen {
                        Screen::Login => {
                            if let Some((stage, login_err)) = login_render.as_ref() {
                                login::render(f, f.area(), &app.login, stage, login_err.as_deref());
                            }
                        }
                        Screen::Chat => {
                            if app.render_mode == RenderMode::Inline {
                                ui::render_inline(f, &mut app);
                            } else {
                                ui::render(f, &mut app);
                            }
                        }
                    }
                })
                .map(|completed| {
                    // While a drag is active, keep a copy of the rendered frame
                    // so the selected text can be read back on release (cheap
                    // no-op otherwise).
                    if app.selection.is_some() {
                        app.last_buffer = Some(completed.buffer.clone());
                    }
                });
            last_draw = std::time::Instant::now();
            dirty = false;
            input_dirty = false;
            // Always close the synchronized update — even when the draw failed
            // — before propagating the error, so the terminal is never left
            // holding frames.
            let _ = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
        }
        draw_result?;

        // While a deferred frame is pending (busy + inside the budget), wake on
        // the budget's remainder so it paints on cadence; otherwise the slow
        // animation tick is enough.
        let idle_tick = if app.hatch.is_some() {
            Duration::from_millis(45)
        } else {
            Duration::from_millis(80)
        };
        let tick = if dirty {
            idle_tick
                .min(frame_budget.saturating_sub(last_draw.elapsed()))
                .max(Duration::from_millis(1))
        } else {
            idle_tick
        };

        tokio::select! {
            biased;
            maybe_ev = events.next() => {
                let Some(ev0) = maybe_ev else { break; };
                dirty = true;
                input_dirty = true;
                // Coalesce a burst of input events. The OS delivers a paste as a
                // contiguous block of events arriving back-to-back; a human
                // keystroke arrives alone. `pasting` (a burst of >1 event) tells
                // the Chat key handler to treat an Enter inside a paste as a
                // newline, not a submit — the Windows fix for "long paste
                // auto-sends" (crossterm emits no Event::Paste on Windows).
                // Draining also collapses an N-keystroke paste into a single
                // insert+redraw instead of N (the paste-lag fix).
                let mut batch = vec![ev0];
                // Probe for a second event without blocking: a lone keystroke has
                // nothing queued behind it, so this stays None and we fall through
                // immediately — no added latency for ordinary typing. A paste
                // delivers a burst, so the probe hits; from there we *wait* up to
                // PASTE_COALESCE_GAP for each next event so the reader thread's
                // chunked parsing can't split one paste across batches (which
                // stranded a newline alone and submitted mid-paste).
                if let Some(Some(ev)) = events.next().now_or_never() {
                    batch.push(ev);
                    while batch.len() < PASTE_BURST_MAX {
                        match tokio::time::timeout(PASTE_COALESCE_GAP, events.next()).await {
                            Ok(Some(ev)) => batch.push(ev),
                            // Timed out (paste over) or stream closed → stop.
                            _ => break,
                        }
                    }
                }
                let pasting = batch.len() > 1;
                let mut stop = false;
                for ev in batch {
                    match ev {
                        Ok(Event::Key(key)) => {
                            if key.kind != KeyEventKind::Press {
                                continue;
                            }
                            match app.screen {
                                Screen::Login => {
                                    if app.login.handle_key(key).await? {
                                        let stage = app.login.stage().await;
                                        if let LoginStage::Success(mode) = stage {
                                            app.auth_mode = mode;
                                            app.screen = Screen::Chat;
                                            app.login = LoginScreen::new();
                                        } else if matches!(stage, LoginStage::Cancelled) {
                                            stop = true;
                                            break;
                                        }
                                    }
                                }
                                Screen::Chat => {
                                    if handle_key(&mut app, key, &agent, &agent_tx, &bang_tx, pasting)
                                        .await?
                                    {
                                        stop = true;
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(Event::Resize(_, _)) => {}
                        // Bracketed paste on the login screen: route the clipboard
                        // into the active field (OAuth code / API key). Without this
                        // a paste arrives as Event::Paste and is dropped, since the
                        // Chat arm below ignores it.
                        Ok(Event::Paste(text)) if app.screen == Screen::Login => {
                            app.login.handle_paste_text(&text).await;
                        }
                        // Bracketed paste: the whole clipboard arrives as one event,
                        // so multi-line text lands in the composer for editing
                        // instead of submitting on the first newline.
                        Ok(Event::Paste(text))
                            if app.screen == Screen::Chat
                                && app.pending_approval.is_none()
                                && app.pending_conscience.is_none() =>
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
                                    // Drop any active selection on scroll. Tracking
                                    // it by screen row drifted the highlight onto
                                    // unrelated text (the content moves but the cells
                                    // don't line up), so clearing keeps copy accurate
                                    // — select what's visible, then copy.
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
                if stop { break; }
            }
            Some(ev) = agent_rx.recv() => {
                dirty = true;
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
                dirty = true;
                app.blocks.push(Block::System(r.display));
                app.pending_shell_context.push(r.context);
                app.auto_scroll = true;
            }
            Some(card) = prove_rx.recv() => {
                // The background `/prove` collection finished; show the capsule.
                dirty = true;
                app.proving = false;
                // `/prove explain`: hand the CLI-collected card to the agent to
                // interpret — the card is on screen and in the prompt verbatim;
                // the model can only explain the numbers, not change them.
                if std::mem::take(&mut app.prove_explain) {
                    app.message_queue.push(prove_explain_prompt(&card));
                }
                app.blocks.push(Block::System(card));
                app.auto_scroll = true;
            }
            _ = tokio::time::sleep(tick) => {
                // Wake periodically so the spinner redraws while a turn runs and
                // no agent events arrive (and, when a deferred frame is pending,
                // on the frame budget's remainder); the glyph itself advances on
                // elapsed wall-clock time in render_spinner, not on a counter.
            }
        }
    }
    Ok(())
}

/// Seed a one-time setup card when an important external tool is missing,
/// showing the OS-specific install command (read-only — we never run it). The
/// list is empty on a ready machine, so this is a no-op in the common case.
fn seed_setup_card(app: &mut App) {
    let missing = tomte_core::setup::missing_important_tools();
    if missing.is_empty() {
        return;
    }
    let mut lines = vec![
        "⚠ Setup — install these, then restart tomte (tomte shows the command, never runs it):"
            .to_string(),
    ];
    for item in missing {
        lines.push(format!("  • {} — {}", item.tool, item.why));
        lines.push(format!("      {}", item.install));
    }
    app.blocks.push(Block::System(lines.join("\n")));
}

/// Finish the hatch animation: adopt the pet (locked to the account) and let
/// the small corner companion take over.
pub fn finish_hatch(app: &mut App) {
    if let Some(h) = app.hatch.take() {
        app.buddy_pet = Some(h.pet);
        app.buddy_hidden = false;
    }
}

/// SOUL Pillar 4 (inline viewport): push finished blocks into the terminal's
/// native scrollback via `insert_before`, so the live viewport only ever
/// renders the active turn. "Finished" = every block but the last while a turn
/// streams (the last may still grow); when idle, every block. Already-committed
/// blocks are tracked by `app.committed_blocks`.
/// Index one past the last block safe to push to scrollback this tick. While a
/// turn streams, the final block may still grow, so it stays live — and so does
/// everything from the first still-running tool block on: parallel tool calls
/// (a `dispatch_agent` fleet, concurrent shells) leave in-flight Tool blocks
/// BEFORE the last block, and committing one freezes its "working…" row into
/// native scrollback forever (`insert_before` can never repaint it, so the
/// transcript lies about finished work). When idle, every block is finished
/// (turn end settles any still-open tool — see `settle_inflight_tools`).
/// Split out as a pure fn for testing.
pub fn committed_end(blocks: &[Block], busy: bool) -> usize {
    let cap = if busy {
        blocks.len().saturating_sub(1)
    } else {
        blocks.len()
    };
    blocks[..cap]
        .iter()
        .position(|b| matches!(b, Block::Tool { output: None, .. }))
        .unwrap_or(cap)
}

pub fn commit_finished_blocks<B: ratatui::backend::Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
) {
    // A /clear or /resume that shrank or replaced the transcript resets the
    // cursor (those sites reset it explicitly; this backstops a missed one).
    if app.committed_blocks > app.blocks.len() {
        app.committed_blocks = 0;
    }
    // First screen: keep the lone welcome card live (not yet pushed to
    // scrollback) so the bottom-anchored tail tucks it right above the input,
    // instead of stranding it at the top with a big empty gap below. It commits
    // normally the moment the conversation grows past it.
    if app.blocks.len() == 1 && matches!(app.blocks.first(), Some(Block::Welcome)) {
        return;
    }
    let end = committed_end(&app.blocks, app.busy);
    if app.committed_blocks >= end {
        return;
    }
    let width = terminal
        .size()
        .map(|s| s.width)
        .unwrap_or(app.last_width)
        .max(1);
    let inner_width = width.saturating_sub(2) as usize;
    let lines = ui::inline_blocks_to_lines(
        &app.blocks[app.committed_blocks..end],
        inner_width,
        app.expanded_tools,
        app,
    );
    app.committed_blocks = end;
    if lines.is_empty() {
        return;
    }
    // height = line count; insert_before chunks it to the terminal height.
    let height = lines.len() as u16;
    let _ = terminal.insert_before(height, move |buf| {
        ratatui::widgets::Widget::render(ratatui::widgets::Paragraph::new(lines), buf.area, buf);
    });
}

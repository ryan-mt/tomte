//! Key and paste event handling. Split out of `app`; logic unchanged.

use super::*;

pub async fn handle_key(
    app: &mut App,
    key: KeyEvent,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    tx: &mpsc::Sender<AgentEvent>,
    bang_tx: &mpsc::Sender<BangResult>,
    // True when this key arrived inside a coalesced input burst (a paste). On
    // Windows crossterm delivers a paste as individual key events rather than
    // one `Event::Paste`, so a pasted newline would otherwise submit the partial
    // message; while `pasting`, Enter inserts a newline instead. See `main_loop`.
    pasting: bool,
) -> Result<bool> {
    // A keypress during the hatch animation skips straight to the reveal.
    if app.hatch.is_some() {
        finish_hatch(app);
        return Ok(false);
    }

    // Any keypress dismisses a mouse text selection's highlight + copy notice.
    app.clear_selection();

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if app.pending_conscience.is_some() {
        // Pillar 5 (A2 Tier 2) conscience card: 0 = abort, 1 = supersede,
        // 2 = edit anyway. Up/Down + Enter, with a/s/e and Esc (= abort) as
        // shortcuts.
        const N_OPTS: usize = 3;
        match key.code {
            KeyCode::Up => {
                if let Some(p) = app.pending_conscience.as_mut() {
                    p.selected = (p.selected + N_OPTS - 1) % N_OPTS;
                }
                return Ok(false);
            }
            KeyCode::Down => {
                if let Some(p) = app.pending_conscience.as_mut() {
                    p.selected = (p.selected + 1) % N_OPTS;
                }
                return Ok(false);
            }
            _ => {}
        }
        let sel = app.pending_conscience.as_ref().map_or(0, |p| p.selected);
        let choice = match key.code {
            KeyCode::Enter => Some(sel),
            KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Esc => Some(0),
            KeyCode::Char('s') | KeyCode::Char('S') => Some(1),
            KeyCode::Char('e') | KeyCode::Char('E') => Some(2),
            _ => None,
        };
        if let Some(choice) = choice {
            let p = app
                .pending_conscience
                .take()
                .expect("pending_conscience present");
            let decision = match choice {
                0 => tomte_core::agent::ConscienceChoice::Abort,
                1 => tomte_core::agent::ConscienceChoice::Supersede,
                _ => tomte_core::agent::ConscienceChoice::EditAnyway,
            };
            let label = match decision {
                tomte_core::agent::ConscienceChoice::Abort => "aborted (kept the decision)",
                tomte_core::agent::ConscienceChoice::Supersede => "superseded the decision",
                tomte_core::agent::ConscienceChoice::EditAnyway => "edited anyway",
            };
            app.blocks
                .push(Block::System(format!("conscience: {label} — {}", p.file)));
            if let Some(handle) = app.conscience_handle.clone() {
                let call_id = p.call_id.clone();
                tokio::spawn(async move {
                    let sender = {
                        let mut map = handle.lock().await;
                        map.remove(&call_id)
                    };
                    if let Some(s) = sender {
                        let _ = s.send(decision);
                    }
                });
            }
        }
        return Ok(false);
    }

    if app.pending_approval.is_some() {
        // Three-option menu, navigable with Up/Down + Enter, with y/n/Esc kept
        // as shortcuts. 0 = allow once, 1 = allow in this project (persisted to
        // .tomte/permissions.json), 2 = deny.
        const N_OPTS: usize = 3;
        match key.code {
            KeyCode::Up => {
                if let Some(p) = app.pending_approval.as_mut() {
                    p.selected = (p.selected + N_OPTS - 1) % N_OPTS;
                }
                return Ok(false);
            }
            KeyCode::Down => {
                if let Some(p) = app.pending_approval.as_mut() {
                    p.selected = (p.selected + 1) % N_OPTS;
                }
                return Ok(false);
            }
            _ => {}
        }
        let sel = app.pending_approval.as_ref().map_or(0, |p| p.selected);
        let choice = match key.code {
            KeyCode::Enter => Some(sel),
            KeyCode::Char('y') | KeyCode::Char('Y') => Some(0),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(2),
            _ => None,
        };
        if let Some(choice) = choice {
            let p = app
                .pending_approval
                .take()
                .expect("pending_approval present");
            let granted = choice != 2;
            // Option 1 = "allow in this project": persist a rule to the
            // owner-only user-level store (outside the repo, keyed by project
            // path) so this tool/command never prompts again in this project,
            // and a cloned repo can't pre-grant it. Unlike a session-wide
            // bypass, it is scoped and durable.
            if choice == 1 {
                let args_val: serde_json::Value =
                    serde_json::from_str(&p.args_json).unwrap_or(serde_json::Value::Null);
                match tomte_core::permissions::allow_in_project(&app.cwd, &p.tool_name, &args_val) {
                    Ok(rule) => app
                        .blocks
                        .push(Block::System(format!("✓ allowed in this project: {rule}"))),
                    Err(e) => app.blocks.push(Block::System(format!(
                        "could not save project permission: {e}"
                    ))),
                }
            } else {
                let label = if granted { "approved" } else { "denied" };
                app.blocks
                    .push(Block::System(format!("{label}: {}", p.tool_name)));
            }
            // CRITICAL: do NOT lock the outer agent mutex here — run_turn
            // holds it for the entire turn and is itself awaiting this
            // approval. Use the handle Arc captured at turn start instead.
            if let Some(handle) = app.approval_handle.clone() {
                let call_id = p.call_id.clone();
                tokio::spawn(async move {
                    let sender = {
                        let mut map = handle.lock().await;
                        map.remove(&call_id)
                    };
                    if let Some(s) = sender {
                        let _ = s.send(granted);
                    }
                });
            }
        }
        return Ok(false);
    }

    if app.pending_goal_replacement.is_some() {
        if key.code == KeyCode::Char('c') && ctrl {
            return Ok(true);
        }
        handle_goal_replacement_key(app, key);
        return Ok(false);
    }

    if app.pending_plan_exit.is_some() {
        if key.code == KeyCode::Char('c') && ctrl {
            return Ok(true);
        }
        if app.busy {
            return Ok(false);
        }
        handle_plan_exit_key(app, key);
        return Ok(false);
    }

    if app.overlay.is_some() {
        return handle_overlay_key(app, key).await;
    }

    if matches!(key.code, KeyCode::BackTab) {
        let next = app.permission_mode().next();
        // Persist so the mode survives quit/relaunch (mirrors how /model and
        // /effort save on change). Errors are surfaced, not swallowed.
        set_permission_mode_and_save(app, next);
        // No chat notification: the status-bar footer already shows the active
        // mode (see render_status); the indicator updates on Shift+Tab rather
        // than printing a line.
        return Ok(false);
    }
    match key.code {
        KeyCode::Char('c') if ctrl => return Ok(true),
        KeyCode::Char('d') if ctrl && app.input.is_empty() => return Ok(true),
        KeyCode::Char('l') if ctrl => {
            app.blocks.clear();
            app.committed_blocks = 0;
        }
        KeyCode::Char('u') if ctrl => {
            app.input.clear();
            app.history_pos = None;
        }
        KeyCode::Char('w') if ctrl => app.input.delete_word_left(),
        KeyCode::Char('a') if ctrl => app.input.move_to_start(),
        KeyCode::Char('k') if ctrl => {
            app.input.kill_to_line_end();
            app.history_pos = None;
        }
        // Ctrl+V and Alt+V both paste from the clipboard. Alt+V matters on
        // Windows Terminal, which binds Ctrl+V to its own paste (delivered as
        // keystrokes, never reaching us), so Alt+V is the reliable in-app paste.
        KeyCode::Char('v') if ctrl || alt => {
            handle_paste(app).await;
        }
        KeyCode::Char('o') if ctrl => {
            app.expanded_tools = !app.expanded_tools;
        }
        KeyCode::Char('t') if ctrl => {
            app.show_todos = !app.show_todos;
        }
        KeyCode::Char('/') if app.input.is_empty() => {
            // Trigger the slash menu overlay; also insert '/' into the input
            // so users can keep typing to filter.
            app.input.insert_char('/');
            app.open_overlay(OverlayKind::SlashMenu);
        }
        KeyCode::Char('@') if ctrl == alt => {
            // Insert '@' and open the file typeahead; characters typed after it
            // filter the list (handled in handle_overlay_key), the standard
            // `@file` reference flow.
            app.input.insert_char('@');
            app.history_pos = None;
            app.open_overlay(OverlayKind::FilePicker);
        }
        // Insert only for a plain key or AltGr (Ctrl+Alt, used for `@{}[]` etc.
        // on international layouts — there ctrl==alt). A lone Ctrl or lone Alt is
        // a command/no-op, not text; otherwise unhandled combos like Ctrl+A typed
        // a literal 'a' into the composer.
        KeyCode::Char(ch) if ctrl == alt => {
            app.input.insert_char(ch);
            app.history_pos = None;
        }
        KeyCode::Enter if shift || alt || pasting => app.input.insert_newline(),
        KeyCode::Enter => {
            if app.input.is_empty() {
                return Ok(false);
            }
            let text = app.input.take();
            app.record_history(&text);
            if !app.busy && !app.compacting {
                if let Some(rest) = text.strip_prefix('/') {
                    handle_slash(app, rest.trim()).await;
                    return Ok(false);
                }
                if let Some(cmd) = text.strip_prefix('!') {
                    composer::handle_bang_shell(app, bang_tx, cmd.trim());
                    return Ok(false);
                }
                if let Some(note) = text.strip_prefix('#') {
                    composer::handle_hash_memory(app, agent, note.trim()).await;
                    return Ok(false);
                }
                app.blocks.push(Block::User(text.clone()));
                app.auto_scroll = true;
                launch_turn(app, agent, tx, text).await;
            } else {
                // Busy or compacting: queue the message. During compaction
                // launch_turn would block on the agent mutex the compaction task
                // holds, freezing the UI; the post-compaction queue flush picks
                // it up instead. Slash commands queue too, no special-casing, so
                // the user's intent is preserved.
                app.message_queue.push(text);
            }
        }
        KeyCode::Backspace => {
            app.input.backspace();
            app.history_pos = None;
        }
        KeyCode::Left => app.input.move_left(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Up => {
            // On the first line, recall older history; otherwise move the
            // cursor within the (multi-line) composer.
            if app.input.cursor_pos().0 == 0 {
                app.history_prev();
            } else {
                app.input.move_up();
            }
        }
        KeyCode::Down => {
            // On the last line, walk toward newer history (and the draft);
            // otherwise move the cursor down within the composer.
            if app.input.cursor_pos().0 + 1 >= app.input.line_count() {
                app.history_next();
            } else {
                app.input.move_down();
            }
        }
        KeyCode::Home => app.input.move_home(),
        KeyCode::End if ctrl => app.auto_scroll = true,
        KeyCode::End => app.input.move_end(),
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(10);
            app.auto_scroll = false;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(10);
        }
        KeyCode::Esc => {
            if app.busy {
                cancel_current_turn(app).await;
            } else {
                app.input.clear();
            }
        }
        _ => {}
    }
    Ok(false)
}

pub async fn handle_paste(app: &mut App) {
    use super::clipboard::{try_paste, PasteResult};
    match try_paste() {
        Ok(PasteResult::Image(path)) => {
            let n = app.next_image_num;
            app.pending_images.push(path);
            app.next_image_num += 1;
            app.input.insert_str(&format!("[Image #{n}] "));
        }
        Ok(PasteResult::Text(t)) => {
            // One shift, not char-by-char (avoids O(n²) on a large paste).
            let cleaned: String = t.chars().filter(|&c| c != '\r').collect();
            app.input.insert_str(&cleaned);
        }
        Ok(PasteResult::Empty) => {}
        Err(e) => {
            app.blocks.push(Block::System(format!("paste failed: {e}")));
        }
    }
}

/// True while the user owes a Y/N decision that must be resolved before any new
/// turn launches. Used to hold the message-queue flush so queued type-ahead
/// can't relaunch a turn under an open approval prompt (which would lock the
/// user out of pressing Y — see the flush gate in `main_loop`).
pub fn turn_launch_blocked_by_pending_decision(app: &App) -> bool {
    app.pending_plan_exit.is_some() || app.pending_goal_replacement.is_some()
}

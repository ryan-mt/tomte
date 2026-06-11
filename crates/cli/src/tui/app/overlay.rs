//! Worktree slash, goal-replacement/plan-exit keys, and overlay handling. Split out of `app`; logic unchanged.

use super::*;

pub fn handle_worktree_slash(app: &mut App, arg: &str) {
    let mut parts = arg.split_whitespace();
    let Some(cmd) = parts.next() else {
        app.blocks.push(Block::System(
            "Usage:\n  /worktree create [name]\n  /worktree exit keep\n  /worktree exit remove [--discard]"
                .into(),
        ));
        return;
    };

    match cmd {
        "create" | "enter" => {
            let name = parts.next().map(str::to_string);
            if parts.next().is_some() {
                app.blocks
                    .push(Block::System("Usage: /worktree create [name]".into()));
                return;
            }
            let prompt = match &name {
                Some(name) => format!(
                    "Create and enter an isolated git worktree for this session using the enter_worktree tool. Pass name={name:?}, then report the worktree path and branch."
                ),
                None => "Create and enter an isolated git worktree for this session using the enter_worktree tool with name=null, then report the worktree path and branch."
                    .to_string(),
            };
            app.message_queue.push(prompt);
            app.blocks.push(Block::System(
                "Queued worktree creation; the agent will create it with enter_worktree.".into(),
            ));
            app.auto_scroll = true;
        }
        "exit" | "leave" => {
            let Some(action) = parts.next() else {
                app.blocks.push(Block::System(
                    "Usage: /worktree exit keep|remove [--discard]".into(),
                ));
                return;
            };
            if !matches!(action, "keep" | "remove") {
                app.blocks.push(Block::System(
                    "Usage: /worktree exit keep|remove [--discard]".into(),
                ));
                return;
            }
            let mut discard = false;
            for part in parts {
                if part == "--discard" {
                    discard = true;
                } else {
                    app.blocks.push(Block::System(format!(
                        "Unknown worktree flag `{part}`. Usage: /worktree exit keep|remove [--discard]"
                    )));
                    return;
                }
            }
            let prompt = format!(
                "Exit the active session worktree using the exit_worktree tool. Pass action={action:?} and discard_changes={discard}. Report what happened."
            );
            app.message_queue.push(prompt);
            app.blocks.push(Block::System(format!(
                "Queued worktree exit ({action}); the agent will call exit_worktree."
            )));
            app.auto_scroll = true;
        }
        _ => app.blocks.push(Block::System(
            "Usage:\n  /worktree create [name]\n  /worktree exit keep\n  /worktree exit remove [--discard]"
                .into(),
        )),
    }
}

pub fn handle_goal_replacement_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(pending) = app.pending_goal_replacement.take() else {
                return;
            };
            start_goal(app, pending.objective, true);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            let Some(pending) = app.pending_goal_replacement.take() else {
                return;
            };
            if let Some(goal) = &app.active_goal {
                app.blocks.push(Block::System(format!(
                    "Kept active goal: {}\nDiscarded new goal: {}",
                    goal.objective, pending.objective
                )));
            }
            queue_goal_continuation(app);
            app.auto_scroll = true;
        }
        _ => {}
    }
}

pub fn handle_plan_exit_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let Some(pending) = app.pending_plan_exit.take() else {
                return;
            };
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = false;
                goal.last_summary = Some("plan approved".into());
                app.pending_session_save = true;
            }
            app.set_permission_mode(permission_mode_after_plan_approval(app));
            app.message_queue.push(format!(
                "{PLAN_APPROVED_PREFIX}\n\nThe user approved this plan and exited plan mode. Implement it now and verify the result.\n\nApproved plan:\n{}",
                pending.plan
            ));
            app.blocks.push(Block::System(
                "Plan approved — leaving plan mode and continuing.".into(),
            ));
            app.auto_scroll = true;
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            let Some(pending) = app.pending_plan_exit.take() else {
                return;
            };
            if let Some(goal) = app.active_goal.as_mut() {
                goal.waiting_for_user = false;
                goal.last_summary = Some("plan rejected; revising".into());
                app.pending_session_save = true;
            }
            app.set_permission_mode(PermissionMode::Plan);
            app.message_queue.push(format!(
                "{PLAN_REJECTED_PREFIX}\n\nThe user rejected this plan. Stay in plan mode, revise the plan, and call exit_plan_mode again only when the revised plan is ready.\n\nRejected plan:\n{}",
                pending.plan
            ));
            app.blocks.push(Block::System(
                "Plan rejected — staying in plan mode.".into(),
            ));
            app.auto_scroll = true;
        }
        _ => {}
    }
}

pub async fn handle_overlay_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    // Ctrl+C never reaches here — handle_key's quit guard intercepts it ahead
    // of the overlay branch, so the double-press rule holds inside pickers too.
    let Some((kind, picker)) = app.overlay.as_mut() else {
        return Ok(false);
    };
    let kind = *kind;
    match key.code {
        KeyCode::Up => picker.move_up(),
        KeyCode::Down => picker.move_down(),
        // FilePicker arms must precede the generic Esc/Enter below, which assume
        // a slash/model/effort selection.
        KeyCode::Esc if kind == OverlayKind::FilePicker => {
            // Dismiss the typeahead but keep whatever the user typed.
            app.overlay = None;
        }
        KeyCode::Enter | KeyCode::Tab if kind == OverlayKind::FilePicker => {
            if let Some(key_sel) = picker.selected_key() {
                app.input.complete_at_token(&key_sel);
            }
            app.overlay = None;
        }
        KeyCode::Backspace if kind == OverlayKind::FilePicker => {
            app.input.backspace();
            match app.input.active_at_token() {
                Some((_, q)) => {
                    if let Some((_, p)) = app.overlay.as_mut() {
                        p.query = q;
                        p.ensure_visible_selected();
                    }
                }
                None => app.overlay = None,
            }
        }
        // Ctrl-chords (Ctrl+W/U/A…) are composer shortcuts, not text — inserting
        // their letter into the input here was the old behavior. No-op instead.
        KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::CONTROL) => {}
        KeyCode::Char(c) if kind == OverlayKind::FilePicker => {
            app.input.insert_char(c);
            // A space (or any whitespace) ends the `@`-token; close the overlay.
            match app.input.active_at_token() {
                Some((_, q)) => {
                    if let Some((_, p)) = app.overlay.as_mut() {
                        p.query = q;
                        p.ensure_visible_selected();
                    }
                }
                None => app.overlay = None,
            }
        }
        KeyCode::Esc => {
            app.overlay = None;
            if kind == OverlayKind::SlashMenu {
                app.input.clear();
            }
            app.chain_to_effort = false;
        }
        KeyCode::Enter => {
            let key_sel = picker.selected_key();
            app.overlay = None;
            match key_sel {
                Some(key_sel) => handle_overlay_select(app, kind, &key_sel).await,
                // Nothing matches the query — the normal way to type a command
                // WITH arguments (`/goal fix the tests` empties the filter).
                // Run the composer text through the regular slash path instead
                // of clearing it into "Unknown command /".
                None if kind == OverlayKind::SlashMenu => {
                    let raw = app.input.buffer.trim().to_string();
                    app.input.clear();
                    if let Some(cmd) = raw.strip_prefix('/') {
                        if !cmd.is_empty() {
                            handle_slash(app, cmd).await;
                        }
                    }
                }
                None => {}
            }
        }
        KeyCode::Tab if kind == OverlayKind::SlashMenu => {
            // Autocomplete the highlighted command into the input (Tab-complete),
            // then close the menu. The completed `/<cmd> ` lands in
            // the normal composer so the user can add arguments or press Enter
            // to run it via the standard slash path.
            if let Some(key_sel) = picker.selected_key() {
                app.input.set_text(format!("/{key_sel} "));
                app.overlay = None;
            }
        }
        KeyCode::Backspace if kind == OverlayKind::SlashMenu => {
            app.input.backspace();
            let buf = app.input.buffer.clone();
            if let Some(rest) = buf.strip_prefix('/') {
                let q = rest.to_string();
                if let Some((_, p)) = app.overlay.as_mut() {
                    p.query = q;
                    p.ensure_visible_selected();
                }
            } else {
                app.overlay = None;
                app.input.clear();
            }
        }
        KeyCode::Char(c) if kind == OverlayKind::SlashMenu => {
            app.input.insert_char(c);
            let buf = app.input.buffer.clone();
            if let Some(rest) = buf.strip_prefix('/') {
                let q = rest.to_string();
                if let Some((_, p)) = app.overlay.as_mut() {
                    p.query = q;
                    p.ensure_visible_selected();
                }
            }
        }
        _ => {}
    }
    Ok(false)
}

pub async fn handle_overlay_select(app: &mut App, kind: OverlayKind, key_sel: &str) {
    match kind {
        OverlayKind::SlashMenu => {
            // The user picked a slash command. Some commands open another picker;
            // others run the slash handler directly.
            app.input.clear();
            match key_sel {
                "model" => {
                    app.chain_to_effort = true;
                    app.open_overlay(OverlayKind::ModelPicker);
                }
                "thinking" | "effort" => {
                    app.open_overlay(OverlayKind::EffortPicker);
                }
                "verbosity" => {
                    app.open_overlay(OverlayKind::VerbosityPicker);
                }
                other => {
                    handle_slash(app, other).await;
                }
            }
        }
        // FilePicker commits via its own Enter/Tab arm in handle_overlay_key
        // (it rewrites the `@`-token in place), so it never reaches here.
        OverlayKind::FilePicker => {}
        OverlayKind::ModelPicker => {
            app.config.model = key_sel.to_string();
            if let Err(e) = config::save(&app.config) {
                app.blocks
                    .push(Block::System(format!("config save failed: {e}")));
            }
            app.blocks.push(Block::System(format!("model → {key_sel}")));
            app.note_trail_follows_model(key_sel);
            if app.chain_to_effort {
                app.chain_to_effort = false;
                app.open_overlay(OverlayKind::EffortPicker);
            }
        }
        OverlayKind::EffortPicker => {
            app.config.reasoning_effort = key_sel.to_string();
            if let Err(e) = config::save(&app.config) {
                app.blocks
                    .push(Block::System(format!("config save failed: {e}")));
            }
            app.blocks
                .push(Block::System(format!("effort → {key_sel}")));
        }
        OverlayKind::VerbosityPicker => {
            app.config.verbosity = key_sel.to_string();
            if let Err(e) = config::save(&app.config) {
                app.blocks
                    .push(Block::System(format!("config save failed: {e}")));
            }
            app.blocks
                .push(Block::System(format!("verbosity → {key_sel}")));
        }
        OverlayKind::ResumePicker => {
            if !key_sel.is_empty() {
                app.pending_resume_id = Some(key_sel.to_string());
            }
        }
        OverlayKind::RewindPicker => {
            // key is the checkpoint ordinal; main_loop runs the rewind (it has
            // the agent Arc), the same deferral as resume.
            if let Ok(ordinal) = key_sel.parse::<usize>() {
                app.pending_rewind_ordinal = Some(ordinal);
            }
        }
        OverlayKind::LogoutPicker => {
            if let Some(target) = tomte_core::auth::LogoutTarget::from_key(key_sel) {
                // Locked read-modify-write: an unserialized load→save here
                // could interleave with an in-flight token refresh and revert
                // its freshly-rotated refresh token (see `auth::mutate_auth`).
                let saved = auth::mutate_auth(|record| {
                    tomte_core::auth::clear_credential(record, target);
                })
                .await;
                match saved {
                    Ok(record) => {
                        app.auth_mode = auth::effective_mode_with_env(&record);
                        app.blocks.push(Block::System("✅ Logged out.".into()));
                    }
                    Err(e) => app
                        .blocks
                        .push(Block::System(format!("logout failed: {e}"))),
                }
            }
        }
    }
}

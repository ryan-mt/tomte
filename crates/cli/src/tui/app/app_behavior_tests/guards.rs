use super::super::*;

/// Dummy wiring for driving `handle_key` directly: no agent, channels that go
/// nowhere. Fine for key paths that never lock the agent (Ctrl+C, Esc, `?`).
async fn press(app: &mut App, key: KeyEvent) -> bool {
    let agent = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let (tx, _rx) = mpsc::channel(8);
    let (bang_tx, _bang_rx) = mpsc::channel(8);
    handle_key(app, key, &agent, &tx, &bang_tx, false)
        .await
        .unwrap()
}

// One reflexive Ctrl+C must never kill a live session: the first press arms
// the guard (stashing any draft into ↑ history), only a second press inside
// the window quits, and a press after the window lapses re-arms instead.
#[test]
fn ctrl_c_quits_only_on_a_double_press_within_the_window() {
    let mut app = App::new();
    let t0 = std::time::Instant::now();

    app.input.insert_str("half-typed prompt");
    assert!(!app.handle_ctrl_c_at(t0), "first press must not quit");
    assert!(app.input.is_empty(), "first press clears the composer");
    app.history_prev();
    assert_eq!(
        app.input.buffer, "half-typed prompt",
        "the cleared draft is recoverable via ↑"
    );

    app.input.clear();
    assert!(
        app.handle_ctrl_c_at(t0 + Duration::from_millis(500)),
        "second press inside the window quits"
    );

    // A press long after the arm re-arms rather than quitting.
    app.ctrl_c_armed_at = Some(t0);
    assert!(!app.handle_ctrl_c_at(t0 + CTRL_C_QUIT_WINDOW + Duration::from_millis(1)));
    assert!(
        app.ctrl_c_armed_at.is_some(),
        "stale press re-arms the guard"
    );
}

#[test]
fn quit_hint_active_only_inside_the_window() {
    let mut app = App::new();
    assert!(!app.quit_hint_active());
    app.ctrl_c_armed_at = Some(std::time::Instant::now());
    assert!(app.quit_hint_active());
    app.ctrl_c_armed_at =
        Some(std::time::Instant::now() - CTRL_C_QUIT_WINDOW - Duration::from_secs(1));
    assert!(
        !app.quit_hint_active(),
        "a lapsed arm must not keep the hint"
    );
}

// Routing: the guard intercepts ahead of every modal branch, so Ctrl+C behaves
// the same under an open approval card (which used to swallow it entirely).
#[tokio::test]
async fn ctrl_c_is_guarded_in_every_state_including_modals() {
    let mut app = App::new();
    app.pending_approval = Some(PendingApproval {
        call_id: "c1".into(),
        tool_name: "run_shell".into(),
        args_json: "{}".into(),
        diff_preview: None,
        selected: 0,
    });
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(
        !press(&mut app, ctrl_c).await,
        "first press arms, must not quit"
    );
    assert!(app.pending_approval.is_some(), "the modal stays put");
    assert!(press(&mut app, ctrl_c).await, "second press quits");
}

#[tokio::test]
async fn any_other_key_disarms_the_quit_guard() {
    let mut app = App::new();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(!press(&mut app, ctrl_c).await);
    assert!(app.ctrl_c_armed_at.is_some());
    press(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.ctrl_c_armed_at.is_none(), "typing disarms the guard");
    assert!(
        !press(&mut app, ctrl_c).await,
        "the next Ctrl+C arms again instead of quitting"
    );
}

// Esc on an idle composer clears it but keeps the draft one ↑ away — a
// reflexive Esc on a long draft must not be an unrecoverable loss.
#[tokio::test]
async fn esc_stashes_the_draft_into_recall_history() {
    let mut app = App::new();
    app.busy = false;
    app.input.insert_str("long careful draft");
    let quit = press(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;
    assert!(!quit);
    assert!(app.input.is_empty(), "Esc clears the composer");
    app.history_prev();
    assert_eq!(app.input.buffer, "long careful draft");
}

// The status line advertises "? for shortcuts" — a bare `?` on an empty
// composer must honor it; with text present, `?` stays an ordinary character.
#[tokio::test]
async fn bare_question_mark_shows_help_only_on_empty_composer() {
    let mut app = App::new();
    let qmark = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert!(!press(&mut app, qmark).await);
    assert!(
        matches!(app.blocks.last(), Some(Block::System(s)) if s.contains("Keyboard shortcuts")),
        "bare ? shows the help card"
    );
    assert!(app.input.is_empty(), "the ? itself must not be inserted");

    let blocks_before = app.blocks.len();
    app.input.insert_str("what");
    press(&mut app, qmark).await;
    assert_eq!(app.input.buffer, "what?", "mid-draft ? is just a character");
    assert_eq!(app.blocks.len(), blocks_before, "no second help card");
}

// === Ctrl+O: pager at rest, live toggle mid-turn ==============================

// Inline mode commits finished turns into native scrollback (insert_before),
// which can never be repainted — so at rest Ctrl+O must open the modal pager
// instead of flipping a flag nothing redraws. Mid-turn the live tail repaints
// every frame, so the flag toggle still works there; alt-screen mode repaints
// the whole transcript each frame, so the toggle is always right for it.
#[tokio::test]
async fn ctrl_o_opens_pager_at_rest_and_toggles_while_busy_inline() {
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);

    press(&mut app, ctrl_o).await;
    assert!(app.open_transcript_pager, "idle inline → pager request");
    assert!(
        !app.expanded_tools,
        "idle inline must not flip the dead flag"
    );

    app.open_transcript_pager = false;
    app.busy = true;
    press(&mut app, ctrl_o).await;
    assert!(
        !app.open_transcript_pager,
        "mid-turn: no modal (it would stall the event pump)"
    );
    assert!(
        app.expanded_tools,
        "mid-turn: the live tail toggle still works"
    );
}

#[tokio::test]
async fn ctrl_o_keeps_the_plain_toggle_in_alt_screen_mode() {
    let mut app = App::new();
    app.render_mode = RenderMode::AltScreen;
    let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    press(&mut app, ctrl_o).await;
    assert!(app.expanded_tools);
    assert!(!app.open_transcript_pager);
    press(&mut app, ctrl_o).await;
    assert!(!app.expanded_tools);
}

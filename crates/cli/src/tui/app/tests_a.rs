//! App tests (part a), split out of `app`.

use super::tests_common::*;
use super::*;

// Regression: agent-locking deferred ops (resume/undo) must be blocked while
// a turn is streaming, or apply_resume would deadlock against the turn task.
#[test]
fn deferred_agent_ops_blocked_while_busy_or_compacting() {
    let mut app = App::new();
    app.busy = false;
    app.compacting = false;
    assert!(app.can_run_deferred_agent_op());
    app.busy = true;
    assert!(!app.can_run_deferred_agent_op());
    app.busy = false;
    app.compacting = true;
    assert!(!app.can_run_deferred_agent_op());
}

#[test]
fn goal_word_count_and_limit_boundary() {
    assert_eq!(goal_word_count("  fix   the   build  "), 3);
    assert_eq!(goal_word_count(""), 0);
    // A concise, detailed objective stays well under the cap.
    let ok = "Refactor the auth module to the new token format, update all call \
                  sites, keep every existing test passing, and verify the login flow.";
    assert!(
        goal_word_count(ok) <= GOAL_MAX_WORDS,
        "detailed goal must fit"
    );
    assert!(!goal_exceeds_limit(ok), "detailed goal must fit");
    // Exactly at the cap is accepted; one over is rejected.
    let at_cap = "w ".repeat(GOAL_MAX_WORDS);
    let over_cap = "w ".repeat(GOAL_MAX_WORDS + 1);
    assert!(goal_word_count(&at_cap) <= GOAL_MAX_WORDS);
    assert!(!goal_exceeds_limit(&at_cap));
    assert!(goal_word_count(&over_cap) > GOAL_MAX_WORDS);
    assert!(goal_exceeds_limit(&over_cap));
    // A space-free CJK objective counts as one "word" but must NOT bypass the
    // limit — the char ceiling catches it.
    let cjk_huge = "目".repeat(GOAL_MAX_CHARS + 1);
    assert_eq!(goal_word_count(&cjk_huge), 1, "no whitespace → one word");
    assert!(
        goal_exceeds_limit(&cjk_huge),
        "char ceiling must catch CJK abuse"
    );
    // A short CJK objective is still fine.
    assert!(!goal_exceeds_limit(&"目标".repeat(5)));
}

#[test]
fn initial_screen_allows_any_supported_env_key() {
    assert_eq!(initial_screen(AuthMode::None, false), Screen::Login);
    assert_eq!(initial_screen(AuthMode::None, true), Screen::Chat);
    assert_eq!(
        initial_screen(AuthMode::AnthropicApiKey, false),
        Screen::Chat
    );
}

#[test]
fn cwd_arg_resolves_relative_to_current_app_cwd() {
    let root = temp_dir("cwd-root");
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();

    let resolved = resolve_cwd_arg(&root, "nested").unwrap();
    assert_eq!(resolved, nested.canonicalize().unwrap());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn markdown_export_preserves_fences_and_details_markers() {
    let blocks = vec![
        Block::Assistant {
            text: "visible answer".to_string(),
            reasoning: "```\n</details>\n".to_string(),
            done: true,
            thought_for_secs: None,
            reasoning_started_at: None,
        },
        Block::Tool {
            call_id: "call_1".to_string(),
            name: "run_shell".to_string(),
            args: "{\"cmd\":\"```\"}".to_string(),
            output: Some("```\n</details>\n".to_string()),
            error: false,
        },
    ];

    let md = render_blocks_as_markdown(&blocks);

    assert!(
        md.contains(
            "<details><summary>reasoning</summary>\n\n````\n```\n</details>\n````\n\n</details>"
        ),
        "{md}"
    );
    assert!(
        md.contains("**args:**\n\n````json\n{\"cmd\":\"```\"}\n````\n\n"),
        "{md}"
    );
    assert!(
        md.contains("**output:**\n\n````\n```\n</details>\n````\n\n"),
        "{md}"
    );
}

#[test]
fn slash_command_splits_on_any_whitespace() {
    assert_eq!(split_slash_command("cwd\t./nested"), ("cwd", "./nested"));
    assert_eq!(
        split_slash_command("review   --focus security"),
        ("review", "--focus security")
    );
    assert_eq!(split_slash_command("status"), ("status", ""));
}

#[test]
fn goal_elapsed_formats_seconds_then_minutes() {
    assert_eq!(format_goal_elapsed(Duration::from_secs(0)), "0s");
    assert_eq!(format_goal_elapsed(Duration::from_secs(60)), "60s");
    assert_eq!(format_goal_elapsed(Duration::from_secs(62)), "1m2");
    assert_eq!(format_goal_elapsed(Duration::from_secs(92)), "1m32");
}

#[test]
fn active_goal_session_snapshot_roundtrips_state() {
    let mut goal = ActiveGoal::new("finish verification".to_string());
    goal.turns_completed = 7;
    goal.waiting_for_user = true;
    goal.last_summary = Some("waiting on approval".to_string());
    goal.started_at_ms = tomte_core::session::now_ms().saturating_sub(92_000);

    let restored = ActiveGoal::from_session_snapshot(goal.to_session_snapshot());

    assert_eq!(restored.objective, "finish verification");
    assert_eq!(restored.turns_completed, 7);
    assert!(restored.waiting_for_user);
    assert_eq!(
        restored.last_summary.as_deref(),
        Some("waiting on approval")
    );
    assert_eq!(restored.started_at_ms, goal.started_at_ms);
    assert!(restored.started_at.elapsed() >= Duration::from_secs(90));
}

#[test]
fn host_state_session_record_includes_active_goal() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));
    let mut record = tomte_core::session::SessionRecord {
        meta: tomte_core::session::SessionMeta {
            id: "test".to_string(),
            cwd: app.cwd.clone(),
            model: "gpt-5".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            message_count: 0,
            preview: "test".to_string(),
        },
        state: tomte_core::session::SessionSnapshot::default(),
        history: Vec::new(),
    };

    apply_host_state_to_session_record(&app, &mut record);

    assert_eq!(
        record
            .state
            .active_goal
            .as_ref()
            .map(|g| g.objective.as_str()),
        Some("finish verification")
    );
}

#[tokio::test]
async fn login_slash_opens_login_screen_instead_of_detached_task() {
    let mut app = App::new();
    app.screen = Screen::Chat;
    app.status_line = "stale".to_string();

    handle_slash(&mut app, "login").await;

    assert_eq!(app.screen, Screen::Login);
    assert!(matches!(app.login.stage().await, LoginStage::PickMode));
    assert!(app.status_line.is_empty());
}

#[tokio::test]
async fn goal_slash_starts_active_goal_and_queues_prompt() {
    let mut app = App::new();

    handle_slash(&mut app, "goal stabilize the release flow").await;

    let goal = app.active_goal.as_ref().expect("goal should be active");
    assert_eq!(goal.objective, "stabilize the release flow");
    assert_eq!(goal.turns_completed, 0);
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].contains("stabilize the release flow"));
    assert!(app.message_queue[0].contains("goal_update"));
    assert!(app.pending_session_save);
}

#[tokio::test]
async fn context_slash_renders_rich_report() {
    let mut app = App::new();
    handle_slash(&mut app, "context").await;
    let Some(Block::Rich(lines)) = app.blocks.last() else {
        panic!("expected a rich block from /context");
    };
    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Context Usage"));
    assert!(text.contains(&app.config.model));
    assert!(text.contains("System prompt"));
    assert!(text.contains("Skills"));
    assert!(text.contains("/context all to expand"));
}

#[tokio::test]
async fn context_all_expands_and_drops_hint() {
    let mut app = App::new();
    handle_slash(&mut app, "context all").await;
    let Some(Block::Rich(lines)) = app.blocks.last() else {
        panic!("expected a rich block from /context all");
    };
    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    // Expanded view lists items and hides the "expand" hint.
    assert!(!text.contains("/context all to expand"));
}

#[test]
fn rich_block_renders_to_terminal_buffer() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.blocks.push(Block::Rich(vec![ratatui::text::Line::from(
        "CTXMARKER-line",
    )]));
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render(f, &mut app))
        .unwrap();
    let dump: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        dump.contains("CTXMARKER-line"),
        "Block::Rich content must reach the terminal buffer"
    );
}

#[tokio::test]
async fn buddy_starts_hatch_then_locks() {
    let mut app = App::new();
    handle_slash(&mut app, "buddy").await;
    assert!(
        app.hatch.is_some(),
        "/buddy should start the hatch animation"
    );

    finish_hatch(&mut app);
    assert!(
        app.buddy_pet.is_some(),
        "finishing the hatch adopts the pet"
    );
    assert!(app.hatch.is_none());

    // Locked: a second /buddy must NOT re-hatch or spawn another pet.
    handle_slash(&mut app, "buddy").await;
    assert!(app.hatch.is_none(), "must not re-hatch once adopted");
    assert!(
        matches!(app.blocks.last(), Some(Block::System(t)) if t.contains("already")),
        "expected the locked note"
    );
}

// --- Pillar 4: inline viewport ---

fn streaming_assistant(text: &str, done: bool) -> Block {
    Block::Assistant {
        text: text.into(),
        reasoning: String::new(),
        done,
        thought_for_secs: None,
        reasoning_started_at: None,
    }
}

#[test]
fn render_mode_parses_env_value() {
    assert_eq!(RenderMode::from_env_value(Some("1")), RenderMode::Inline);
    assert_eq!(RenderMode::from_env_value(Some(" on ")), RenderMode::Inline);
    assert_eq!(RenderMode::from_env_value(Some("true")), RenderMode::Inline);
    assert_eq!(RenderMode::from_env_value(Some("0")), RenderMode::AltScreen);
    assert_eq!(RenderMode::from_env_value(None), RenderMode::AltScreen);
    assert_eq!(
        RenderMode::from_env_value(Some("nope")),
        RenderMode::AltScreen
    );
}

#[test]
fn committed_end_keeps_streaming_block_while_busy() {
    assert_eq!(committed_end(5, true), 4);
    assert_eq!(committed_end(5, false), 5);
    assert_eq!(committed_end(0, true), 0);
    assert_eq!(committed_end(0, false), 0);
}

#[test]
fn render_inline_shows_live_tail_not_committed_history() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.blocks = vec![
        Block::User("OLDUSERMARK".into()),
        streaming_assistant("OLDANSWERMARK", true),
        streaming_assistant("LIVEANSWERMARK", false),
    ];
    // First two blocks have already been pushed to native scrollback.
    app.committed_blocks = 2;
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render_inline(f, &mut app))
        .unwrap();
    let dump: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        dump.contains("LIVEANSWERMARK"),
        "the live tail must render in the inline viewport"
    );
    assert!(
        !dump.contains("OLDANSWERMARK") && !dump.contains("OLDUSERMARK"),
        "committed history must NOT be redrawn in the viewport (it lives in scrollback)"
    );
}

#[test]
fn commit_advances_cursor_and_leaves_streaming_block() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.committed_blocks = 0;
    app.blocks = vec![
        Block::User("q".into()),
        streaming_assistant("streaming…", false),
    ];
    app.busy = true;
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 12),
        TerminalOptions {
            viewport: Viewport::Inline(6),
        },
    )
    .unwrap();
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 1,
        "while busy, only the finished User block commits; the streaming block stays live"
    );
    // Turn finishes: the streaming block is now finished and commits too.
    app.busy = false;
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(app.committed_blocks, 2);
}

#[test]
fn commit_resets_after_transcript_shrinks() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.blocks = vec![Block::System("x".into())];
    app.committed_blocks = 9; // stale (simulates a /clear that shrank the transcript)
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 12),
        TerminalOptions {
            viewport: Viewport::Inline(6),
        },
    )
    .unwrap();
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 1,
        "a stale cursor past the end resets to 0, then commits the surviving block"
    );
}

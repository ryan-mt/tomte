//! App tests: general behavior — slash parsing, cwd, context/usage rendering,
//! markdown export, streaming/commit, and goal/session snapshots.

use super::app_test_support::*;
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

// `/compact <focus>` records the steer for start_compaction; a bare `/compact`
// clears any stale steer so it can't leak into the next summary.
#[tokio::test]
async fn compact_slash_captures_and_clears_focus() {
    let mut app = App::new();
    app.busy = false;
    app.compacting = false;

    handle_slash(&mut app, "compact the auth refactor").await;
    assert!(app.pending_compact);
    assert_eq!(app.compact_focus.as_deref(), Some("the auth refactor"));

    // The queued run is consumed (start_compaction takes the focus); a bare
    // `/compact` afterwards must reset it to None, not reuse the old steer.
    app.pending_compact = false;
    app.compact_focus = Some("stale".into());
    handle_slash(&mut app, "compact").await;
    assert!(app.pending_compact);
    assert_eq!(app.compact_focus, None);
}

// Guard: `/compact` mid-turn neither queues nor records a steer.
#[tokio::test]
async fn compact_slash_is_a_noop_while_busy() {
    let mut app = App::new();
    app.busy = true;
    handle_slash(&mut app, "compact focus ignored").await;
    assert!(!app.pending_compact);
    assert_eq!(app.compact_focus, None);
}

// `/thoughts [on|off]` toggles the live-reasoning display; bare `/thoughts` flips it.
#[tokio::test]
async fn thoughts_slash_toggles_show_thinking() {
    let mut app = App::new();
    app.config.show_thinking = true;
    handle_slash(&mut app, "thoughts off").await;
    assert!(!app.config.show_thinking);
    handle_slash(&mut app, "thoughts on").await;
    assert!(app.config.show_thinking);
    handle_slash(&mut app, "thoughts").await;
    assert!(
        !app.config.show_thinking,
        "bare /thoughts flips the current state"
    );
}

#[test]
fn subagent_fleet_row_tracks_tokens_not_steps() {
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::SubagentStarted {
            id: "sub-1".into(),
            subagent_type: "Explore".into(),
            prompt: "find the bug".into(),
        },
    );
    assert_eq!(app.subagents.len(), 1);
    assert_eq!(app.subagents[0].tokens, 0);
    // Activity updates the label without bumping any step counter.
    apply_agent_event(
        &mut app,
        AgentEvent::SubagentActivity {
            id: "sub-1".into(),
            summary: "searching".into(),
        },
    );
    assert_eq!(app.subagents[0].activity, "searching");
    // Cumulative output tokens are mirrored onto the row.
    apply_agent_event(
        &mut app,
        AgentEvent::SubagentTokens {
            id: "sub-1".into(),
            output_tokens: 1234,
        },
    );
    assert_eq!(app.subagents[0].tokens, 1234);
}

#[test]
fn clear_rebaselines_window_title_flag() {
    let mut app = App::new();
    app.set_window_title_for_prompt("fix the auth bug");
    assert!(app.window_titled, "first prompt titles the window");
    // A second prompt is a no-op while still titled.
    app.set_window_title_for_prompt("another task");
    assert!(app.window_titled);
    // /clear re-baselines so the next prompt can re-title.
    app.reset_window_title();
    assert!(!app.window_titled);
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
            preflight: None,
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
    use RenderMode::{AltScreen, Inline};
    // The full-screen alternate screen is the default; only an explicit truthy
    // value (TOMTE_INLINE=1 / true / yes / on) opts into the inline viewport
    // (Pillar 4).
    let cases = [
        (None, AltScreen),
        (Some("1"), Inline),
        (Some(" on "), Inline),
        (Some("true"), Inline),
        (Some("yes"), Inline),
        (Some("nope"), AltScreen),
        (Some("0"), AltScreen),
        (Some(" off "), AltScreen),
        (Some("false"), AltScreen),
        (Some("no"), AltScreen),
    ];
    for (value, want) in cases {
        assert_eq!(RenderMode::from_env_value(value), want, "value={value:?}");
    }
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

#[test]
fn commit_keeps_lone_welcome_live_until_conversation_starts() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    // Fresh app: the transcript is just the welcome card, nothing committed.
    assert!(matches!(app.blocks.as_slice(), [Block::Welcome]));
    app.committed_blocks = 0;
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 16),
        TerminalOptions {
            viewport: Viewport::Inline(13),
        },
    )
    .unwrap();
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 0,
        "the lone welcome stays live on the first screen (no scrollback gap above the input)"
    );

    // Once a turn begins, the welcome commits to scrollback like any finished block.
    app.blocks.push(Block::User("hi".into()));
    app.busy = true;
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 1,
        "the welcome commits the moment the conversation grows past it"
    );
}

#[test]
fn first_screen_renders_welcome_and_cute_placeholder() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 16),
        TerminalOptions {
            viewport: Viewport::Inline(13),
        },
    )
    .unwrap();
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
    // The lone welcome must render live in the viewport (it is bottom-anchored
    // right above the input), and the input shows the new playful placeholder.
    assert!(
        dump.contains("hi, i'm tomte!"),
        "the welcome greeting renders in the live viewport, not just scrollback"
    );
    assert!(
        dump.contains("what shall we build today?"),
        "the cute input placeholder renders"
    );
}

#[test]
fn welcome_panel_is_complete_with_setup_and_getting_started() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.last_width = 100;
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
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
    // The welcome is a complete panel, not just a greeting: it carries the
    // tagline, the active setup (model/workspace), and a live getting-started
    // signal — all welcome-only strings, so they can't match the status line.
    for needle in ["keep your codebase tidy", "workspace", "house rules"] {
        assert!(dump.contains(needle), "welcome panel missing {needle:?}");
    }
}

#[test]
fn fleet_idle_verb_is_stable_per_agent_and_from_the_pool() {
    // A finished sub-agent's settled verb must be deterministic per id (no drift
    // once done) and always a real entry from the past-tense pool.
    let v = fleet_idle_verb("agent-7");
    assert_eq!(v, fleet_idle_verb("agent-7"), "same id → same verb");
    assert!(FLEET_IDLE_VERBS.contains(&v), "verb comes from the pool");
    assert!(
        FLEET_IDLE_VERBS.contains(&fleet_idle_verb("a-different-agent-id")),
        "any id maps into the pool"
    );
}

#[test]
fn altscreen_pins_input_to_the_bottom() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.render_mode = RenderMode::AltScreen;
    let h = 24u16;
    let mut terminal = Terminal::new(TestBackend::new(80, h)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render(f, &mut app))
        .unwrap();
    let buf = terminal.backend().buffer();
    let w = buf.area().width as usize;
    let rows: Vec<String> = buf
        .content()
        .chunks(w)
        .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
        .collect();
    // The default full-screen layout: welcome at the top, input pinned to the
    // bottom edge of the terminal.
    let greet = rows
        .iter()
        .position(|r| r.contains("hi, i'm tomte!"))
        .expect("welcome greeting renders");
    let input = rows
        .iter()
        .position(|r| r.contains("what shall we build today?"))
        .expect("input placeholder renders");
    assert!(greet < 8, "welcome sits at the top (row {greet})");
    assert!(
        input >= h as usize - 4,
        "input is pinned to the bottom (row {input} of {h})"
    );
}

#[test]
fn spinner_words_are_a_distinct_hundreds_strong_pool() {
    use std::collections::HashSet;
    // Hundreds of words, every entry unique, so the drift never stalls and
    // never shows the same word twice in a row.
    assert!(
        SPINNER_WORDS.len() >= 150,
        "expected a large pool, got {}",
        SPINNER_WORDS.len()
    );
    let unique: HashSet<&&str> = SPINNER_WORDS.iter().collect();
    assert_eq!(
        unique.len(),
        SPINNER_WORDS.len(),
        "spinner words must all be distinct"
    );
}

#[test]
fn spinner_word_holds_in_window_then_drifts() {
    use std::time::Duration;
    let len = SPINNER_WORDS.len();
    let seed = 7u32;
    let i0 = spinner_word_index(seed, Duration::from_secs(0), len);
    // Holds steady for the whole drift window — no flicker between draws.
    assert_eq!(
        i0,
        spinner_word_index(seed, Duration::from_secs(SPINNER_WORD_SECS - 1), len)
    );
    // Then steps to a different word in the next window.
    assert_ne!(
        i0,
        spinner_word_index(seed, Duration::from_secs(SPINNER_WORD_SECS), len)
    );
    // Any seed / elapsed / pool length yields a valid in-range index (no panic),
    // and a zero-length pool clamps to 0 instead of dividing by zero.
    for seed in [0u32, 1, 42, u32::MAX] {
        for secs in [0u64, SPINNER_WORD_SECS, 999, 1_000_000] {
            for n in [1usize, len, 200] {
                assert!(spinner_word_index(seed, Duration::from_secs(secs), n) < n);
            }
        }
    }
    assert_eq!(spinner_word_index(0, Duration::from_secs(5), 0), 0);
}

#[test]
fn resolve_spinner_words_appends_or_replaces_like_claude() {
    use tomte_core::config::SpinnerVerbs;
    // Empty JSON applies every serde default → a real default Config.
    let mut cfg: tomte_core::config::Config = serde_json::from_str("{}").unwrap();
    let base = SPINNER_WORDS.len();

    // No override → the built-in pool verbatim.
    assert_eq!(resolve_spinner_words(&cfg).len(), base);

    // Append (default): built-in pool + the user's words.
    cfg.spinner_verbs = Some(SpinnerVerbs {
        verbs: vec!["Hacking".into(), "Vibing".into()],
        exclude_default: false,
    });
    let appended = resolve_spinner_words(&cfg);
    assert_eq!(appended.len(), base + 2);
    assert!(appended.iter().any(|w| w == "Hacking"));
    assert!(appended.iter().any(|w| w == "Pottering"), "built-in kept");

    // Replace: only the user's words.
    cfg.spinner_verbs = Some(SpinnerVerbs {
        verbs: vec!["Solo".into()],
        exclude_default: true,
    });
    assert_eq!(resolve_spinner_words(&cfg), vec!["Solo".to_string()]);

    // Replace with no words → keep the built-in pool (never leave nothing).
    cfg.spinner_verbs = Some(SpinnerVerbs {
        verbs: vec![],
        exclude_default: true,
    });
    assert_eq!(resolve_spinner_words(&cfg).len(), base);
}

#[test]
fn spinner_prefers_the_active_task_then_a_pool_word() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let render_to_string = |app: &mut App| -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::tui::ui::render(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    };

    let mut app = App::new();
    app.render_mode = RenderMode::AltScreen;
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());

    // Claude-parity: an in-progress task shows ITS active form on the spinner.
    app.session_todos = vec![tomte_core::tools::TodoItem {
        content: "refactor the parser".into(),
        status: tomte_core::tools::TodoStatus::InProgress,
        active_form: "Refactoring the parser".into(),
        id: None,
        blocked_by: vec![],
    }];
    assert!(
        render_to_string(&mut app).contains("Refactoring the parser"),
        "spinner shows the active task's form"
    );

    // No task in progress → it falls back to a word from the pool. Seed 0 at ~0s
    // maps to index 0, so the first pool word must appear.
    app.session_todos.clear();
    app.spinner_seed = 0;
    let first = app.spinner_words[0].clone();
    assert!(
        render_to_string(&mut app).contains(first.as_str()),
        "spinner falls back to a pool word ({first})"
    );
}

// ---- decision trail surfaced inside the TUI (Pillar 2 parity with the CLI) ----

#[tokio::test]
async fn blame_slash_reports_an_empty_trail_for_a_file() {
    let mut app = App::new();
    // A fresh, unique cwd keys to an empty trail, so the read is deterministic
    // and never touches a real project's decisions.jsonl.
    app.cwd = std::env::temp_dir().join(format!("tomte-blame-{}", rand::random::<u64>()));
    handle_slash(&mut app, "blame src/x.rs").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /blame");
    };
    assert!(text.contains("no decisions recorded"), "{text}");
    assert!(text.contains("src/x.rs"), "{text}");
}

#[tokio::test]
async fn blame_slash_without_a_file_shows_usage() {
    let mut app = App::new();
    handle_slash(&mut app, "blame").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /blame");
    };
    assert!(text.contains("Usage: /blame"), "{text}");
}

#[tokio::test]
async fn why_reconcile_reports_a_tidy_trail_in_the_tui() {
    let mut app = App::new();
    app.cwd = std::env::temp_dir().join(format!("tomte-why-recon-{}", rand::random::<u64>()));
    handle_slash(&mut app, "why --reconcile").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /why --reconcile");
    };
    assert!(text.contains("in order"), "{text}");
}

#[tokio::test]
async fn model_switch_announces_the_trail_follows_only_when_non_empty() {
    let mut app = App::new();
    app.cwd = std::env::temp_dir().join(format!("tomte-trail-follow-{}", rand::random::<u64>()));
    // Empty trail → nothing to carry → stay silent.
    app.note_trail_follows_model("claude-opus-4-8");
    assert!(
        !app.blocks
            .iter()
            .any(|b| matches!(b, Block::System(t) if t.contains("follow you"))),
        "an empty trail must not announce anything"
    );
    // Seed one decision, then switch → one announcement naming the target model.
    let rec = tomte_core::decisions::DecisionRecord {
        loc: "src/a.rs:1".into(),
        decision: "use argon2".into(),
        why: "memory-hard".into(),
        rejected: Vec::new(),
        model: "gpt-5.5".into(),
        ts: 1,
        anchor: None,
        supersedes: None,
    };
    tomte_core::decisions::append(&app.cwd, &rec).unwrap();
    app.note_trail_follows_model("claude-opus-4-8");
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from the model switch");
    };
    assert!(text.contains("follow you to claude-opus-4-8"), "{text}");
    assert!(text.contains("1 recorded decision"), "{text}");
    // Clean up the per-project artifact written under the config dir.
    let store = tomte_core::decisions::store_path(&app.cwd);
    let _ = std::fs::remove_file(&store);
    if let Some(parent) = store.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}

#[test]
fn conscience_conflict_event_raises_the_card() {
    // Pillar 5 (A2 Tier 2): the event puts the three-way card into pending state
    // with the overturned decision's details, selection defaulting to abort.
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::ConscienceConflict {
            call_id: "c1".into(),
            tool_name: "edit_file".into(),
            file: "src/auth.rs".into(),
            ts: 42,
            prev_decision: "use argon2".into(),
            prev_model: "gpt-5.5".into(),
            reason: "switches to bcrypt".into(),
        },
    );
    let p = app
        .pending_conscience
        .as_ref()
        .expect("the conflict event should raise the card");
    assert_eq!(p.file, "src/auth.rs");
    assert_eq!(p.ts, 42);
    assert_eq!(p.prev_decision, "use argon2");
    assert_eq!(p.selected, 0);
}

#[test]
fn decision_overturned_event_logs_the_audit_line() {
    // Pillar 5 (A3): a supersede surfaces an on-the-record "superseded" line;
    // an edit-anyway (recorded=false) reads as "overridden".
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::DecisionOverturned {
            file: "src/auth.rs".into(),
            prev_decision: "use argon2".into(),
            prev_model: "gpt-5.5".into(),
            reason: "switched to bcrypt".into(),
            recorded: true,
        },
    );
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected an audit system block");
    };
    assert!(text.contains("superseded"), "{text}");
    assert!(
        text.contains("src/auth.rs") && text.contains("argon2"),
        "{text}"
    );

    let mut app2 = App::new();
    apply_agent_event(
        &mut app2,
        AgentEvent::DecisionOverturned {
            file: "x.rs".into(),
            prev_decision: "d".into(),
            prev_model: "m".into(),
            reason: "r".into(),
            recorded: false,
        },
    );
    let Some(Block::System(t2)) = app2.blocks.last() else {
        panic!("expected an audit system block");
    };
    assert!(t2.contains("overridden"), "{t2}");
}

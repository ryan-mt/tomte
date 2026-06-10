use super::super::app_test_support::*;
use super::super::*;

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

// `/prove` arms the deferred background collection; a second `/prove` while one
// is already running is a no-op (the `proving` guard) so it can't double-spawn.
#[tokio::test]
async fn prove_slash_arms_collection_and_guards_double_run() {
    let mut app = App::new();
    assert!(!app.pending_prove);

    handle_slash(&mut app, "prove").await;
    assert!(app.pending_prove);

    // Simulate main_loop having started the run: the flag is consumed and
    // `proving` is set. A second `/prove` must NOT re-arm it.
    app.pending_prove = false;
    app.proving = true;
    handle_slash(&mut app, "prove").await;
    assert!(
        !app.pending_prove,
        "must not re-arm while a run is in flight"
    );
    let last = app.blocks.last().expect("a note");
    assert!(
        matches!(last, Block::System(s) if s.contains("Already collecting")),
        "should report the in-flight run"
    );
}

// `/memory edit` arms the deferred editor open at rest; mid-turn it leaves the
// flag down (the suspend would fight the streaming viewport — and the post-edit
// refresh would stall on the agent mutex) and says so.
#[tokio::test]
async fn memory_edit_slash_arms_editor_only_at_rest() {
    let mut app = App::new();
    app.busy = false;
    app.compacting = false;
    handle_slash(&mut app, "memory edit").await;
    assert!(app.open_memory_editor, "at rest → editor request");

    let mut app = App::new();
    app.busy = true;
    handle_slash(&mut app, "memory edit").await;
    assert!(!app.open_memory_editor, "mid-turn must not suspend the TUI");
    let last = app.blocks.last().expect("a note");
    assert!(
        matches!(last, Block::System(s) if s.contains("mid-turn")),
        "should explain why nothing opened"
    );
}

// `/prove explain` arms the collection AND the explain follow-up; a bare
// `/prove` must clear any stale explain flag; an unknown arg prints usage.
#[tokio::test]
async fn prove_explain_arms_the_follow_up_and_bare_prove_clears_it() {
    let mut app = App::new();

    handle_slash(&mut app, "prove explain").await;
    assert!(app.pending_prove);
    assert!(app.prove_explain, "explain must arm the follow-up");

    // Simulate the run finishing, then a bare `/prove`: no stale explain.
    app.pending_prove = false;
    app.prove_explain = false;
    handle_slash(&mut app, "prove").await;
    assert!(app.pending_prove);
    assert!(!app.prove_explain, "bare /prove must not inherit explain");

    let mut app2 = App::new();
    handle_slash(&mut app2, "prove nonsense").await;
    assert!(!app2.pending_prove, "unknown arg must not arm a run");
    assert!(
        matches!(app2.blocks.last(), Some(Block::System(s)) if s.contains("Usage: /prove")),
        "unknown arg prints usage"
    );
}

// `/why-context` with no seed prints usage instead of building the twin —
// the X-ray needs a file/symbol to trace, so a bare call must stay instant.
#[tokio::test]
async fn why_context_slash_without_seed_prints_usage() {
    let mut app = App::new();
    handle_slash(&mut app, "why-context").await;
    let last = app.blocks.last().expect("a note");
    assert!(
        matches!(last, Block::System(s) if s.contains("Usage: /why-context")),
        "bare /why-context should print usage"
    );
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
            thinking_expanded: false,
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

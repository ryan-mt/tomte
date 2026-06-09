//! App tests: plan-mode entry/exit gating and its interaction with active goals.

use super::*;

#[test]
fn plan_mode_required_forces_session_plan_without_persisting_config_mode() {
    let mut app = App::new();
    let persisted_mode = app.config.default_permission_mode.clone();

    apply_plan_mode_required(&mut app);

    assert_eq!(app.permission_mode(), PermissionMode::Plan);
    assert_eq!(app.config.default_permission_mode, persisted_mode);
    match app.blocks.last() {
        Some(Block::System(text)) => assert!(text.contains("plan mode required")),
        other => panic!("expected plan mode required system block, got {other:?}"),
    }
}

#[test]
fn model_enter_plan_mode_is_session_only_and_does_not_persist_config_mode() {
    let mut app = App::new();
    app.config.default_permission_mode = PermissionMode::Default.config_str().to_string();
    app.set_permission_mode(PermissionMode::Default);
    app.pending_plan_exit = Some(PendingPlanExit {
        plan: "old plan".to_string(),
    });

    apply_agent_event(&mut app, AgentEvent::PlanModeRequested);

    assert_eq!(app.permission_mode(), PermissionMode::Plan);
    assert_eq!(
        app.config.default_permission_mode,
        PermissionMode::Default.config_str()
    );
    assert!(app.pending_plan_exit.is_none());
    match app.blocks.last() {
        Some(Block::System(text)) => assert!(text.contains("plan mode")),
        other => panic!("expected plan mode system block, got {other:?}"),
    }
}

#[test]
fn active_goal_pauses_when_agent_requests_user_input() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("choose a safe path".to_string()));
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallStarted {
            name: "ask_user_question".to_string(),
            call_id: "ask_1".to_string(),
        },
    );

    apply_agent_event(
        &mut app,
        AgentEvent::ToolResult {
            call_id: "ask_1".to_string(),
            output: "{}".to_string(),
            error: false,
        },
    );
    apply_agent_event(&mut app, AgentEvent::TurnComplete);

    let goal = app.active_goal.as_ref().expect("goal should remain active");
    assert!(goal.waiting_for_user);
    assert!(app.message_queue.is_empty());
    assert!(app.pending_session_save);
}

#[test]
fn active_goal_pauses_after_agent_error() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());

    apply_agent_event(
        &mut app,
        AgentEvent::Error {
            message: "provider 400 invalid schema".to_string(),
        },
    );

    let goal = app.active_goal.as_ref().expect("goal should remain active");
    assert!(goal.waiting_for_user);
    assert_eq!(
        goal.last_summary.as_deref(),
        Some("paused after turn error: provider 400 invalid schema")
    );
    assert!(!app.busy);
    assert!(app.turn_started_at.is_none());
    assert!(app.message_queue.is_empty());
    assert!(app.pending_session_save);
}

#[test]
fn plan_exit_request_waits_for_user_approval() {
    let mut app = App::new();
    app.set_permission_mode(PermissionMode::Plan);

    apply_agent_event(
        &mut app,
        AgentEvent::PlanExitRequested {
            plan: "1. Patch\n2. Test".to_string(),
        },
    );

    assert_eq!(
        app.pending_plan_exit.as_ref().map(|p| p.plan.as_str()),
        Some("1. Patch\n2. Test")
    );
    match app.blocks.last() {
        Some(Block::System(text)) => {
            assert!(text.contains("Plan ready for approval"));
            assert!(text.contains("Press Y"));
        }
        other => panic!("expected plan approval prompt, got {other:?}"),
    }
}

#[test]
fn plan_exit_request_pauses_active_goal_continuation() {
    let mut app = App::new();
    app.set_permission_mode(PermissionMode::Plan);
    app.active_goal = Some(ActiveGoal::new("ship the feature".to_string()));

    apply_agent_event(
        &mut app,
        AgentEvent::PlanExitRequested {
            plan: "1. Patch\n2. Test".to_string(),
        },
    );
    apply_agent_event(&mut app, AgentEvent::TurnComplete);

    let goal = app.active_goal.as_ref().expect("goal should remain active");
    assert!(goal.waiting_for_user);
    assert_eq!(
        goal.last_summary.as_deref(),
        Some("waiting for plan approval")
    );
    assert!(app.message_queue.is_empty());
    assert!(app.pending_session_save);
}

#[test]
fn approving_plan_exit_leaves_plan_mode_and_queues_implementation() {
    let mut app = App::new();
    app.config.default_permission_mode = PermissionMode::Default.config_str().to_string();
    app.set_permission_mode(PermissionMode::Plan);
    let mut goal = ActiveGoal::new("ship the feature".to_string());
    goal.waiting_for_user = true;
    goal.last_summary = Some("waiting for plan approval".to_string());
    app.active_goal = Some(goal);
    app.pending_plan_exit = Some(PendingPlanExit {
        plan: "1. Patch\n2. Test".to_string(),
    });

    handle_plan_exit_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );

    assert_eq!(app.permission_mode(), PermissionMode::Default);
    assert!(app.pending_plan_exit.is_none());
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].starts_with(PLAN_APPROVED_PREFIX));
    assert!(app.message_queue[0].contains("Approved plan"));
    let goal = app.active_goal.as_ref().expect("goal should stay active");
    assert!(!goal.waiting_for_user);
    assert_eq!(goal.last_summary.as_deref(), Some("plan approved"));
    assert!(app.pending_session_save);
}

#[test]
fn pending_plan_exit_holds_queued_type_ahead_until_approved() {
    // Repro: user types a follow-up while the agent is planning (it queues),
    // then the turn ends with exit_plan_mode. The queued message must NOT
    // flush a new turn while the approval prompt is up — otherwise `busy`
    // flips true and the Y keystroke is swallowed, locking the user out.
    let mut app = App::new();
    app.set_permission_mode(PermissionMode::Plan);
    app.message_queue
        .push("also handle the edge case".to_string());

    apply_agent_event(
        &mut app,
        AgentEvent::PlanExitRequested {
            plan: "1. Patch\n2. Test".to_string(),
        },
    );
    apply_agent_event(&mut app, AgentEvent::TurnComplete);

    assert!(!app.busy);
    assert!(app.pending_plan_exit.is_some());
    // The flush gate must refuse to launch while the decision is pending.
    assert!(turn_launch_blocked_by_pending_decision(&app));
    assert_eq!(app.message_queue.len(), 1);

    // Approving clears the gate and queues the implementation prompt, so the
    // next flush is allowed (now with both the approval and the type-ahead).
    handle_plan_exit_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );

    assert!(!turn_launch_blocked_by_pending_decision(&app));
    assert!(app.pending_plan_exit.is_none());
    assert!(app
        .message_queue
        .iter()
        .any(|m| m.starts_with(PLAN_APPROVED_PREFIX)));
}

#[test]
fn approving_plan_exit_restores_configured_non_plan_mode() {
    let mut app = App::new();
    app.config.default_permission_mode = PermissionMode::AcceptEdits.config_str().to_string();
    app.set_permission_mode(PermissionMode::Plan);
    app.pending_plan_exit = Some(PendingPlanExit {
        plan: "1. Patch\n2. Test".to_string(),
    });

    handle_plan_exit_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );

    assert_eq!(app.permission_mode(), PermissionMode::AcceptEdits);
    assert_eq!(
        app.config.default_permission_mode,
        PermissionMode::AcceptEdits.config_str()
    );
}

#[test]
fn approving_plan_exit_never_keeps_session_in_plan_mode() {
    let mut app = App::new();
    app.config.default_permission_mode = PermissionMode::Plan.config_str().to_string();
    app.set_permission_mode(PermissionMode::Plan);
    app.pending_plan_exit = Some(PendingPlanExit {
        plan: "1. Patch\n2. Test".to_string(),
    });

    handle_plan_exit_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );

    assert_eq!(app.permission_mode(), PermissionMode::Default);
    assert_eq!(
        app.config.default_permission_mode,
        PermissionMode::Plan.config_str()
    );
}

#[test]
fn rejecting_plan_exit_stays_in_plan_mode_and_queues_revision() {
    let mut app = App::new();
    app.set_permission_mode(PermissionMode::Plan);
    let mut goal = ActiveGoal::new("ship the feature".to_string());
    goal.waiting_for_user = true;
    goal.last_summary = Some("waiting for plan approval".to_string());
    app.active_goal = Some(goal);
    app.pending_plan_exit = Some(PendingPlanExit {
        plan: "1. Patch\n2. Test".to_string(),
    });

    handle_plan_exit_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
    );

    assert_eq!(app.permission_mode(), PermissionMode::Plan);
    assert!(app.pending_plan_exit.is_none());
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].starts_with(PLAN_REJECTED_PREFIX));
    assert!(app.message_queue[0].contains("Rejected plan"));
    let goal = app.active_goal.as_ref().expect("goal should stay active");
    assert!(!goal.waiting_for_user);
    assert_eq!(
        goal.last_summary.as_deref(),
        Some("plan rejected; revising")
    );
    assert!(app.pending_session_save);
}

#[test]
fn internal_goal_prompt_renders_as_system_progress_not_full_user_message() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

    push_visible_user_block(&mut app, &goal_start_prompt("finish verification"));

    match app.blocks.last() {
        Some(Block::System(text)) => assert!(text.contains("Goal running")),
        other => panic!("expected system goal progress block, got {other:?}"),
    }
}

#[tokio::test]
async fn tab_autocompletes_highlighted_slash_command() {
    let mut app = App::new();
    app.input.set_text("/".to_string());
    app.open_overlay(OverlayKind::SlashMenu);
    // Filter to a single command so the highlight is deterministic.
    if let Some((_, p)) = app.overlay.as_mut() {
        p.query = "model".to_string();
        p.ensure_visible_selected();
    }

    handle_overlay_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .await
        .unwrap();

    // The command is completed into the composer and the menu closes, so
    // the user can add arguments or press Enter to run it.
    assert!(app.overlay.is_none(), "menu should close after Tab");
    assert_eq!(app.input.buffer, "/model ");
}

#[test]
fn empty_args_done_does_not_clear_streamed_tool_args() {
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallStarted {
            name: "read_file".to_string(),
            call_id: "call_1".to_string(),
        },
    );
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallArgsDelta {
            call_id: "call_1".to_string(),
            delta: "{\"path\":\"src/lib.rs\"}".to_string(),
        },
    );
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallArgsDone {
            call_id: "call_1".to_string(),
            arguments: String::new(),
        },
    );

    match find_tool_mut(&mut app.blocks, "call_1") {
        Some(Block::Tool { args, .. }) => assert_eq!(args, "{\"path\":\"src/lib.rs\"}"),
        other => panic!("expected tool block, got {other:?}"),
    }
}

#[test]
fn finishing_open_assistant_drops_empty_block() {
    let mut blocks = vec![Block::Assistant {
        text: String::new(),
        reasoning: String::new(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
    }];

    finish_open_assistant_block(&mut blocks);
    assert!(blocks.is_empty());
}

#[test]
fn finishing_open_assistant_marks_non_empty_block_done() {
    let mut blocks = vec![Block::Assistant {
        text: "hello".to_string(),
        reasoning: String::new(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
    }];

    finish_open_assistant_block(&mut blocks);
    match &blocks[0] {
        Block::Assistant { done, .. } => assert!(*done),
        other => panic!("expected assistant block, got {other:?}"),
    }
}

#[tokio::test]
async fn cancel_current_turn_clears_pending_approval_sender() {
    let mut app = App::new();
    app.busy = true;
    app.pending_approval = Some(PendingApproval {
        call_id: "call_1".to_string(),
        tool_name: "run_shell".to_string(),
        args_json: "{}".to_string(),
        diff_preview: None,
        selected: 0,
    });

    let approvals = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let (tx, rx) = tokio::sync::oneshot::channel();
    approvals.lock().await.insert("call_1".to_string(), tx);
    app.approval_handle = Some(approvals.clone());

    cancel_current_turn(&mut app).await;

    assert!(!app.busy);
    assert!(app.pending_approval.is_none());
    assert!(app.approval_handle.is_none());
    assert!(!approvals.lock().await.contains_key("call_1"));
    assert_eq!(rx.await, Ok(false));
}

#[test]
fn tool_result_leaves_render_cache_alone() {
    // Event handlers no longer manage the render cache: the mutated tool
    // block lives in the live tail, which `render_chat` re-wraps every frame
    // (correctness of the rendered output is covered by the cache-vs-fresh
    // equivalence test in `ui::tests`). A handler that started clearing the
    // cache again would silently reintroduce a full-transcript rebuild per
    // tool event — the original streaming-jank bug.
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallStarted {
            name: "read_file".to_string(),
            call_id: "c1".to_string(),
        },
    );
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallArgsDone {
            call_id: "c1".to_string(),
            arguments: "{\"path\":\"x\"}".to_string(),
        },
    );
    // Stand in for a frame the renderer already cached.
    app.chat_render_cache = Some(ChatRenderCache {
        inner_width: 80,
        expanded_tools: false,
        show_thinking: true,
        welcome_fp: 0,
        stable_blocks: 0,
        stable_fp: 0,
        lines: Vec::new(),
    });
    apply_agent_event(
        &mut app,
        AgentEvent::ToolResult {
            call_id: "c1".to_string(),
            output: "done".to_string(),
            error: false,
        },
    );
    assert!(
        app.chat_render_cache.is_some(),
        "tool result must not clear the render cache (the live tail re-wraps per frame)"
    );
}

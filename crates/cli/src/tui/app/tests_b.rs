//! App tests (part b), split out of `app`.

use super::tests_common::*;
use super::*;

#[tokio::test]
async fn buddy_reset_allows_rehatch() {
    let mut app = App::new();
    handle_slash(&mut app, "buddy").await;
    finish_hatch(&mut app);
    assert!(app.buddy_pet.is_some());

    handle_slash(&mut app, "buddy reset").await;
    assert!(app.buddy_pet.is_none(), "reset clears the adopted pet");

    handle_slash(&mut app, "buddy").await;
    assert!(app.hatch.is_some(), "can hatch again after reset");
}

#[tokio::test]
async fn buddy_off_hides_the_corner_companion() {
    let mut app = App::new();
    handle_slash(&mut app, "buddy off").await;
    assert!(app.buddy_hidden, "/buddy off should hide the corner buddy");
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /buddy off");
    };
    assert!(text.contains("hidden"), "{text}");
}

#[tokio::test]
async fn usage_slash_without_quota_shows_hint() {
    let mut app = App::new();
    handle_slash(&mut app, "usage").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /usage");
    };
    assert!(text.contains("Usage — provider:"), "{text}");
    assert!(text.contains(&app.config.model), "{text}");
    assert!(text.contains("No live quota data yet"), "{text}");
}

#[tokio::test]
async fn usage_slash_renders_captured_quota() {
    let mut app = App::new();
    app.last_quota = Some(tomte_core::usage::QuotaSnapshot {
        provider: Some(tomte_core::provider::Provider::OpenAi),
        plan: Some("pro".into()),
        windows: vec![tomte_core::usage::QuotaWindow {
            label: "5h".into(),
            used_percent: Some(12.5),
            remaining: None,
            limit: None,
            resets_at_epoch: None,
        }],
        captured_at_epoch: 0,
    });
    handle_slash(&mut app, "usage").await;
    let Some(Block::System(text)) = app.blocks.last() else {
        panic!("expected a system block from /usage");
    };
    assert!(text.contains("Plan: pro"), "{text}");
    assert!(text.contains("5-hour: 12.5% used"), "{text}");
}

#[test]
fn record_history_is_bounded() {
    let mut app = App::new();
    for i in 0..(MAX_INPUT_HISTORY + 50) {
        app.record_history(&format!("msg {i}"));
    }
    assert_eq!(app.input_history.len(), MAX_INPUT_HISTORY);
    // Oldest dropped, newest kept.
    assert_eq!(app.input_history.first().unwrap(), "msg 50");
    assert_eq!(
        app.input_history.last().unwrap(),
        &format!("msg {}", MAX_INPUT_HISTORY + 49)
    );
}

#[test]
fn up_down_recalls_submitted_messages() {
    let mut app = App::new();
    for m in ["first", "second", "third"] {
        app.record_history(m);
    }
    // An in-progress draft is stashed; Up recalls newest → oldest.
    app.input.set_text("draft".into());
    app.history_prev();
    assert_eq!(app.input.buffer, "third");
    app.history_prev();
    assert_eq!(app.input.buffer, "second");
    app.history_prev();
    assert_eq!(app.input.buffer, "first");
    // Clamps at the oldest entry.
    app.history_prev();
    assert_eq!(app.input.buffer, "first");
    // Down walks back toward newer, then restores the draft past the newest.
    app.history_next();
    assert_eq!(app.input.buffer, "second");
    app.history_next();
    assert_eq!(app.input.buffer, "third");
    app.history_next();
    assert_eq!(app.input.buffer, "draft");
}

#[tokio::test]
async fn commit_slash_queues_prompt_with_git_safety_protocol() {
    let mut app = App::new();
    handle_slash(&mut app, "commit").await;
    assert_eq!(app.message_queue.len(), 1);
    let p = &app.message_queue[0];
    assert!(p.contains("Conventional-Commits"));
    assert!(p.contains("NEVER force-push"));
    assert!(p.contains("--no-verify"));
}

#[tokio::test]
async fn commit_push_pr_slash_queues_prompt_with_pr_step_and_user_arg() {
    let mut app = App::new();
    handle_slash(&mut app, "commit-push-pr fixes #42").await;
    assert_eq!(app.message_queue.len(), 1);
    let p = &app.message_queue[0];
    assert!(p.contains("gh pr create"));
    assert!(p.contains("fixes #42"));
    assert!(p.contains("force-push"));
}

#[tokio::test]
async fn goal_slash_asks_before_replacing_active_goal() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

    handle_slash(&mut app, "goal ship a different feature").await;

    assert_eq!(
        app.active_goal.as_ref().map(|g| g.objective.as_str()),
        Some("finish verification")
    );
    assert_eq!(
        app.pending_goal_replacement
            .as_ref()
            .map(|p| p.objective.as_str()),
        Some("ship a different feature")
    );
    assert!(app.message_queue.is_empty());
    match app.blocks.last() {
        Some(Block::System(text)) => {
            assert!(text.contains("Replace the current goal?"));
            assert!(text.contains("Press Y"));
            assert!(text.contains("N/Esc"));
        }
        other => panic!("expected replacement confirmation, got {other:?}"),
    }
}

#[test]
fn confirming_goal_replacement_starts_new_goal() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("old goal".to_string()));
    app.pending_goal_replacement = Some(PendingGoalReplacement {
        objective: "new goal".to_string(),
    });

    handle_goal_replacement_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );

    assert_eq!(
        app.active_goal.as_ref().map(|g| g.objective.as_str()),
        Some("new goal")
    );
    assert!(app.pending_goal_replacement.is_none());
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].starts_with(GOAL_START_PREFIX));
    assert!(app.message_queue[0].contains("new goal"));
}

#[test]
fn declining_goal_replacement_keeps_goal_and_continues() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("old goal".to_string()));
    app.pending_goal_replacement = Some(PendingGoalReplacement {
        objective: "new goal".to_string(),
    });

    handle_goal_replacement_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
    );

    assert_eq!(
        app.active_goal.as_ref().map(|g| g.objective.as_str()),
        Some("old goal")
    );
    assert!(app.pending_goal_replacement.is_none());
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].starts_with(GOAL_CONTINUATION_PREFIX));
    assert!(app.message_queue[0].contains("old goal"));
}

#[test]
fn active_goal_queues_continuation_after_turn_complete() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

    apply_agent_event(&mut app, AgentEvent::TurnComplete);

    let goal = app.active_goal.as_ref().expect("goal should remain active");
    assert_eq!(goal.turns_completed, 1);
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].contains("Continue the active /goal"));
    assert!(app.message_queue[0].contains("finish verification"));
    assert!(app.pending_session_save);
}

#[test]
fn active_goal_keeps_continuing_past_previous_turn_cap() {
    let mut app = App::new();
    let mut goal = ActiveGoal::new("finish a long migration".to_string());
    goal.turns_completed = 50;
    app.active_goal = Some(goal);

    apply_agent_event(&mut app, AgentEvent::TurnComplete);

    let goal = app.active_goal.as_ref().expect("goal should remain active");
    assert_eq!(goal.turns_completed, 51);
    assert_eq!(app.message_queue.len(), 1);
    assert!(app.message_queue[0].contains("finish a long migration"));
}

#[test]
fn todo_snapshot_tracks_recent_completion_transitions() {
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::TodosSnapshot {
            todos: vec![todo("run tests", TodoStatus::Pending)],
        },
    );
    assert!(app.todo_completed_at.is_empty());

    apply_agent_event(
        &mut app,
        AgentEvent::TodosSnapshot {
            todos: vec![todo("run tests", TodoStatus::Completed)],
        },
    );
    assert!(app
        .todo_completed_at
        .contains_key(&todo_completion_key(&todo(
            "run tests",
            TodoStatus::Completed
        ))));

    apply_agent_event(
        &mut app,
        AgentEvent::TodosSnapshot {
            todos: vec![todo("new task", TodoStatus::Pending)],
        },
    );
    assert!(app.todo_completed_at.is_empty());
}

#[test]
fn empty_todo_snapshot_keeps_recent_completed_todos_until_ttl() {
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::TodosSnapshot {
            todos: vec![todo("run tests", TodoStatus::Completed)],
        },
    );
    assert_eq!(app.session_todos.len(), 1);
    assert!(!app.todo_completed_at.is_empty());

    apply_agent_event(&mut app, AgentEvent::TodosSnapshot { todos: Vec::new() });

    assert_eq!(app.session_todos.len(), 1);
    assert!(matches!(app.session_todos[0].status, TodoStatus::Completed));

    let expired_at =
        std::time::Instant::now() - (TODO_RECENT_COMPLETED_TTL + Duration::from_secs(1));
    for completed_at in app.todo_completed_at.values_mut() {
        *completed_at = expired_at;
    }
    prune_expired_completed_todos(&mut app);

    assert!(app.session_todos.is_empty());
    assert!(app.todo_completed_at.is_empty());
}

#[test]
fn completed_todos_without_recent_timestamp_are_pruned() {
    let mut app = App::new();
    app.session_todos = vec![todo("run tests", TodoStatus::Completed)];

    prune_expired_completed_todos(&mut app);

    assert!(app.session_todos.is_empty());
    assert!(app.todo_completed_at.is_empty());
}

#[test]
fn empty_todo_snapshot_clears_non_completed_todos() {
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::TodosSnapshot {
            todos: vec![todo("run tests", TodoStatus::InProgress)],
        },
    );

    apply_agent_event(&mut app, AgentEvent::TodosSnapshot { todos: Vec::new() });

    assert!(app.session_todos.is_empty());
    assert!(app.todo_completed_at.is_empty());
}

#[test]
fn goal_update_complete_clears_active_goal_before_continuation() {
    let mut app = App::new();
    app.active_goal = Some(ActiveGoal::new("finish verification".to_string()));

    apply_agent_event(
        &mut app,
        AgentEvent::GoalStatusUpdated {
            status: "complete".to_string(),
            summary: "verified".to_string(),
        },
    );
    apply_agent_event(&mut app, AgentEvent::TurnComplete);

    assert!(app.active_goal.is_none());
    assert!(app.message_queue.is_empty());
    assert!(app.pending_session_save);
}

#[test]
fn failed_compaction_rearms_auto_compaction() {
    // The success arm clears auto_compact_done_this_window; a failed/no-op
    // compaction must clear it too, or a session that can't summarize stays
    // disarmed for the whole over-threshold window and drifts into a hard
    // context overflow.
    let mut app = App::new();
    app.auto_compact_done_this_window = true;
    apply_agent_event(
        &mut app,
        AgentEvent::CompactDone {
            original_len: 0,
            error: Some("summary request overflowed".to_string()),
        },
    );
    assert!(!app.auto_compact_done_this_window);
}

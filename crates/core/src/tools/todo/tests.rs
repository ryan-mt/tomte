use super::*;
use crate::config::Config;
use crate::tools::{ApprovalMode, SessionState};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

fn ctx() -> ToolContext {
    ToolContext {
        cwd: PathBuf::from("/tmp"),
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

#[test]
fn parameters_schema_prefers_claude_code_active_form_spelling() {
    let schema = TodoWrite.parameters_schema();
    let item = &schema["properties"]["todos"]["items"];
    assert!(item["properties"].get("activeForm").is_some());
    assert!(item["properties"].get("active_form").is_none());
    let required = item["required"].as_array().expect("required array");
    assert!(required.iter().any(|v| v == "activeForm"));
    assert!(!required.iter().any(|v| v == "active_form"));
}

#[test]
fn todo_write_is_session_only_read_only_for_plan_mode() {
    assert!(TodoWrite.is_read_only());
}

#[tokio::test]
async fn execute_stores_incomplete_todo_list() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Read the code",
                        "active_form": "Reading the code",
                        "status": "completed"
                    },
                    {
                        "content": "Run tests",
                        "active_form": "Running tests",
                        "status": "in_progress"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write succeeds");

    assert_eq!(out, "Stored 2 todo(s) — 0 pending, 1 in progress");
    let session = ctx.session.lock().await;
    assert_eq!(session.todos.len(), 2);
    assert!(matches!(session.todos[1].status, TodoStatus::InProgress));
}

#[tokio::test]
async fn execute_accepts_claude_code_active_form_spelling() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Run tests",
                        "activeForm": "Running tests",
                        "status": "in_progress"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write accepts activeForm");

    assert_eq!(out, "Stored 1 todo(s) — 0 pending, 1 in progress");
    let session = ctx.session.lock().await;
    assert_eq!(session.todos[0].active_form, "Running tests");
}

#[tokio::test]
async fn execute_accepts_common_status_aliases() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Read code",
                        "activeForm": "Reading code",
                        "status": "done"
                    },
                    {
                        "content": "Run tests",
                        "activeForm": "Running tests",
                        "status": "in progress"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write accepts semantic status aliases");

    assert_eq!(out, "Stored 2 todo(s) — 0 pending, 1 in progress");
    let session = ctx.session.lock().await;
    assert!(matches!(session.todos[0].status, TodoStatus::Completed));
    assert!(matches!(session.todos[1].status, TodoStatus::InProgress));
}

#[tokio::test]
async fn execute_rejects_empty_content_or_active_form() {
    let ctx = ctx();
    let err = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "",
                        "activeForm": "Running tests",
                        "status": "in_progress"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect_err("empty content is rejected");
    assert!(err.to_string().contains("content cannot be empty"));

    let err = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Run tests",
                        "activeForm": " ",
                        "status": "in_progress"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect_err("empty active form is rejected");
    assert!(err
        .to_string()
        .contains("active_form/activeForm cannot be empty"));
}

#[tokio::test]
async fn execute_clears_session_todos_when_all_completed() {
    let mut ctx = ctx();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    ctx.events = Some(tx);
    {
        let mut session = ctx.session.lock().await;
        session.todos.push(TodoItem {
            content: "Old item".to_string(),
            status: TodoStatus::InProgress,
            active_form: "Working old item".to_string(),
            id: None,
            blocked_by: Vec::new(),
        });
    }

    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Read the code",
                        "active_form": "Reading the code",
                        "status": "completed"
                    },
                    {
                        "content": "Run tests",
                        "active_form": "Running tests",
                        "status": "completed"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write succeeds");

    assert_eq!(out, "Completed 2 todo(s); cleared the session todo list");
    match rx.recv().await.expect("completed snapshot event") {
        AgentEvent::TodosSnapshot { todos } => {
            assert_eq!(todos.len(), 2);
            assert!(todos
                .iter()
                .all(|todo| matches!(todo.status, TodoStatus::Completed)));
        }
        other => panic!("expected todos snapshot, got {other:?}"),
    }
    let session = ctx.session.lock().await;
    assert!(session.todos.is_empty());
}

#[tokio::test]
async fn execute_nudges_when_large_completed_list_has_no_verification_step() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Read the code",
                        "activeForm": "Reading the code",
                        "status": "completed"
                    },
                    {
                        "content": "Patch the parser",
                        "activeForm": "Patching the parser",
                        "status": "completed"
                    },
                    {
                        "content": "Update the UI",
                        "activeForm": "Updating the UI",
                        "status": "completed"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write succeeds");

    assert!(out.contains("Completed 3 todo(s)"));
    assert!(out.contains("verification step"));
    let session = ctx.session.lock().await;
    assert!(session.todos.is_empty());
}

#[tokio::test]
async fn execute_skips_verification_nudge_when_completed_list_has_test_step() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "content": "Read the code",
                        "activeForm": "Reading the code",
                        "status": "completed"
                    },
                    {
                        "content": "Patch the parser",
                        "activeForm": "Patching the parser",
                        "status": "completed"
                    },
                    {
                        "content": "Run tests",
                        "activeForm": "Running tests",
                        "status": "completed"
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write succeeds");

    assert_eq!(out, "Completed 3 todo(s); cleared the session todo list");
}

#[tokio::test]
async fn execute_reports_unblocked_items_when_dependencies_are_used() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {
                        "id": "design",
                        "content": "Design the schema",
                        "activeForm": "Designing the schema",
                        "status": "completed"
                    },
                    {
                        "id": "impl",
                        "content": "Implement the migration",
                        "activeForm": "Implementing the migration",
                        "status": "pending",
                        "blockedBy": ["design"]
                    },
                    {
                        "id": "test",
                        "content": "Run the migration tests",
                        "activeForm": "Running the migration tests",
                        "status": "pending",
                        "blockedBy": ["impl"]
                    }
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write succeeds");

    // `impl`'s blocker (`design`) is done so it is ready; `test` still waits on `impl`.
    assert_eq!(
        out,
        "Stored 3 todo(s) — 2 pending, 0 in progress · ready now: Implement the migration"
    );
    let session = ctx.session.lock().await;
    assert_eq!(session.todos[1].blocked_by, vec!["design".to_string()]);
}

#[tokio::test]
async fn execute_keeps_flat_list_summary_when_no_dependencies() {
    let ctx = ctx();
    let out = TodoWrite
        .execute(
            json!({
                "todos": [
                    {"content": "A", "activeForm": "Doing A", "status": "pending"},
                    {"content": "B", "activeForm": "Doing B", "status": "pending"}
                ]
            }),
            &ctx,
        )
        .await
        .expect("todo_write succeeds");

    // No `blocked_by` anywhere → original one-line summary, no "ready now".
    assert_eq!(out, "Stored 2 todo(s) — 2 pending, 0 in progress");
}

#[test]
fn unblocked_summary_reports_none_when_all_pending_items_are_blocked() {
    let items = vec![
        TodoItem {
            content: "Build".to_string(),
            status: TodoStatus::InProgress,
            active_form: "Building".to_string(),
            id: Some("build".to_string()),
            blocked_by: Vec::new(),
        },
        TodoItem {
            content: "Ship".to_string(),
            status: TodoStatus::Pending,
            active_form: "Shipping".to_string(),
            id: Some("ship".to_string()),
            blocked_by: vec!["build".to_string()],
        },
    ];
    // `build` is in_progress (not completed) so `ship` stays blocked.
    assert_eq!(
        unblocked_summary(&items).as_deref(),
        Some("none (all pending items are still blocked)")
    );
}

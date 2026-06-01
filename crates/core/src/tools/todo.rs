use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::AgentEvent;

use super::{BuiltinTool, TodoItem, TodoStatus, ToolContext};

pub struct TodoWrite;

const VERIFICATION_NUDGE_MIN_TODOS: usize = 3;
const VERIFICATION_NUDGE: &str = "NOTE: You just completed 3+ todo items, but none looked like a verification step. Before writing the final response, run or record a relevant verification step if one is available.";

#[derive(Deserialize)]
struct TodoWriteArgs {
    todos: Vec<TodoInput>,
}

#[derive(Deserialize)]
struct TodoInput {
    content: String,
    status: String,
    #[serde(alias = "activeForm")]
    active_form: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(
        default,
        alias = "blockedBy",
        alias = "blocked_by_ids",
        deserialize_with = "super::deserialize_optional_string_vec"
    )]
    blocked_by: Option<Vec<String>>,
}

#[async_trait]
impl BuiltinTool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }
    fn description(&self) -> &'static str {
        "Replace the session todo list with the supplied entries. This is the canonical way to plan and track multi-step work: write the list once when you begin, then update it after every meaningful step by re-calling this tool with the full updated list.\n\
\n\
Use this whenever a task has three or more discrete steps, when the user provides multiple items, or when you want the user to see your progress. Skip it for single-step or trivial tasks.\n\
\n\
Each entry has:\n\
- `content`: Imperative form of the task (e.g. \"Run the test suite\").\n\
- `activeForm`: Present-continuous form shown while in progress (e.g. \"Running the test suite\"). The legacy `active_form` spelling is also accepted.\n\
- `status`: One of `pending`, `in_progress`, `completed`. Keep exactly one task `in_progress` at a time. Mark `completed` immediately after finishing; do not batch.\n\
\n\
For implementation todo lists with three or more items, include a verification task (tests, build, lint, type-check, or an equivalent project-specific check) before final completion.\n\
\n\
To express ordering between steps, give an item a short `id` and list the ids it waits on in `blocked_by`. An item is ready to start only once every id in its `blocked_by` is `completed`. Leave both off for a plain flat list.\n\
\n\
Returns a short summary of how many items were stored and how many are still pending, and — when dependencies are used — which items are now unblocked."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Full replacement todo list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string", "description": "Imperative form of the task."},
                            "activeForm": {"type": "string", "description": "Present-continuous form shown while the task is in progress."},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Current status."},
                            "id": {"type": "string", "description": "Optional stable id so other items can depend on this one."},
                            "blockedBy": {"type": "array", "items": {"type": "string"}, "description": "Ids of items that must be completed before this one can start."}
                        },
                        "required": ["content", "activeForm", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["todos"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: TodoWriteArgs = super::parse_args("todo_write", args)?;
        let mut items: Vec<TodoItem> = Vec::with_capacity(a.todos.len());
        let mut in_progress = 0usize;
        for (i, t) in a.todos.into_iter().enumerate() {
            if t.content.trim().is_empty() {
                return Err(anyhow!("todo #{}: content cannot be empty", i + 1));
            }
            if t.active_form.trim().is_empty() {
                return Err(anyhow!(
                    "todo #{}: active_form/activeForm cannot be empty",
                    i + 1
                ));
            }
            let status = TodoStatus::parse(&t.status).ok_or_else(|| {
                anyhow!(
                    "todo #{}: invalid status `{}` (expected pending|in_progress|completed)",
                    i + 1,
                    t.status
                )
            })?;
            if matches!(status, TodoStatus::InProgress) {
                in_progress += 1;
            }
            items.push(TodoItem {
                content: t.content,
                status,
                active_form: t.active_form,
                id: t.id.filter(|s| !s.trim().is_empty()),
                blocked_by: t
                    .blocked_by
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| !s.trim().is_empty())
                    .collect(),
            });
        }
        if in_progress > 1 {
            return Err(anyhow!(
                "exactly one todo may be `in_progress` at a time (got {in_progress})"
            ));
        }
        let pending = items
            .iter()
            .filter(|t| matches!(t.status, TodoStatus::Pending))
            .count();
        let total = items.len();
        let all_completed = total > 0 && pending == 0 && in_progress == 0;
        let needs_verification_nudge = needs_verification_nudge(&items, all_completed);
        if all_completed {
            if let Some(events) = &ctx.events {
                let _ = events
                    .send(AgentEvent::TodosSnapshot {
                        todos: items.clone(),
                    })
                    .await;
            }
        }
        let mut session = ctx.session.lock().await;
        session.todos = if all_completed { Vec::new() } else { items };
        if all_completed {
            let mut message = format!("Completed {total} todo(s); cleared the session todo list");
            if needs_verification_nudge {
                message.push_str("\n\n");
                message.push_str(VERIFICATION_NUDGE);
            }
            return Ok(message);
        }
        let mut message =
            format!("Stored {total} todo(s) — {pending} pending, {in_progress} in progress");
        if let Some(ready) = unblocked_summary(&session.todos) {
            message.push_str(&format!(" · ready now: {ready}"));
        }
        Ok(message)
    }
}

/// Comma-joined labels of the pending items whose dependencies are all
/// satisfied (every id in `blocked_by` belongs to a completed item). Returns
/// `None` when no item declares a dependency, so a plain flat list keeps its
/// original one-line summary unchanged. Unknown blocker ids are treated as
/// unsatisfied, so a typo simply leaves the item blocked rather than erroring.
fn unblocked_summary(items: &[TodoItem]) -> Option<String> {
    if !items.iter().any(|t| !t.blocked_by.is_empty()) {
        return None;
    }
    let completed: std::collections::HashSet<&str> = items
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .filter_map(|t| t.id.as_deref())
        .collect();
    let ready: Vec<&str> = items
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .filter(|t| t.blocked_by.iter().all(|b| completed.contains(b.as_str())))
        .map(|t| t.content.as_str())
        .collect();
    if ready.is_empty() {
        Some("none (all pending items are still blocked)".to_string())
    } else {
        Some(ready.join(", "))
    }
}

fn needs_verification_nudge(items: &[TodoItem], all_completed: bool) -> bool {
    all_completed
        && items.len() >= VERIFICATION_NUDGE_MIN_TODOS
        && !items.iter().any(todo_mentions_verification)
}

fn todo_mentions_verification(todo: &TodoItem) -> bool {
    text_mentions_verification(&todo.content) || text_mentions_verification(&todo.active_form)
}

fn text_mentions_verification(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("verif")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| {
                matches!(
                    word,
                    "test"
                        | "tests"
                        | "tested"
                        | "testing"
                        | "build"
                        | "builds"
                        | "built"
                        | "lint"
                        | "lints"
                        | "linting"
                        | "typecheck"
                        | "typechecks"
                        | "typechecking"
                        | "check"
                        | "checks"
                        | "checking"
                        | "qa"
                )
            })
}

#[cfg(test)]
mod tests {
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
}

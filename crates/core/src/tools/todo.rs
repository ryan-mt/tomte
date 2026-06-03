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
mod tests;

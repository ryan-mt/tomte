use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, TodoItem, TodoStatus, ToolContext};

pub struct TodoWrite;

#[derive(Deserialize)]
struct TodoWriteArgs {
    todos: Vec<TodoInput>,
}

#[derive(Deserialize)]
struct TodoInput {
    content: String,
    status: String,
    active_form: String,
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
- `active_form`: Present-continuous form shown while in progress (e.g. \"Running the test suite\").\n\
- `status`: One of `pending`, `in_progress`, `completed`. Keep exactly one task `in_progress` at a time. Mark `completed` immediately after finishing; do not batch.\n\
\n\
Returns a short summary of how many items were stored and how many are still pending."
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
                            "active_form": {"type": "string", "description": "Present-continuous form shown while the task is in progress."},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"], "description": "Current status."}
                        },
                        "required": ["content", "active_form", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["todos"],
            "additionalProperties": false
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: TodoWriteArgs = super::parse_args("todo_write", args)?;
        let mut items: Vec<TodoItem> = Vec::with_capacity(a.todos.len());
        let mut in_progress = 0usize;
        for (i, t) in a.todos.into_iter().enumerate() {
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
        let mut session = ctx.session.lock().await;
        session.todos = items;
        Ok(format!(
            "Stored {total} todo(s) — {pending} pending, {in_progress} in progress"
        ))
    }
}

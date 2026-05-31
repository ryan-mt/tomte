use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

pub struct GoalUpdate;

#[derive(Deserialize)]
struct GoalUpdateArgs {
    #[serde(alias = "state", alias = "goal_status", alias = "goalStatus")]
    status: String,
    #[serde(alias = "message", alias = "details", alias = "note")]
    summary: String,
}

#[async_trait]
impl BuiltinTool for GoalUpdate {
    fn name(&self) -> &'static str {
        "goal_update"
    }

    fn description(&self) -> &'static str {
        "Report progress or a terminal state for an active `/goal` command.\n\
\n\
Use only when the user started a `/goal`. Call with:\n\
- `in_progress` after meaningful progress when more work remains.\n\
- `complete` only after the objective is fully done and relevant checks have passed.\n\
- `blocked` only when you cannot make meaningful progress without user input or an external-state change.\n\
\n\
The host uses `complete` and `blocked` to stop automatic `/goal` continuation, so never mark complete optimistically."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["in_progress", "complete", "blocked"],
                    "description": "Current goal state."
                },
                "summary": {
                    "type": "string",
                    "description": "Concise evidence, progress, or the exact blocker."
                }
            },
            "required": ["status", "summary"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: GoalUpdateArgs = super::parse_args("goal_update", args)?;
        let Some(status) = normalize_goal_status(&a.status) else {
            return Err(anyhow!(
                "invalid goal status `{}` (expected in_progress|complete|blocked)",
                a.status
            ));
        };

        let summary = a.summary.trim();
        if summary.is_empty() {
            return Err(anyhow!("summary is required"));
        }

        if let Some(events) = &ctx.events {
            let _ = events
                .send(crate::agent::AgentEvent::GoalStatusUpdated {
                    status: status.clone(),
                    summary: summary.to_string(),
                })
                .await;
        }

        Ok(format!("goal {status}: {summary}"))
    }
}

fn normalize_goal_status(status: &str) -> Option<String> {
    let normalized = status.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let status = match normalized.as_str() {
        "in_progress" | "inprogress" | "progress" | "continue" | "continuing" | "working" => {
            "in_progress"
        }
        "complete" | "completed" | "done" | "success" | "succeeded" => "complete",
        "blocked" | "stuck" | "needs_input" | "needs_user_input" | "waiting_for_user" => "blocked",
        _ => return None,
    };
    Some(status.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ApprovalMode;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn execute_emits_goal_status_update() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        ctx.events = Some(tx);

        let out = GoalUpdate
            .execute(
                json!({"status": "complete", "summary": "tests passed"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out, "goal complete: tests passed");
        match rx.recv().await.unwrap() {
            crate::agent::AgentEvent::GoalStatusUpdated { status, summary } => {
                assert_eq!(status, "complete");
                assert_eq!(summary, "tests passed");
            }
            other => panic!("expected GoalStatusUpdated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_rejects_empty_summary() {
        let ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);

        let err = GoalUpdate
            .execute(json!({"status": "complete", "summary": "  "}), &ctx)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("summary is required"));
    }

    #[tokio::test]
    async fn execute_accepts_provider_aliases() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        ctx.events = Some(tx);

        let out = GoalUpdate
            .execute(
                json!({"state": "completed", "message": "checks passed"}),
                &ctx,
            )
            .await
            .unwrap();

        assert_eq!(out, "goal complete: checks passed");
        match rx.recv().await.unwrap() {
            crate::agent::AgentEvent::GoalStatusUpdated { status, summary } => {
                assert_eq!(status, "complete");
                assert_eq!(summary, "checks passed");
            }
            other => panic!("expected GoalStatusUpdated, got {other:?}"),
        }
    }
}

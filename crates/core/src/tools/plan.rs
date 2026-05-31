use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApprovalMode, BuiltinTool, ToolContext};

pub struct EnterPlanMode;

#[async_trait]
impl BuiltinTool for EnterPlanMode {
    fn name(&self) -> &'static str {
        "enter_plan_mode"
    }

    fn description(&self) -> &'static str {
        "Use before non-trivial implementation work when you need to inspect the codebase and design an approach before editing. \
This switches the host into plan mode: external mutating tools are blocked, while read/search tools, todo_write, ask_user_question, and exit_plan_mode remain available."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _args: Value, ctx: &ToolContext) -> Result<String> {
        if let Some(events) = &ctx.events {
            let _ = events
                .send(crate::agent::AgentEvent::PlanModeRequested)
                .await;
        }
        Ok("Entered plan mode. Explore the codebase with read-only tools, update todos as useful, and call exit_plan_mode with the complete plan when it is ready for approval.".to_string())
    }
}

pub struct ExitPlanMode;

#[derive(Deserialize)]
struct ExitPlanModeArgs {
    #[serde(alias = "summary", alias = "proposal")]
    plan: String,
}

#[async_trait]
impl BuiltinTool for ExitPlanMode {
    fn name(&self) -> &'static str {
        "exit_plan_mode"
    }

    fn description(&self) -> &'static str {
        "Use in plan mode when the implementation plan is complete and ready for user approval. \
This pauses the turn and asks the user whether to leave plan mode and start implementing. \
Do not use for research-only tasks or when unresolved requirements remain."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plan": {
                    "type": "string",
                    "description": "Complete, actionable implementation plan for the user to approve."
                }
            },
            "required": ["plan"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        if ctx.approval != ApprovalMode::Plan {
            return Err(anyhow!(
                "exit_plan_mode can only be used while plan mode is active"
            ));
        }
        let a: ExitPlanModeArgs = super::parse_args("exit_plan_mode", args)?;
        let plan = a.plan.trim();
        if plan.is_empty() {
            return Err(anyhow!("plan is required"));
        }

        if let Some(events) = &ctx.events {
            let _ = events
                .send(crate::agent::AgentEvent::PlanExitRequested {
                    plan: plan.to_string(),
                })
                .await;
        }

        Ok("plan presented for approval".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn enter_plan_mode_emits_plan_mode_request() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        ctx.events = Some(tx);

        let out = EnterPlanMode.execute(json!({}), &ctx).await.unwrap();

        assert!(out.contains("Entered plan mode"));
        match rx.recv().await.unwrap() {
            crate::agent::AgentEvent::PlanModeRequested => {}
            other => panic!("expected PlanModeRequested, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_emits_plan_exit_request() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::Plan);
        ctx.events = Some(tx);

        let out = ExitPlanMode
            .execute(json!({"plan": "1. Read code\n2. Patch\n3. Test"}), &ctx)
            .await
            .unwrap();

        assert_eq!(out, "plan presented for approval");
        match rx.recv().await.unwrap() {
            crate::agent::AgentEvent::PlanExitRequested { plan } => {
                assert!(plan.contains("Patch"));
            }
            other => panic!("expected PlanExitRequested, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_rejects_outside_plan_mode() {
        let ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);

        let err = ExitPlanMode
            .execute(json!({"plan": "ship it"}), &ctx)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("plan mode is active"));
    }
}

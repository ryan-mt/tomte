use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};

/// A no-op scratchpad. The model calls it to reason in the open — the `thought`
/// is appended to the conversation and the tool returns a short ack. It changes
/// nothing on disk and fetches nothing, so it is safe to run unattended and
/// inside sub-agents.
pub struct Think;

#[derive(Deserialize)]
struct ThinkArgs {
    #[serde(default, alias = "text", alias = "note", alias = "reasoning")]
    thought: Option<String>,
}

#[async_trait]
impl BuiltinTool for Think {
    fn name(&self) -> &'static str {
        "think"
    }

    fn description(&self) -> &'static str {
        "Record a short reasoning note without taking any action. It writes nothing to disk, fetches nothing, and returns no new information — it just gives you a place to think out loud mid-task so the reasoning is in the record and survives into later turns.\n\
\n\
When to use:\n\
- After a batch of tool results, to plan the next move before acting on them.\n\
- To weigh a trade-off (an API shape, an error-handling policy) where the rejected option is worth stating.\n\
- To check a plan against a constraint (a rule from the user, a `record_decision` you must honor) before an edit.\n\
\n\
When NOT to use:\n\
- As a substitute for acting — if you already know the next step, take it. A think call that only narrates what you're about to do is wasted.\n\
- For anything the user must see: put that in your text reply. A thought is for your own working-out, not a status update.\n\
- To record a durable design decision — use `record_decision` so it's keyed to a `file:line` and re-injected for future sessions.\n\
\n\
Parameters:\n\
- `thought`: the note to record (a sentence or two)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thought": {
                    "type": "string",
                    "description": "The reasoning note to record. A sentence or two is ideal."
                }
            },
            "required": ["thought"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: ThinkArgs = super::parse_args("think", args)?;
        let thought = a.thought.ok_or_else(|| anyhow!("thought is required"))?;
        if thought.trim().is_empty() {
            return Err(anyhow!("thought must not be empty"));
        }
        // The thought text already lives in the call arguments (and thus the
        // history); the result only confirms it landed and nudges forward.
        Ok("Thought recorded. Continue.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ApprovalMode;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest)
    }

    #[tokio::test]
    async fn records_a_thought_and_is_read_only() {
        let out = Think
            .execute(json!({"thought": "plan: read config, then edit"}), &ctx())
            .await
            .unwrap();
        assert_eq!(out, "Thought recorded. Continue.");
        assert!(Think.is_read_only());
    }

    #[tokio::test]
    async fn accepts_aliases_for_the_field() {
        let out = Think
            .execute(json!({"note": "weighing option A vs B"}), &ctx())
            .await
            .unwrap();
        assert_eq!(out, "Thought recorded. Continue.");
    }

    #[tokio::test]
    async fn rejects_missing_and_empty_thoughts() {
        let err = Think.execute(json!({}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("required"));
        let err = Think
            .execute(json!({"thought": "   "}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty"));
    }
}

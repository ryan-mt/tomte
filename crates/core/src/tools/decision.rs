//! The `record_decision` tool: lets the agent log *why* it made a non-obvious
//! change — the decision, the reasoning, and the rejected alternatives — into
//! the project's decision trail ([`crate::decisions`]). The harness stamps the
//! model in play, so the reasoning survives a mid-task model switch (Pillar 2 of
//! docs/SOUL.md).
//!
//! Like `memory`, writes are refused in unattended headless runs: the trail is
//! replayed into later sessions, so an unattended prompt-injected write would be
//! a durable injection vector.

use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BuiltinTool, ToolContext};
use crate::decisions::{self, DecisionRecord};

pub struct RecordDecision;

#[derive(Deserialize)]
struct Args {
    loc: String,
    decision: String,
    why: String,
    #[serde(default, alias = "rejected_alternatives", alias = "alternatives")]
    rejected: Vec<String>,
}

#[async_trait]
impl BuiltinTool for RecordDecision {
    fn name(&self) -> &'static str {
        "record_decision"
    }

    fn is_read_only(&self) -> bool {
        // It writes to disk; auto-approval comes from the ALWAYS_AUTO_TOOLS
        // allowlist (like `memory`), not from a read-only claim.
        false
    }

    fn description(&self) -> &'static str {
        "Record WHY you made a non-obvious change, so the reasoning survives across sessions and model switches. The trail is re-injected into a later session's context even under a different model, so a future model (or a mid-task model switch) inherits your reasoning rather than a lossy summary. Persist the decision, the reasoning, and the alternatives you rejected, keyed to a code location.\n\
\n\
When to use:\n\
- After a real decision: a trade-off, a constraint you honored, or a design choice with genuine alternatives (an API shape, an error-handling policy, a data-structure or concurrency choice).\n\
- NOT for trivial or self-evident edits, and NOT a replacement for code comments.\n\
\n\
The model in play and a timestamp are stamped automatically — do not pass them. Read the trail back with `tomte why <loc>` or `tomte why --all`. Writes are disabled in unattended headless runs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "loc": {
                    "type": "string",
                    "description": "Code location the decision is about, e.g. \"src/parser.rs:88\" or \"src/api/auth.rs\"."
                },
                "decision": {
                    "type": "string",
                    "description": "The choice you made, in one line."
                },
                "why": {
                    "type": "string",
                    "description": "The reasoning: the constraint, trade-off, or principle behind the choice."
                },
                "rejected": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Alternatives you considered and dropped, each with its consequence, e.g. \"panic!() -> crashes callers\". Pass an empty array if none."
                }
            },
            "required": ["loc", "decision", "why", "rejected"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        // The trail is replayed into later sessions, so block writes in an
        // unattended headless run — the same gate the `memory` tool uses.
        if ctx.non_interactive && ctx.require_approval {
            bail!(
                "record_decision is disabled in unattended headless runs because the decision trail is replayed into later sessions. Pass --dangerously-skip-permissions to allow it, or run tomte interactively."
            );
        }
        let a: Args = super::parse_args("record_decision", args)?;
        let loc = a.loc.trim().to_string();
        if loc.is_empty() {
            bail!("record_decision requires a non-empty `loc` (e.g. \"src/parser.rs:88\").");
        }
        if a.decision.trim().is_empty() || a.why.trim().is_empty() {
            bail!("record_decision requires both a `decision` and a `why`.");
        }
        let record = DecisionRecord {
            loc: loc.clone(),
            decision: a.decision.trim().to_string(),
            why: a.why.trim().to_string(),
            rejected: a
                .rejected
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            model: ctx.config.model.clone(),
            ts: now_ms(),
        };
        decisions::append(&ctx.cwd, &record)?;
        Ok(format!(
            "Recorded decision at {loc} (model: {}). Read it back with `tomte why {loc}`.",
            record.model
        ))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::tools::ApprovalMode;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ctx_in(
        cwd: std::path::PathBuf,
        non_interactive: bool,
        require_approval: bool,
    ) -> ToolContext {
        let mut config = crate::config::Config::default();
        config.model = "gpt-5.5".to_string();
        ToolContext {
            cwd,
            approval: ApprovalMode::Auto,
            require_approval,
            auto_approve_edits: false,
            non_interactive,
            session: Arc::new(Mutex::new(crate::tools::SessionState::default())),
            config,
            cwd_override: Arc::new(Mutex::new(None)),
            events: None,
        }
    }

    #[tokio::test]
    async fn records_and_stamps_the_live_model() {
        let dir = std::env::temp_dir().join(format!("tomte_rd_ok_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `cwd` is a non-repo temp dir, so the trail is keyed to it under the
        // config dir. Clear any prior artifact, exercise the real write path,
        // assert it stamped the live model, then clean the artifact up.
        let store = crate::decisions::store_path(&dir);
        let _ = std::fs::remove_file(&store);
        let ctx = ctx_in(dir.clone(), false, false);
        let out = RecordDecision
            .execute(
                json!({
                    "loc": "src/parser.rs:88",
                    "decision": "return Err on empty input",
                    "why": "validate at the boundary",
                    "rejected": ["panic!() -> crashes callers"]
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.contains("gpt-5.5"), "stamps the live model: {out}");
        let trail = crate::decisions::for_loc(&dir, "src/parser.rs:88");
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].model, "gpt-5.5");
        assert_eq!(trail[0].rejected.len(), 1);
        let _ = std::fs::remove_file(&store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn blocked_in_unattended_headless() {
        let dir = std::env::temp_dir().join(format!("tomte_rd_block_{}", std::process::id()));
        let ctx = ctx_in(dir, true, true);
        let err = RecordDecision
            .execute(
                json!({"loc": "a:1", "decision": "d", "why": "w", "rejected": []}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unattended headless"));
    }

    #[tokio::test]
    async fn rejects_empty_loc() {
        let dir = std::env::temp_dir().join(format!("tomte_rd_empty_{}", std::process::id()));
        let ctx = ctx_in(dir, false, false);
        let err = RecordDecision
            .execute(
                json!({"loc": "  ", "decision": "d", "why": "w", "rejected": []}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("loc"));
    }
}

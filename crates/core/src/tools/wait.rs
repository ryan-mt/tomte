use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use super::{deserialize_optional_u64, BuiltinTool, ToolContext};

pub struct Wait;

/// Hard ceiling for a single wait. Kept comfortably under the agent loop's
/// 180s `TOOL_HARD_TIMEOUT` so a long pause is clamped rather than aborted
/// mid-wait (which would waste the turn). For longer pauses, poll in several
/// steps or run a background shell.
const MAX_WAIT_SECS: u64 = 120;

#[derive(Deserialize)]
struct WaitArgs {
    #[serde(
        default,
        alias = "duration",
        alias = "secs",
        alias = "duration_seconds",
        alias = "durationSeconds",
        deserialize_with = "deserialize_optional_u64"
    )]
    seconds: Option<u64>,
}

#[async_trait]
impl BuiltinTool for Wait {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn description(&self) -> &'static str {
        "Pause for a fixed number of seconds, then continue the turn. Prefer this over `run_shell {command: \"sleep N\"}` — it doesn't occupy a foreground shell slot (which blocks the parallel read-only batch) or a background shell handle.\n\
\n\
When to use:\n\
- Poll-and-wait loops: check a deploy/build/job, wait, then check again.\n\
- Letting an external state settle (a file appearing, a server coming up) before re-reading it.\n\
\n\
When NOT to use:\n\
- Waiting on a process YOU started — use `run_shell {run_in_background: true}` + `bash_output` to stream its output instead of blind-sleeping.\n\
- Waits longer than the cap below — make several `wait` calls, or background the work.\n\
\n\
Note: each wake costs one model call, so don't poll in a tight loop. The duration is capped at 120 seconds; a larger value is clamped (the result reports the actual wait).\n\
\n\
Parameters:\n\
- `seconds`: How long to pause, 1–120 seconds."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_WAIT_SECS,
                    "description": "Seconds to pause before continuing (1–120; larger values are clamped to 120)."
                }
            },
            "required": ["seconds"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: WaitArgs = super::parse_args("wait", args)?;
        let requested = a.seconds.ok_or_else(|| anyhow!("seconds is required"))?;
        if requested == 0 {
            return Err(anyhow!("seconds must be at least 1"));
        }
        let secs = requested.min(MAX_WAIT_SECS);
        tokio::time::sleep(Duration::from_secs(secs)).await;
        Ok(wait_message(requested, secs))
    }
}

/// Result string for a completed wait, noting any clamp. Pure so it can be
/// unit-tested without actually sleeping for the clamped duration.
fn wait_message(requested: u64, waited: u64) -> String {
    if waited < requested {
        format!("waited {waited}s (requested {requested}s, clamped to the {MAX_WAIT_SECS}s max)")
    } else {
        format!("waited {waited}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ApprovalMode;

    #[tokio::test]
    async fn waits_for_the_requested_duration() {
        let ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        let start = std::time::Instant::now();
        let out = Wait.execute(json!({"seconds": 1}), &ctx).await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(1));
        assert_eq!(out, "waited 1s");
    }

    #[test]
    fn clamp_message_notes_when_request_exceeds_the_cap() {
        // Pure check — no 120s sleep. A request over the cap reports the clamp.
        assert_eq!(
            wait_message(600, MAX_WAIT_SECS),
            "waited 120s (requested 600s, clamped to the 120s max)"
        );
        assert_eq!(wait_message(30, 30), "waited 30s");
    }

    #[tokio::test]
    async fn rejects_zero_and_is_read_only() {
        let ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        let err = Wait.execute(json!({"seconds": 0}), &ctx).await.unwrap_err();
        assert!(err.to_string().contains("at least 1"));
        assert!(Wait.is_read_only());
    }

    #[tokio::test]
    async fn accepts_string_and_alias_for_duration() {
        let ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        let out = Wait.execute(json!({"duration": "1"}), &ctx).await.unwrap();
        assert_eq!(out, "waited 1s");
    }
}

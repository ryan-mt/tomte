//! The `bash_output` and `kill_shell` tools for background shells. Split out
//! of `shell`; logic unchanged.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tools::{BuiltinTool, ToolContext};

use super::support::drain_utf8;

pub struct BashOutput;
pub struct KillShell;

#[derive(Deserialize)]
struct BashOutputArgs {
    #[serde(alias = "bashId", alias = "id", alias = "shell_id", alias = "shellId")]
    bash_id: String,
}

#[async_trait]
impl BuiltinTool for BashOutput {
    fn name(&self) -> &'static str {
        "bash_output"
    }
    fn description(&self) -> &'static str {
        "Read new stdout/stderr from a background shell started with `run_shell {run_in_background: true}`. Returns only the bytes written since the last `bash_output` call, plus the current status.\n\
\n\
When to use:\n\
- Poll a long-running background command (dev server, watcher, build) to see progress or detect that it crashed.\n\
- Drain output before calling `kill_shell` so you don't lose the tail of the log.\n\
\n\
When NOT to use:\n\
- A command you ran in foreground — its full output already came back in the `run_shell` response.\n\
- A `bash_id` you've already seen `exited(...)` or `killed` from with no remaining buffered bytes.\n\
\n\
Response format: a JSON object `{bash_id, status, stdout, stderr}` where `stdout`/`stderr` are the NEW bytes since the last read. `status` is one of `running`, `exited(<code>)`, `killed`, or `error(<msg>)`.\n\
\n\
Parameters:\n\
- `bash_id`: The id returned by the original `run_shell` call."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "bash_id": {"type": "string", "description": "The id returned by run_shell when started in background mode."}
            },
            "required": ["bash_id"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: BashOutputArgs = crate::tools::parse_args("bash_output", args)?;
        let state = {
            let session = ctx.session.lock().await;
            session
                .background_shells
                .get(&a.bash_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown bash_id: {}", a.bash_id))?
        };
        // Drain new stdout/stderr bytes since the last cursor. Lock order
        // (buf, cursor) matches append_capped so the writer can't append new
        // bytes between us locking buf and updating the cursor — previously
        // that produced torn reads where the cursor advanced past appended
        // bytes that were never returned.
        let new_stdout = {
            let buf = state.stdout.lock().await;
            let mut cursor = state.stdout_cursor.lock().await;
            drain_utf8(&buf, &mut cursor)
        };
        let new_stderr = {
            let buf = state.stderr.lock().await;
            let mut cursor = state.stderr_cursor.lock().await;
            drain_utf8(&buf, &mut cursor)
        };
        let status = state.status.lock().await.label();
        Ok(serde_json::to_string(&json!({
            "bash_id": a.bash_id,
            "status": status,
            "stdout": new_stdout,
            "stderr": new_stderr,
        }))?)
    }
}

#[derive(Deserialize)]
struct KillShellArgs {
    #[serde(alias = "bashId", alias = "id", alias = "shell_id", alias = "shellId")]
    bash_id: String,
}

#[async_trait]
impl BuiltinTool for KillShell {
    fn name(&self) -> &'static str {
        "kill_shell"
    }
    fn description(&self) -> &'static str {
        "Terminate a background shell started with `run_shell {run_in_background: true}`. Sends SIGKILL and waits for the child to exit. Idempotent — calling it on an already-terminated bash_id is a no-op.\n\
\n\
Always drain remaining output with `bash_output` before killing if you care about the tail of the log.\n\
\n\
Parameters:\n\
- `bash_id`: The id returned by the original `run_shell` call."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "bash_id": {"type": "string", "description": "The id returned by run_shell when started in background mode."}
            },
            "required": ["bash_id"],
            "additionalProperties": false
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: KillShellArgs = crate::tools::parse_args("kill_shell", args)?;
        let state = {
            let session = ctx.session.lock().await;
            session
                .background_shells
                .get(&a.bash_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown bash_id: {}", a.bash_id))?
        };
        let tx_opt = state.kill_tx.lock().await.take();
        match tx_opt {
            Some(tx) => {
                let _ = tx.send(());
                Ok(format!(
                    "{{\"bash_id\": \"{}\", \"status\": \"kill_requested\"}}",
                    a.bash_id
                ))
            }
            None => {
                let status = state.status.lock().await.label();
                Ok(format!(
                    "{{\"bash_id\": \"{}\", \"status\": \"{}\", \"note\": \"already terminated\"}}",
                    a.bash_id, status
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::RunShell;
    use super::*;

    #[tokio::test]
    async fn background_bash_output_returns_only_new_bytes() {
        let ctx = ctx();
        // Accumulate every poll's stdout so we can assert (a) each line
        // appears exactly once and (b) both lines arrived — without depending
        // on a specific poll/print interleaving (inherently racy on CI).
        let out = RunShell
            .execute(
                json!({
                    "command": delayed_two_line_command(),
                    "timeout_ms": null,
                    "run_in_background": true,
                    "dangerous_override": null,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let id = parse_bash_id(&out);
        let mut accumulated = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        let last_status = loop {
            let raw = BashOutput
                .execute(json!({"bash_id": id}), &ctx)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let chunk = v.get("stdout").unwrap().as_str().unwrap().to_string();
            accumulated.push_str(&chunk);
            let status = v.get("status").unwrap().as_str().unwrap().to_string();
            if status.contains("exited") {
                break status;
            }
            if std::time::Instant::now() > deadline {
                panic!("background command never exited; last={raw}, acc={accumulated:?}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        };
        assert_eq!(
            accumulated.matches("first").count(),
            1,
            "first must appear exactly once; status={last_status}, got: {accumulated:?}"
        );
        assert_eq!(
            accumulated.matches("second").count(),
            1,
            "second must appear exactly once; status={last_status}, got: {accumulated:?}"
        );
    }

    #[tokio::test]
    async fn bash_output_accepts_bash_id_aliases() {
        let ctx = ctx();
        let out = RunShell
            .execute(
                json!({
                    "command": print_command("alias-bg"),
                    "timeout_ms": null,
                    "run_in_background": true,
                    "dangerous_override": null,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let id = parse_bash_id(&out);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(5000);
        loop {
            let raw = BashOutput
                .execute(json!({"bashId": id.clone()}), &ctx)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let stdout = v.get("stdout").unwrap().as_str().unwrap();
            if stdout.contains("alias-bg") {
                assert_eq!(v.get("bash_id").unwrap().as_str().unwrap(), id);
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("alias bash_output never returned expected stdout; last={raw}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
    }

    #[tokio::test]
    async fn kill_shell_stops_a_running_background_process() {
        let ctx = ctx();
        let out = RunShell
            .execute(
                json!({
                    "command": long_sleep_command(),
                    "timeout_ms": null,
                    "run_in_background": true,
                    "dangerous_override": null,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let id = parse_bash_id(&out);
        // Make sure the child is actually alive before we kill.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let killed = KillShell
            .execute(json!({"bash_id": id.clone()}), &ctx)
            .await
            .unwrap();
        assert!(killed.contains("kill_requested"), "got: {killed}");
        let final_out = wait_until_status(&ctx, &id, "killed", 3000).await;
        let v: serde_json::Value = serde_json::from_str(&final_out).unwrap();
        assert_eq!(v.get("status").unwrap().as_str().unwrap(), "killed");

        // Second kill must be idempotent.
        let again = KillShell
            .execute(json!({"bash_id": id}), &ctx)
            .await
            .unwrap();
        assert!(again.contains("already terminated"), "got: {again}");
    }

    #[tokio::test]
    async fn kill_shell_accepts_bash_id_aliases() {
        let ctx = ctx();
        let out = RunShell
            .execute(
                json!({
                    "command": long_sleep_command(),
                    "timeout_ms": null,
                    "run_in_background": true,
                    "dangerous_override": null,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let id = parse_bash_id(&out);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let killed = KillShell
            .execute(json!({"id": id.clone()}), &ctx)
            .await
            .unwrap();
        assert!(killed.contains("kill_requested"), "got: {killed}");
        let final_out = wait_until_status(&ctx, &id, "killed", 3000).await;
        let v: serde_json::Value = serde_json::from_str(&final_out).unwrap();
        assert_eq!(v.get("status").unwrap().as_str().unwrap(), "killed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_shell_kills_background_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("survived-kill");
        let ctx = ctx_at(tmp.path().to_path_buf());
        let command = format!(
            "(sleep 0.5; printf survived > {}) & wait",
            sh_quote(&marker)
        );

        let out = RunShell
            .execute(
                json!({
                    "command": command,
                    "timeout_ms": null,
                    "run_in_background": true,
                    "dangerous_override": null,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let id = parse_bash_id(&out);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let killed = KillShell
            .execute(json!({"bash_id": id.clone()}), &ctx)
            .await
            .unwrap();
        assert!(killed.contains("kill_requested"), "got: {killed}");
        let _ = wait_until_status(&ctx, &id, "killed", 3000).await;

        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        assert!(
            !marker.exists(),
            "kill_shell killed only the shell; a background descendant survived"
        );
    }

    #[tokio::test]
    async fn bash_output_rejects_unknown_id() {
        let ctx = ctx();
        let err = BashOutput
            .execute(json!({"bash_id": "bash_does_not_exist"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown bash_id"));
    }
}

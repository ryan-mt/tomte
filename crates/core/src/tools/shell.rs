use std::process::Stdio;

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use super::{BuiltinTool, ToolContext};

pub struct RunShell;

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Env vars whose names contain any of these substrings are scrubbed from the
/// child's environment. Prevents the LLM from exfiltrating tokens via
/// `env | curl …`. Substring match (case-insensitive) catches the long tail
/// of `*_TOKEN`, `*_KEY`, `*_SECRET`, etc.
const ENV_DENYLIST_SUBSTRINGS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "PRIVATE_KEY",
    "CREDENTIALS",
    "OPENAI",
    "ANTHROPIC",
    "AWS_",
    "GOOGLE_",
    "GITHUB_",
    "GH_",
    "SUPABASE",
];

#[async_trait]
impl BuiltinTool for RunShell {
    fn name(&self) -> &'static str {
        "run_shell"
    }
    fn description(&self) -> &'static str {
        "Run a shell command via `sh -c` in the working directory. Returns combined exit code, stdout, and stderr in a single string.\n\
\n\
Use this for builds, tests, formatters, version checks, package managers, git operations, and any other one-shot CLI invocation. Prefer the dedicated tools (`read_file`, `grep`, `glob`, `list_dir`) over their shell equivalents (`cat`, `grep`, `find`, `ls`) — they are faster and return structured output.\n\
\n\
Safety:\n\
- Default timeout is 120 seconds. Pass `timeout_ms` for long-running commands. On timeout the child process is sent SIGKILL.\n\
- Environment variables that look like secrets (names containing TOKEN, SECRET, KEY, OPENAI, AWS_, GITHUB_, etc.) are stripped from the child process.\n\
- Never run destructive commands (rm -rf, force-push, dropping tables, etc.) unless the user explicitly asked.\n\
\n\
Parameters:\n\
- `command`: Shell command to execute. Quote arguments that contain spaces.\n\
- `timeout_ms`: Hard timeout in milliseconds, or `null` for the default of 120000."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute (interpreted by `sh -c`)."},
                "timeout_ms": {"type": ["integer", "null"], "description": "Hard timeout in milliseconds; null uses the default of 120000."}
            },
            "required": ["command", "timeout_ms"],
            "additionalProperties": false
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: ShellArgs = serde_json::from_value(args)?;
        let timeout = std::time::Duration::from_millis(a.timeout_ms.unwrap_or(120_000));
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&a.command)
            .current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Strip likely-secret env vars before spawn so the LLM can't echo them.
        for (k, _) in std::env::vars() {
            let upper = k.to_ascii_uppercase();
            if ENV_DENYLIST_SUBSTRINGS.iter().any(|p| upper.contains(p)) {
                cmd.env_remove(&k);
            }
        }
        let child = cmd.spawn()?;
        let wait = child.wait_with_output();
        let out = match tokio::time::timeout(timeout, wait).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                // Future was dropped; kill_on_drop fires SIGKILL on the
                // child so it does not orphan with the parent env still
                // mapped in its fds.
                return Err(anyhow::anyhow!("timed out after {}ms", timeout.as_millis()));
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let code = out.status.code().unwrap_or(-1);
        Ok(format!(
            "exit_code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ))
    }
}

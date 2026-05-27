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
When to use:\n\
- Builds and tests: `cargo build`, `cargo test`, `npm test`, `pytest`, `go test ./...`.\n\
- Formatters and linters: `cargo fmt`, `prettier`, `eslint`, `black`.\n\
- Git operations the user asked for: `git status`, `git diff`, `git log`, `git add`, `git commit`.\n\
- Package managers: `pnpm add`, `cargo add`, `pip install`.\n\
- One-shot scripts and binaries the user provides.\n\
\n\
When NOT to use (use the dedicated tool instead — faster, structured):\n\
- `cat file` → `read_file`\n\
- `grep -rn pattern .` → `grep`\n\
- `find . -name '*.rs'` → `glob`\n\
- `ls path` → `list_dir`\n\
- Editing a file via `sed -i`/`awk` → `edit_file` or `multi_edit`\n\
\n\
Safety:\n\
- Default timeout is 120 seconds. Pass `timeout_ms` for long-running commands. On timeout the child process is sent SIGKILL.\n\
- Environment variables that look like secrets (names containing TOKEN, SECRET, KEY, OPENAI, AWS_, GITHUB_, etc.) are stripped from the child process.\n\
- Never run destructive commands (`rm -rf`, `git reset --hard`, force-push, dropping tables, etc.) unless the user explicitly asked.\n\
- Network commands (curl, wget) are allowed but prefer `web_fetch` for HTTP — it has stricter limits and won't pull in unexpected redirects.\n\
\n\
Common mistakes:\n\
- Forgetting to quote paths with spaces — wrap them in single quotes.\n\
- Pipelines without `set -o pipefail` swallow upstream failures; check intermediate exit codes when it matters.\n\
- Long-running interactive commands (REPLs, watchers) will block until timeout — only run non-interactive commands.\n\
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

//! The `run_shell` tool: foreground/background command execution. Split out
//! of `shell`; logic unchanged.

use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::oneshot;

use crate::tools::{BackgroundShellState, BgStatus, BuiltinTool, ToolContext};

use super::danger::classify_danger;
use super::support::{
    append_capped, bash_id, configure_platform_shell, format_capped_stream, isolate_process_group,
    kill_process_group, now_ms, platform_shell_name, read_capped_output,
    FOREGROUND_OUTPUT_MAX_BYTES_PER_STREAM,
};

pub struct RunShell;

#[derive(Deserialize)]
struct ShellArgs {
    #[serde(alias = "cmd")]
    command: String,
    #[serde(
        default,
        alias = "timeout",
        alias = "timeoutMs",
        deserialize_with = "crate::tools::deserialize_optional_u64"
    )]
    timeout_ms: Option<u64>,
    #[serde(
        default,
        alias = "runInBackground",
        deserialize_with = "crate::tools::deserialize_optional_bool"
    )]
    run_in_background: Option<bool>,
    #[serde(
        default,
        alias = "dangerousOverride",
        deserialize_with = "crate::tools::deserialize_optional_bool"
    )]
    dangerous_override: Option<bool>,
}

#[async_trait]
impl BuiltinTool for RunShell {
    fn name(&self) -> &'static str {
        "run_shell"
    }
    fn description(&self) -> &'static str {
        "Run a command via the platform shell (`sh -c` on Unix, `cmd /C` on Windows) in the working directory. Returns combined exit code, stdout, and stderr in a single string.\n\
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
Background mode (`run_in_background: true`):\n\
- Use it for processes that legitimately keep running while you do other work — dev servers (`npm run dev`, `cargo run -- web`), watchers (`cargo watch`), tailing a log, or any command that would otherwise hit the foreground timeout.\n\
- The tool returns immediately with `{bash_id, status: 'running'}`. The child keeps writing stdout/stderr into a per-session buffer.\n\
- Read new output with `bash_output {bash_id}`. Each call returns only bytes written since the previous read, plus the current `status` (`running`, `exited(<code>)`, `killed`, `error(...)`).\n\
- Stop the child with `kill_shell {bash_id}` when you're done — otherwise it keeps running until the session ends.\n\
- DO NOT use background mode for short builds/tests; the foreground call is simpler and gives you the full output in one response.\n\
\n\
Safety:\n\
- Foreground timeout is 120 seconds by default. Pass `timeout_ms` for long-running commands. On timeout the child process is sent SIGKILL.\n\
- Background commands have no automatic timeout — use `kill_shell` to stop them.\n\
- Foreground stdout/stderr are capped per stream; if a command is too noisy, redirect to a file and inspect slices with `read_file` or narrower shell commands.\n\
- Environment variables that look like secrets (names containing TOKEN, SECRET, KEY, OPENAI, AWS_, GITHUB_, etc.) are stripped from the child process.\n\
- Never run destructive commands (`rm -rf`, `git reset --hard`, force-push, dropping tables, etc.) unless the user explicitly asked.\n\
- Network commands (curl, wget) are allowed but prefer `web_fetch` for HTTP — it has stricter limits and won't pull in unexpected redirects.\n\
\n\
Common mistakes:\n\
- Forgetting to quote paths with spaces — wrap them in single quotes.\n\
- Pipelines without `set -o pipefail` swallow upstream failures; check intermediate exit codes when it matters.\n\
- Long-running interactive commands (REPLs, watchers) will block until timeout — only run non-interactive commands, OR use `run_in_background: true`.\n\
- Spawning a dev server in foreground — it will hang until timeout. Use background mode.\n\
\n\
Parameters:\n\
- `command`: Shell command to execute with the platform shell. Quote arguments that contain spaces.\n\
- `timeout_ms`: Foreground hard timeout in milliseconds, or `null` for the default of 120000. Ignored when `run_in_background` is true.\n\
- `run_in_background`: When true, spawn detached and return `bash_id` immediately. When false or null, run synchronously and return the full result."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute (interpreted by `sh -c` on Unix and `cmd /C` on Windows)."},
                "timeout_ms": {"type": ["integer", "null"], "description": "Foreground hard timeout in milliseconds; null uses the default of 120000. Ignored in background mode."},
                "run_in_background": {"type": ["boolean", "null"], "description": "Spawn detached and return bash_id immediately; null/false runs synchronously."},
                "dangerous_override": {"type": ["boolean", "null"], "description": "Set true ONLY after user explicitly confirmed."}
            },
            "required": ["command", "timeout_ms", "run_in_background", "dangerous_override"],
            "additionalProperties": false
        })
    }
    fn timeout(&self, args: &Value) -> std::time::Duration {
        const DEFAULT: std::time::Duration = std::time::Duration::from_secs(180);
        // Background runs return immediately; the default backstop is plenty.
        // Check the same key aliases the deserializer accepts (ShellArgs).
        let background = ["run_in_background", "runInBackground"]
            .iter()
            .find_map(|k| args.get(*k))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if background {
            return DEFAULT;
        }
        // Foreground: honor the caller's timeout_ms (default 120s, accepted as a
        // number or numeric string, under any alias the deserializer accepts)
        // and give the inner SIGKILL+cleanup ~30s of headroom before this outer
        // backstop — otherwise a timeout above 180s would be aborted at the
        // default. Reading only `timeout_ms` here would miss the aliases and
        // re-introduce the early-kill for `timeoutMs`/`timeout` callers.
        let inner_ms = ["timeout_ms", "timeoutMs", "timeout"]
            .iter()
            .find_map(|k| args.get(*k))
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
            .unwrap_or(120_000);
        std::time::Duration::from_millis(inner_ms.saturating_add(30_000)).max(DEFAULT)
    }

    fn danger_reason(&self, args: &Value) -> Option<&'static str> {
        args.get("command")
            .or_else(|| args.get("cmd"))
            .and_then(|v| v.as_str())
            .and_then(classify_danger)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: ShellArgs = crate::tools::parse_args("run_shell", args)?;
        if let Some(reason) = classify_danger(&a.command) {
            // The approval gate (agent::exec::approval_outcome) forces a human
            // prompt for any classifier-flagged command before execution — even
            // under an allow rule or a bypass mode — so in an interactive run a
            // human has already seen and approved THIS exact command and the
            // model-supplied `dangerous_override` only acknowledges it. A
            // non-interactive run has no approver, so the override is ignored
            // and the command is refused outright (also enforced at the gate).
            let overridden = a.dangerous_override.unwrap_or(false) && !ctx.non_interactive;
            if !overridden {
                let hint = if ctx.non_interactive {
                    "This is a non-interactive run; destructive commands are refused and cannot be overridden by the model."
                } else {
                    "Confirm with the user first, then retry with `dangerous_override: true`."
                };
                return Err(anyhow!(
                    "refused: {reason}. {hint} Command was: {}",
                    a.command
                ));
            }
            tracing::warn!(command = %a.command, reason, "run_shell.dangerous_override_used");
        }
        let mut cmd = Command::new(platform_shell_name());
        configure_platform_shell(&mut cmd, &a.command);
        cmd.current_dir(&ctx.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        isolate_process_group(&mut cmd);
        // Strip likely-secret env vars before spawn so the LLM can't echo them.
        crate::secret_env::scrub_secret_env(&mut cmd);

        if a.run_in_background.unwrap_or(false) {
            return spawn_background(cmd, &a.command, ctx).await;
        }

        let timeout = std::time::Duration::from_millis(a.timeout_ms.unwrap_or(120_000));
        let mut child = cmd.spawn()?;
        let child_pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no stdout handle"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("no stderr handle"))?;
        let stdout_task = tokio::spawn(read_capped_output(
            stdout,
            FOREGROUND_OUTPUT_MAX_BYTES_PER_STREAM,
        ));
        let stderr_task = tokio::spawn(read_capped_output(
            stderr,
            FOREGROUND_OUTPUT_MAX_BYTES_PER_STREAM,
        ));
        // Bound the ENTIRE wait+drain by the timeout. Reading stdout/stderr to
        // EOF can outlast the child when a backgrounded grandchild inherits the
        // pipe (`cmd &`, `( sleep 999 & )`); previously only `child.wait()` was
        // inside the timeout, so the drain afterwards could hang up to the outer
        // backstop. On timeout we kill the whole process group (closing the
        // inherited fds and reaping descendants).
        let collected = tokio::time::timeout(timeout, async {
            let status = child.wait().await?;
            let stdout = stdout_task
                .await
                .map_err(|e| anyhow!("stdout reader task failed: {e}"))?;
            let stderr = stderr_task
                .await
                .map_err(|e| anyhow!("stderr reader task failed: {e}"))?;
            Ok::<_, anyhow::Error>((status, stdout, stderr))
        })
        .await;
        let (status, stdout, stderr) = match collected {
            Ok(inner) => inner?,
            Err(_) => {
                // `child` (kill_on_drop) was dropped when the timeout future was
                // dropped; also kill the group so grandchildren holding the pipe
                // die and the detached reader tasks reach EOF.
                kill_process_group(child_pid);
                return Err(anyhow::anyhow!("timed out after {}ms", timeout.as_millis()));
            }
        };
        let stdout = format_capped_stream("stdout", stdout);
        let stderr = format_capped_stream("stderr", stderr);
        let code = status.code().unwrap_or(-1);
        Ok(format!(
            "exit_code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ))
    }
}

/// Spawn `cmd` detached and register a `BackgroundShellState` in the session.
/// Two reader tasks drain stdout/stderr into per-state buffers; a third waits
/// on the child (or a kill signal) and flips `status` on exit.
async fn spawn_background(
    mut cmd: Command,
    command_str: &str,
    ctx: &ToolContext,
) -> Result<String> {
    isolate_process_group(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to spawn background shell: {e}"))?;
    let child_pid = child.id();

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout handle"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("no stderr handle"))?;

    let (kill_tx, kill_rx) = oneshot::channel::<()>();

    let state = Arc::new(BackgroundShellState {
        command: command_str.to_string(),
        started_at_ms: now_ms(),
        stdout: tokio::sync::Mutex::new(Vec::new()),
        stderr: tokio::sync::Mutex::new(Vec::new()),
        status: tokio::sync::Mutex::new(BgStatus::Running),
        stdout_cursor: tokio::sync::Mutex::new(0),
        stderr_cursor: tokio::sync::Mutex::new(0),
        kill_tx: tokio::sync::Mutex::new(Some(kill_tx)),
        pid: child_pid,
    });

    // stdout reader.
    {
        let state = state.clone();
        let mut reader = stdout;
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        append_capped(&state.stdout, &state.stdout_cursor, &buf[..n]).await;
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // stderr reader.
    {
        let state = state.clone();
        let mut reader = stderr;
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        append_capped(&state.stderr, &state.stderr_cursor, &buf[..n]).await;
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // Waiter: either child exits naturally, or a kill signal arrives.
    {
        let state = state.clone();
        tokio::spawn(async move {
            tokio::select! {
                wait_result = child.wait() => {
                    let mut st = state.status.lock().await;
                    *st = match wait_result {
                        Ok(status) => BgStatus::Exited(status.code().unwrap_or(-1)),
                        Err(e) => BgStatus::Error(e.to_string()),
                    };
                }
                _ = kill_rx => {
                    kill_process_group(child_pid);
                    // SIGKILL — `kill_on_drop` will also fire when `child` drops.
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    let mut st = state.status.lock().await;
                    *st = BgStatus::Killed;
                }
            }
            // Drop the kill_tx slot so future kill_shell calls report
            // "already terminated" cleanly.
            *state.kill_tx.lock().await = None;
        });
    }

    let id = bash_id();
    {
        let mut session = ctx.session.lock().await;
        session.background_shells.insert(id.clone(), state);
    }
    Ok(format!(
        "{{\"bash_id\": \"{id}\", \"status\": \"running\"}}"
    ))
}

#[cfg(test)]
mod tests;

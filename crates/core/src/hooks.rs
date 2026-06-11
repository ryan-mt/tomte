//! Lifecycle hooks loaded from `~/.config/tomte/settings.json`.
//!
//! Supported events:
//!   - **PreToolUse**: before a tool runs. Exit 2 to block; stdout becomes
//!     the block reason surfaced to the model.
//!   - **PostToolUse**: after a tool finishes (success or error). Best-effort
//!     side-effects only; exit code does not block.
//!   - **UserPromptSubmit**: when the user submits a prompt, before the
//!     model sees it. Exit 2 to BLOCK the prompt entirely (stdout = reason
//!     shown to the user).
//!   - **SessionStart**: once per Agent creation, before any turn runs.
//!   - **Stop**: after every assistant turn finishes (success or error).
//!
//! Matcher syntax:
//!   - `"*"`          — matches anything
//!   - `"run_shell"`  — exact tool name (PreToolUse/PostToolUse only)
//!   - `"re:^edit_"` — regex match on tool name
//!   - `"file:**/*.rs"` — file-path glob (matched against path-style fields
//!     such as `path`/`file_path`/`filePath`/`notebook_path`; useful for file
//!     tools).
//!
//! Format (settings.json):
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse":       [{ "matcher": "run_shell",  "command": "guard.sh" }],
//!     "PostToolUse":      [{ "matcher": "file:**/*.rs", "command": "rustfmt-staged.sh" }],
//!     "UserPromptSubmit": [{ "matcher": "*", "command": "redact-secrets.sh" }],
//!     "SessionStart":     [{ "matcher": "*", "command": "warm-cache.sh" }],
//!     "Stop":             [{ "matcher": "*", "command": "session-end.sh" }]
//!   }
//! }
//! ```

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const HOOK_STDOUT_MAX_BYTES: usize = 64 * 1024;
const HOOK_READ_CHUNK_BYTES: usize = 8 * 1024;

#[cfg(unix)]
fn isolate_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    unsafe {
        let _ = kill(-pid, SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {}

/// Choose the shell that runs a hook command, per OS. Unix always uses
/// `sh -c`. Windows prefers `sh -c` when a `sh` interpreter is on PATH (Git
/// Bash / MSYS, so existing POSIX-style hooks keep working) and otherwise falls
/// back to `cmd /C`, so a hook still runs on a stock Windows box with no Git
/// Bash installed. Pure in `sh_available` so both Windows branches are
/// unit-testable on any host.
fn shell_invocation(sh_available: bool) -> (&'static str, &'static str) {
    if cfg!(windows) {
        if sh_available {
            ("sh", "-c")
        } else {
            ("cmd", "/C")
        }
    } else {
        let _ = sh_available;
        ("sh", "-c")
    }
}

/// Is a `sh` interpreter on PATH? Consulted only on Windows (on Unix `sh` is a
/// POSIX given, so we never scan). A bare name searched on PATH, honoring the
/// OS executable extension — the same lightweight `which` the doctor uses.
fn sh_on_path() -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exe_ext = std::env::consts::EXE_EXTENSION;
    std::env::split_paths(&paths).any(|dir| {
        dir.join("sh").is_file()
            || (!exe_ext.is_empty() && dir.join(format!("sh.{exe_ext}")).is_file())
    })
}

/// Build the `Command` that runs `command` through the OS-appropriate shell.
/// `&&` short-circuits so the PATH scan only happens on Windows.
fn build_shell_command(command: &str) -> Command {
    let (program, flag) = shell_invocation(cfg!(windows) && sh_on_path());
    let mut cmd = Command::new(program);
    cmd.arg(flag).arg(command);
    cmd
}

/// Human label of the shell that runs hooks on this OS (`sh -c` or `cmd /C`),
/// for `tomte doctor`. Uses the same selection as the real runner.
pub fn hook_shell_label() -> String {
    let (program, flag) = shell_invocation(cfg!(windows) && sh_on_path());
    format!("{program} {flag}")
}

/// Run `command` through the OS-appropriate shell exactly as a hook would (same
/// shell selection and secret-env scrub), returning the exit code and captured
/// stdout+stderr. Backs `tomte hooks run`, so a user can confirm a hook actually
/// works on their machine. No stdin payload is fed (stdin is closed).
pub async fn probe_command(command: &str, timeout: Duration) -> Result<(i32, String)> {
    let mut cmd = build_shell_command(command);
    cmd.kill_on_drop(true);
    // The probe's output is shown to the user, so strip secrets just like the
    // real hook runner does.
    crate::secret_env::scrub_secret_env(&mut cmd);
    isolate_process_group(&mut cmd);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(result) => result?,
        Err(_) => return Err(anyhow!("timed out after {}s", timeout.as_secs())),
    };
    let code = output.status.code().unwrap_or(-1);
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim_end();
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(stderr);
    }
    Ok((code, combined))
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookEntry>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookEntry>,
    #[serde(default, rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<HookEntry>,
    #[serde(default, rename = "SessionStart")]
    pub session_start: Vec<HookEntry>,
    #[serde(default, rename = "Stop")]
    pub stop: Vec<HookEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookEntry {
    /// Matcher: `"*"`, exact name, `re:<regex>`, or `file:<glob>`. See module
    /// docs for syntax.
    pub matcher: String,
    /// Shell command (interpreted by `sh -c`).
    pub command: String,
}

#[derive(Debug)]
pub enum HookDecision {
    Allow,
    Block(String),
}

#[derive(Debug, Default)]
pub struct HookSet {
    pub config: HooksConfig,
}

pub fn settings_path() -> PathBuf {
    crate::config::config_dir().join("settings.json")
}

pub fn load() -> HookSet {
    let path = settings_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HookSet::default();
    };
    #[derive(Deserialize)]
    struct SettingsFile {
        #[serde(default)]
        hooks: HooksConfig,
    }
    match serde_json::from_str::<SettingsFile>(crate::config::strip_bom(&text)) {
        Ok(s) => HookSet { config: s.hooks },
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to parse settings.json");
            HookSet::default()
        }
    }
}

/// Test whether `matcher` selects an event keyed by `tool` (for PreToolUse
/// / PostToolUse the key is the tool name; for UserPromptSubmit the key is
/// the prompt text; for SessionStart / Stop the key is the empty string and
/// only `"*"` matches).
///
/// `path_hint`, when Some, is also considered for the `file:<glob>` matcher
/// (we check args for known file-path aliases).
pub fn matches(matcher: &str, key: &str, path_hint: Option<&str>) -> bool {
    if matcher == "*" {
        return true;
    }
    if let Some(rx) = matcher.strip_prefix("re:") {
        return match regex::Regex::new(rx) {
            Ok(re) => re.is_match(key),
            Err(e) => {
                // A typo'd guard regex previously failed silently (no match → the
                // hook never fired → the tool ran unguarded). Surface it so the
                // user can fix the matcher rather than wonder why it never runs.
                tracing::warn!(matcher = %matcher, error = %e, "invalid `re:` hook matcher; treating as no-match");
                false
            }
        };
    }
    if let Some(pat) = matcher.strip_prefix("file:") {
        let Some(p) = path_hint else { return false };
        return glob_match(pat, p);
    }
    matcher == key
}

/// Minimal glob matcher: `**` matches any path segments, `*` matches a
/// path segment, `?` matches a single char, and `{a,b}` alternation expands
/// (single level) like ripgrep's `--glob` — `*.{ts,tsx}` used to be treated
/// as the literal braces and silently matched nothing. Good enough for the
/// typical `**/*.rs` patterns; sidesteps adding the `glob` crate as a hard dep.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    expand_braces(pattern, 0)
        .iter()
        .any(|p| glob_match_single(p, path))
}

/// Expand `{a,b}` alternations into the flat pattern list they stand for.
/// Single-level (no nested braces) with a combination cap; an unbalanced or
/// empty brace falls back to the literal pattern, matching the old behavior.
fn expand_braces(pattern: &str, depth: usize) -> Vec<String> {
    const MAX_EXPANSIONS: usize = 64;
    let (Some(open), false) = (pattern.find('{'), depth >= 4) else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}').map(|i| open + i) else {
        return vec![pattern.to_string()];
    };
    let (prefix, body, suffix) = (
        &pattern[..open],
        &pattern[open + 1..close],
        &pattern[close + 1..],
    );
    if body.is_empty() {
        return vec![pattern.to_string()];
    }
    let mut out = Vec::new();
    for alt in body.split(',') {
        for rest in expand_braces(suffix, depth + 1) {
            out.push(format!("{prefix}{alt}{rest}"));
            if out.len() >= MAX_EXPANSIONS {
                return out;
            }
        }
    }
    out
}

fn glob_match_single(pattern: &str, path: &str) -> bool {
    fn push_literal(out: &mut String, ch: char) {
        if matches!(
            ch,
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }

    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                while chars.peek() == Some(&'*') {
                    chars.next();
                }
                if chars.peek() == Some(&'/') {
                    chars.next();
                    regex.push_str("(?:.*/)?");
                } else {
                    regex.push_str(".*");
                }
            }
            '*' => regex.push_str("[^/]*"),
            '?' => regex.push_str("[^/]"),
            '/' => regex.push('/'),
            other => push_literal(&mut regex, other),
        }
    }
    regex.push('$');
    regex::Regex::new(&regex)
        .map(|re| re.is_match(&path))
        .unwrap_or(false)
}

impl HookSet {
    fn select<'a>(
        entries: &'a [HookEntry],
        key: &str,
        path_hint: Option<&str>,
    ) -> Vec<&'a HookEntry> {
        entries
            .iter()
            .filter(|h| matches(&h.matcher, key, path_hint))
            .collect()
    }

    fn path_hint_from_args(args: &Value) -> Option<String> {
        const PATH_KEYS: &[&str] = &[
            "path",
            "file_path",
            "filePath",
            "notebook_path",
            "notebookPath",
            "directory",
            "dir",
            "folder",
        ];
        let obj = args.as_object()?;
        PATH_KEYS
            .iter()
            .find_map(|key| obj.get(*key)?.as_str())
            .map(|s| s.to_string())
    }

    /// Fire every matching PreToolUse hook in declaration order. The first
    /// hook to return exit code 2 short-circuits and the call is blocked.
    pub async fn fire_pre(&self, tool: &str, args: &Value) -> HookDecision {
        let hint = Self::path_hint_from_args(args);
        for hook in Self::select(&self.config.pre_tool_use, tool, hint.as_deref()) {
            let payload = serde_json::json!({
                "hook": "PreToolUse",
                "tool": tool,
                "args": args,
            });
            match run_hook(&hook.command, &payload).await {
                Ok((code, stdout)) => {
                    if code == 2 {
                        let reason = stdout.trim().to_string();
                        let reason = if reason.is_empty() {
                            format!("blocked by PreToolUse hook (matcher={})", hook.matcher)
                        } else {
                            reason
                        };
                        return HookDecision::Block(reason);
                    }
                }
                Err(e) => {
                    return HookDecision::Block(format!(
                        "PreToolUse hook error (matcher={}): {}",
                        hook.matcher, e
                    ));
                }
            }
        }
        HookDecision::Allow
    }

    /// Fire every matching PostToolUse hook. Best-effort: failures and
    /// non-zero exits are logged but never block, since the tool call has
    /// already happened.
    pub async fn fire_post(&self, tool: &str, args: &Value, output: &str, error: bool) {
        let hint = Self::path_hint_from_args(args);
        for hook in Self::select(&self.config.post_tool_use, tool, hint.as_deref()) {
            let payload = serde_json::json!({
                "hook": "PostToolUse",
                "tool": tool,
                "args": args,
                "output": output,
                "error": error,
            });
            if let Err(e) = run_hook(&hook.command, &payload).await {
                tracing::warn!(error = %e, matcher = %hook.matcher, "PostToolUse hook failed");
            }
        }
    }

    /// Fire every matching UserPromptSubmit hook. Exit 2 BLOCKS the prompt
    /// — the model never sees it; the user sees the hook's stdout as the
    /// rejection reason.
    pub async fn fire_user_prompt_submit(&self, prompt: &str) -> HookDecision {
        for hook in Self::select(&self.config.user_prompt_submit, prompt, None) {
            let payload = serde_json::json!({
                "hook": "UserPromptSubmit",
                "prompt": prompt,
            });
            match run_hook(&hook.command, &payload).await {
                Ok((code, stdout)) => {
                    if code == 2 {
                        let reason = stdout.trim().to_string();
                        let reason = if reason.is_empty() {
                            format!(
                                "prompt blocked by UserPromptSubmit hook (matcher={})",
                                hook.matcher
                            )
                        } else {
                            reason
                        };
                        return HookDecision::Block(reason);
                    }
                }
                Err(e) => {
                    return HookDecision::Block(format!(
                        "UserPromptSubmit hook error (matcher={}): {}",
                        hook.matcher, e
                    ));
                }
            }
        }
        HookDecision::Allow
    }

    /// Fire SessionStart hooks. Best-effort.
    pub async fn fire_session_start(&self) {
        for hook in Self::select(&self.config.session_start, "", None) {
            let payload = serde_json::json!({ "hook": "SessionStart" });
            if let Err(e) = run_hook(&hook.command, &payload).await {
                tracing::warn!(error = %e, matcher = %hook.matcher, "SessionStart hook failed");
            }
        }
    }

    /// Fire Stop hooks. Best-effort.
    pub async fn fire_stop(&self) {
        for hook in Self::select(&self.config.stop, "", None) {
            let payload = serde_json::json!({ "hook": "Stop" });
            if let Err(e) = run_hook(&hook.command, &payload).await {
                tracing::warn!(error = %e, matcher = %hook.matcher, "Stop hook failed");
            }
        }
    }
}

/// Run a hook command via `sh -c`, write the JSON payload to its stdin, and
/// return the exit code + captured stdout. Times out at 30 seconds.
async fn run_hook(command: &str, payload: &Value) -> Result<(i32, String)> {
    run_hook_with_timeout(command, payload, Duration::from_secs(30)).await
}

async fn run_hook_with_timeout(
    command: &str,
    payload: &Value,
    timeout: Duration,
) -> Result<(i32, String)> {
    let mut cmd = build_shell_command(command);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Strip inherited secret env so a hook command can't exfiltrate API keys or
    // tokens — its stdout is fed back into the model transcript.
    crate::secret_env::scrub_secret_env(&mut cmd);
    isolate_process_group(&mut cmd);

    let mut child = cmd.spawn()?;
    let child_pid = child.id();

    // Spawn the stdout/stderr drainers BEFORE writing stdin. Otherwise a hook
    // that emits output before reading its stdin deadlocks: we block on
    // `write_all` (the stdin pipe is full because the hook isn't reading yet)
    // while the hook blocks on its stdout write (that pipe is full because no
    // one is draining it). The PostToolUse payload carries the tool output (up
    // to ~1 MiB), far larger than the ~64 KiB pipe buffer, so this is reachable.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        match stdout {
            Some(out) => read_capped(out, HOOK_STDOUT_MAX_BYTES).await,
            None => Ok(CappedOutput::default()),
        }
    });
    let stderr_task = tokio::spawn(async move {
        match stderr {
            // Drain stderr so a noisy hook cannot block forever on a full pipe.
            // We do not surface stderr in hook decisions.
            Some(err) => read_capped(err, 0).await,
            None => Ok(CappedOutput::default()),
        }
    });

    // Feed stdin on its own task so a hook that never reads stdin can't block the
    // whole call — the write completes, errors (EPIPE once the child exits), or
    // is aborted at the deadline. Dropping the handle closes stdin (EOF).
    let stdin = child.stdin.take();
    let payload_bytes = serde_json::to_vec(payload).unwrap_or_default();
    let stdin_task = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&payload_bytes).await;
            let _ = stdin.flush().await;
        }
    });

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            stdin_task.abort();
            stdout_task.abort();
            stderr_task.abort();
            return Err(e.into());
        }
        Err(e) => {
            kill_process_group(child_pid);
            let _ = child.kill().await;
            stdin_task.abort();
            stdout_task.abort();
            stderr_task.abort();
            return Err(e.into());
        }
    };
    // Child has exited; its stdin read-end is closed so any pending write fails
    // fast. Abort defensively so this can never hang.
    stdin_task.abort();
    // Bound the pipe drain too: a hook that backgrounds a child (`… &`) exits
    // fast itself, but the grandchild inherits stdout/stderr and holds the
    // pipes open — an unbounded await here hangs the agent forever. On a stall
    // reap the process group (closing the pipes lets the readers hit EOF and
    // hand back whatever the hook already wrote), then give the drain one more
    // bounded chance.
    const HOOK_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
    let stdout_abort = stdout_task.abort_handle();
    let stderr_abort = stderr_task.abort_handle();
    let drain = async { (stdout_task.await, stderr_task.await) };
    tokio::pin!(drain);
    let (stdout_res, stderr_res) = match tokio::time::timeout(HOOK_DRAIN_TIMEOUT, &mut drain).await
    {
        Ok(pair) => pair,
        Err(_) => {
            kill_process_group(child_pid);
            match tokio::time::timeout(HOOK_DRAIN_TIMEOUT, &mut drain).await {
                Ok(pair) => pair,
                Err(_) => {
                    stdout_abort.abort();
                    stderr_abort.abort();
                    anyhow::bail!(
                        "hook output pipes stayed open past the drain deadline \
                             (a backgrounded child still holds them)"
                    );
                }
            }
        }
    };
    let stdout = stdout_res??.into_string("stdout");
    let _ = stderr_res??;
    let code = status.code().unwrap_or(-1);
    Ok((code, stdout))
}

#[derive(Default)]
struct CappedOutput {
    bytes: Vec<u8>,
    omitted: usize,
}

impl CappedOutput {
    fn into_string(self, label: &str) -> String {
        let mut out = String::from_utf8_lossy(&self.bytes).to_string();
        if self.omitted > 0 {
            out.push_str(&format!(
                "\n[tomte truncated hook {label}: omitted {} byte(s)]",
                self.omitted
            ));
        }
        out
    }
}

async fn read_capped<R>(mut reader: R, limit: usize) -> std::io::Result<CappedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut out = CappedOutput::default();
    let mut buf = [0u8; HOOK_READ_CHUNK_BYTES];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let remaining = limit.saturating_sub(out.bytes.len());
        let keep = remaining.min(n);
        if keep > 0 {
            out.bytes.extend_from_slice(&buf[..keep]);
        }
        out.omitted += n - keep;
    }
    Ok(out)
}

pub mod presets;

#[cfg(test)]
mod tests;

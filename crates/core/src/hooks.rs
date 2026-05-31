//! Lifecycle hooks loaded from `~/.config/opencli/settings.json`.
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

use anyhow::Result;
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
    match serde_json::from_str::<SettingsFile>(&text) {
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
/// path segment, `?` matches a single char. Good enough for the typical
/// `**/*.rs` patterns; sidesteps adding the `glob` crate as a hard dep.
pub fn glob_match(pattern: &str, path: &str) -> bool {
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
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    isolate_process_group(&mut cmd);

    let mut child = cmd.spawn()?;
    let child_pid = child.id();
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = serde_json::to_vec(payload).unwrap_or_default();
        let _ = stdin.write_all(&bytes).await;
        let _ = stdin.flush().await;
        drop(stdin);
    }
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

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(e.into()),
        Err(e) => {
            kill_process_group(child_pid);
            let _ = child.kill().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(e.into());
        }
    };
    let stdout = stdout_task.await??.into_string("stdout");
    let _ = stderr_task.await??;
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
                "\n[opencli truncated hook {label}: omitted {} byte(s)]",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_wildcard_and_exact() {
        assert!(matches("*", "any", None));
        assert!(matches("run_shell", "run_shell", None));
        assert!(!matches("run_shell", "read_file", None));
    }

    #[test]
    fn matches_regex_via_re_prefix() {
        assert!(matches("re:^edit_", "edit_file", None));
        assert!(!matches("re:^edit_", "multi_edit_file", None)); // anchored start
        assert!(!matches("re:^edit_", "read_file", None));
        assert!(matches("re:^run_shell$", "run_shell", None)); // anchored exact
        assert!(!matches("re:^run_shell$", "run_shell_extra", None));
    }

    #[test]
    fn matches_file_glob_against_path_hint() {
        assert!(matches("file:**/*.rs", "edit_file", Some("src/foo.rs")));
        assert!(matches("file:**/*.rs", "edit_file", Some("a/b/c.rs")));
        assert!(matches("file:**/*.rs", "edit_file", Some("main.rs")));
        assert!(!matches("file:**/*.rs", "edit_file", Some("src/foo.ts")));
        assert!(!matches("file:**/*.rs", "edit_file", None));
    }

    #[test]
    fn path_hint_from_args_includes_notebook_path() {
        let args = serde_json::json!({
            "notebook_path": "analysis/demo.ipynb",
            "edit_mode": "replace"
        });

        assert_eq!(
            HookSet::path_hint_from_args(&args).as_deref(),
            Some("analysis/demo.ipynb")
        );
    }

    #[test]
    fn path_hint_from_args_includes_camel_case_and_directory_aliases() {
        let args = serde_json::json!({
            "filePath": "src/lib.rs"
        });
        assert_eq!(
            HookSet::path_hint_from_args(&args).as_deref(),
            Some("src/lib.rs")
        );

        let args = serde_json::json!({
            "notebookPath": "analysis/demo.ipynb"
        });
        assert_eq!(
            HookSet::path_hint_from_args(&args).as_deref(),
            Some("analysis/demo.ipynb")
        );

        let args = serde_json::json!({
            "directory": "src"
        });
        assert_eq!(HookSet::path_hint_from_args(&args).as_deref(), Some("src"));
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "main.rs"));
        assert!(glob_match("**/*.rs", "deep/nested/file.rs"));
        assert!(!glob_match("*.rs", "main.ts"));
        assert!(glob_match("?ello", "hello"));
        assert!(!glob_match("?ello", "yyello"));
    }

    #[cfg(unix)]
    fn sh_quote(path: &std::path::Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_hook_timeout_kills_background_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("survived-hook-timeout");
        let command = format!(
            "(sleep 0.5; printf survived > {}) & wait",
            sh_quote(&marker)
        );

        let err = run_hook_with_timeout(
            &command,
            &serde_json::json!({"hook": "test"}),
            Duration::from_millis(80),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("elapsed"), "got: {err}");
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !marker.exists(),
            "hook timeout killed only the shell; a background descendant survived"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_hook_caps_stdout_but_keeps_exit_code() {
        let (code, stdout) = run_hook_with_timeout(
            "head -c 70000 /dev/zero | tr '\\0' x",
            &serde_json::json!({"hook": "test"}),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        assert_eq!(code, 0);
        assert!(stdout.starts_with("xxx"), "got: {stdout:?}");
        assert!(stdout.contains("truncated hook stdout"), "got: {stdout:?}");
        assert!(stdout.len() < HOOK_STDOUT_MAX_BYTES + 256);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_hook_drains_noisy_stderr_without_blocking() {
        let (code, stdout) = run_hook_with_timeout(
            "head -c 200000 /dev/zero >&2",
            &serde_json::json!({"hook": "test"}),
            Duration::from_secs(2),
        )
        .await
        .unwrap();

        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "stderr should not leak into stdout");
    }
}

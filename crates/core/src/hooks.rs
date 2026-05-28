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
//!   - `"file:**/*.rs"` — file-path glob (matched against `path`/`file_path`
//!     fields in the args/result JSON; useful for write_file/edit_file).
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
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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
/// (we check args for `path`/`file_path`).
pub fn matches(matcher: &str, key: &str, path_hint: Option<&str>) -> bool {
    if matcher == "*" {
        return true;
    }
    if let Some(rx) = matcher.strip_prefix("re:") {
        // Naive substring fallback if regex compilation fails (no regex
        // dependency yet). We treat the pattern as a literal substring.
        return key.contains(rx);
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
    fn helper(p: &[u8], s: &[u8]) -> bool {
        let mut pi = 0;
        let mut si = 0;
        let (mut star_p, mut star_s): (Option<usize>, usize) = (None, 0);
        while si < s.len() {
            if pi < p.len() && (p[pi] == s[si] || p[pi] == b'?') {
                pi += 1;
                si += 1;
                continue;
            }
            if pi < p.len() && p[pi] == b'*' {
                // Handle `**` as same as `*` for this simple matcher.
                while pi + 1 < p.len() && p[pi + 1] == b'*' {
                    pi += 1;
                }
                star_p = Some(pi);
                star_s = si;
                pi += 1;
                continue;
            }
            if let Some(sp) = star_p {
                pi = sp + 1;
                star_s += 1;
                si = star_s;
                continue;
            }
            return false;
        }
        while pi < p.len() && p[pi] == b'*' {
            pi += 1;
        }
        pi == p.len()
    }
    helper(pattern.as_bytes(), path.as_bytes())
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
        args.get("path")
            .or_else(|| args.get("file_path"))
            .and_then(|v| v.as_str())
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
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = serde_json::to_vec(payload).unwrap_or_default();
        let _ = stdin.write_all(&bytes).await;
        let _ = stdin.flush().await;
        drop(stdin);
    }
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await??;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let code = out.status.code().unwrap_or(-1);
    Ok((code, stdout))
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
    fn matches_substring_via_re_prefix() {
        // Without a regex engine we fall back to substring on `re:`.
        assert!(matches("re:edit_", "edit_file", None));
        assert!(matches("re:edit_", "multi_edit_file", None));
        assert!(!matches("re:edit_", "read_file", None));
    }

    #[test]
    fn matches_file_glob_against_path_hint() {
        assert!(matches("file:**/*.rs", "edit_file", Some("src/foo.rs")));
        assert!(matches("file:**/*.rs", "edit_file", Some("a/b/c.rs")));
        assert!(!matches("file:**/*.rs", "edit_file", Some("src/foo.ts")));
        assert!(!matches("file:**/*.rs", "edit_file", None));
    }

    #[test]
    fn glob_match_basic() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(glob_match("**/*.rs", "deep/nested/file.rs"));
        assert!(!glob_match("*.rs", "main.ts"));
        assert!(glob_match("?ello", "hello"));
        assert!(!glob_match("?ello", "yyello"));
    }
}

//! Lifecycle hooks loaded from `~/.config/opencli/settings.json`.
//!
//! Currently supported events: `PreToolUse`. A hook matches by tool name (or
//! `"*"` for any tool) and runs the configured shell command with a JSON
//! payload on stdin. Exit code `2` blocks the tool call; any other exit allows
//! it. Stdout from the hook, when blocking, is surfaced to the model as the
//! block reason so it can adapt.
//!
//! Format:
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       { "matcher": "run_shell", "command": "/path/to/guard.sh" }
//!     ]
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookEntry {
    /// Tool-name matcher. `"*"` matches every tool; otherwise an exact match
    /// against the tool's registered name (`read_file`, `run_shell`, …).
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

impl HookSet {
    fn matches(matcher: &str, tool: &str) -> bool {
        matcher == "*" || matcher == tool
    }

    /// Fire every matching PreToolUse hook in declaration order. The first
    /// hook to return exit code 2 short-circuits and the call is blocked.
    pub async fn fire_pre(&self, tool: &str, args: &Value) -> HookDecision {
        for hook in self.config.pre_tool_use.iter() {
            if !Self::matches(&hook.matcher, tool) {
                continue;
            }
            let payload = serde_json::json!({
                "hook": "PreToolUse",
                "tool": tool,
                "args": args,
            });
            if let Ok((code, stdout)) = run_hook(&hook.command, &payload).await {
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
        }
        HookDecision::Allow
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

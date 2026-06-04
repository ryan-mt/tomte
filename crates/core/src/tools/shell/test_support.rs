//! Shared test helpers for the shell submodules.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::Mutex;

use crate::tools::{ApprovalMode, BuiltinTool, SessionState, ToolContext};

use super::BashOutput;

pub(super) fn ctx_at(cwd: std::path::PathBuf) -> ToolContext {
    // These tests exercise run_shell mechanics, not the OS sandbox. Under the
    // default `workspace-write` mode run_shell re-execs the *test* binary as the
    // sandbox helper (which doesn't understand `__sandbox`), so disable it here.
    let mut config = crate::config::Config::default();
    config.sandbox.mode = "danger-full-access".to_string();
    ToolContext {
        cwd,
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: Arc::new(Mutex::new(SessionState::default())),
        config,
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

pub(super) fn ctx() -> ToolContext {
    ctx_at(std::env::current_dir().unwrap())
}

pub(super) fn print_command(text: &str) -> String {
    #[cfg(windows)]
    {
        format!("echo {text}")
    }
    #[cfg(not(windows))]
    {
        format!("printf '{}\\n'", text.replace('\'', "'\\''"))
    }
}

pub(super) fn delayed_two_line_command() -> &'static str {
    #[cfg(windows)]
    {
        "echo first & ping -n 2 127.0.0.1 >NUL & echo second"
    }
    #[cfg(not(windows))]
    {
        "printf 'first\\n'; sleep 0.2; printf 'second\\n'"
    }
}

pub(super) fn long_sleep_command() -> &'static str {
    #[cfg(windows)]
    {
        "powershell -NoProfile -Command \"Start-Sleep -Seconds 30\""
    }
    #[cfg(not(windows))]
    {
        "sleep 30"
    }
}

#[cfg(unix)]
pub(super) fn sh_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

pub(super) fn parse_bash_id(s: &str) -> String {
    // Background spawn returns: {"bash_id": "bash_xxxx", "status": "running"}
    let v: serde_json::Value = serde_json::from_str(s).unwrap();
    v.get("bash_id").unwrap().as_str().unwrap().to_string()
}

pub(super) async fn wait_until_status(
    ctx: &ToolContext,
    id: &str,
    want: &str,
    max_ms: u64,
) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(max_ms);
    loop {
        let out = BashOutput
            .execute(json!({"bash_id": id}), ctx)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let status = v.get("status").unwrap().as_str().unwrap().to_string();
        if status.contains(want) {
            return out;
        }
        if std::time::Instant::now() > deadline {
            panic!("status never reached `{want}`; last={out}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
}

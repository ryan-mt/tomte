//! Shared test helpers for the search submodules.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::tools::{ApprovalMode, SessionState, ToolContext};

pub(super) fn ctx(cwd: std::path::PathBuf) -> ToolContext {
    ToolContext {
        cwd,
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: crate::config::Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

pub(super) fn write(dir: &std::path::Path, rel: &str, body: &str) {
    let full = dir.join(rel);
    if let Some(p) = full.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

pub(super) fn rg_available() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(super) fn grep_available() -> bool {
    std::process::Command::new("grep")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(super) fn missing_rg(dir: &std::path::Path) -> String {
    dir.join("opencli-missing-rg").display().to_string()
}

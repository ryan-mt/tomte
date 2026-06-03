//! Shared test helpers for the fs submodules.

use std::sync::Arc;

use serde_json::{json, Value};
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

pub(super) fn read_args(path: &str, offset: Option<usize>, limit: Option<usize>) -> Value {
    json!({"path": path, "offset": offset, "limit": limit})
}

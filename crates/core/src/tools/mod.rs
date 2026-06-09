pub mod ask;
pub mod decision;
pub mod dispatch;
pub mod fs;
pub mod goal;
pub mod lsp;
pub mod memory;
pub mod notebook;
pub mod plan;
pub mod search;
pub mod shell;
pub mod skill;
pub mod todo;
pub mod tool_search;
pub mod wait;
pub mod web;
pub mod worktree;
pub mod xray;

mod validate;
pub use validate::{schema_hint, suggest_tool_names, ArgSchemaError};

mod args;
mod registry;
mod schema;
mod types;

pub use args::{
    deserialize_bool, deserialize_optional_bool, deserialize_optional_string_vec,
    deserialize_optional_u64, deserialize_optional_usize, parse_args,
};
pub use registry::Registry;
pub use types::{
    BackgroundShellState, BgStatus, Checkpoint, RewindOutcome, RewindPointView, SessionState,
    TodoItem, TodoStatus, UndoEntry, WorktreeState,
};

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::openai::{Tool, ToolFunctionDef};

use schema::strict_parameters_schema;

/// A tool result that may carry media (images, PDFs) for the model to SEE, not
/// just text. Most tools return plain text via `execute`; a tool that returns
/// media (e.g. `read_file` on an image) overrides `execute_rich`. `From<String>`
/// and [`ToolOutput::text`] keep the text-only path ergonomic.
pub struct ToolOutput {
    pub text: String,
    pub media: Vec<crate::openai::ToolMedia>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            media: Vec::new(),
        }
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

/// A built-in tool the agent can call.
#[async_trait]
pub trait BuiltinTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters_schema(&self) -> Value;
    /// Read-only tools can be auto-approved.
    fn is_read_only(&self) -> bool {
        false
    }
    /// A best-effort reason this specific call is destructive (e.g. `rm -rf /`,
    /// a force-push), or `None`. When `Some`, the approval gate forces a human
    /// to see and approve THIS exact call before it runs — even under a
    /// persisted allow rule or a bypass mode — so a `run_shell(<prog>:*)` grant
    /// can't silently auto-run a destructive command the user never saw.
    fn danger_reason(&self, _args: &Value) -> Option<&'static str> {
        None
    }
    /// Outer hard timeout for this tool. Most tools share the agent default;
    /// long-running orchestration tools such as `dispatch_agent` can opt into a
    /// larger cap, and `run_shell` derives it from the caller's `timeout_ms` so
    /// a legitimately long build isn't aborted at the default. `args` are the
    /// raw call arguments, available so a tool can size its cap accordingly.
    fn timeout(&self, _args: &Value) -> std::time::Duration {
        std::time::Duration::from_secs(180)
    }
    async fn compute_preview(&self, _args: &Value, _ctx: &ToolContext) -> Option<String> {
        None
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String>;

    /// Rich variant of [`execute`](BuiltinTool::execute) that may attach media
    /// (images, PDFs) for the model to SEE, not just text. Defaults to the text
    /// from `execute`; override ONLY when a tool returns media (e.g. `read_file`
    /// on an image). The agent loop calls this; `execute` stays the text
    /// contract used by direct callers and tests.
    async fn execute_rich(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::text(self.execute(args, ctx).await?))
    }

    fn definition(&self) -> Tool {
        Tool::Function(ToolFunctionDef {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: strict_parameters_schema(self.parameters_schema()),
            strict: true,
        })
    }
}

/// Per-call execution context: working directory, approval policy, and a
/// handle to mutable session state (todos, …).
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: std::path::PathBuf,
    pub approval: ApprovalMode,
    pub require_approval: bool,
    pub auto_approve_edits: bool,
    /// True when no interactive approver is attached to this run (headless
    /// `chat`/`run`). Tools fail closed instead of trusting a model-supplied
    /// confirmation: `run_shell` ignores `dangerous_override` here so a
    /// prompt-injected model cannot self-clear the destructive-command guard.
    pub non_interactive: bool,
    pub session: Arc<Mutex<SessionState>>,
    pub config: crate::config::Config,
    /// Session cwd requested by a tool such as `enter_worktree`. The Agent owns
    /// the actual cwd and applies this after each mutating tool completes.
    pub cwd_override: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// Parent agent's live UI event channel, present only while running inside
    /// an interactive turn. Tools that spawn sub-agents (`dispatch_agent`)
    /// forward sub-agent lifecycle events here so the TUI can render a live
    /// fleet view. `None` in headless tool tests and non-interactive paths.
    pub events: Option<tokio::sync::mpsc::Sender<crate::agent::AgentEvent>>,
}

impl ToolContext {
    /// Construct a fresh context with an empty session. Most callers want
    /// `Agent::tool_context()` instead so the session is shared across turns.
    pub fn new(cwd: std::path::PathBuf, approval: ApprovalMode) -> Self {
        Self {
            cwd,
            approval,
            require_approval: false,
            auto_approve_edits: false,
            non_interactive: false,
            session: Arc::new(Mutex::new(SessionState::default())),
            config: crate::config::Config::default(),
            cwd_override: Arc::new(Mutex::new(None)),
            events: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Auto-approve everything (dangerous).
    Auto,
    /// Auto-approve read-only ops, require approval for writes/shell.
    #[default]
    OnRequest,
    /// Prompt for every action.
    Manual,
    /// Read-only execution. Tools that are not `is_read_only()` are rejected
    /// before they run; the model receives an error so it can adjust the
    /// plan instead of stalling.
    Plan,
}

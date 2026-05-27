pub mod ask;
pub mod fs;
pub mod search;
pub mod shell;
pub mod todo;
pub mod web;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use crate::openai::{Tool, ToolFunctionDef};

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
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String>;

    fn definition(&self) -> Tool {
        Tool::Function(ToolFunctionDef {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
            strict: true,
        })
    }
}

/// Status of a single todo item.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// One entry in the session todo list. Mirrors Claude Code's TodoWrite shape
/// closely so existing prompts and skills transfer.
#[derive(Debug, Clone, Serialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub active_form: String,
}

/// Status of a background shell command. Returned as part of every
/// `bash_output` poll so the model can tell when a job has finished.
#[derive(Debug, Clone)]
pub enum BgStatus {
    Running,
    Exited(i32),
    Killed,
    Error(String),
}

impl BgStatus {
    pub fn label(&self) -> String {
        match self {
            BgStatus::Running => "running".into(),
            BgStatus::Exited(c) => format!("exited({c})"),
            BgStatus::Killed => "killed".into(),
            BgStatus::Error(e) => format!("error({e})"),
        }
    }
    pub fn is_terminal(&self) -> bool {
        !matches!(self, BgStatus::Running)
    }
}

/// Handle to a background shell command spawned by `run_shell {run_in_background: true}`.
/// Lives inside `SessionState.background_shells` so the model can later poll
/// output via `bash_output` or terminate via `kill_shell`.
#[derive(Debug)]
pub struct BackgroundShellState {
    pub command: String,
    pub started_at_ms: u64,
    pub stdout: Mutex<Vec<u8>>,
    pub stderr: Mutex<Vec<u8>>,
    pub status: Mutex<BgStatus>,
    /// Read cursors so successive `bash_output` calls only return new bytes.
    pub stdout_cursor: Mutex<usize>,
    pub stderr_cursor: Mutex<usize>,
    /// `Some` while the child is alive; `None` after termination or kill.
    pub kill_tx: Mutex<Option<oneshot::Sender<()>>>,
}

/// Per-session mutable state that tools can read or write. Lives behind an
/// Arc<Mutex<>> so it survives across turns and across concurrent tool calls.
#[derive(Debug, Default)]
pub struct SessionState {
    pub todos: Vec<TodoItem>,
    pub background_shells: HashMap<String, Arc<BackgroundShellState>>,
}

/// Per-call execution context: working directory, approval policy, and a
/// handle to mutable session state (todos, …).
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: std::path::PathBuf,
    pub approval: ApprovalMode,
    pub session: Arc<Mutex<SessionState>>,
}

impl ToolContext {
    /// Construct a fresh context with an empty session. Most callers want
    /// `Agent::tool_context()` instead so the session is shared across turns.
    pub fn new(cwd: std::path::PathBuf, approval: ApprovalMode) -> Self {
        Self {
            cwd,
            approval,
            session: Arc::new(Mutex::new(SessionState::default())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Auto-approve everything (dangerous).
    Auto,
    /// Auto-approve read-only ops, require approval for writes/shell.
    OnRequest,
    /// Prompt for every action.
    Manual,
    /// Read-only execution. Tools that are not `is_read_only()` are rejected
    /// before they run; the model receives an error so it can adjust the
    /// plan instead of stalling. Mirrors Claude Code's Plan mode.
    Plan,
}

impl Default for ApprovalMode {
    fn default() -> Self {
        Self::OnRequest
    }
}

pub struct Registry {
    tools: Vec<Box<dyn BuiltinTool>>,
}

impl Registry {
    pub fn standard() -> Self {
        Self {
            tools: vec![
                Box::new(fs::ReadFile),
                Box::new(fs::WriteFile),
                Box::new(fs::EditFile),
                Box::new(fs::MultiEdit),
                Box::new(fs::ListDir),
                Box::new(search::Grep),
                Box::new(search::Glob),
                Box::new(shell::RunShell),
                Box::new(shell::BashOutput),
                Box::new(shell::KillShell),
                Box::new(todo::TodoWrite),
                Box::new(web::WebFetch),
                Box::new(ask::AskUserQuestion),
            ],
        }
    }

    pub fn definitions(&self) -> Vec<Tool> {
        self.tools.iter().map(|t| t.definition()).collect()
    }

    pub fn find(&self, name: &str) -> Option<&dyn BuiltinTool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|b| b.as_ref())
    }

    /// Append a tool to the registry. Used by `Agent::load_mcp` to register
    /// tools discovered from MCP servers after the standard built-ins.
    pub fn add(&mut self, tool: Box<dyn BuiltinTool>) {
        self.tools.push(tool);
    }
}

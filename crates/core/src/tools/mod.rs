pub mod ask;
pub mod dispatch;
pub mod fs;
pub mod notebook;
pub mod search;
pub mod shell;
pub mod skill;
pub mod todo;
pub mod web;

/// Deserialize a tool's `Value` arguments into the tool's typed struct,
/// prefixing the error with `tool <name>` so the model receives an actionable
/// hint instead of a bare serde message.
pub fn parse_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: serde_json::Value,
) -> anyhow::Result<T> {
    serde_json::from_value::<T>(args)
        .map_err(|e| anyhow::anyhow!("tool `{tool}` argument schema mismatch: {e}"))
}

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
    async fn compute_preview(&self, _args: &Value, _ctx: &ToolContext) -> Option<String> {
        None
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

#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub path: std::path::PathBuf,
    pub original_content: Option<String>,
    /// Mtime snapshot captured immediately after the tool wrote the file.
    /// Compared against the current mtime at undo time — if the file has
    /// been touched in between (user edited it externally, another tool,
    /// an editor save) we refuse to restore so the user's manual changes
    /// are not silently overwritten. `None` disables the check.
    pub post_edit_mtime: Option<std::time::SystemTime>,
}

#[derive(Debug, Default)]
pub struct SessionState {
    pub todos: Vec<TodoItem>,
    pub background_shells: HashMap<String, Arc<BackgroundShellState>>,
    pub undo_stack: std::collections::VecDeque<UndoEntry>,
}

impl SessionState {
    pub fn push_undo_entry(&mut self, entry: UndoEntry) {
        const MAX_UNDO: usize = 32;
        if self.undo_stack.len() >= MAX_UNDO {
            self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(entry);
    }
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
    /// plan instead of stalling. Mirrors Claude Code's Plan mode.
    Plan,
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
                Box::new(fs::UndoLastEdit),
                Box::new(fs::ListDir),
                Box::new(search::Grep),
                Box::new(search::Glob),
                Box::new(shell::RunShell),
                Box::new(shell::BashOutput),
                Box::new(shell::KillShell),
                Box::new(todo::TodoWrite),
                Box::new(web::WebFetch),
                Box::new(web::WebSearch),
                Box::new(notebook::NotebookEdit),
                Box::new(skill::LoadSkill),
                Box::new(ask::AskUserQuestion),
                Box::new(dispatch::DispatchAgent),
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

    /// Build a registry that contains only the named built-in tools.
    ///
    /// - An empty list, or one containing `"*"`, returns `Self::standard()`
    ///   minus `dispatch_agent` (subagents must not recurse into themselves).
    /// - Unknown names are silently skipped — the caller (typically the
    ///   `dispatch_agent` tool) is expected to surface a useful error if the
    ///   subagent file references a non-existent tool.
    /// - `dispatch_agent` is always stripped so a sub-agent can never spawn
    ///   another sub-agent (avoids unbounded fan-out and recursive cost).
    pub fn filtered(allowed: &[String]) -> Self {
        let wildcard = allowed.is_empty() || allowed.iter().any(|t| t == "*");
        if wildcard {
            let mut s = Self::standard();
            s.tools.retain(|t| t.name() != "dispatch_agent");
            return s;
        }
        let mut tools: Vec<Box<dyn BuiltinTool>> = Vec::new();
        for name in allowed {
            let Some(canon) = canonical_tool_name(name) else {
                tracing::warn!(tool = %name, "subagent referenced unknown tool; skipping");
                continue;
            };
            // `dispatch_agent` (and its Claude Code alias `Task`) canonicalise
            // to the dispatch tool, which is always stripped so sub-agents
            // cannot recurse.
            if canon == "dispatch_agent" {
                continue;
            }
            let tool: Box<dyn BuiltinTool> = match canon {
                "read_file" => Box::new(fs::ReadFile),
                "write_file" => Box::new(fs::WriteFile),
                "edit_file" => Box::new(fs::EditFile),
                "multi_edit" => Box::new(fs::MultiEdit),
                "undo_last_edit" => Box::new(fs::UndoLastEdit),
                "list_dir" => Box::new(fs::ListDir),
                "grep" => Box::new(search::Grep),
                "glob" => Box::new(search::Glob),
                "run_shell" => Box::new(shell::RunShell),
                "bash_output" => Box::new(shell::BashOutput),
                "kill_shell" => Box::new(shell::KillShell),
                "todo_write" => Box::new(todo::TodoWrite),
                "web_fetch" => Box::new(web::WebFetch),
                "web_search" => Box::new(web::WebSearch),
                "notebook_edit" => Box::new(notebook::NotebookEdit),
                "skill" => Box::new(skill::LoadSkill),
                "ask_user_question" => Box::new(ask::AskUserQuestion),
                _ => continue,
            };
            tools.push(tool);
        }
        Self { tools }
    }

    /// Append a tool to the registry. Used by `Agent::load_mcp` to register
    /// tools discovered from MCP servers after the standard built-ins.
    pub fn add(&mut self, tool: Box<dyn BuiltinTool>) {
        self.tools.push(tool);
    }
}

/// Canonicalise a tool name from a sub-agent's `tools:` whitelist to an
/// opencli built-in name. Accepts both opencli's snake_case names and Claude
/// Code's PascalCase names (so a `~/.claude/agents/*.md` file with
/// `tools: ["Read", "Grep", "Bash"]` resolves correctly). Returns `None` for
/// names with no opencli equivalent. `Task` maps to `dispatch_agent`, which
/// the caller always strips.
fn canonical_tool_name(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "read_file" | "read" => Some("read_file"),
        "write_file" | "write" => Some("write_file"),
        "edit_file" | "edit" => Some("edit_file"),
        "multi_edit" | "multiedit" => Some("multi_edit"),
        "undo_last_edit" => Some("undo_last_edit"),
        "list_dir" | "ls" => Some("list_dir"),
        "grep" => Some("grep"),
        "glob" => Some("glob"),
        "run_shell" | "bash" | "shell" => Some("run_shell"),
        "bash_output" => Some("bash_output"),
        "kill_shell" => Some("kill_shell"),
        "todo_write" | "todowrite" => Some("todo_write"),
        "web_fetch" | "webfetch" => Some("web_fetch"),
        "web_search" | "websearch" => Some("web_search"),
        "notebook_edit" | "notebookedit" => Some("notebook_edit"),
        "skill" => Some("skill"),
        "ask_user_question" | "askuserquestion" => Some("ask_user_question"),
        "dispatch_agent" | "task" => Some("dispatch_agent"),
        _ => None,
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn names(reg: &Registry) -> Vec<&'static str> {
        reg.tools.iter().map(|t| t.name()).collect()
    }

    #[test]
    fn filtered_maps_claude_code_tool_names() {
        // A Claude Code agent whitelist: PascalCase names + `Task`.
        let reg = Registry::filtered(&["Read".into(), "Grep".into(), "Bash".into(), "Task".into()]);
        let n = names(&reg);
        assert!(n.contains(&"read_file"));
        assert!(n.contains(&"grep"));
        assert!(n.contains(&"run_shell"));
        // Task → dispatch_agent, which is always stripped.
        assert!(!n.contains(&"dispatch_agent"));
        assert_eq!(n.len(), 3);
    }

    #[test]
    fn filtered_skips_unknown_names() {
        let reg = Registry::filtered(&["Read".into(), "TotallyMadeUp".into()]);
        assert_eq!(names(&reg), vec!["read_file"]);
    }

    #[test]
    fn wildcard_includes_skill_but_not_dispatch() {
        let reg = Registry::filtered(&[]);
        let n = names(&reg);
        assert!(n.contains(&"skill"));
        assert!(!n.contains(&"dispatch_agent"));
    }

    #[test]
    fn standard_includes_skill_and_dispatch() {
        let n = names(&Registry::standard());
        assert!(n.contains(&"skill"));
        assert!(n.contains(&"dispatch_agent"));
    }
}

pub mod ask;
pub mod dispatch;
pub mod fs;
pub mod goal;
pub mod lsp;
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

pub fn deserialize_bool<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        parse_optional_bool_value(Some(serde_json::Value::deserialize(deserializer)?))?
            .unwrap_or(false),
    )
}

pub fn deserialize_optional_bool<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    parse_optional_bool_value(value)
}

pub fn deserialize_optional_usize<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let Some(n) = parse_optional_u64_value(value)? else {
        return Ok(None);
    };
    usize::try_from(n)
        .map(Some)
        .map_err(|_| serde::de::Error::custom("integer is too large for this platform"))
}

pub fn deserialize_optional_u64<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_optional_u64_value(value)
}

pub fn deserialize_optional_string_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    serde_json::Value::String(s) => {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                        }
                    }
                    _ => return Err(serde::de::Error::custom("expected string array")),
                }
            }
            Ok(if out.is_empty() { None } else { Some(out) })
        }
        serde_json::Value::String(s) => {
            let out = split_string_list(&s);
            Ok(if out.is_empty() { None } else { Some(out) })
        }
        _ => Err(serde::de::Error::custom(
            "expected string, string array, or null",
        )),
    }
}

fn parse_optional_bool_value<E: serde::de::Error>(
    value: Option<serde_json::Value>,
) -> std::result::Result<Option<bool>, E> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Bool(b) => Ok(Some(b)),
        serde_json::Value::Number(n) => match n.as_u64() {
            Some(0) => Ok(Some(false)),
            Some(1) => Ok(Some(true)),
            _ => Err(E::custom("expected boolean or 0/1")),
        },
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "null" => Ok(None),
            "true" | "1" | "yes" => Ok(Some(true)),
            "false" | "0" | "no" => Ok(Some(false)),
            _ => Err(E::custom("expected boolean string")),
        },
        _ => Err(E::custom("expected boolean, boolean string, 0/1, or null")),
    }
}

fn parse_optional_u64_value<E: serde::de::Error>(
    value: serde_json::Value,
) -> std::result::Result<Option<u64>, E> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| E::custom("expected a non-negative integer")),
        serde_json::Value::String(s) => match s.trim() {
            "" | "null" => Ok(None),
            trimmed => trimmed
                .parse::<u64>()
                .map(Some)
                .map_err(|_| E::custom("expected a non-negative integer")),
        },
        _ => Err(E::custom("expected an integer, integer string, or null")),
    }
}

fn split_string_list(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{oneshot, Mutex};

use crate::openai::{Tool, ToolFunctionDef};

/// OpenAI strict function schemas require every object property to be listed in
/// `required`. Fields that are optional at the Rust boundary are represented as
/// nullable instead of omitted.
fn strict_parameters_schema(mut schema: Value) -> Value {
    stricten_schema_object(&mut schema);
    schema
}

fn stricten_schema_object(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(values) = obj.get_mut(key).and_then(Value::as_array_mut) {
            for value in values {
                stricten_schema_object(value);
            }
        }
    }
    if let Some(items) = obj.get_mut("items") {
        stricten_schema_object(items);
    }

    let is_object =
        schema_type_contains(obj.get("type"), "object") || obj.contains_key("properties");
    if !is_object {
        return;
    }

    let originally_required = obj
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let property_names = obj
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    if let Some(properties) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, property) in properties {
            if !originally_required.contains(name) {
                mark_schema_nullable(property);
            }
            stricten_schema_object(property);
        }
    }

    obj.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    obj.entry("additionalProperties".to_string())
        .or_insert(Value::Bool(false));
}

fn schema_type_contains(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::String(s)) => s == expected,
        Some(Value::Array(items)) => items.iter().any(|item| item.as_str() == Some(expected)),
        _ => false,
    }
}

fn mark_schema_nullable(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    match obj.get_mut("type") {
        Some(Value::String(ty)) if ty != "null" => {
            let ty = Value::String(std::mem::take(ty));
            obj.insert(
                "type".to_string(),
                Value::Array(vec![ty, Value::String("null".into())]),
            );
        }
        Some(Value::Array(types)) => {
            if !types.iter().any(|ty| ty.as_str() == Some("null")) {
                types.push(Value::String("null".into()));
            }
        }
        Some(_) => {}
        None => {
            obj.insert(
                "anyOf".to_string(),
                Value::Array(vec![
                    Value::Object(obj.clone()),
                    serde_json::json!({"type": "null"}),
                ]),
            );
        }
    }

    if let Some(values) = obj.get_mut("enum").and_then(Value::as_array_mut) {
        if !values.iter().any(Value::is_null) {
            values.push(Value::Null);
        }
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

    fn definition(&self) -> Tool {
        Tool::Function(ToolFunctionDef {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: strict_parameters_schema(self.parameters_schema()),
            strict: true,
        })
    }
}

/// Status of a single todo item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    pub fn parse(s: &str) -> Option<Self> {
        let normalized = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match normalized.as_str() {
            "pending" | "todo" | "open" | "not_started" => Some(Self::Pending),
            "in_progress" | "inprogress" | "active" | "doing" | "started" => Some(Self::InProgress),
            "completed" | "complete" | "done" | "finished" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// One entry in the session todo list. Mirrors Claude Code's TodoWrite shape
/// closely so existing prompts and skills transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
    pub active_form: String,
    /// Optional stable id used to express dependencies between items. `None`
    /// for a plain flat list. Skipped on the wire when absent so existing
    /// (idless) session records round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Ids of items that must reach `completed` before this one can start.
    /// Empty for an unconstrained item; lets the model plan a DAG instead of a
    /// flat list. Skipped on the wire when empty for round-trip parity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
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
    pub original_content: Option<Vec<u8>>,
    /// Mtime snapshot captured immediately after the tool wrote the file.
    /// Compared against the current mtime at undo time — if the file has
    /// been touched in between (user edited it externally, another tool,
    /// an editor save) we refuse to restore so the user's manual changes
    /// are not silently overwritten. `None` disables the check.
    pub post_edit_mtime: Option<std::time::SystemTime>,
    /// File size snapshot captured alongside `post_edit_mtime`. Compared too
    /// at undo time so a same-second external edit (which a coarse 1s-resolution
    /// mtime can't distinguish) is still caught whenever it changes the length.
    pub post_edit_size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WorktreeState {
    pub original_cwd: std::path::PathBuf,
    pub repo_root: std::path::PathBuf,
    pub worktree_path: std::path::PathBuf,
    pub branch: String,
    pub base_head: String,
}

#[derive(Debug, Default)]
pub struct SessionState {
    pub todos: Vec<TodoItem>,
    pub background_shells: HashMap<String, Arc<BackgroundShellState>>,
    pub undo_stack: std::collections::VecDeque<UndoEntry>,
    /// Canonical paths read this session, keyed exactly as `fs::resolve`
    /// produces them. Powers Claude Code's read-before-write safety:
    /// `write_file` refuses to overwrite, and `edit_file`/`multi_edit` refuse
    /// to touch, a file that was never read — so the model can't clobber
    /// content it has not seen. A successful write/edit also records the path.
    pub read_files: std::collections::HashSet<std::path::PathBuf>,
    /// `(mtime, size)` captured for each file when it was last read or written
    /// this session. Lets `edit_file`/`multi_edit`/`write_file` force a re-read
    /// when the file changed on disk since the model last saw it (the user's
    /// editor, another tool, or another process touched it) — editing a stale
    /// view would either fail to match or apply against bytes the model never
    /// saw. Refreshed after each successful write/edit so back-to-back edits to
    /// a file the model itself just changed don't spuriously demand a re-read.
    /// Runtime only (not part of `SessionRecord`); a resumed session starts
    /// empty and falls back to the plain read-once check until the file is read
    /// again.
    pub read_file_meta:
        std::collections::HashMap<std::path::PathBuf, (Option<std::time::SystemTime>, Option<u64>)>,
    /// Worktree created by this session via `enter_worktree`. Exit/remove tools
    /// are scoped to this state so opencli never cleans up a user-created worktree.
    pub worktree: Option<WorktreeState>,
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

#[cfg(test)]
mod scalar_arg_tests {
    use super::TodoStatus;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize)]
    struct Args {
        #[serde(default, deserialize_with = "crate::tools::deserialize_bool")]
        flag: bool,
        #[serde(default, deserialize_with = "crate::tools::deserialize_optional_bool")]
        maybe_flag: Option<bool>,
        #[serde(default, deserialize_with = "crate::tools::deserialize_optional_usize")]
        count: Option<usize>,
        #[serde(default, deserialize_with = "crate::tools::deserialize_optional_u64")]
        bytes: Option<u64>,
        #[serde(
            default,
            deserialize_with = "crate::tools::deserialize_optional_string_vec"
        )]
        list: Option<Vec<String>>,
    }

    #[test]
    fn semantic_scalar_deserializers_accept_common_model_spellings() {
        let args: Args = serde_json::from_value(json!({
            "flag": "yes",
            "maybe_flag": 0,
            "count": "42",
            "bytes": "9000",
            "list": "docs.rs, crates.io example.com"
        }))
        .unwrap();

        assert!(args.flag);
        assert_eq!(args.maybe_flag, Some(false));
        assert_eq!(args.count, Some(42));
        assert_eq!(args.bytes, Some(9000));
        assert_eq!(
            args.list,
            Some(vec![
                "docs.rs".to_string(),
                "crates.io".to_string(),
                "example.com".to_string()
            ])
        );
    }

    #[test]
    fn semantic_scalar_deserializers_treat_blank_and_null_as_none() {
        let args: Args = serde_json::from_value(json!({
            "flag": null,
            "maybe_flag": "",
            "count": "null",
            "bytes": null,
            "list": ""
        }))
        .unwrap();

        assert!(!args.flag);
        assert_eq!(args.maybe_flag, None);
        assert_eq!(args.count, None);
        assert_eq!(args.bytes, None);
        assert_eq!(args.list, None);
    }

    #[test]
    fn todo_status_parse_accepts_common_model_spellings() {
        assert_eq!(TodoStatus::parse("pending"), Some(TodoStatus::Pending));
        assert_eq!(TodoStatus::parse("todo"), Some(TodoStatus::Pending));
        assert_eq!(
            TodoStatus::parse("in progress"),
            Some(TodoStatus::InProgress)
        );
        assert_eq!(TodoStatus::parse("active"), Some(TodoStatus::InProgress));
        assert_eq!(TodoStatus::parse("done"), Some(TodoStatus::Completed));
        assert_eq!(TodoStatus::parse("complete"), Some(TodoStatus::Completed));
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
    /// plan instead of stalling. Mirrors Claude Code's Plan mode.
    Plan,
}

pub struct Registry {
    tools: Vec<Box<dyn BuiltinTool>>,
    /// Names of tools withheld from `definitions()` until `tool_search` loads
    /// them. Empty unless `enable_tool_search()` has run (many MCP tools).
    deferred: std::collections::HashSet<String>,
    /// Deferred tools the model has loaded via `tool_search`. Shared with the
    /// `tool_search` tool so its `execute` records activations that
    /// `definitions()` then reflects on the next turn.
    activated: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl Registry {
    pub fn standard() -> Self {
        Self {
            deferred: std::collections::HashSet::new(),
            activated: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
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
                Box::new(goal::GoalUpdate),
                Box::new(web::WebFetch),
                Box::new(web::WebSearch),
                Box::new(lsp::Lsp),
                Box::new(notebook::NotebookEdit),
                Box::new(plan::EnterPlanMode),
                Box::new(plan::ExitPlanMode),
                Box::new(worktree::EnterWorktree),
                Box::new(worktree::ExitWorktree),
                Box::new(skill::LoadSkill),
                Box::new(ask::AskUserQuestion),
                Box::new(wait::Wait),
                Box::new(dispatch::DispatchAgent),
            ],
        }
    }

    pub fn definitions(&self) -> Vec<Tool> {
        if self.deferred.is_empty() {
            return self.tools.iter().map(|t| t.definition()).collect();
        }
        let activated = self.activated.lock().unwrap();
        self.tools
            .iter()
            .filter(|t| {
                let n = t.name();
                !self.deferred.contains(n) || activated.contains(n)
            })
            .map(|t| t.definition())
            .collect()
    }

    pub fn find(&self, name: &str) -> Option<&dyn BuiltinTool> {
        let trimmed = name.trim();
        self.tools
            .iter()
            .find(|t| t.name() == trimmed)
            .or_else(|| {
                let canon = canonical_tool_name(trimmed)?;
                self.tools.iter().find(|t| t.name() == canon)
            })
            .map(|b| b.as_ref())
    }

    /// Build a registry that contains only the named built-in tools.
    ///
    /// - An empty list, or one containing `"*"`, returns `Self::standard()`
    ///   minus tools that cannot run coherently inside subagents.
    /// - Unknown names are silently skipped — the caller (typically the
    ///   `dispatch_agent` tool) is expected to surface a useful error if the
    ///   subagent file references a non-existent tool.
    /// - `dispatch_agent` is always stripped so a sub-agent can never spawn
    ///   another sub-agent (avoids unbounded fan-out and recursive cost).
    /// - `ask_user_question` is always stripped because sub-agents have no UI
    ///   channel for a follow-up answer from the user.
    pub fn filtered(allowed: &[String]) -> Self {
        let wildcard = allowed.is_empty() || allowed.iter().any(|t| t == "*");
        if wildcard {
            let mut s = Self::standard();
            s.tools.retain(|t| {
                !matches!(
                    t.name(),
                    "dispatch_agent"
                        | "ask_user_question"
                        | "goal_update"
                        | "enter_plan_mode"
                        | "exit_plan_mode"
                )
            });
            return s;
        }
        let mut tools: Vec<Box<dyn BuiltinTool>> = Vec::new();
        let mut seen: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        for name in allowed {
            let Some(canon) = canonical_tool_name(name) else {
                tracing::warn!(tool = %name, "subagent referenced unknown tool; skipping");
                continue;
            };
            // Tools that need parent-level orchestration are stripped from
            // sub-agents even when explicitly whitelisted.
            if matches!(
                canon,
                "dispatch_agent"
                    | "ask_user_question"
                    | "goal_update"
                    | "enter_plan_mode"
                    | "exit_plan_mode"
            ) {
                continue;
            }
            // Dedup: aliases that canonicalise to the same built-in (e.g.
            // `Read`+`read_file`, `Bash`+`shell`) would otherwise push two boxes
            // with the same name; `definitions()` would then emit a duplicate
            // function name and the provider rejects the request with a 400.
            if !seen.insert(canon) {
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
                "goal_update" => Box::new(goal::GoalUpdate),
                "web_fetch" => Box::new(web::WebFetch),
                "web_search" => Box::new(web::WebSearch),
                "lsp" => Box::new(lsp::Lsp),
                "notebook_edit" => Box::new(notebook::NotebookEdit),
                "enter_plan_mode" => Box::new(plan::EnterPlanMode),
                "exit_plan_mode" => Box::new(plan::ExitPlanMode),
                "enter_worktree" => Box::new(worktree::EnterWorktree),
                "exit_worktree" => Box::new(worktree::ExitWorktree),
                "skill" => Box::new(skill::LoadSkill),
                "ask_user_question" => Box::new(ask::AskUserQuestion),
                "wait" => Box::new(wait::Wait),
                _ => continue,
            };
            tools.push(tool);
        }
        Self {
            tools,
            deferred: std::collections::HashSet::new(),
            activated: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Append a tool to the registry. Used by `Agent::load_mcp` to register
    /// tools discovered from MCP servers after the standard built-ins.
    pub fn add(&mut self, tool: Box<dyn BuiltinTool>) {
        self.tools.push(tool);
    }

    /// Turn on progressive tool disclosure: withhold every MCP tool's schema
    /// from `definitions()` and register a `tool_search` tool that loads them
    /// on demand. Called by `Agent::load_mcp` when the MCP tool count crosses
    /// the defer threshold. The withheld tools are advertised to the model via
    /// the system-prompt manifest (`deferred_summaries`).
    pub fn enable_tool_search(&mut self) {
        let catalog: Vec<tool_search::DeferredToolInfo> = self
            .tools
            .iter()
            .filter(|t| t.name().starts_with("mcp__"))
            .map(|t| tool_search::DeferredToolInfo {
                name: t.name().to_string(),
                description: t.description().to_string(),
                schema: t.parameters_schema(),
            })
            .collect();
        if catalog.is_empty() {
            return;
        }
        for info in &catalog {
            self.deferred.insert(info.name.clone());
        }
        self.tools.push(Box::new(tool_search::ToolSearch::new(
            catalog,
            self.activated.clone(),
        )));
    }

    /// `(name, description)` for every deferred tool, for building the
    /// system-prompt manifest. Empty unless `enable_tool_search()` ran.
    pub fn deferred_summaries(&self) -> Vec<(&str, &str)> {
        self.tools
            .iter()
            .filter(|t| self.deferred.contains(t.name()))
            .map(|t| (t.name(), t.description()))
            .collect()
    }
}

/// Canonicalise a tool name from a sub-agent's `tools:` whitelist to an
/// opencli built-in name. Accepts both opencli's snake_case names and Claude
/// Code's PascalCase names (so a `~/.claude/agents/*.md` file with
/// `tools: ["Read", "Grep", "Bash"]` resolves correctly). Returns `None` for
/// names with no opencli equivalent. `Task` maps to `dispatch_agent`, which
/// the caller always strips.
fn canonical_tool_name(name: &str) -> Option<&'static str> {
    let lowered = name.trim().to_ascii_lowercase();
    let name = strip_tool_namespace(&lowered);
    match name {
        "read_file" | "readfile" | "read" => Some("read_file"),
        "write_file" | "writefile" | "write" => Some("write_file"),
        "edit_file" | "editfile" | "edit" => Some("edit_file"),
        "multi_edit" | "multiedit" => Some("multi_edit"),
        "undo_last_edit" | "undolastedit" => Some("undo_last_edit"),
        "list_dir" | "listdir" | "list_directory" | "listdirectory" | "listfiles" | "ls" => {
            Some("list_dir")
        }
        "grep" => Some("grep"),
        "glob" => Some("glob"),
        "run_shell" | "runshell" | "bash" | "shell" | "powershell" | "pwsh" => Some("run_shell"),
        "bash_output" | "bashoutput" => Some("bash_output"),
        "kill_shell" | "killshell" => Some("kill_shell"),
        "todo_write" | "todowrite" => Some("todo_write"),
        "goal_update" | "goalupdate" | "update_goal" | "updategoal" | "goal_status"
        | "goalstatus" => Some("goal_update"),
        "enter_plan_mode" | "enterplanmode" | "enter_plan" | "enterplan" | "plan_mode"
        | "planmode" => Some("enter_plan_mode"),
        "exit_plan_mode" | "exitplanmode" | "exit_plan" | "exitplan" => Some("exit_plan_mode"),
        "enter_worktree" | "enterworktree" | "worktree_enter" | "worktreeenter" => {
            Some("enter_worktree")
        }
        "exit_worktree" | "exitworktree" | "worktree_exit" | "worktreeexit" => {
            Some("exit_worktree")
        }
        "web_fetch" | "webfetch" => Some("web_fetch"),
        "web_search" | "websearch" => Some("web_search"),
        "lsp" | "lsptool" => Some("lsp"),
        "notebook_edit" | "notebookedit" => Some("notebook_edit"),
        "skill" | "load_skill" | "loadskill" => Some("skill"),
        "ask_user_question" | "askuserquestion" => Some("ask_user_question"),
        "wait" | "sleep" => Some("wait"),
        "dispatch_agent" | "dispatchagent" | "agent" | "task" => Some("dispatch_agent"),
        _ => None,
    }
}

fn strip_tool_namespace(name: &str) -> &str {
    for prefix in ["functions.", "function.", "tools.", "tool.", "builtin."] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn names(reg: &Registry) -> Vec<&'static str> {
        reg.tools.iter().map(|t| t.name()).collect()
    }

    /// Function names actually advertised to the provider (post-deferral).
    fn def_names(reg: &Registry) -> Vec<String> {
        reg.definitions()
            .into_iter()
            .filter_map(|t| match t {
                crate::openai::models::Tool::Function(f) => Some(f.name),
                _ => None,
            })
            .collect()
    }

    /// Minimal fake MCP tool so registry tests don't need a live server.
    struct FakeMcp(&'static str, &'static str);
    #[async_trait::async_trait]
    impl BuiltinTool for FakeMcp {
        fn name(&self) -> &'static str {
            self.0
        }
        fn description(&self) -> &'static str {
            self.1
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
        }
        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> Result<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn enable_tool_search_defers_mcp_until_activated() {
        let mut reg = Registry::standard();
        reg.add(Box::new(FakeMcp(
            "mcp__gh__create_issue",
            "Open a GitHub issue",
        )));
        reg.add(Box::new(FakeMcp(
            "mcp__gh__list_pulls",
            "List pull requests",
        )));
        reg.enable_tool_search();

        let defs = def_names(&reg);
        // Built-ins still advertised; tool_search added; MCP tools withheld.
        assert!(defs.contains(&"read_file".to_string()));
        assert!(defs.contains(&"tool_search".to_string()));
        assert!(!defs.contains(&"mcp__gh__create_issue".to_string()));
        assert!(!defs.contains(&"mcp__gh__list_pulls".to_string()));
        // But they are advertised in the manifest summaries.
        let summary_names: Vec<&str> = reg
            .deferred_summaries()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert!(summary_names.contains(&"mcp__gh__create_issue"));

        // Activating one (as `tool_search` would) surfaces only that one.
        reg.activated
            .lock()
            .unwrap()
            .insert("mcp__gh__create_issue".to_string());
        let defs = def_names(&reg);
        assert!(defs.contains(&"mcp__gh__create_issue".to_string()));
        assert!(!defs.contains(&"mcp__gh__list_pulls".to_string()));
    }

    #[test]
    fn no_deferral_keeps_all_tools_callable() {
        // Without enable_tool_search, every added MCP tool is directly callable
        // and no tool_search appears.
        let mut reg = Registry::standard();
        reg.add(Box::new(FakeMcp("mcp__x__a", "a")));
        let defs = def_names(&reg);
        assert!(defs.contains(&"mcp__x__a".to_string()));
        assert!(!defs.contains(&"tool_search".to_string()));
        assert!(reg.deferred_summaries().is_empty());
    }

    #[test]
    fn filtered_maps_claude_code_tool_names() {
        // A Claude Code agent whitelist: PascalCase names + `Task`/`Agent`.
        let reg = Registry::filtered(&[
            "Read".into(),
            "Grep".into(),
            "Bash".into(),
            "Task".into(),
            "Agent".into(),
        ]);
        let n = names(&reg);
        assert!(n.contains(&"read_file"));
        assert!(n.contains(&"grep"));
        assert!(n.contains(&"run_shell"));
        // Task/Agent -> dispatch_agent, which is always stripped.
        assert!(!n.contains(&"dispatch_agent"));
        assert_eq!(n.len(), 3);
    }

    #[test]
    fn filtered_skips_unknown_names() {
        let reg = Registry::filtered(&["Read".into(), "TotallyMadeUp".into()]);
        assert_eq!(names(&reg), vec!["read_file"]);
    }

    #[test]
    fn find_accepts_provider_aliases_for_builtin_tools() {
        let reg = Registry::standard();

        let cases = [
            (" Read ", "read_file"),
            ("ReadFile", "read_file"),
            ("Write", "write_file"),
            ("WriteFile", "write_file"),
            ("Edit", "edit_file"),
            ("EditFile", "edit_file"),
            ("MultiEdit", "multi_edit"),
            ("UndoLastEdit", "undo_last_edit"),
            ("ListDir", "list_dir"),
            ("ListDirectory", "list_dir"),
            ("Grep", "grep"),
            ("Glob", "glob"),
            ("Bash", "run_shell"),
            ("PowerShell", "run_shell"),
            ("pwsh", "run_shell"),
            ("RunShell", "run_shell"),
            ("BashOutput", "bash_output"),
            ("KillShell", "kill_shell"),
            ("TodoWrite", "todo_write"),
            ("GoalUpdate", "goal_update"),
            ("UpdateGoal", "goal_update"),
            ("functions.update_goal", "goal_update"),
            ("functions.GoalUpdate", "goal_update"),
            ("EnterPlanMode", "enter_plan_mode"),
            ("functions.EnterPlanMode", "enter_plan_mode"),
            ("ExitPlanMode", "exit_plan_mode"),
            ("functions.ExitPlanMode", "exit_plan_mode"),
            ("tool.BashOutput", "bash_output"),
            ("builtin.Agent", "dispatch_agent"),
            ("WebFetch", "web_fetch"),
            ("WebSearch", "web_search"),
            ("LSP", "lsp"),
            ("LspTool", "lsp"),
            ("NotebookEdit", "notebook_edit"),
            ("LoadSkill", "skill"),
            ("AskUserQuestion", "ask_user_question"),
            ("Agent", "dispatch_agent"),
            ("Task", "dispatch_agent"),
            ("DispatchAgent", "dispatch_agent"),
            ("goalupdate", "goal_update"),
            ("goalstatus", "goal_update"),
            ("enterplanmode", "enter_plan_mode"),
            ("exitplanmode", "exit_plan_mode"),
        ];

        for (alias, canonical) in cases {
            assert_eq!(reg.find(alias).unwrap().name(), canonical, "{alias}");
        }
    }

    #[test]
    fn wildcard_includes_skill_but_not_dispatch() {
        let reg = Registry::filtered(&[]);
        let n = names(&reg);
        assert!(n.contains(&"skill"));
        assert!(!n.contains(&"dispatch_agent"));
        assert!(!n.contains(&"ask_user_question"));
        assert!(!n.contains(&"goal_update"));
        assert!(!n.contains(&"enter_plan_mode"));
        assert!(!n.contains(&"exit_plan_mode"));
    }

    #[test]
    fn filtered_strips_user_prompt_tool_from_subagents() {
        let reg = Registry::filtered(&["ask_user_question".into(), "Read".into()]);
        assert_eq!(names(&reg), vec!["read_file"]);
    }

    #[test]
    fn standard_includes_skill_and_dispatch() {
        let n = names(&Registry::standard());
        assert!(n.contains(&"skill"));
        assert!(n.contains(&"dispatch_agent"));
        assert!(n.contains(&"goal_update"));
        assert!(n.contains(&"enter_plan_mode"));
        assert!(n.contains(&"exit_plan_mode"));
    }

    #[test]
    fn standard_tool_definitions_use_portable_function_names() {
        for def in Registry::standard().definitions() {
            let crate::openai::Tool::Function(f) = def else {
                continue;
            };
            assert!(
                !f.name.is_empty() && f.name.len() <= 64,
                "bad tool name length: {}",
                f.name
            );
            assert!(
                f.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')),
                "non-portable tool name: {}",
                f.name
            );
            assert!(
                f.parameters.get("type").and_then(|v| v.as_str()) == Some("object"),
                "tool schema root must be object: {}",
                f.name
            );
            assert!(
                f.parameters.get("additionalProperties").is_some(),
                "tool schema must state additionalProperties: {}",
                f.name
            );
            assert_openai_strict_object_schema(&f.parameters, &f.name);
        }
    }

    #[test]
    fn dispatch_schema_keeps_optional_fields_nullable_and_required() {
        let dispatch = Registry::standard()
            .find("dispatch_agent")
            .expect("dispatch_agent")
            .definition();
        let crate::openai::Tool::Function(f) = dispatch else {
            panic!("dispatch_agent must be a function tool");
        };
        let required = f.parameters["required"].as_array().expect("required array");
        for key in [
            "subagent_type",
            "prompt",
            "description",
            "model",
            "cwd",
            "plan_mode_required",
        ] {
            assert!(
                required.iter().any(|item| item == key),
                "dispatch_agent missing required key {key}"
            );
        }
        let props = f.parameters["properties"]
            .as_object()
            .expect("dispatch properties");
        assert_schema_type_contains(&props["cwd"], "null");
        assert_schema_type_contains(&props["model"], "null");
        assert_schema_type_contains(&props["description"], "null");
        assert_schema_type_contains(&props["plan_mode_required"], "null");
        assert_schema_type_contains(&props["plan_mode_required"], "boolean");
    }

    fn assert_openai_strict_object_schema(schema: &Value, label: &str) {
        if let Some(items) = schema.get("items") {
            assert_openai_strict_object_schema(items, &format!("{label}.items"));
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            if let Some(values) = schema.get(key).and_then(Value::as_array) {
                for (idx, value) in values.iter().enumerate() {
                    assert_openai_strict_object_schema(value, &format!("{label}.{key}[{idx}]"));
                }
            }
        }

        let is_object = schema_type_contains(schema.get("type"), "object")
            || schema.get("properties").is_some();
        if !is_object {
            return;
        }

        assert_eq!(
            schema.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "object schema must disable additional properties: {label}"
        );
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .expect("object schema must contain properties");
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .expect("object schema must contain required");
        for key in properties.keys() {
            assert!(
                required.iter().any(|item| item == key),
                "object schema required must include {label}.{key}"
            );
            assert_openai_strict_object_schema(&properties[key], &format!("{label}.{key}"));
        }
    }

    fn assert_schema_type_contains(schema: &Value, expected: &str) {
        assert!(
            schema_type_contains(schema.get("type"), expected),
            "schema {schema:?} does not contain type {expected}"
        );
    }
}

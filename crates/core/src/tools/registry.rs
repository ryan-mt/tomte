//! The tool `Registry`: the standard tool set, sub-agent filtering, MCP
//! deferral, and tool-name canonicalization. Split out of `tools`; logic
//! unchanged.

use crate::openai::Tool;

use super::{
    ask, decision, dispatch, fs, goal, lsp, memory, notebook, plan, search, shell, skill, todo,
    tool_search, wait, web, worktree, BuiltinTool,
};

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
                Box::new(memory::Memory),
                Box::new(decision::RecordDecision),
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

    /// Names of every built-in tool, for "did you mean" suggestions on an
    /// unknown tool call. Includes deferred tools so a typo of a not-yet-loaded
    /// tool still resolves to its real name.
    pub fn tool_names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
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
                "memory" => Box::new(memory::Memory),
                "notebook_edit" => Box::new(notebook::NotebookEdit),
                "enter_plan_mode" => Box::new(plan::EnterPlanMode),
                "exit_plan_mode" => Box::new(plan::ExitPlanMode),
                "enter_worktree" => Box::new(worktree::EnterWorktree),
                "exit_worktree" => Box::new(worktree::ExitWorktree),
                "skill" => Box::new(skill::LoadSkill),
                "ask_user_question" => Box::new(ask::AskUserQuestion),
                "record_decision" => Box::new(decision::RecordDecision),
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
/// tomte built-in name. Accepts both tomte's snake_case names and Claude
/// Code's PascalCase names (so a `~/.claude/agents/*.md` file with
/// `tools: ["Read", "Grep", "Bash"]` resolves correctly). Returns `None` for
/// names with no tomte equivalent. `Task` maps to `dispatch_agent`, which
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
        "memory" | "update_memory" | "updatememory" | "memories" => Some("memory"),
        "skill" | "load_skill" | "loadskill" => Some("skill"),
        "ask_user_question" | "askuserquestion" => Some("ask_user_question"),
        "record_decision" | "recorddecision" => Some("record_decision"),
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
mod tests;

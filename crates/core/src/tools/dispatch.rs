//! Sub-agent dispatch tool. The Claude Code analogue of `Task`.
//!
//! Given a `subagent_type` (matching a file in `~/.config/opencli/agents/`)
//! and a `prompt`, spins up a child `Agent` with a restricted tool registry,
//! drives one full turn (loops until the model produces final text), and
//! returns the final assistant text to the parent agent as the tool result.
//!
//! The child shares the parent's `cwd`, credential, and config (except the
//! model, which the subagent definition may override). Recursive dispatch
//! is blocked: the child registry never includes `dispatch_agent` itself.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ApprovalMode, BuiltinTool, Registry, ToolContext};
use crate::agent::{Agent, AgentEvent};
use crate::client::LlmClient;
use crate::subagent::{load_all, load_by_name, resolve_model_alias};

/// Process-wide sequence for unique sub-agent ids in the live fleet view.
static SUBAGENT_SEQ: AtomicU64 = AtomicU64::new(1);

const DEFAULT_SUBAGENT_TYPE: &str = "general-purpose";

/// Short, human label for the tool a sub-agent just started — surfaced in the
/// parent's fleet view so the user sees what each child is doing.
fn activity_label(tool: &str) -> &'static str {
    match tool {
        "read_file" => "reading files",
        "grep" => "searching",
        "glob" => "finding files",
        "list_dir" => "listing files",
        "run_shell" | "bash_output" => "running shell",
        "write_file" | "edit_file" | "multi_edit" => "editing",
        "web_fetch" | "web_search" => "browsing the web",
        "todo_write" => "planning",
        _ => "working",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_accept_subagent_type_camel_alias() {
        let args: DispatchArgs = super::super::parse_args(
            "dispatch_agent",
            json!({
                "subagentType": "code-explorer",
                "prompt": "Inspect the repo"
            }),
        )
        .unwrap();

        assert_eq!(args.subagent_type(), "code-explorer");
        assert_eq!(args.prompt, "Inspect the repo");
    }

    #[test]
    fn args_accept_claude_task_aliases() {
        let args: DispatchArgs = super::super::parse_args(
            "dispatch_agent",
            json!({
                "agent_type": "code-explorer",
                "instructions": "Inspect the repo",
                "description": "repo scan",
                "mode": "plan",
                "model": "sonnet",
                "directory": "."
            }),
        )
        .unwrap();

        assert_eq!(args.subagent_type(), "code-explorer");
        assert_eq!(args.prompt, "Inspect the repo");
        assert_eq!(args.description.as_deref(), Some("repo scan"));
        assert_eq!(args.model.as_deref(), Some("sonnet"));
        assert!(args.requires_plan_mode());
    }

    #[test]
    fn args_default_missing_subagent_to_general_purpose() {
        let args: DispatchArgs = super::super::parse_args(
            "dispatch_agent",
            json!({
                "prompt": "Inspect the repo"
            }),
        )
        .unwrap();

        assert_eq!(args.subagent_type(), DEFAULT_SUBAGENT_TYPE);
    }

    #[test]
    fn args_accept_plan_mode_required_alias() {
        let args: DispatchArgs = super::super::parse_args(
            "dispatch_agent",
            json!({
                "subagentType": "code-explorer",
                "prompt": "Inspect the repo",
                "planModeRequired": "yes"
            }),
        )
        .unwrap();

        assert!(args.plan_mode_required);
    }

    #[test]
    fn child_policy_forces_plan_when_parent_would_need_approval() {
        let mut ctx = ToolContext::new(std::env::temp_dir(), ApprovalMode::OnRequest);
        ctx.require_approval = true;
        assert!(child_requires_plan_mode(&ctx, false));

        ctx.auto_approve_edits = true;
        assert!(child_requires_plan_mode(&ctx, false));

        ctx.require_approval = false;
        ctx.auto_approve_edits = false;
        assert!(!child_requires_plan_mode(&ctx, false));

        ctx.approval = ApprovalMode::Auto;
        ctx.require_approval = true;
        assert!(!child_requires_plan_mode(&ctx, false));

        assert!(child_requires_plan_mode(&ctx, true));
    }
}

pub struct DispatchAgent;

#[derive(Deserialize)]
struct DispatchArgs {
    #[serde(
        default,
        alias = "subagentType",
        alias = "agent_type",
        alias = "agentType",
        alias = "agent",
        alias = "type"
    )]
    subagent_type: Option<String>,
    #[serde(
        alias = "task",
        alias = "instructions",
        alias = "instruction",
        alias = "input",
        alias = "message"
    )]
    prompt: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(
        default,
        alias = "working_dir",
        alias = "workingDir",
        alias = "directory",
        alias = "dir"
    )]
    cwd: Option<String>,
    #[serde(
        default,
        alias = "permission_mode",
        alias = "permissionMode",
        alias = "spawnMode"
    )]
    mode: Option<String>,
    #[serde(
        default,
        alias = "planModeRequired",
        alias = "plan_required",
        alias = "planRequired",
        deserialize_with = "super::deserialize_bool"
    )]
    plan_mode_required: bool,
}

impl DispatchArgs {
    fn subagent_type(&self) -> &str {
        self.subagent_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_SUBAGENT_TYPE)
    }

    fn requires_plan_mode(&self) -> bool {
        self.plan_mode_required
            || self
                .mode
                .as_deref()
                .map(mode_requires_plan)
                .unwrap_or(false)
    }

    fn child_cwd(&self, parent: &Path) -> Result<PathBuf> {
        let Some(raw) = self.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(parent.to_path_buf());
        };
        let path = PathBuf::from(raw);
        let path = if path.is_absolute() {
            path
        } else {
            parent.join(path)
        };
        let canonical = std::fs::canonicalize(&path).map_err(|e| {
            anyhow!(
                "dispatch_agent cwd `{}` could not be resolved: {e}",
                path.display()
            )
        })?;
        if !canonical.is_dir() {
            return Err(anyhow!(
                "dispatch_agent cwd `{}` is not a directory",
                canonical.display()
            ));
        }
        Ok(canonical)
    }
}

fn mode_requires_plan(mode: &str) -> bool {
    let normalized = mode.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    matches!(
        normalized.as_str(),
        "plan" | "plan_mode" | "planning" | "read_only" | "readonly"
    )
}

#[async_trait]
impl BuiltinTool for DispatchAgent {
    fn name(&self) -> &'static str {
        "dispatch_agent"
    }

    fn description(&self) -> &'static str {
        "Spawn a child sub-agent to handle a self-contained sub-task and return its final answer.\n\
\n\
When to use:\n\
- The work is large enough to warrant its own context (heavy exploration, multi-file research, broad refactor planning) and would crowd out the main conversation if done inline.\n\
- A specialised sub-agent (defined under ~/.config/opencli/agents/<name>.md) has a tighter tool whitelist and prompt than the main agent, e.g. a `code-explorer` that can only read+grep, or a `security-reviewer` that focuses on a checklist.\n\
- You want to run several independent read-only planning/research sub-tasks in parallel — issue multiple `dispatch_agent` calls with `plan_mode_required: true` in the same turn and they execute concurrently. Dispatches that may write are serialized by the host to avoid file races.\n\
\n\
When NOT to use:\n\
- Quick lookups you can do with one or two direct tool calls — the overhead of spinning up an agent is not worth it.\n\
- Tasks that require back-and-forth with the user (sub-agents cannot prompt).\n\
- Editing files the parent should review — when parent approvals are enabled, sub-agents are forced into read-only plan mode because they cannot present nested approval prompts.\n\
\n\
Parameters:\n\
- `subagent_type`: Sub-agent name from the definition's `name:` frontmatter (or the bare filename without `.md`). The file lives at `~/.config/opencli/agents/<name>.md` and looks like:\n\
  ```\n\
  ---\n\
  name: code-explorer\n\
  description: walks the repo and answers questions\n\
  tools: read_file, grep, glob, list_dir\n\
  model: gpt-5-mini\n\
  ---\n\
  <system prompt body>\n\
  ```\n\
- `prompt`: Self-contained instruction passed to the sub-agent. Include any context it needs — sub-agents do not see the parent's history.\n\
- `description`: Optional short label shown in the live sub-agent view.\n\
- `model`: Optional per-call model override. Claude aliases `sonnet`, `opus`, and `haiku` are resolved before use.\n\
- `cwd`: Optional working directory for the child agent's filesystem and shell tools. Relative paths are resolved from the parent cwd.\n\
- `plan_mode_required`: Optional boolean. When true, the child runs in enforced plan mode and can research/plan but cannot use external mutating tools.\n\
\n\
Compatibility aliases are accepted at runtime for provider/model portability: `subagentType`, `agent_type`, `agentType`, `instructions`, `task`, `message`, `mode: \"plan\"`, and `planModeRequired`.\n\
\n\
Behaviour:\n\
- The child shares the parent's cwd and credential.\n\
- Model is taken from the subagent definition if present, otherwise from the parent's config.\n\
- The child's tool registry contains only the tools listed under `tools:` (empty or `*` means all built-ins). `dispatch_agent` itself is never included, so sub-agents cannot recurse.\n\
- Reasoning, intermediate tool calls, and progress messages are not surfaced to the parent — only the final assistant text. If the child errors before producing text, the tool returns the error so the parent can adapt."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "description": "Sub-agent name from frontmatter, or the bare definition filename under ~/.config/opencli/agents (no extension)."
                },
                "prompt": {
                    "type": "string",
                    "description": "Self-contained instruction passed to the sub-agent."
                },
                "description": {
                    "type": "string",
                    "description": "Optional short label shown in the live sub-agent view."
                },
                "model": {
                    "type": "string",
                    "description": "Optional per-call model override; Claude aliases sonnet/opus/haiku are accepted."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for the child agent. Relative paths resolve from the parent cwd."
                },
                "plan_mode_required": {
                    "type": "boolean",
                    "description": "If true, force the child agent into plan mode so external mutating tools are blocked."
                }
            },
            "required": ["subagent_type", "prompt"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: DispatchArgs = super::parse_args("dispatch_agent", args)?;
        let subagent_type = a.subagent_type().to_string();
        let child_cwd = a.child_cwd(&ctx.cwd)?;
        let def = load_by_name(&ctx.cwd, &subagent_type).map_err(|e| {
            let available = load_all(&ctx.cwd)
                .into_iter()
                .map(|d| d.name)
                .collect::<Vec<_>>();
            if available.is_empty() {
                anyhow!(
                    "{e}. No subagents installed yet — create one at ~/.config/opencli/agents/<name>.md"
                )
            } else {
                anyhow!(
                    "{e}. Available subagents: {}",
                    available.join(", ")
                )
            }
        })?;

        let mut cfg = ctx.config.clone();
        if let Some(m) = a.model.as_ref().or(def.model.as_ref()) {
            // `inherit`/`default` (a Claude Code convention) mean "use the parent
            // agent's model" — keep cfg.model instead of sending a bogus model id
            // that 404s and breaks every tool call in the subagent turn.
            let alias = m.trim().to_ascii_lowercase();
            if alias != "inherit" && alias != "default" {
                cfg.model = resolve_model_alias(m);
            }
        }
        let client = LlmClient::for_config(&cfg).await?;
        let mut agent = Agent::new(client, cfg);
        agent.cwd = child_cwd;
        agent.approval = ctx.approval;
        agent.require_approval = ctx.require_approval;
        agent.auto_approve_edits = ctx.auto_approve_edits;
        agent.registry = Registry::filtered(&def.tools);
        if !def.system_prompt.trim().is_empty() {
            agent.system_prompt = def.system_prompt.clone();
        }
        let enforce_plan_mode = child_requires_plan_mode(ctx, a.requires_plan_mode());
        if enforce_plan_mode {
            agent.approval = ApprovalMode::Plan;
            agent.require_approval = true;
            agent.auto_approve_edits = false;
            agent.system_prompt.push_str(
                "\n\n# Enforced Plan Mode\nYou are running as a sub-agent in enforced plan mode. Investigate with read-only tools, produce an actionable plan or findings, and do not attempt writes, shell commands, commits, installs, or other external mutations. Parent approval is not available inside sub-agents, so external mutations are blocked instead of silently bypassing approval.",
            );
        }
        // Live fleet view: announce this sub-agent to the parent's UI channel
        // before we start, forward its tool activity while it runs, and report
        // completion below. `up` is None in headless/test paths (no-op).
        let up = ctx.events.clone();
        let sub_id = format!("sub-{}", SUBAGENT_SEQ.fetch_add(1, Ordering::Relaxed));
        let prompt_summary: String = a
            .description
            .as_deref()
            .unwrap_or_else(|| a.prompt.lines().next().unwrap_or(""))
            .chars()
            .take(80)
            .collect();
        if let Some(up) = &up {
            let _ = up
                .send(AgentEvent::SubagentStarted {
                    id: sub_id.clone(),
                    subagent_type: def.name.clone(),
                    prompt: prompt_summary,
                })
                .await;
        }

        agent.push_user_message(a.prompt);

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let task = tokio::spawn(async move { agent.run_turn(tx).await });

        let mut final_text = String::new();
        let mut error_msgs: Vec<String> = Vec::new();
        let mut tool_errors: Vec<String> = Vec::new();
        while let Some(ev) = rx.recv().await {
            // Forward discrete progress to the parent fleet view. Only tool
            // starts (not every text/reasoning delta) so the channel isn't
            // flooded by a fast token stream.
            if let Some(up) = &up {
                if let AgentEvent::ToolCallStarted { name, .. } = &ev {
                    let _ = up
                        .send(AgentEvent::SubagentActivity {
                            id: sub_id.clone(),
                            summary: activity_label(name).to_string(),
                        })
                        .await;
                }
            }
            match ev {
                AgentEvent::AssistantTextDone { text } if !text.trim().is_empty() => {
                    final_text = text;
                }
                AgentEvent::Error { message } => error_msgs.push(message),
                AgentEvent::ToolResult {
                    output,
                    error: true,
                    ..
                } => {
                    // Capture for diagnostic context; do not surface unless
                    // the child produced no final text at all.
                    tool_errors.push(output);
                }
                _ => {}
            }
        }
        if let Some(up) = &up {
            let _ = up
                .send(AgentEvent::SubagentDone {
                    id: sub_id.clone(),
                    ok: !final_text.trim().is_empty(),
                })
                .await;
        }
        // run_turn may itself fail (network, schema). Propagate that as an
        // error rather than returning an empty success.
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let msg = e.to_string();
                if !error_msgs.iter().any(|seen| seen == &msg) {
                    error_msgs.push(msg);
                }
            }
            Err(join_err) => error_msgs.push(format!("subagent task panicked: {join_err}")),
        }

        if final_text.trim().is_empty() {
            if !error_msgs.is_empty() {
                return Err(anyhow!(
                    "subagent `{}` produced no final text. Errors: {}. Tool errors: {}",
                    def.name,
                    error_msgs.join("; "),
                    tool_errors.join("; "),
                ));
            }
            return Err(anyhow!(
                "subagent `{}` produced no final text and no error — investigate the agents/{name}.md prompt",
                def.name,
                name = def.name,
            ));
        }

        Ok(final_text)
    }
}

fn child_requires_plan_mode(ctx: &ToolContext, requested_plan_mode: bool) -> bool {
    requested_plan_mode
        || ctx.approval == ApprovalMode::Plan
        || (ctx.require_approval && ctx.approval != ApprovalMode::Auto)
}

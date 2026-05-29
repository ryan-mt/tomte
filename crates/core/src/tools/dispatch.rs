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

use super::{BuiltinTool, Registry, ToolContext};
use crate::agent::{Agent, AgentEvent};
use crate::auth::resolve_credential;
use crate::client::LlmClient;
use crate::provider::Provider;
use crate::subagent::{load_all, load_by_name, resolve_model_alias};

pub struct DispatchAgent;

#[derive(Deserialize)]
struct DispatchArgs {
    subagent_type: String,
    prompt: String,
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
- You want to run several independent sub-tasks in parallel — issue multiple `dispatch_agent` calls in the same turn and they execute concurrently.\n\
\n\
When NOT to use:\n\
- Quick lookups you can do with one or two direct tool calls — the overhead of spinning up an agent is not worth it.\n\
- Tasks that require back-and-forth with the user (sub-agents cannot prompt).\n\
- Editing files the parent should review — sub-agents that write files do so without parent oversight.\n\
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
                }
            },
            "required": ["subagent_type", "prompt"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: DispatchArgs = super::parse_args("dispatch_agent", args)?;
        let def = load_by_name(&ctx.cwd, &a.subagent_type).map_err(|e| {
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
        if let Some(ref m) = def.model {
            // `inherit`/`default` (a Claude Code convention) mean "use the parent
            // agent's model" — keep cfg.model instead of sending a bogus model id
            // that 404s and breaks every tool call in the subagent turn.
            let alias = m.trim().to_ascii_lowercase();
            if alias != "inherit" && alias != "default" {
                cfg.model = resolve_model_alias(m);
            }
        }
        let provider = Provider::from_model(&cfg.model);
        let credential = resolve_credential(provider).await?;
        let client = LlmClient::new(credential)?;
        let mut agent = Agent::new(client, cfg);
        agent.cwd = ctx.cwd.clone();
        agent.registry = Registry::filtered(&def.tools);
        if !def.system_prompt.trim().is_empty() {
            agent.system_prompt = def.system_prompt.clone();
        }
        agent.push_user_message(a.prompt);

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let task = tokio::spawn(async move { agent.run_turn(tx).await });

        let mut final_text = String::new();
        let mut error_msgs: Vec<String> = Vec::new();
        let mut tool_errors: Vec<String> = Vec::new();
        while let Some(ev) = rx.recv().await {
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

use std::io::{Read, Write};

use anyhow::Result;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tomte_core::agent::{Agent, AgentEvent};
use tomte_core::client::LlmClient;
use tomte_core::config;
use tomte_core::tools::ApprovalMode;

mod render;
use render::{render_text_event, TextEventOutcome};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    prompt: String,
    model: Option<String>,
    reasoning: Option<String>,
    output_format: String,
    plan_mode_required: bool,
    cwd: Option<std::path::PathBuf>,
    prompt_file: Option<std::path::PathBuf>,
    dangerously_skip_permissions: bool,
) -> Result<()> {
    let mut prompt = prompt;
    // Prompt precedence: positional argument → --prompt-file → stdin.
    // Resolve --prompt-file BEFORE changing the working directory so a relative
    // path is read from the directory the command was invoked in (the usual
    // file-argument convention), not from --cwd.
    if prompt.trim().is_empty() {
        if let Some(path) = &prompt_file {
            prompt = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("--prompt-file {}: {e}", path.display()))?;
        }
    }

    // Apply the working directory so config/project-memory/skill discovery and
    // the agent's relative-path tools all resolve against it. A scheduler
    // (cron/systemd) starts with a bare cwd, so unattended runs must set --cwd.
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }

    if prompt.trim().is_empty() {
        // Only read stdin when it's piped/redirected. Blocking on an interactive
        // terminal (or an unattended run with no stdin) would hang forever with
        // no prompt; bail with guidance instead. A redirected /dev/null returns
        // EOF immediately and falls through to the "no prompt provided" error.
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            anyhow::bail!(
                "no prompt provided (pass a prompt argument, --prompt-file, or pipe one on stdin)"
            );
        }
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        prompt = buf;
    }
    if prompt.trim().is_empty() {
        anyhow::bail!("no prompt provided");
    }

    let mut cfg = config::load_for_cwd(&std::env::current_dir().unwrap_or_default());
    if let Some(m) = model {
        // Accept an explicit `provider/model` spec from --model; store the bare
        // wire id (matches how config.json values are normalised on load).
        cfg.model = config::normalize_model_name(&m);
    }
    if let Some(r) = reasoning {
        let Some(r) = config::normalize_reasoning_effort(&r) else {
            anyhow::bail!(
                "invalid reasoning effort `{r}`; expected one of: {}",
                config::VALID_REASONING_EFFORTS.join(", ")
            );
        };
        cfg.reasoning_effort = r;
    }
    let format = normalize_output_format(&output_format)?;

    let client = LlmClient::for_config(&cfg).await?;
    let mut agent = Agent::new(client, cfg);
    // This one-shot command has no interactive approver — the event loop below
    // never answers an ApprovalRequest. Mark the run non-interactive so the gate
    // fails closed (immediate deny) instead of blocking for the approval
    // timeout, and so tools ignore model-supplied confirmations such as
    // run_shell's `dangerous_override`.
    agent.non_interactive = true;
    if plan_mode_required {
        agent.approval = ApprovalMode::Plan;
        agent.require_approval = true;
        agent.auto_approve_edits = false;
    } else if dangerously_skip_permissions {
        // Explicit operator opt-in: let side-effecting tools run without a
        // prompt. The run_shell destructive-command guard still applies and
        // stays non-overridable here, so a prompt-injected model still cannot
        // run an obviously destructive command.
        eprintln!(
            "⚠ --dangerously-skip-permissions: side-effecting tools will run without approval"
        );
    } else {
        // Safe default for unattended runs: require approval, which the
        // non-interactive gate turns into an immediate deny for side-effecting
        // tools (run_shell, file writes, MCP, …). Read-only tools still run, so
        // summaries and inspections work headlessly.
        agent.require_approval = true;
    }
    // Match the interactive TUI: load project/global memory and the skill
    // manifest so a one-shot `chat` sees the same context an interactive
    // session would (skills become loadable via the `skill` tool).
    agent.apply_project_memory();
    agent.apply_memory_store();
    agent.apply_skill_manifest();
    // Best-effort: spawn MCP servers configured in settings.json. A
    // misconfigured server logs a warning but does not abort the turn.
    agent.load_mcp().await.ok();

    // Lifecycle hooks: SessionStart (best-effort), then UserPromptSubmit which
    // may BLOCK the prompt (exit 2) before the model ever sees it.
    agent.hooks.fire_session_start().await;
    if let tomte_core::hooks::HookDecision::Block(reason) =
        agent.hooks.fire_user_prompt_submit(&prompt).await
    {
        eprintln!("prompt blocked by UserPromptSubmit hook: {reason}");
        return Ok(());
    }
    agent.push_user_message(prompt);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
    let agent_task = tokio::spawn(async move { agent.run_turn(tx).await });

    let json_mode = matches!(format.as_str(), "json" | "stream-json");

    let mut stdout = std::io::stdout().lock();
    let mut event_error: Option<String> = None;
    let mut tool_names: HashMap<String, String> = HashMap::new();
    while let Some(ev) = rx.recv().await {
        if json_mode {
            // One AgentEvent per line so consumers can pipe through `jq`.
            // A serialisation failure is non-fatal — skip the event and
            // keep the stream flowing instead of aborting the turn.
            if let Ok(line) = serde_json::to_string(&ev) {
                writeln!(stdout, "{}", line).ok();
                stdout.flush().ok();
            }
            if matches!(ev, AgentEvent::TurnComplete | AgentEvent::Error { .. }) {
                break;
            }
            continue;
        }
        match render_text_event(ev, &mut stdout, &mut tool_names) {
            TextEventOutcome::Continue => {}
            TextEventOutcome::Done => break,
            TextEventOutcome::Error(message) => {
                event_error = Some(message);
                break;
            }
        }
    }
    match agent_task.await {
        Ok(Ok(())) => {
            if let Some(message) = event_error {
                anyhow::bail!(message);
            }
            Ok(())
        }
        Ok(Err(e)) => Err(event_error.map_or(e, anyhow::Error::msg)),
        Err(e) => Err(anyhow::anyhow!("agent task failed: {e}")),
    }
}

fn normalize_output_format(value: &str) -> Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "text" | "json" | "stream-json") {
        Ok(normalized)
    } else {
        anyhow::bail!("invalid output format `{value}`; expected one of: text, json, stream-json")
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_output_format;

    #[test]
    fn output_format_accepts_known_values_case_insensitively() {
        assert_eq!(normalize_output_format(" TEXT ").unwrap(), "text");
        assert_eq!(normalize_output_format("json").unwrap(), "json");
        assert_eq!(
            normalize_output_format("STREAM-JSON").unwrap(),
            "stream-json"
        );
    }

    #[test]
    fn output_format_rejects_unknown_values() {
        let err = normalize_output_format("yaml").unwrap_err();
        assert!(err.to_string().contains("invalid output format"));
    }
}

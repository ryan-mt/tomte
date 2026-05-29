use std::io::{Read, Write};

use anyhow::Result;
use opencli_core::agent::{Agent, AgentEvent};
use opencli_core::auth::resolve_credential;
use opencli_core::client::LlmClient;
use opencli_core::config;
use opencli_core::provider::Provider;
use tokio::sync::mpsc;

pub async fn run(
    prompt: String,
    model: Option<String>,
    reasoning: Option<String>,
    output_format: String,
) -> Result<()> {
    let mut prompt = prompt;
    if prompt.trim().is_empty() {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        prompt = buf;
    }
    if prompt.trim().is_empty() {
        anyhow::bail!("no prompt provided");
    }

    let mut cfg = config::load();
    if let Some(m) = model {
        cfg.model = m;
    }
    if let Some(r) = reasoning {
        cfg.reasoning_effort = r;
    }

    let provider = Provider::from_model(&cfg.model);
    let credential = resolve_credential(provider).await?;
    let client = LlmClient::new(credential)?;
    let mut agent = Agent::new(client, cfg);
    // Match the interactive TUI: load project/global memory and the skill
    // manifest so a one-shot `chat` sees the same context an interactive
    // session would (skills become loadable via the `skill` tool).
    agent.apply_project_memory();
    agent.apply_skill_manifest();
    // Best-effort: spawn MCP servers configured in settings.json. A
    // misconfigured server logs a warning but does not abort the turn.
    agent.load_mcp().await.ok();

    // Lifecycle hooks: SessionStart (best-effort), then UserPromptSubmit which
    // may BLOCK the prompt (exit 2) before the model ever sees it.
    agent.hooks.fire_session_start().await;
    if let opencli_core::hooks::HookDecision::Block(reason) =
        agent.hooks.fire_user_prompt_submit(&prompt).await
    {
        eprintln!("prompt blocked by UserPromptSubmit hook: {reason}");
        return Ok(());
    }
    agent.push_user_message(prompt);

    let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
    let agent_task = tokio::spawn(async move { agent.run_turn(tx).await });

    let format = output_format.to_ascii_lowercase();
    let json_mode = matches!(format.as_str(), "json" | "stream-json");

    let mut stdout = std::io::stdout().lock();
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
        match ev {
            AgentEvent::AssistantTextDelta { text } => {
                write!(stdout, "{}", text).ok();
                stdout.flush().ok();
            }
            AgentEvent::ReasoningDelta { .. } => {}
            AgentEvent::ToolCallStarted { name, call_id: _ } => {
                writeln!(stdout, "\n\x1b[2m▸ tool: {name}\x1b[0m").ok();
            }
            AgentEvent::ToolCallArgsDone { arguments, .. } => {
                let pretty = serde_json::from_str::<serde_json::Value>(&arguments)
                    .map(|v| serde_json::to_string_pretty(&v).unwrap_or(arguments.clone()))
                    .unwrap_or(arguments);
                writeln!(stdout, "\x1b[2m  args:\x1b[0m {pretty}").ok();
            }
            AgentEvent::ToolResult { output, error, .. } => {
                let prefix = if error { "✗" } else { "✓" };
                let mut snippet = output.lines().take(20).collect::<Vec<_>>().join("\n");
                if output.lines().count() > 20 {
                    snippet.push_str("\n…");
                }
                writeln!(stdout, "\x1b[2m  {prefix}\x1b[0m {snippet}").ok();
            }
            AgentEvent::TurnComplete => {
                writeln!(stdout).ok();
                break;
            }
            AgentEvent::Error { message } => {
                eprintln!("\nerror: {message}");
                break;
            }
            _ => {}
        }
    }
    let _ = agent_task.await;
    Ok(())
}

use std::io::{Read, Write};

use anyhow::Result;
use opencli_core::agent::{Agent, AgentEvent};
use opencli_core::client::LlmClient;
use opencli_core::config;
use opencli_core::tools::ApprovalMode;
use std::collections::HashMap;
use tokio::sync::mpsc;

pub async fn run(
    prompt: String,
    model: Option<String>,
    reasoning: Option<String>,
    output_format: String,
    plan_mode_required: bool,
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
    if plan_mode_required {
        agent.approval = ApprovalMode::Plan;
        agent.require_approval = true;
        agent.auto_approve_edits = false;
    }
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

enum TextEventOutcome {
    Continue,
    Done,
    Error(String),
}

fn render_text_event<W: Write>(
    ev: AgentEvent,
    stdout: &mut W,
    tool_names: &mut HashMap<String, String>,
) -> TextEventOutcome {
    match ev {
        AgentEvent::AssistantTextDelta { text } => {
            write!(stdout, "{}", text).ok();
            stdout.flush().ok();
        }
        AgentEvent::ReasoningDelta { .. } => {}
        AgentEvent::ToolCallStarted { name, call_id } => {
            tool_names.insert(call_id, name.clone());
            writeln!(stdout, "\n\x1b[2m▸ tool: {name}\x1b[0m").ok();
        }
        AgentEvent::ToolCallArgsDone { call_id, arguments } => {
            if tool_names
                .get(&call_id)
                .is_some_and(|name| suppress_headless_control_tool_body(name))
            {
                return TextEventOutcome::Continue;
            }
            let pretty = serde_json::from_str::<serde_json::Value>(&arguments)
                .map(|v| serde_json::to_string_pretty(&v).unwrap_or(arguments.clone()))
                .unwrap_or(arguments);
            writeln!(stdout, "\x1b[2m  args:\x1b[0m {pretty}").ok();
        }
        AgentEvent::ToolResult {
            call_id,
            output,
            error,
        } => {
            if !error
                && tool_names
                    .get(&call_id)
                    .is_some_and(|name| name == "ask_user_question")
            {
                if let Some(rendered) = opencli_core::tools::ask::render_ask_envelope(&output) {
                    writeln!(stdout, "\n{rendered}").ok();
                    return TextEventOutcome::Continue;
                }
            }
            if !error
                && tool_names
                    .get(&call_id)
                    .is_some_and(|name| suppress_headless_control_tool_body(name))
            {
                return TextEventOutcome::Continue;
            }
            let prefix = if error { "✗" } else { "✓" };
            let mut snippet = output.lines().take(20).collect::<Vec<_>>().join("\n");
            if output.lines().count() > 20 {
                snippet.push_str("\n…");
            }
            writeln!(stdout, "\x1b[2m  {prefix}\x1b[0m {snippet}").ok();
        }
        AgentEvent::PlanModeRequested => {
            writeln!(
                stdout,
                "\n\x1b[2mplan mode → on (read-only until a plan is approved)\x1b[0m"
            )
            .ok();
        }
        AgentEvent::PlanExitRequested { plan } => {
            writeln!(
                stdout,
                "\nPlan ready for approval:\n{plan}\n\nHeadless mode stops at the approved-plan boundary. Run the TUI to approve, or continue with a follow-up prompt after reviewing the plan."
            )
            .ok();
        }
        AgentEvent::GoalStatusUpdated { status, summary } => {
            writeln!(stdout, "\nGoal status: {status}\n{summary}").ok();
        }
        AgentEvent::TurnComplete => {
            writeln!(stdout).ok();
            return TextEventOutcome::Done;
        }
        AgentEvent::Error { message } => {
            return TextEventOutcome::Error(message);
        }
        _ => {}
    }
    TextEventOutcome::Continue
}

fn suppress_headless_control_tool_body(name: &str) -> bool {
    matches!(
        name,
        "ask_user_question" | "enter_plan_mode" | "exit_plan_mode"
    )
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
    use super::{normalize_output_format, render_text_event, TextEventOutcome};
    use opencli_core::agent::AgentEvent;
    use std::collections::HashMap;

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

    #[test]
    fn text_renderer_prints_plan_exit_payload() {
        let mut out = Vec::new();
        let mut tool_names = HashMap::new();

        let outcome = render_text_event(
            AgentEvent::PlanExitRequested {
                plan: "1. inspect\n2. patch\n3. test".to_string(),
            },
            &mut out,
            &mut tool_names,
        );

        assert!(matches!(outcome, TextEventOutcome::Continue));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Plan ready for approval"));
        assert!(text.contains("1. inspect"));
        assert!(text.contains("Headless mode"));
    }

    #[test]
    fn text_renderer_prints_plan_mode_request() {
        let mut out = Vec::new();
        let mut tool_names = HashMap::new();

        let outcome = render_text_event(AgentEvent::PlanModeRequested, &mut out, &mut tool_names);

        assert!(matches!(outcome, TextEventOutcome::Continue));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("plan mode"));
        assert!(text.contains("read-only"));
    }

    #[test]
    fn text_renderer_suppresses_control_tool_args_and_success_result() {
        let mut out = Vec::new();
        let mut tool_names = HashMap::new();

        render_text_event(
            AgentEvent::ToolCallStarted {
                name: "exit_plan_mode".to_string(),
                call_id: "call_plan".to_string(),
            },
            &mut out,
            &mut tool_names,
        );
        render_text_event(
            AgentEvent::ToolCallArgsDone {
                call_id: "call_plan".to_string(),
                arguments: r#"{"plan":"1. inspect\n2. patch"}"#.to_string(),
            },
            &mut out,
            &mut tool_names,
        );
        render_text_event(
            AgentEvent::PlanExitRequested {
                plan: "1. inspect\n2. patch".to_string(),
            },
            &mut out,
            &mut tool_names,
        );
        render_text_event(
            AgentEvent::ToolResult {
                call_id: "call_plan".to_string(),
                output: "plan presented for approval".to_string(),
                error: false,
            },
            &mut out,
            &mut tool_names,
        );

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Plan ready for approval"));
        assert!(!text.contains("args:"));
        assert!(!text.contains("plan presented for approval"));
    }
}

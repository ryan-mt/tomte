//! Headless text rendering of agent events for the one-shot `chat` command.
//!
//! Mirrors the interactive TUI's event handling for `--output-format text`:
//! streams assistant text, prints tool calls/results, and sanitizes any
//! untrusted terminal control sequences before they reach the user's terminal.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;

use opencli_core::agent::AgentEvent;

pub(crate) enum TextEventOutcome {
    Continue,
    Done,
    Error(String),
}

pub(crate) fn render_text_event<W: Write>(
    ev: AgentEvent,
    stdout: &mut W,
    tool_names: &mut HashMap<String, String>,
) -> TextEventOutcome {
    match ev {
        AgentEvent::AssistantTextDelta { text } => {
            write!(stdout, "{}", sanitize_terminal_text(&text)).ok();
            stdout.flush().ok();
        }
        AgentEvent::ReasoningDelta { .. } => {}
        AgentEvent::ToolCallStarted { name, call_id } => {
            tool_names.insert(call_id, name.clone());
            let name = sanitize_terminal_text(&name);
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
            let pretty = sanitize_terminal_text(&pretty);
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
                    let rendered = sanitize_terminal_text(&rendered);
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
            let output = sanitize_terminal_text(&output);
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
                "\nPlan ready for approval:\n{}\n\nHeadless mode stops at the approved-plan boundary. Run the TUI to approve, or continue with a follow-up prompt after reviewing the plan.",
                sanitize_terminal_text(&plan)
            )
            .ok();
        }
        AgentEvent::GoalStatusUpdated { status, summary } => {
            writeln!(
                stdout,
                "\nGoal status: {}\n{}",
                sanitize_terminal_text(&status),
                sanitize_terminal_text(&summary)
            )
            .ok();
        }
        AgentEvent::TurnComplete => {
            writeln!(stdout).ok();
            return TextEventOutcome::Done;
        }
        AgentEvent::Error { message } => {
            return TextEventOutcome::Error(message);
        }
        AgentEvent::FallbackSwitched { from, to, .. } => {
            let from = sanitize_terminal_text(&from);
            let to = sanitize_terminal_text(&to);
            writeln!(
                stdout,
                "\n\x1b[2m⚠ {from} rate-limited/overloaded — switched to fallback model {to}\x1b[0m"
            )
            .ok();
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

fn sanitize_terminal_text(text: &str) -> Cow<'_, str> {
    if !text
        .bytes()
        .any(|byte| (byte < 0x20 && byte != b'\n') || byte == 0x7f)
    {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    let mut col = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&p) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(p) = chars.next() {
                        if p == '\u{07}' {
                            break;
                        }
                        if p == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => {
                let spaces = 4 - (col % 4);
                out.extend(std::iter::repeat_n(' ', spaces));
                col += spaces;
            }
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\r' | '\u{00}'..='\u{08}' | '\u{0b}'..='\u{1f}' | '\u{7f}' => {}
            other => {
                out.push(other);
                col += 1;
            }
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::{render_text_event, TextEventOutcome};
    use opencli_core::agent::AgentEvent;
    use std::collections::HashMap;

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

    #[test]
    fn text_renderer_sanitizes_untrusted_terminal_controls() {
        let mut assistant = Vec::new();
        let mut tool_names = HashMap::new();

        render_text_event(
            AgentEvent::AssistantTextDelta {
                text: "\x1b[31mred\x1b[0m\x1b]2;owned-title\x07line\r\nok".to_string(),
            },
            &mut assistant,
            &mut tool_names,
        );

        let assistant_text = String::from_utf8(assistant).unwrap();
        assert!(!assistant_text.contains('\x1b'), "{assistant_text:?}");
        assert!(!assistant_text.contains('\x07'), "{assistant_text:?}");
        assert!(!assistant_text.contains('\r'), "{assistant_text:?}");
        assert!(
            !assistant_text.contains("owned-title"),
            "{assistant_text:?}"
        );
        assert!(assistant_text.contains("red"));
        assert!(assistant_text.contains("line\nok"));

        let mut tool = Vec::new();
        render_text_event(
            AgentEvent::ToolCallStarted {
                name: "run_shell".to_string(),
                call_id: "call_1".to_string(),
            },
            &mut tool,
            &mut tool_names,
        );
        render_text_event(
            AgentEvent::ToolCallArgsDone {
                call_id: "call_1".to_string(),
                arguments: "bad\x1b]52;c;clipboard-secret\x07".to_string(),
            },
            &mut tool,
            &mut tool_names,
        );
        render_text_event(
            AgentEvent::ToolResult {
                call_id: "call_1".to_string(),
                output: "ok\x1b[2Jcleared\r\nnext".to_string(),
                error: false,
            },
            &mut tool,
            &mut tool_names,
        );

        let tool_text = String::from_utf8(tool).unwrap();
        assert!(
            tool_text.contains("\x1b[2m"),
            "renderer-owned dim styling may remain"
        );
        assert!(!tool_text.contains("clipboard-secret"), "{tool_text:?}");
        assert!(!tool_text.contains("\x1b[2J"), "{tool_text:?}");
        assert!(!tool_text.contains('\r'), "{tool_text:?}");
        assert!(tool_text.contains("cleared\nnext"));
    }
}

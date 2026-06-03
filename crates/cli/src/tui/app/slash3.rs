//! `/slash` command dispatch (part 3: todos..commands + unknown fallback).
//! Chained from `handle_slash_2`; preserves match order. Logic unchanged.

use super::*;

pub async fn handle_slash_3(app: &mut App, head: &str, arg: &str) {
    match head {
        "todos" | "todo" => {
            if app.session_todos.is_empty() {
                app.blocks.push(Block::System(
                    "No session todos. The model writes them with `todo_write` \
                     when it plans a multi-step task."
                        .into(),
                ));
            } else {
                let done = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::Completed))
                    .count();
                let in_progress = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::InProgress))
                    .count();
                let pending = app
                    .session_todos
                    .iter()
                    .filter(|t| matches!(t.status, TodoStatus::Pending))
                    .count();
                let mut msg = format!(
                    "{} tasks ({} done, {} in progress, {} open)\n",
                    app.session_todos.len(),
                    done,
                    in_progress,
                    pending
                );
                for t in &app.session_todos {
                    let marker = match t.status {
                        TodoStatus::Completed => "✓",
                        TodoStatus::InProgress => "▪",
                        TodoStatus::Pending => "□",
                    };
                    let label = match t.status {
                        TodoStatus::InProgress => &t.active_form,
                        _ => &t.content,
                    };
                    msg.push_str(&format!("  {marker} {label}\n"));
                }
                app.blocks.push(Block::System(msg));
            }
        }
        "about" => {
            app.blocks.push(Block::System(format!(
                "opencli v{}\n\
                 model:  {}\n\
                 effort: {}\n\
                 build:  {}",
                env!("CARGO_PKG_VERSION"),
                app.config.model,
                app.config.reasoning_effort,
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
            )));
        }
        "quit" | "exit" => app.should_exit = true,
        "agents" => {
            let defs = opencli_core::subagent::load_all(&app.cwd);
            if defs.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No subagents installed. Create one at {}/agents/<name>.md, or install Claude/Codex agents under ~/.claude/agents or ~/.codex/agents.",
                    opencli_core::config::config_dir().display()
                )));
            } else {
                let mut out = String::from(
                    "Installed subagents:
",
                );
                for d in &defs {
                    let tools = if d.tools.is_empty() {
                        "all".to_string()
                    } else {
                        d.tools.join(", ")
                    };
                    out.push_str(&format!(
                        "  {:<20} {} [tools: {}]
",
                        d.name, d.description, tools
                    ));
                }
                out.push_str(
                    "
Invoke from the model via dispatch_agent with subagent_type set to the name.",
                );
                app.blocks.push(Block::System(out));
            }
        }
        "skills" => {
            let skills = opencli_core::skill::discover(&app.cwd);
            if skills.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No skills installed. Create one at {}/skills/<name>/SKILL.md, or install Claude Code/Codex skills under ~/.claude/skills, ~/.codex/skills, or their plugin directories.",
                    opencli_core::config::config_dir().display()
                )));
            } else {
                let mut out = format!("Available skills ({}):\n", skills.len());
                for s in &skills {
                    out.push_str(&format!("  {:<28} {}\n", s.name, s.description));
                }
                out.push_str(
                    "\nThe model loads a skill's full instructions on demand via the `skill` tool.",
                );
                app.blocks.push(Block::System(out));
            }
        }
        "commands" => {
            let cmds = opencli_core::command::load_all(&app.cwd);
            if cmds.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No custom commands installed. Create one at {}/commands/<name>.md or {}/.opencli/commands/<name>.md",
                    opencli_core::config::config_dir().display(),
                    app.cwd.display()
                )));
            } else {
                let mut out = String::from(
                    "Custom commands:
",
                );
                for c in &cmds {
                    let hint = if c.argument_hint.is_empty() {
                        "".to_string()
                    } else {
                        format!(" {}", c.argument_hint)
                    };
                    out.push_str(&format!(
                        "  /{:<20} {}
      ↳ {}{}
",
                        c.name, c.description, c.name, hint
                    ));
                }
                out.push_str(
                    "
Type /<name> [args] to expand and send.",
                );
                app.blocks.push(Block::System(out));
            }
        }
        other => {
            // Check if it matches a custom command before reporting unknown.
            let cmds = opencli_core::command::load_all(&app.cwd);
            if let Some(cmd) = cmds.iter().find(|c| c.name == other) {
                let expanded = opencli_core::command::expand(&cmd.body, &cmd.name, arg);
                app.input.buffer = expanded;
                app.input.cursor = app.input.buffer.len();
                app.blocks.push(Block::System(format!(
                    "Expanded /{} into input — press Enter to send.",
                    cmd.name
                )));
            } else {
                app.blocks.push(Block::System(format!(
                    "Unknown command /{other}. Try /help, /commands, /agents, or /skills."
                )));
            }
        }
    }
}

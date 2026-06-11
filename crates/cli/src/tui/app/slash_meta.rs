//! `/slash` command dispatch — meta/info commands (todos, about, agents,
//! skills, commands, quit) plus the unknown-command fallback. The final link in
//! the chain from `handle_slash` → `handle_slash_ops`; preserves match order.

use super::*;

pub async fn handle_slash_meta(app: &mut App, head: &str, arg: &str) {
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
                        TodoStatus::InProgress => "◆",
                        TodoStatus::Pending => "◇",
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
                "tomte v{}\n\
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
            let defs = tomte_core::subagent::load_all(&app.cwd);
            if defs.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No subagents installed. Create one at {}/agents/<name>.md, or install Claude/Codex agents under ~/.claude/agents or ~/.codex/agents.",
                    tomte_core::config::config_dir().display()
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
            let skills = tomte_core::skill::discover(&app.cwd);
            if skills.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No skills installed. Create one at {}/skills/<name>/SKILL.md, or install Claude Code/Codex skills under ~/.claude/skills, ~/.codex/skills, or their plugin directories.",
                    tomte_core::config::config_dir().display()
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
            let cmds = tomte_core::command::load_all(&app.cwd);
            if cmds.is_empty() {
                app.blocks.push(Block::System(format!(
                    "No custom commands installed. Create one at {}/commands/<name>.md or {}/.tomte/commands/<name>.md",
                    tomte_core::config::config_dir().display(),
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
            let cmds = tomte_core::command::load_all(&app.cwd);
            if let Some(cmd) = cmds.iter().find(|c| c.name == other) {
                let expanded = tomte_core::command::expand(&cmd.body, &cmd.name, arg);
                app.input.buffer = expanded;
                app.input.cursor = app.input.buffer.len();
                app.blocks.push(Block::System(format!(
                    "Expanded /{} into input — press Enter to send.",
                    cmd.name
                )));
            } else if let Ok((_dir, body)) = tomte_core::skill::load_body(&app.cwd, other) {
                // Manually trigger a skill: drop its instructions into the input
                // so the user can review (and append args) before sending — the
                // same flow as a custom command. Works for any installed skill,
                // while the `/` menu surfaces the project-local ones.
                let mut text = body.trim().to_string();
                if !arg.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(arg);
                }
                app.input.buffer = text;
                app.input.cursor = app.input.buffer.len();
                app.blocks.push(Block::System(format!(
                    "Loaded skill /{other} into input — press Enter to send."
                )));
            } else {
                app.blocks.push(Block::System(format!(
                    "Unknown command /{other}. Try /help, /commands, /agents, or /skills."
                )));
            }
        }
    }
}

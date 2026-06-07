//! `/slash` command entry point and parsing. The handler is chained across
//! `slash.rs` → `slash_ops.rs` → `slash_meta.rs` by command group to keep each
//! file small while preserving the original match order: this file matches the
//! setup/auth/model commands, then delegates the rest down the chain.

use super::*;

pub async fn handle_slash(app: &mut App, cmd: &str) {
    let (head, arg) = split_slash_command(cmd);
    match head {
        "help" | "?" => {
            app.blocks.push(Block::System(
                "Commands:\n  \
                 /login              sign in (OpenAI / Anthropic, OAuth or API key)\n  \
                 /apikey <sk-…>      save an API key\n  \
                 /logout             clear credentials\n  \
                 /model              pick model (arrow keys), then reasoning\n  \
                 /thinking           pick reasoning (none|low|medium|high|xhigh)\n  \
                 /verbosity          pick verbosity (low|medium|high)\n  \
                 /img <path>         attach a file as an image (or use Ctrl+V)\n  \
                 /clear              clear conversation\n  \
                 /resume             pick a previous session to continue\n  \
                 /cwd [path]         show / set working directory\n  \
                 /worktree create [name]  create and enter an isolated git worktree\n  \
                 /worktree exit keep|remove [--discard]\n  \
                 /goal <objective>   keep working until the objective is complete\n  \
                 /status             show auth status\n  \
                 /doctor             diagnose setup (auth, config, MCP, tools)\n  \
                 /cost               show token usage and estimated cost\n  \
                 /usage              show the provider's real quota / rate-limit status\n  \
                 /context            show context-window usage + composition\n  \
                 /buddy [off|reset]  meet your account's pixel companion\n  \
                 /config             show current configuration\n  \
                 /hooks              list configured PreToolUse hooks\n  \
                 /mcp                list configured MCP servers\n  \
                 /init               create CLAUDE.md for this project\n  \
                 /memory             show CLAUDE.md\n  \
                 /diff               show `git diff` for the working tree\n  \
                 /why [loc]          show the decision trail (why changes were made)\n  \
                 /review             ask the agent to review uncommitted changes\n  \
                 /commit             stage & commit with a generated message\n  \
                 /commit-push-pr     commit, push a branch, and open a PR\n  \
                 /export [path]      save conversation as markdown\n  \
                 /compact            ask the agent to compact the conversation\n  \
                 /todos              show the session todo list\n  \
                 /about              show tomte version + build info\n  \
                 /perms [on|off]     toggle the approval modal for writes/shell\n  \
                 /undo               revert the most recent file edit\n  \
                 /quit               exit\n\n\
                 Composer prefixes:\n  \
                 @<path>             reference a file (typeahead); its contents are attached\n  \
                 !<command>          run a shell command now (!! to force past the guard)\n  \
                 #<note>             save a note to this project's CLAUDE.md\n\n\
                 Keyboard shortcuts:\n  \
                 ↑ / ↓               recall older / newer messages (on the first / last composer line)\n  \
                 Esc                 cancel the running turn (while busy)\n  \
                 Ctrl+O              toggle tool-call detail view\n  \
                 Ctrl+T              show / hide the live todo panel\n  \
                 Ctrl+L              clear the screen\n  \
                 Ctrl+V              paste text or image\n  \
                 Left-drag           select text and copy it to the clipboard\n  \
                 Ctrl+C              quit"
                    .to_string(),
            ));
        }
        "login" => {
            app.login = LoginScreen::new();
            app.screen = Screen::Login;
            app.status_line.clear();
        }
        "apikey" => {
            if arg.is_empty() {
                app.blocks
                    .push(Block::System("Usage: /apikey sk-…".to_string()));
            } else {
                let mut record = auth::load_auth().unwrap_or_default();
                auth::activate_openai_api_key(&mut record, arg.to_string());
                match auth::save_auth(&record) {
                    Ok(_) => {
                        app.auth_mode = AuthMode::OpenaiApiKey;
                        app.blocks.push(Block::System("✅ API key saved.".into()));
                        let models = Provider::OpenAi
                            .available_models()
                            .iter()
                            .map(|m| {
                                let win = tomte_core::agent::context_window_label(m);
                                format!("  · {m:<20} ({win} context)")
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        app.blocks
                            .push(Block::System(format!("Available models:\n{models}")));
                    }
                    Err(e) => app.blocks.push(Block::System(format!("Error: {e}"))),
                }
            }
        }
        "logout" => {
            // Open a picker so the user chooses WHICH stored credential to
            // remove (a session can hold several at once) instead of nuking all
            // of auth.json. Env-var credentials aren't listed — they can't be
            // cleared by logging out.
            if picker::logout_targets().is_empty() {
                app.blocks.push(Block::System(
                    "Nothing to log out — no stored credentials.".into(),
                ));
            } else {
                app.open_overlay(OverlayKind::LogoutPicker);
            }
        }
        "status" => {
            let record = auth::load_auth().unwrap_or_default();
            let active_mode = auth::effective_mode_with_env(&record);
            let mut msg = match active_mode {
                AuthMode::None => "Not signed in.".to_string(),
                AuthMode::OpenaiApiKey => "Signed in with OpenAI API key.".to_string(),
                AuthMode::OpenaiOauth => {
                    let acc = record
                        .tokens
                        .as_ref()
                        .and_then(|t| t.account_id.clone())
                        .unwrap_or_default();
                    format!("Signed in with ChatGPT. account_id={acc}")
                }
                AuthMode::AnthropicApiKey => "Signed in with Anthropic API key.".to_string(),
                AuthMode::AnthropicOauth => "Signed in with Claude Pro/Max OAuth.".to_string(),
            };
            let mut extra = Vec::new();
            if auth::has_openai_oauth(&record) && !matches!(active_mode, AuthMode::OpenaiOauth) {
                extra.push("OpenAI OAuth token is also stored");
            }
            if auth::has_openai_api_key(&record) && !matches!(active_mode, AuthMode::OpenaiApiKey) {
                extra.push("OpenAI API key is also stored");
            }
            if auth::has_anthropic_oauth(&record)
                && !matches!(active_mode, AuthMode::AnthropicOauth)
            {
                extra.push("Anthropic OAuth token is also stored");
            }
            if auth::has_anthropic_api_key(&record)
                && !matches!(active_mode, AuthMode::AnthropicApiKey)
            {
                extra.push("Anthropic API key is also stored");
            }
            for note in extra {
                msg.push_str(&format!("\n  ({note})"));
            }
            let coverage = auth::credential_coverage();
            msg.push_str(&format!(
                "\n\nCredential coverage:\n  OpenAI OAuth:       {}\n  OpenAI API key:     {}\n  Anthropic OAuth:    {}\n  Anthropic API key:  {}",
                coverage.openai_oauth.label(),
                coverage.openai_api_key.label(),
                coverage.anthropic_oauth.label(),
                coverage.anthropic_api_key.label(),
            ));
            app.blocks.push(Block::System(msg));
            let catalogs = auth::signed_in_model_catalogs();
            if !catalogs.is_empty() {
                let mut text = String::from("Available models:");
                for catalog in catalogs {
                    text.push_str(&format!(
                        "\n  {} ({}):",
                        catalog.provider.display_name(),
                        catalog.provider
                    ));
                    for m in catalog.models {
                        let win = tomte_core::agent::context_window_label(m);
                        text.push_str(&format!("\n    · {m:<20} ({win} context)"));
                    }
                }
                app.blocks.push(Block::System(text));
            }
        }
        "model" => {
            if arg.is_empty() {
                app.chain_to_effort = true;
                app.open_overlay(OverlayKind::ModelPicker);
            } else {
                // Accept an explicit `provider/model` spec; store the bare wire
                // id used everywhere downstream.
                let model = config::normalize_model_name(arg);
                app.config.model = model.clone();
                if let Err(e) = config::save(&app.config) {
                    app.blocks
                        .push(Block::System(format!("config save failed: {e}")));
                }
                app.blocks.push(Block::System(format!("model → {model}")));
            }
        }
        "effort" | "thinking" => {
            if arg.is_empty() {
                app.open_overlay(OverlayKind::EffortPicker);
            } else if let Some(effort) = config::normalize_reasoning_effort(arg) {
                app.config.reasoning_effort = effort.clone();
                if let Err(e) = config::save(&app.config) {
                    app.blocks
                        .push(Block::System(format!("config save failed: {e}")));
                }
                app.blocks.push(Block::System(format!("effort → {effort}")));
            } else {
                app.blocks.push(Block::System(format!(
                    "Invalid effort `{arg}`. Expected one of: {}",
                    config::VALID_REASONING_EFFORTS.join(", ")
                )));
            }
        }
        "verbosity" => {
            if arg.is_empty() {
                app.open_overlay(OverlayKind::VerbosityPicker);
            } else if let Some(verbosity) = config::normalize_verbosity(arg) {
                app.config.verbosity = verbosity.clone();
                if let Err(e) = config::save(&app.config) {
                    app.blocks
                        .push(Block::System(format!("config save failed: {e}")));
                }
                app.blocks
                    .push(Block::System(format!("verbosity → {verbosity}")));
            } else {
                app.blocks.push(Block::System(format!(
                    "Invalid verbosity `{arg}`. Expected one of: {}",
                    config::VALID_VERBOSITIES.join(", ")
                )));
            }
        }
        "img" | "image" => {
            if arg.is_empty() {
                app.blocks.push(Block::System(
                    "Usage: /img <path>  — attach image to next message".into(),
                ));
            } else {
                let p = std::path::PathBuf::from(arg);
                let path = if p.is_absolute() { p } else { app.cwd.join(&p) };
                if !path.is_file() {
                    app.blocks
                        .push(Block::System(format!("File not found: {}", path.display())));
                } else {
                    let n = app.next_image_num;
                    app.pending_images.push(path.clone());
                    app.next_image_num += 1;
                    let marker = format!("[Image #{n}] ");
                    for c in marker.chars() {
                        app.input.insert_char(c);
                    }
                    app.blocks.push(Block::System(format!(
                        "attached [Image #{n}]: {}",
                        path.display()
                    )));
                }
            }
        }
        "cwd" => {
            if arg.is_empty() {
                app.blocks
                    .push(Block::System(format!("cwd: {}", app.cwd.display())));
            } else if let Some(path) = resolve_cwd_arg(&app.cwd, arg) {
                app.cwd = path;
                app.blocks
                    .push(Block::System(format!("cwd → {}", app.cwd.display())));
            } else {
                app.blocks.push(Block::System("Invalid path.".into()));
            }
        }
        "worktree" => {
            handle_worktree_slash(app, arg);
        }
        "goal" => {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                match &app.active_goal {
                    Some(goal) => {
                        let mut msg = format!(
                            "Active goal:\n  {}\n  turns completed: {}",
                            goal.objective, goal.turns_completed
                        );
                        if goal.waiting_for_user {
                            msg.push_str("\n  waiting for user input");
                        }
                        if let Some(summary) = &goal.last_summary {
                            msg.push_str(&format!("\n  last update: {summary}"));
                        }
                        app.blocks.push(Block::System(msg));
                    }
                    None => app.blocks.push(Block::System(
                        "Usage: /goal <objective>\nExample: /goal fix the failing tests and verify the build"
                            .into(),
                    )),
                }
            } else if matches!(trimmed, "stop" | "cancel") {
                if let Some(goal) = app.active_goal.take() {
                    app.pending_goal_replacement = None;
                    remove_pending_goal_continuations(&mut app.message_queue);
                    app.pending_session_save = true;
                    app.blocks
                        .push(Block::System(format!("Stopped goal: {}", goal.objective)));
                } else {
                    app.blocks
                        .push(Block::System("No active goal to stop.".into()));
                }
            } else if goal_exceeds_limit(trimmed) {
                app.blocks.push(Block::System(format!(
                    "Goal too long ({} words / {} chars; max {GOAL_MAX_WORDS} words or {GOAL_MAX_CHARS} chars). The objective is re-sent to the model every turn, so a long one crowds out the real work and dulls its reasoning. Tighten it to one clear objective and put the extra detail in a normal message before running a short /goal.",
                    goal_word_count(trimmed),
                    trimmed.chars().count()
                )));
                app.auto_scroll = true;
            } else {
                let objective = trimmed.to_string();
                if let Some(goal) = &app.active_goal {
                    if goal.objective == objective {
                        app.blocks.push(Block::System(format!(
                            "Goal already active: {}",
                            goal.objective
                        )));
                    } else {
                        app.pending_goal_replacement = Some(PendingGoalReplacement {
                            objective: objective.clone(),
                        });
                        app.blocks.push(Block::System(format!(
                            "A goal is already active:\n  current: {}\n  new: {}\n\nReplace the current goal? Press Y to replace, or N/Esc to keep the current goal.",
                            goal.objective, objective
                        )));
                    }
                    app.auto_scroll = true;
                } else {
                    start_goal(app, objective, false);
                }
            }
        }
        _ => handle_slash_ops(app, head, arg).await,
    }
}

pub fn split_slash_command(cmd: &str) -> (&str, &str) {
    let trimmed = cmd.trim();
    let Some((idx, ch)) = trimmed.char_indices().find(|(_, c)| c.is_whitespace()) else {
        return (trimmed, "");
    };
    let head = &trimmed[..idx];
    let arg = trimmed[idx + ch.len_utf8()..].trim();
    (head, arg)
}

/// chars/4 estimate of the visible conversation — user/assistant/reasoning/tool
/// I/O and system notes on screen. Feeds the "Messages" category of the
/// `/context` report; the prompt-side categories (system prompt, tool schemas,
/// skills, …) are estimated in `tomte_core::context_report`.
pub fn estimate_messages_tokens(blocks: &[Block]) -> u64 {
    fn est(s: &str) -> u64 {
        (s.chars().count() as u64).div_ceil(4)
    }
    let mut total = 0u64;
    for b in blocks {
        match b {
            Block::User(t) => total += est(t),
            Block::Assistant {
                text, reasoning, ..
            } => total += est(text) + est(reasoning),
            Block::Tool { args, output, .. } => {
                total += est(args) + output.as_deref().map(est).unwrap_or(0);
            }
            Block::System(t) => total += est(t),
            Block::Welcome | Block::Rich(_) => {}
        }
    }
    total
}

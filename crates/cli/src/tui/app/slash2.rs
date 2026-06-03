//! `/slash` command dispatch (part 2: plan..compact). Chained from
//! `handle_slash`; preserves match order. Logic unchanged.

use super::*;

pub async fn handle_slash_2(app: &mut App, head: &str, arg: &str) {
    match head {
        "plan" => {
            set_permission_mode_and_save(app, PermissionMode::Plan);
            app.blocks.push(Block::System(
                "plan mode → on (read-only tools only; write/edit/shell will be blocked)".into(),
            ));
        }
        "normal" => {
            set_permission_mode_and_save(app, PermissionMode::Default);
            app.pending_plan_exit = None;
            app.blocks.push(Block::System("plan mode → off".into()));
        }
        "perms" | "approvals" => {
            let new_state = match arg {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                "" => !app.require_approval,
                other => {
                    app.blocks.push(Block::System(format!(
                        "Usage: /perms [on|off]  (current: {}). Got: {other}",
                        if app.require_approval { "on" } else { "off" }
                    )));
                    return;
                }
            };
            app.require_approval = new_state;
            app.blocks.push(Block::System(format!(
                "approval modal → {}",
                if new_state {
                    "on"
                } else {
                    "off (writes/shell auto-approved)"
                }
            )));
        }
        "undo" => {
            // main_loop drains this flag so the agent Arc stays out of
            // handle_slash (same pattern as `pending_resume_id`).
            app.pending_undo = true;
        }
        "clear" => {
            app.blocks.clear();
            if app.active_goal.take().is_some() {
                app.pending_session_save = true;
            }
            app.pending_goal_replacement = None;
            app.pending_plan_exit = None;
            remove_pending_goal_continuations(&mut app.message_queue);
        }
        "resume" => {
            app.open_overlay(OverlayKind::ResumePicker);
        }
        "cost" => {
            let report = opencli_core::pricing::render_cost_report(
                &app.usage_by_model,
                &app.config.model,
                app.turn_count,
            );
            app.blocks.push(Block::System(report));
        }
        "usage" => {
            app.blocks.push(Block::System(render_usage_report(app)));
        }
        "context" | "ctx" => {
            let expanded = arg.trim().eq_ignore_ascii_case("all");
            let messages = estimate_messages_tokens(&app.blocks);
            let report = opencli_core::context_report::build(
                &app.cwd,
                &app.config,
                messages,
                app.tokens_used,
            );
            let lines = crate::tui::context_view::render(&report, expanded);
            app.blocks.push(Block::Rich(lines));
        }
        "buddy" | "pet" => {
            let arg = arg.trim();
            if arg.eq_ignore_ascii_case("off") {
                app.buddy_hidden = true;
                app.blocks.push(Block::System(
                    "buddy hidden — run /buddy to bring your companion back".to_string(),
                ));
            } else if arg.eq_ignore_ascii_case("reset") || arg.eq_ignore_ascii_case("clear") {
                // Dev/testing: forget the adopted companion so the next /buddy
                // hatches again. This only replays the hatch — WHICH pet you get
                // is still derived from the account (or OPENCLI_BUDDY_SEED), so
                // it can't be tricked into a different companion.
                app.buddy_pet = None;
                app.buddy_hidden = false;
                app.hatch = None;
                app.blocks.push(Block::System(
                    "buddy reset — run /buddy to hatch again".to_string(),
                ));
            } else if app.hatch.is_some() {
                // Already hatching; ignore repeat presses.
            } else if let Some(pet) = app.buddy_pet {
                // Locked: the companion is already adopted for this account.
                app.buddy_hidden = false;
                app.blocks.push(Block::System(format!(
                    "{} is already your companion — locked to this account.",
                    crate::tui::buddy::pet_name(pet)
                )));
            } else {
                // First hatch: the pet is a pure function of the signed-in
                // account, so it persists for that account and re-rolls only on
                // account switch — nothing is stored to delete or tamper with.
                // `OPENCLI_BUDDY_SEED` lets a dev preview other pets by seeding
                // the roll directly instead of from the account.
                let identity = std::env::var("OPENCLI_BUDDY_SEED")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        auth::account_identity(&auth::load_auth().unwrap_or_default())
                    });
                let pet = crate::tui::buddy::roll(&identity);
                app.hatch = Some(HatchAnim {
                    pet,
                    started: std::time::Instant::now(),
                });
                app.auto_scroll = true;
            }
        }
        "config" => {
            let auth = match app.auth_mode {
                AuthMode::None => "none",
                AuthMode::OpenaiApiKey => "api_key",
                AuthMode::OpenaiOauth => "chatgpt",
                AuthMode::AnthropicApiKey => "anthropic_api_key",
                AuthMode::AnthropicOauth => "claude_oauth",
            };
            let mcp_count = opencli_core::mcp::load_servers_config().len();
            let hooks = opencli_core::hooks::load();
            let hook_count = hooks.config.pre_tool_use.len();
            let approval = match app.approval {
                ApprovalMode::Auto => "auto",
                ApprovalMode::OnRequest => "on_request",
                ApprovalMode::Manual => "manual",
                ApprovalMode::Plan => "plan",
            };
            app.blocks.push(Block::System(format!(
                "Current configuration:\n  \
                 model:            {}\n  \
                 reasoning_effort: {}\n  \
                 verbosity:        {}\n  \
                 cwd:              {}\n  \
                 approval:         {}\n  \
                 auth_mode:        {}\n  \
                 mcp_servers:      {}\n  \
                 hooks (PreToolUse): {}",
                app.config.model,
                app.config.reasoning_effort,
                app.config.verbosity,
                app.cwd.display(),
                approval,
                auth,
                mcp_count,
                hook_count,
            )));
        }
        "hooks" => {
            let hooks = opencli_core::hooks::load();
            let entries = &hooks.config.pre_tool_use;
            if entries.is_empty() {
                app.blocks.push(Block::System(
                    "No PreToolUse hooks configured.\n\
                     Add some in ~/.config/opencli/settings.json under .hooks.PreToolUse"
                        .into(),
                ));
            } else {
                let mut msg = String::from("PreToolUse hooks:\n");
                for (i, h) in entries.iter().enumerate() {
                    msg.push_str(&format!(
                        "  {}. matcher={:<14}  command={}\n",
                        i + 1,
                        h.matcher,
                        h.command
                    ));
                }
                app.blocks.push(Block::System(msg));
            }
        }
        "mcp" => {
            let servers = opencli_core::mcp::load_servers_config();
            if servers.is_empty() {
                app.blocks.push(Block::System(
                    "No MCP servers configured.\n\
                     Add some in ~/.config/opencli/settings.json under mcp_servers"
                        .into(),
                ));
            } else {
                let mut msg = String::from("MCP servers (from settings.json):\n");
                let mut names: Vec<&String> = servers.keys().collect();
                names.sort();
                for n in names {
                    let cfg = &servers[n];
                    msg.push_str(&format!(
                        "  · {}  ·  {} {}\n",
                        n,
                        cfg.command,
                        cfg.args.join(" ")
                    ));
                }
                msg.push_str("\nServers are spawned on first turn; tools register under mcp__<server>__<tool>.");
                app.blocks.push(Block::System(msg));
            }
        }
        "doctor" => {
            let report = opencli_core::doctor::diagnose(&app.cwd);
            app.blocks.push(Block::System(report.render()));
        }
        "init" => {
            let claude_md = app.cwd.join("CLAUDE.md");
            if claude_md.exists() {
                app.blocks.push(Block::System(format!(
                    "CLAUDE.md already exists at {}. Use /memory to view it.",
                    claude_md.display()
                )));
            } else {
                // Queue a prompt asking the agent to analyse the repo and
                // write a CLAUDE.md. The Enter handler will run it on the
                // next tick of main_loop.
                let prompt = "Analyze this repository and create a CLAUDE.md file at the repo root. \
                              The file should describe: project purpose, tech stack, key architecture / \
                              module layout, build + test commands, and any non-obvious conventions a new \
                              contributor must know. Keep it concise (under 80 lines) and write it as \
                              terse engineer-to-engineer notes, not a tutorial.";
                app.message_queue.push(prompt.to_string());
                app.blocks.push(Block::System(
                    "Queued: agent will analyse the repo and create CLAUDE.md.".into(),
                ));
            }
        }
        "memory" => {
            let claude_md = app.cwd.join("CLAUDE.md");
            match std::fs::read_to_string(&claude_md) {
                Ok(text) => app.blocks.push(Block::System(format!(
                    "CLAUDE.md ({}):\n{}",
                    claude_md.display(),
                    text
                ))),
                Err(_) => app.blocks.push(Block::System(format!(
                    "No CLAUDE.md at {}. Run /init to create one.",
                    claude_md.display()
                ))),
            }
        }
        "diff" => {
            // Pipe `git diff` from the cwd and surface its output. Empty
            // output means a clean tree; non-zero exit (no git, not a repo)
            // surfaces stderr so the user knows why.
            let cwd = app.cwd.clone();
            let out = tokio::process::Command::new("git")
                .args(["diff", "--no-color"])
                .current_dir(&cwd)
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    if stdout.trim().is_empty() {
                        app.blocks
                            .push(Block::System("(no uncommitted changes)".into()));
                    } else {
                        app.blocks
                            .push(Block::System(format!("$ git diff\n{stdout}")));
                    }
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    app.blocks
                        .push(Block::System(format!("git diff failed:\n{stderr}")));
                }
                Err(e) => app.blocks.push(Block::System(format!("git diff: {e}"))),
            }
        }
        "review" => {
            let prompt = "Review the uncommitted changes in this repository. Run `git diff` (or \
                          the run_shell tool) to see them, then assess for correctness, security \
                          risks, and obvious bugs. Cite locations as `path:line`. Surface only \
                          findings that are CRITICAL/HIGH/MEDIUM — skip stylistic nits.";
            app.message_queue.push(prompt.to_string());
            app.blocks.push(Block::System(
                "Queued: agent will review the uncommitted changes.".into(),
            ));
        }
        "commit" => {
            app.message_queue.push(commit_prompt(arg));
            app.blocks.push(Block::System(
                "Queued: agent will stage and commit the changes.".into(),
            ));
        }
        "commit-push-pr" | "commitpushpr" | "pr" => {
            app.message_queue.push(commit_push_pr_prompt(arg));
            app.blocks.push(Block::System(
                "Queued: agent will commit, push a branch, and open a PR.".into(),
            ));
        }
        "export" => {
            let default_name = format!(
                "opencli-export-{}.md",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            );
            let path = if arg.is_empty() {
                app.cwd.join(default_name)
            } else {
                let p = std::path::PathBuf::from(arg);
                if p.is_absolute() {
                    p
                } else {
                    app.cwd.join(p)
                }
            };
            let md = render_blocks_as_markdown(&app.blocks);
            match std::fs::write(&path, md) {
                Ok(_) => app.blocks.push(Block::System(format!(
                    "Exported conversation → {}",
                    path.display()
                ))),
                Err(e) => app
                    .blocks
                    .push(Block::System(format!("export failed: {e}"))),
            }
        }
        "compact" => {
            // Real compaction: main_loop calls Agent::compact_history() on the
            // next tick, which summarizes the history and REPLACES it with the
            // summary — actually reclaiming context, unlike the old behavior
            // that just appended a summary and left the full history in place.
            if app.busy {
                app.blocks.push(Block::System(
                    "Can't compact mid-turn — wait for the current turn to finish.".into(),
                ));
            } else if app.compacting {
                app.blocks.push(Block::System("Already compacting…".into()));
            } else {
                app.pending_compact = true;
            }
        }
        _ => handle_slash_3(app, head, arg).await,
    }
}

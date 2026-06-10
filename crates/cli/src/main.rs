mod commands;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tomte",
    version,
    about = "Terminal coding agent in Rust (OpenAI Responses + Anthropic Messages)"
)]
struct Cli {
    /// Require plan mode before implementation. Hidden compatibility flag for
    /// teammate/spawn flows that pass it.
    #[arg(long, hide = true, global = true)]
    plan_mode_required: bool,
    /// Resume the most recent session in this directory without the picker
    /// (like `claude --continue`). Ignored when a subcommand is given; starts
    /// fresh if the directory has no saved session.
    #[arg(long = "continue", short = 'c')]
    continue_session: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sign in. With no flags, opens an interactive picker.
    Login {
        /// Use an API key instead of OAuth
        #[arg(long)]
        api_key: bool,
        /// Do not open the browser automatically
        #[arg(long)]
        no_browser: bool,
        /// Skip the picker: `openai` | `anthropic`. Pair with --api-key.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Show authentication status
    Status,
    /// Diagnose your setup — auth, config, model routing, MCP servers, and the
    /// external tools tomte depends on. Exits non-zero if any check fails.
    /// Runs headless (no TUI), so it works even when startup is broken.
    Doctor,
    /// Sign out and remove stored credentials
    Logout,
    /// Run a single chat turn (headless) — output printed to the terminal.
    /// Also the scheduler/cron entry point (alias: `run`): set `--cwd` and read
    /// the prompt from `--prompt-file` or stdin for an unattended invocation.
    #[command(visible_alias = "run")]
    Chat {
        /// Prompt (reads from --prompt-file or stdin if empty)
        prompt: Vec<String>,
        /// Model (defaults to the configured model)
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort: none | minimal | low | medium | high | xhigh | max
        #[arg(long)]
        reasoning: Option<String>,
        /// Output format: `text` (default, human readable) or `json`
        /// (one AgentEvent per line, suitable for scripting).
        #[arg(long, default_value = "text")]
        output_format: String,
        /// Working directory to run in. Defaults to the current directory.
        /// A scheduler (cron/systemd) runs with a bare environment, so set this
        /// explicitly for unattended runs.
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// Read the prompt from this file instead of the argument/stdin.
        #[arg(long)]
        prompt_file: Option<std::path::PathBuf>,
        /// Allow side-effecting tools (run_shell, file writes, MCP, …) to run
        /// without an approval prompt in this headless run. DANGEROUS: a
        /// prompt-injected model can then execute arbitrary commands. Without
        /// it, an unattended run is read-only — side-effecting tools are denied.
        #[arg(long)]
        dangerously_skip_permissions: bool,
        /// Sandbox enforcement for `run_shell` this run, overriding config/env:
        /// `read-only` | `workspace-write` | `danger-full-access`. Orthogonal to
        /// `--dangerously-skip-permissions` (which gates approval, not what a
        /// running command may touch).
        #[arg(long)]
        sandbox: Option<String>,
        /// Allow outbound network from sandboxed `run_shell` commands this run
        /// (only meaningful in `workspace-write`; off by default).
        #[arg(long)]
        sandbox_allow_net: bool,
    },
    /// Open the TUI with the resume-session picker open
    Resume,
    /// Inspect or update configuration
    Config {
        /// Show the current config
        #[arg(long)]
        show: bool,
        /// Set the default model
        #[arg(long)]
        set_model: Option<String>,
        /// Set the reasoning effort
        #[arg(long)]
        set_reasoning: Option<String>,
    },
    /// List the model lineup: every model tomte can drive, its context window
    /// and thinking capabilities, which credentials are present (presence
    /// only, never contents), the active model, and the failover chain an
    /// overload would walk.
    Models {
        /// Emit the report as JSON for scripting.
        #[arg(long)]
        json: bool,
    },
    /// Show the decision trail: why earlier changes were made, and by which
    /// model. `tomte why <file:line>` explains one location; `tomte why --all`
    /// lists the whole trail; `tomte why diff [base]` reviews the *reasoning*
    /// against a diff range — new decisions, superseded ones, and changed
    /// files with no recorded why. The agent records the trail with the
    /// `record_decision` tool, and it survives across sessions and model
    /// switches.
    Why {
        /// Code location to explain, e.g. `src/parser.rs:88` — or the word
        /// `diff` to review the reasoning against a base rev. Omit (or pass
        /// `--all`) to list the whole trail.
        loc: Option<String>,
        /// With `diff`: the base rev (e.g. `main`, `origin/main`, a commit).
        /// Omitted, the first of origin/main · main · origin/master · master
        /// that resolves is used.
        base: Option<String>,
        /// List every recorded decision.
        #[arg(long)]
        all: bool,
        /// Reconcile the trail against the working tree: heal decisions whose
        /// line drifted, and flag any whose code is gone. Pillar 5 — Drift Watch.
        #[arg(long)]
        reconcile: bool,
        /// Emit the trail (or the --reconcile report) as JSON instead of the
        /// rendered text — for scripting, piping, and CI drift-gates.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// List the decision trail for one file — every recorded decision in it,
    /// one per line and greppable (`tomte blame src/auth.rs | grep argon2`).
    /// The file-scoped, pipeable view of `tomte why`; `tomte why <file:line>`
    /// zooms into a single location, `tomte why --all` lists everything.
    Blame {
        /// File whose decisions to list, e.g. `src/parser.rs`. A `file:line` is
        /// accepted too — the line is ignored and the whole file is shown.
        file: String,
        /// Emit the file's decisions as a JSON array instead of the rendered,
        /// one-per-line text — for scripting and piping.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Print the cost receipt for a saved session — a per-model breakdown plus
    /// normalized OpenAI/Anthropic subtotals. Defaults to the newest session for
    /// this project; pass `--session <id>` to pick one, or `--all` to merge
    /// every saved session into one ledger. The headless `/cost`.
    Cost {
        /// Session id to report on (defaults to the newest for this project).
        #[arg(long)]
        session: Option<String>,
        /// Merge every saved session for this project into one report.
        #[arg(long, conflicts_with = "session")]
        all: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Manage lifecycle hooks — list, enable, or disable built-in presets that
    /// make tomte auto-trigger an action (e.g. `cargo fmt`) when it edits a
    /// file. Presets write to settings.json and run on Linux, macOS, and Windows.
    Hooks {
        #[command(subcommand)]
        action: Option<commands::hooks::HooksAction>,
    },
    /// Manage MCP (Model Context Protocol) servers — list, add, remove, or
    /// inspect the servers in settings.json without hand-editing JSON. Each
    /// server's tools are exposed to the agent as `mcp__<server>__<tool>`.
    Mcp {
        #[command(subcommand)]
        action: Option<commands::mcp::McpAction>,
    },
    /// Collect a Proof Capsule for the working tree — the files git reports
    /// changed plus the REAL exit codes of the project's own test / typecheck /
    /// lint / build scripts, which tomte runs itself (never the model's word).
    /// Exits non-zero if any check fails, so it can gate a commit or CI step.
    /// Runs headless (no TUI); the in-session companion is `/prove`.
    Prove {
        /// Emit the capsule as JSON instead of the human ✅/❌ card.
        #[arg(long)]
        json: bool,
        /// Working directory to verify. Defaults to the current directory.
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Build and inspect the Repo Twin — a verifiable map of the repository
    /// (file/import graph, symbol graph, test map, git recent-change map, and
    /// project conventions). With no flags it loads the cached index (building
    /// it on first use) and prints a summary; `--rebuild` forces a fresh scan.
    Twin {
        /// Rebuild the index from scratch instead of reusing the cache.
        #[arg(long)]
        rebuild: bool,
        /// Emit the summary as JSON instead of the rendered text.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Agent Tournament: run a task with several contestants (varying
    /// model/effort/style), each in its own git worktree, then judge them on
    /// evidence — the project's own tests, diff size, added coverage, and
    /// risky-command count — and pick the best patch. The judge is deterministic;
    /// an LLM is never the referee. `--apply` applies the winning patch.
    Race {
        /// The task, e.g. "fix the checkout bug".
        task: Vec<String>,
        /// Number of contestants (1–8).
        #[arg(long, default_value_t = 4)]
        agents: usize,
        /// Comma-separated models to spread contestants across (e.g.
        /// `claude-opus-4-8,gpt-5.5`). Defaults to the configured model.
        #[arg(long)]
        models: Option<String>,
        /// Apply the winning patch to the working tree when the race finishes.
        #[arg(long)]
        apply: bool,
        /// Emit the full report as JSON instead of the rendered card.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// The shift report: one paste-ready markdown capsule — git state, the
    /// newest recorded decisions with a drift-watch line, the Repo Twin
    /// summary, and the top of the Repo Pulse — collected from real state, so
    /// the next session (a colleague, tomorrow's you, or a different model
    /// entirely) picks the house up where this one left it.
    Handoff {
        /// Emit the capsule as JSON instead of markdown.
        #[arg(long)]
        json: bool,
        /// Write to a file (e.g. HANDOFF.md) instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// The work receipt: one Markdown/HTML/JSON artifact that proves a stretch
    /// of work — a fresh Proof Capsule (files changed + real check exit
    /// codes), whether HEAD carries a verified Commit Seal, what the session
    /// actually ran and edited (from the persisted session log), the cost
    /// receipt, and the newest recorded decisions. Attach it to a PR:
    /// evidence, not a transcript. Always renders (red or green) — the gates
    /// are `tomte prove` and `tomte seal verify`.
    Receipt {
        /// Emit the receipt as JSON instead of markdown.
        #[arg(long, conflicts_with = "html")]
        json: bool,
        /// Emit a standalone HTML page instead of markdown.
        #[arg(long)]
        html: bool,
        /// Session id to summarize (defaults to this project's newest session).
        #[arg(long)]
        session: Option<String>,
        /// Write to a file (e.g. RECEIPT.md) instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Repo Pulse: which files are most likely to break next, scored from the
    /// Repo Twin's own indexes — commits in the recent git window × import
    /// fan-in × 2 when no test covers the file. Every number on the card is a
    /// real index entry, so the verdict is reproducible. `--json` for scripts.
    Pulse {
        /// Rebuild the twin from scratch before scoring.
        #[arg(long)]
        rebuild: bool,
        /// Emit the report as JSON instead of the rendered card.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Commit Seal: notarize the Proof Capsule onto the commit itself. With no
    /// action it collects the capsule at a clean HEAD (the project's own
    /// test/typecheck/lint/build, real exit codes) and attaches it as a git
    /// note under refs/notes/tomte-seal, bound to the commit and tree ids — so
    /// the proof is pushed/fetched with the history it certifies. `seal show`
    /// reads a commit's seal back; `seal verify` exits 0 only when the commit
    /// is sealed and the sealed capsule is green, so it can gate CI.
    Seal {
        #[command(subcommand)]
        action: Option<commands::seal::SealAction>,
        /// Emit the freshly written seal as JSON instead of the rendered card.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Saved sessions for this project — the ledger behind `tomte resume` /
    /// `--continue`. Bare it lists them (newest first, with age, model, and
    /// preview); `sessions show [id]` prints one as a readable markdown
    /// transcript; `sessions prune --keep N` / `--older-than-days N` deletes
    /// old ones (dry-run unless --yes).
    Sessions {
        #[command(subcommand)]
        action: Option<commands::sessions::SessionsAction>,
        /// Emit the session list as JSON instead of the rendered text.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Night Rounds: the custodian's read-only inspection walk. Rebuilds the
    /// Repo Twin, diffs the Pulse against the last walk (risk risers, newly
    /// hot-and-untested files), reconciles the decision trail (drift watch),
    /// surfaces TODO/FIXME marks added since last rounds, and re-runs the
    /// project's own checks (the Proof Capsule pass — skip with `--no-proof`).
    /// Exits non-zero only when something is red — a decision whose line is
    /// gone, or a failed check — so it can run as a nightly CI morning gate.
    Rounds {
        /// Skip the proof pass (the project's test/typecheck/lint/build run).
        #[arg(long)]
        no_proof: bool,
        /// Emit the report as JSON instead of the rendered card.
        #[arg(long)]
        json: bool,
        /// Also write the rendered report to a file (e.g. rounds.md).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Emit a shell completion script for tomte (bash, zsh, fish, powershell,
    /// elvish) — generated from the same definition that parses the CLI, so
    /// it never drifts from the real commands and flags. Examples:
    /// `tomte completions bash > ~/.local/share/bash-completion/completions/tomte`;
    /// in PowerShell, add `tomte completions powershell | Out-String |
    /// Invoke-Expression` to $PROFILE.
    Completions {
        /// The shell to generate the script for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Context X-Ray: explain why a file or symbol is (or isn't) relevant. Pass a
    /// file (`src/auth/session.rs`), a stack-trace location (`src/x.rs:88`), or a
    /// symbol name (`createSession`). Prints the files the Repo Twin would pull
    /// into context — each with the index it came from — and the nearby files it
    /// deliberately leaves out, each with why it's unreachable.
    #[command(name = "why-context")]
    WhyContext {
        /// The seed: a file path, `file:line`, or symbol name.
        seed: String,
        /// Emit the full selection as JSON instead of the rendered card.
        #[arg(long)]
        json: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
}

fn init_tracing(stderr_logs: bool) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,tomte=info"));
    let config_root = tomte_core::config::config_dir();
    let log_dir = config_root.join("logs");
    // Harden the config tree to owner-only (0o700) at startup — this also covers
    // users who only use env-var auth and never hit the secure-dir path
    // elsewhere — then create the logs subdir the same way.
    let _ = tomte_core::config::create_dir_secure(&config_root);
    let _ = tomte_core::config::create_dir_secure(&log_dir);
    let log_path = log_dir.join(format!(
        "tomte-{}.log",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    let mut log_opts = std::fs::OpenOptions::new();
    log_opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_opts.mode(0o600);
    }
    let file = log_opts.open(&log_path).ok();
    // The full-screen TUI owns the terminal (alternate screen + raw mode), and
    // headless JSON mode promises stdout as one AgentEvent per line for scripts.
    // In those modes log to the file only; keep stderr for human one-shot
    // commands.
    let stderr_layer =
        stderr_logs.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer);
    if let Some(f) = file {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(f))
            .with_ansi(false);
        let _ = registry.with(file_layer).try_init();
    } else {
        let _ = registry.try_init();
    }
}

fn command_uses_json_output(command: &Option<Command>) -> bool {
    matches!(
        command,
        Some(Command::Chat { output_format, .. })
            if matches!(output_format.trim().to_ascii_lowercase().as_str(), "json" | "stream-json")
    )
}

/// True when a panic payload is the stdlib's print-macro abort after the read
/// end of the pipe closed early (`tomte twin | head`). Unix says
/// "Broken pipe (os error 32)"; Windows "The pipe is being closed. (os error
/// 232)". A consumer closing the pipe means it has all the output it wanted —
/// success for a CLI, not a crash.
fn is_broken_pipe_panic(message: &str) -> bool {
    message.contains("failed printing to")
        && (message.contains("Broken pipe") || message.contains("os error 232"))
}

/// Exit 0 instead of aborting with a panic when stdout/stderr is a pipe the
/// consumer closed early. Every evidence command (`twin`, `why-context`,
/// `prove --json`, `why`, `blame`) is built to be piped into `head`/`Select-
/// Object -First` in scripts and CI; without this they'd report a crash
/// (exit -1/101) for a completely routine shell pattern. Any other panic falls
/// through to the default hook untouched.
fn install_broken_pipe_exit() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied());
        if msg.is_some_and(is_broken_pipe_panic) {
            std::process::exit(0);
        }
        default_hook(info);
    }));
}

fn main() -> Result<()> {
    // If we were re-launched as the OS-sandbox helper (`__sandbox …`), apply the
    // sandbox to this process and exec the target command — this never returns on
    // success. A normal launch returns immediately. This MUST run before the async
    // runtime starts: the helper restricts itself with Landlock/seccomp and execs,
    // which is only sound while the process is still single-threaded.
    tomte_core::tools::shell::sandbox::maybe_exec_helper();
    install_broken_pipe_exit();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main())
}

async fn async_main() -> Result<()> {
    let cli = Cli::parse();

    // `tomte` and `tomte resume` launch the full-screen TUI, which owns the
    // terminal. JSON chat output is also machine-readable, so logs must not share
    // stderr with script consumers there either.
    let tui_mode = matches!(cli.command, None | Some(Command::Resume));
    let json_output_mode = command_uses_json_output(&cli.command);
    init_tracing(!tui_mode && !json_output_mode);

    match cli.command {
        None if cli.continue_session => tui::run_continue().await,
        None if cli.plan_mode_required => tui::run_plan_mode_required().await,
        None => tui::run().await,
        Some(Command::Login {
            api_key,
            no_browser,
            provider,
        }) => commands::login::run(api_key, !no_browser, provider).await,
        Some(Command::Status) => commands::login::status().await,
        Some(Command::Doctor) => commands::doctor::run().await,
        Some(Command::Logout) => commands::login::logout().await,
        Some(Command::Chat {
            prompt,
            model,
            reasoning,
            output_format,
            cwd,
            prompt_file,
            dangerously_skip_permissions,
            sandbox,
            sandbox_allow_net,
        }) => {
            commands::chat::run(
                prompt.join(" "),
                model,
                reasoning,
                output_format,
                cli.plan_mode_required,
                cwd,
                prompt_file,
                dangerously_skip_permissions,
                sandbox,
                sandbox_allow_net,
            )
            .await
        }
        Some(Command::Resume) if cli.plan_mode_required => {
            tui::run_resume_plan_mode_required().await
        }
        Some(Command::Resume) => tui::run_resume().await,
        Some(Command::Config {
            show,
            set_model,
            set_reasoning,
        }) => commands::config_cmd::run(show, set_model, set_reasoning).await,
        Some(Command::Models { json }) => commands::models::run(json).await,
        Some(Command::Why {
            loc,
            base,
            all,
            reconcile,
            json,
            cwd,
        }) => commands::why::run(loc, base, all, reconcile, json, cwd).await,
        Some(Command::Blame { file, json, cwd }) => commands::blame::run(file, json, cwd).await,
        Some(Command::Cost { session, all, cwd }) => commands::cost::run(session, all, cwd).await,
        Some(Command::Hooks { action }) => commands::hooks::run(action).await,
        Some(Command::Mcp { action }) => commands::mcp::run(action).await,
        Some(Command::Prove { json, cwd }) => commands::prove::run(json, cwd).await,
        Some(Command::Seal { action, json, cwd }) => commands::seal::run(action, json, cwd).await,
        Some(Command::Twin { rebuild, json, cwd }) => commands::twin::run(rebuild, json, cwd).await,
        Some(Command::Pulse { rebuild, json, cwd }) => {
            commands::pulse::run(rebuild, json, cwd).await
        }
        Some(Command::Handoff { json, out, cwd }) => commands::handoff::run(json, out, cwd).await,
        Some(Command::Receipt {
            json,
            html,
            session,
            out,
            cwd,
        }) => commands::receipt::run(json, html, session, out, cwd).await,
        Some(Command::Sessions { action, json, cwd }) => {
            commands::sessions::run(action, json, cwd).await
        }
        Some(Command::Rounds {
            no_proof,
            json,
            out,
            cwd,
        }) => commands::rounds::run(no_proof, json, out, cwd).await,
        Some(Command::WhyContext { seed, json, cwd }) => {
            commands::why_context::run(seed, json, cwd).await
        }
        Some(Command::Completions { shell }) => {
            commands::completions::run(shell, <Cli as clap::CommandFactory>::command())
        }
        Some(Command::Race {
            task,
            agents,
            models,
            apply,
            json,
            cwd,
        }) => commands::race::run(task, agents, models, apply, json, cwd).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // The broken-pipe classifier must catch the stdlib print-macro panic on
    // every OS wording and nothing else — a real bug's panic must still abort.
    #[test]
    fn broken_pipe_panic_classifier_matches_only_pipe_aborts() {
        // Unix wording (os error 32) and the Windows wording (os error 232).
        assert!(is_broken_pipe_panic(
            "failed printing to stdout: Broken pipe (os error 32)"
        ));
        assert!(is_broken_pipe_panic(
            "failed printing to stdout: The pipe is being closed. (os error 232)"
        ));
        // eprintln!'s stderr variant.
        assert!(is_broken_pipe_panic(
            "failed printing to stderr: Broken pipe (os error 32)"
        ));
        // Real panics must NOT be swallowed.
        assert!(!is_broken_pipe_panic("index out of bounds: the len is 3"));
        assert!(!is_broken_pipe_panic(
            "called `Option::unwrap()` on a `None` value"
        ));
        // A different printing failure (disk full) is not a pipe close.
        assert!(!is_broken_pipe_panic(
            "failed printing to stdout: No space left on device (os error 28)"
        ));
    }

    #[test]
    fn hidden_plan_mode_required_flag_parses_before_subcommand() {
        let cli =
            Cli::try_parse_from(["tomte", "--plan-mode-required", "chat", "inspect", "first"])
                .unwrap();

        assert!(cli.plan_mode_required);
        match cli.command {
            Some(Command::Chat { prompt, .. }) => {
                assert_eq!(prompt, vec!["inspect".to_string(), "first".to_string()]);
            }
            other => panic!("expected chat command, got {other:?}"),
        }
    }

    #[test]
    fn hidden_plan_mode_required_flag_parses_after_subcommand() {
        let cli =
            Cli::try_parse_from(["tomte", "chat", "--plan-mode-required", "inspect"]).unwrap();

        assert!(cli.plan_mode_required);
    }

    #[test]
    fn continue_flag_parses_with_no_subcommand() {
        let cli = Cli::try_parse_from(["tomte", "--continue"]).unwrap();
        assert!(cli.continue_session);
        assert!(cli.command.is_none());

        // Short form `-c`.
        let cli = Cli::try_parse_from(["tomte", "-c"]).unwrap();
        assert!(cli.continue_session);
        assert!(cli.command.is_none());

        // Off by default.
        let cli = Cli::try_parse_from(["tomte"]).unwrap();
        assert!(!cli.continue_session);
    }

    #[test]
    fn run_alias_parses_as_chat_with_cwd_and_prompt_file() {
        let cli = Cli::try_parse_from([
            "tomte",
            "run",
            "--cwd",
            "/tmp/project",
            "--prompt-file",
            "task.md",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Chat {
                cwd, prompt_file, ..
            }) => {
                assert_eq!(cwd, Some(std::path::PathBuf::from("/tmp/project")));
                assert_eq!(prompt_file, Some(std::path::PathBuf::from("task.md")));
            }
            other => panic!("expected chat command via `run` alias, got {other:?}"),
        }
    }

    #[test]
    fn dangerously_skip_permissions_defaults_off_and_parses_when_set() {
        let default_cli = Cli::try_parse_from(["tomte", "chat", "hi"]).unwrap();
        match default_cli.command {
            Some(Command::Chat {
                dangerously_skip_permissions,
                ..
            }) => assert!(
                !dangerously_skip_permissions,
                "unattended runs must default to the safe (gated) posture"
            ),
            other => panic!("expected chat command, got {other:?}"),
        }

        let opt_in = Cli::try_parse_from([
            "tomte",
            "run",
            "--dangerously-skip-permissions",
            "--cwd",
            "/tmp/p",
            "hi",
        ])
        .unwrap();
        match opt_in.command {
            Some(Command::Chat {
                dangerously_skip_permissions,
                ..
            }) => assert!(dangerously_skip_permissions),
            other => panic!("expected chat command, got {other:?}"),
        }
    }

    #[test]
    fn json_chat_output_disables_stderr_tracing() {
        let cli = Cli::try_parse_from(["tomte", "chat", "--output-format", "json", "hi"]).unwrap();
        assert!(command_uses_json_output(&cli.command));

        let stream_cli =
            Cli::try_parse_from(["tomte", "chat", "--output-format", "stream-json", "hi"]).unwrap();
        assert!(command_uses_json_output(&stream_cli.command));

        let text_cli =
            Cli::try_parse_from(["tomte", "chat", "--output-format", "text", "hi"]).unwrap();
        assert!(!command_uses_json_output(&text_cli.command));
    }

    #[test]
    fn blame_parses_file_and_cwd() {
        let cli = Cli::try_parse_from(["tomte", "blame", "--cwd", "/p", "src/auth.rs"]).unwrap();
        match cli.command {
            Some(Command::Blame { file, json, cwd }) => {
                assert_eq!(file, "src/auth.rs");
                assert!(!json, "blame defaults to rendered text, not JSON");
                assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
            }
            other => panic!("expected blame command, got {other:?}"),
        }
    }

    #[test]
    fn why_json_flag_parses_and_defaults_off() {
        let on = Cli::try_parse_from(["tomte", "why", "--all", "--json"]).unwrap();
        match on.command {
            Some(Command::Why { all, json, .. }) => {
                assert!(all);
                assert!(json, "--json must enable JSON output");
            }
            other => panic!("expected why command, got {other:?}"),
        }

        let off = Cli::try_parse_from(["tomte", "why", "src/x.rs:1"]).unwrap();
        match off.command {
            Some(Command::Why { json, .. }) => assert!(!json, "why defaults to rendered text"),
            other => panic!("expected why command, got {other:?}"),
        }
    }

    #[test]
    fn why_diff_parses_with_and_without_a_base() {
        let with_base = Cli::try_parse_from(["tomte", "why", "diff", "origin/main"]).unwrap();
        match with_base.command {
            Some(Command::Why { loc, base, .. }) => {
                assert_eq!(loc.as_deref(), Some("diff"));
                assert_eq!(base.as_deref(), Some("origin/main"));
            }
            other => panic!("expected why command, got {other:?}"),
        }

        let bare = Cli::try_parse_from(["tomte", "why", "diff"]).unwrap();
        match bare.command {
            Some(Command::Why { loc, base, .. }) => {
                assert_eq!(loc.as_deref(), Some("diff"));
                assert!(base.is_none(), "base defaults to auto-detection");
            }
            other => panic!("expected why command, got {other:?}"),
        }
    }

    #[test]
    fn models_parses_with_and_without_json() {
        let cli = Cli::try_parse_from(["tomte", "models", "--json"]).unwrap();
        match cli.command {
            Some(Command::Models { json }) => assert!(json),
            other => panic!("expected models command, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["tomte", "models"]).unwrap();
        match cli.command {
            Some(Command::Models { json }) => assert!(!json),
            other => panic!("expected models command, got {other:?}"),
        }
    }

    #[test]
    fn blame_json_flag_parses() {
        let cli = Cli::try_parse_from(["tomte", "blame", "--json", "src/auth.rs"]).unwrap();
        match cli.command {
            Some(Command::Blame { json, .. }) => assert!(json),
            other => panic!("expected blame command, got {other:?}"),
        }
    }

    #[test]
    fn rounds_flags_parse_and_default_to_proof_on() {
        let cli = Cli::try_parse_from([
            "tomte",
            "rounds",
            "--no-proof",
            "--json",
            "--out",
            "r.md",
            "--cwd",
            "/p",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Rounds {
                no_proof,
                json,
                out,
                cwd,
            }) => {
                assert!(no_proof);
                assert!(json);
                assert_eq!(out, Some(std::path::PathBuf::from("r.md")));
                assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
            }
            other => panic!("expected rounds command, got {other:?}"),
        }

        // Bare `tomte rounds` runs the proof pass — verification is the point.
        let bare = Cli::try_parse_from(["tomte", "rounds"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Rounds {
                no_proof: false,
                json: false,
                out: None,
                cwd: None
            })
        ));
    }

    #[test]
    fn receipt_flags_parse_and_json_conflicts_with_html() {
        let cli = Cli::try_parse_from([
            "tomte",
            "receipt",
            "--html",
            "--session",
            "s-1",
            "--out",
            "RECEIPT.html",
            "--cwd",
            "/p",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Receipt {
                json,
                html,
                session,
                out,
                cwd,
            }) => {
                assert!(!json);
                assert!(html);
                assert_eq!(session.as_deref(), Some("s-1"));
                assert_eq!(out, Some(std::path::PathBuf::from("RECEIPT.html")));
                assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
            }
            other => panic!("expected receipt command, got {other:?}"),
        }

        // Bare `tomte receipt` is markdown to stdout.
        let bare = Cli::try_parse_from(["tomte", "receipt"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Receipt {
                json: false,
                html: false,
                session: None,
                out: None,
                cwd: None
            })
        ));

        // One artifact, one format: --json and --html refuse to combine.
        assert!(Cli::try_parse_from(["tomte", "receipt", "--json", "--html"]).is_err());
    }

    #[test]
    fn seal_parses_bare_show_and_verify() {
        // Bare `tomte seal` writes a seal at HEAD; flags default off.
        let bare = Cli::try_parse_from(["tomte", "seal"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Seal {
                action: None,
                json: false,
                cwd: None
            })
        ));

        let json = Cli::try_parse_from(["tomte", "seal", "--json", "--cwd", "/p"]).unwrap();
        match json.command {
            Some(Command::Seal { action, json, cwd }) => {
                assert!(action.is_none());
                assert!(json);
                assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
            }
            other => panic!("expected seal command, got {other:?}"),
        }

        // `seal show [rev]` — rev optional, defaults to HEAD at run time.
        let show = Cli::try_parse_from(["tomte", "seal", "show", "abc123", "--json"]).unwrap();
        match show.command {
            Some(Command::Seal {
                action: Some(commands::seal::SealAction::Show { rev, json, .. }),
                ..
            }) => {
                assert_eq!(rev, Some("abc123".to_string()));
                assert!(json);
            }
            other => panic!("expected seal show, got {other:?}"),
        }

        // `seal verify` with no rev — the CI-gate spelling.
        let verify = Cli::try_parse_from(["tomte", "seal", "verify"]).unwrap();
        assert!(matches!(
            verify.command,
            Some(Command::Seal {
                action: Some(commands::seal::SealAction::Verify {
                    rev: None,
                    json: false,
                    cwd: None
                }),
                ..
            })
        ));
    }

    #[test]
    fn cost_parses_session_and_cwd() {
        let cli =
            Cli::try_parse_from(["tomte", "cost", "--session", "abc-123", "--cwd", "/p"]).unwrap();
        match cli.command {
            Some(Command::Cost { session, all, cwd }) => {
                assert_eq!(session, Some("abc-123".to_string()));
                assert!(!all, "--all defaults off");
                assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
            }
            other => panic!("expected cost command, got {other:?}"),
        }
        // All flags are optional — `tomte cost` alone defaults to the newest session.
        let bare = Cli::try_parse_from(["tomte", "cost"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Cost {
                session: None,
                all: false,
                cwd: None
            })
        ));
    }

    #[test]
    fn cost_all_parses_and_conflicts_with_session() {
        let cli = Cli::try_parse_from(["tomte", "cost", "--all"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Cost {
                session: None,
                all: true,
                cwd: None
            })
        ));
        // One report, one scope: --all and --session refuse to combine.
        assert!(Cli::try_parse_from(["tomte", "cost", "--all", "--session", "s-1"]).is_err());
    }

    #[test]
    fn completions_parses_known_shells_and_rejects_unknown() {
        use clap_complete::Shell;
        for (name, want) in [
            ("bash", Shell::Bash),
            ("zsh", Shell::Zsh),
            ("fish", Shell::Fish),
            ("powershell", Shell::PowerShell),
            ("elvish", Shell::Elvish),
        ] {
            let cli = Cli::try_parse_from(["tomte", "completions", name]).unwrap();
            match cli.command {
                Some(Command::Completions { shell }) => assert_eq!(shell, want),
                other => panic!("expected completions command, got {other:?}"),
            }
        }
        // An unknown shell is a parse error with the choices listed, not a panic.
        assert!(Cli::try_parse_from(["tomte", "completions", "tcsh"]).is_err());
        // The shell argument is required.
        assert!(Cli::try_parse_from(["tomte", "completions"]).is_err());
    }

    #[test]
    fn completions_scripts_generate_for_every_shell() {
        use clap::CommandFactory;
        use clap_complete::Shell;
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let mut buf = Vec::new();
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "tomte", &mut buf);
            let script = String::from_utf8(buf).expect("script is utf-8");
            assert!(
                script.contains("tomte"),
                "{shell:?} script must mention the binary"
            );
            // The script must know the subcommands, not just the binary name.
            assert!(
                script.contains("sessions") && script.contains("prove"),
                "{shell:?} script must cover the command surface"
            );
        }
    }

    #[test]
    fn sessions_bare_lists_and_json_flag_parses() {
        let bare = Cli::try_parse_from(["tomte", "sessions"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Sessions {
                action: None,
                json: false,
                cwd: None
            })
        ));

        let json = Cli::try_parse_from(["tomte", "sessions", "--json", "--cwd", "/p"]).unwrap();
        match json.command {
            Some(Command::Sessions { action, json, cwd }) => {
                assert!(action.is_none());
                assert!(json);
                assert_eq!(cwd, Some(std::path::PathBuf::from("/p")));
            }
            other => panic!("expected sessions command, got {other:?}"),
        }
    }

    #[test]
    fn sessions_show_parses_optional_id_and_out() {
        let with_id =
            Cli::try_parse_from(["tomte", "sessions", "show", "abc-1", "--out", "t.md"]).unwrap();
        match with_id.command {
            Some(Command::Sessions {
                action: Some(commands::sessions::SessionsAction::Show { id, json, out, .. }),
                ..
            }) => {
                assert_eq!(id.as_deref(), Some("abc-1"));
                assert!(!json, "show defaults to the markdown transcript");
                assert_eq!(out, Some(std::path::PathBuf::from("t.md")));
            }
            other => panic!("expected sessions show, got {other:?}"),
        }

        // Bare `sessions show` — newest session, stdout.
        let bare = Cli::try_parse_from(["tomte", "sessions", "show"]).unwrap();
        assert!(matches!(
            bare.command,
            Some(Command::Sessions {
                action: Some(commands::sessions::SessionsAction::Show {
                    id: None,
                    json: false,
                    out: None,
                    cwd: None
                }),
                ..
            })
        ));
    }

    #[test]
    fn sessions_prune_parses_rules_and_defaults_to_dry_run() {
        let cli = Cli::try_parse_from([
            "tomte",
            "sessions",
            "prune",
            "--keep",
            "5",
            "--older-than-days",
            "30",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Sessions {
                action:
                    Some(commands::sessions::SessionsAction::Prune {
                        keep,
                        older_than_days,
                        yes,
                        ..
                    }),
                ..
            }) => {
                assert_eq!(keep, Some(5));
                assert_eq!(older_than_days, Some(30));
                assert!(!yes, "prune must default to the dry run");
            }
            other => panic!("expected sessions prune, got {other:?}"),
        }

        let armed =
            Cli::try_parse_from(["tomte", "sessions", "prune", "--keep", "3", "--yes"]).unwrap();
        match armed.command {
            Some(Command::Sessions {
                action: Some(commands::sessions::SessionsAction::Prune { yes, .. }),
                ..
            }) => assert!(yes),
            other => panic!("expected sessions prune, got {other:?}"),
        }
    }

    #[test]
    fn mcp_add_parses_name_env_and_trailing_command() {
        let cli = Cli::try_parse_from([
            "tomte", "mcp", "add", "fs", "--env", "K=V", "--", "npx", "-y", "server", "/tmp",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Mcp {
                action: Some(commands::mcp::McpAction::Add { name, env, command }),
            }) => {
                assert_eq!(name, "fs");
                assert_eq!(env, vec!["K=V".to_string()]);
                assert_eq!(
                    command,
                    ["npx", "-y", "server", "/tmp"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                );
            }
            other => panic!("expected mcp add, got {other:?}"),
        }
    }

    #[test]
    fn mcp_remove_accepts_rm_alias_and_bare_lists() {
        let aliased = Cli::try_parse_from(["tomte", "mcp", "rm", "filesystem"]).unwrap();
        assert!(matches!(
            aliased.command,
            Some(Command::Mcp {
                action: Some(commands::mcp::McpAction::Remove { .. })
            })
        ));
        // `tomte mcp` with no action defaults to the list view.
        let bare = Cli::try_parse_from(["tomte", "mcp"]).unwrap();
        assert!(matches!(bare.command, Some(Command::Mcp { action: None })));
    }
}

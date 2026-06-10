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
        /// After the turn completes, collect a Proof Capsule — run the
        /// project's own verification checks (test / typecheck / lint / build)
        /// and append the ✅/❌ card, exiting non-zero when a check fails, so
        /// an unattended run can be gated on "done means verified". With
        /// `--output-format json` the capsule is the final line, tagged
        /// `"kind":"proof_capsule"`.
        #[arg(long)]
        prove: bool,
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
            prove,
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
                prove,
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
mod tests;

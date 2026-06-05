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
    /// Claude Code-style teammate/spawn flows.
    #[arg(long, hide = true, global = true)]
    plan_mode_required: bool,
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
    /// Show the decision trail: why earlier changes were made, and by which
    /// model. `tomte why <file:line>` explains one location; `tomte why --all`
    /// lists the whole trail. The agent records it with the `record_decision`
    /// tool, and it survives across sessions and model switches.
    Why {
        /// Code location to explain, e.g. `src/parser.rs:88`. Omit (or pass
        /// `--all`) to list the whole trail.
        loc: Option<String>,
        /// List every recorded decision.
        #[arg(long)]
        all: bool,
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

fn main() -> Result<()> {
    // If we were re-launched as the OS-sandbox helper (`__sandbox …`), apply the
    // sandbox to this process and exec the target command — this never returns on
    // success. A normal launch returns immediately. This MUST run before the async
    // runtime starts: the helper restricts itself with Landlock/seccomp and execs,
    // which is only sound while the process is still single-threaded.
    tomte_core::tools::shell::sandbox::maybe_exec_helper();
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
        Some(Command::Why { loc, all, cwd }) => commands::why::run(loc, all, cwd).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
}

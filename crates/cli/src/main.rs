mod commands;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "opencli",
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
    /// Sign out and remove stored credentials
    Logout,
    /// Run a single chat turn (headless) — output printed to the terminal
    Chat {
        /// Prompt (reads from stdin if empty)
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
}

fn init_tracing(tui_mode: bool) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,opencli=info"));
    let log_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("opencli")
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join(format!(
        "opencli-{}.log",
        chrono::Utc::now().format("%Y-%m-%d")
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    // The full-screen TUI owns the terminal (alternate screen + raw mode);
    // writing logs to stderr there scribbles over the ratatui display and
    // desyncs its diff renderer (stale, overlapping text). In TUI mode log to
    // the file only; keep stderr for one-shot commands.
    let stderr_layer =
        (!tui_mode).then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // `opencli` and `opencli resume` launch the full-screen TUI, which owns the
    // terminal — logs must go to the file only there, not stderr.
    let tui_mode = matches!(cli.command, None | Some(Command::Resume));
    init_tracing(tui_mode);

    match cli.command {
        None if cli.plan_mode_required => tui::run_plan_mode_required().await,
        None => tui::run().await,
        Some(Command::Login {
            api_key,
            no_browser,
            provider,
        }) => commands::login::run(api_key, !no_browser, provider).await,
        Some(Command::Status) => commands::login::status().await,
        Some(Command::Logout) => commands::login::logout().await,
        Some(Command::Chat {
            prompt,
            model,
            reasoning,
            output_format,
        }) => {
            commands::chat::run(
                prompt.join(" "),
                model,
                reasoning,
                output_format,
                cli.plan_mode_required,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn hidden_plan_mode_required_flag_parses_before_subcommand() {
        let cli = Cli::try_parse_from([
            "opencli",
            "--plan-mode-required",
            "chat",
            "inspect",
            "first",
        ])
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
            Cli::try_parse_from(["opencli", "chat", "--plan-mode-required", "inspect"]).unwrap();

        assert!(cli.plan_mode_required);
    }
}

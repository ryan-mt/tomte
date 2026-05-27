mod commands;
mod server;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "opencli",
    version,
    about = "CLI coding agent using the OpenAI Responses API (Rust + React UI)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Port for the web UI (default 7777)
    #[arg(long, global = true, default_value_t = 7777)]
    port: u16,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in (defaults to OAuth via ChatGPT in the browser)
    Login {
        /// Use an API key instead of OAuth
        #[arg(long)]
        api_key: bool,
        /// Do not open the browser automatically
        #[arg(long)]
        no_browser: bool,
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
        /// Reasoning effort: low | medium | high | xhigh
        #[arg(long)]
        reasoning: Option<String>,
        /// Output format: `text` (default, human readable) or `json`
        /// (one AgentEvent per line, suitable for scripting).
        #[arg(long, default_value = "text")]
        output_format: String,
    },
    /// Start the Web UI (React) in the browser
    Web {
        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,
    },
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

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,opencli=info")),
        )
        .try_init();

    let cli = Cli::parse();

    match cli.command {
        None => tui::run().await,
        Some(Command::Login { api_key, no_browser }) => commands::login::run(api_key, !no_browser).await,
        Some(Command::Status) => commands::login::status().await,
        Some(Command::Logout) => commands::login::logout().await,
        Some(Command::Chat { prompt, model, reasoning, output_format }) => {
            commands::chat::run(prompt.join(" "), model, reasoning, output_format).await
        }
        Some(Command::Web { no_open }) => commands::ui::run(cli.port, !no_open).await,
        Some(Command::Config { show, set_model, set_reasoning }) => {
            commands::config_cmd::run(show, set_model, set_reasoning).await
        }
    }
}

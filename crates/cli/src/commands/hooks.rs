use anyhow::Result;
use tomte_core::hooks::{self, presets};

/// `tomte hooks <action>` — manage built-in lifecycle-hook presets.
#[derive(Debug, clap::Subcommand)]
pub enum HooksAction {
    /// List available presets and whether each is enabled (the default action).
    List,
    /// Enable a preset by id, writing it into settings.json.
    Enable {
        /// Preset id, e.g. `rustfmt` (see `tomte hooks list`).
        id: String,
    },
    /// Disable a preset by id, removing it from settings.json.
    Disable {
        /// Preset id, e.g. `rustfmt`.
        id: String,
    },
}

pub async fn run(action: Option<HooksAction>) -> Result<()> {
    match action.unwrap_or(HooksAction::List) {
        HooksAction::List => list(),
        HooksAction::Enable { id } => {
            match presets::enable(&id)? {
                presets::Change::Applied => println!("✓ enabled preset `{id}`"),
                presets::Change::NoOp => println!("· preset `{id}` was already enabled"),
            }
            println!("  {}", hooks::settings_path().display());
            Ok(())
        }
        HooksAction::Disable { id } => {
            match presets::disable(&id)? {
                presets::Change::Applied => println!("✓ disabled preset `{id}`"),
                presets::Change::NoOp => println!("· preset `{id}` was not enabled"),
            }
            Ok(())
        }
    }
}

fn list() -> Result<()> {
    println!("Hook presets — tomte auto-runs these when it edits a matching file:\n");
    for (p, on) in presets::status() {
        let mark = if on { "on " } else { "off" };
        println!("  [{mark}] {:<9} {}  {}", p.id, p.event.key(), p.matcher);
        println!("            {}", p.description);
        println!("            $ {}", p.command);
    }
    println!("\nEnable:  tomte hooks enable <id>");
    println!("Disable: tomte hooks disable <id>");

    let cfg = hooks::load().config;
    let total = cfg.pre_tool_use.len()
        + cfg.post_tool_use.len()
        + cfg.user_prompt_submit.len()
        + cfg.session_start.len()
        + cfg.stop.len();
    println!(
        "\n{total} hook{} configured in {}",
        if total == 1 { "" } else { "s" },
        hooks::settings_path().display()
    );
    Ok(())
}

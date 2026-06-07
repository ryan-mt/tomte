//! `tomte cost` — print the cost receipt for a saved session: a per-model
//! breakdown plus normalized OpenAI/Anthropic subtotals (the cross-provider
//! receipt). The headless companion to the TUI `/cost`; reads the persisted
//! per-model `ModelUsage`, so it works after the session has ended.

use anyhow::{anyhow, Result};
use tomte_core::openai::InputItem;
use tomte_core::pricing::render_cost_report;
use tomte_core::session;

pub async fn run(id: Option<String>, cwd: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir).map_err(|e| anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    // Default to the newest session for this project; `--session` picks one.
    let id = match id {
        Some(id) => id,
        None => session::list(&here)
            .first()
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow!("no saved sessions for this project yet"))?,
    };
    let record =
        session::load(&here, &id).map_err(|e| anyhow!("could not load session {id}: {e}"))?;

    // The snapshot stores per-model usage, not a turn count — approximate turns
    // by the user messages in the history (one user message ≈ one turn).
    let turns = record
        .history
        .iter()
        .filter(|item| matches!(item, InputItem::Message { role, .. } if role == "user"))
        .count() as u64;

    println!(
        "{}",
        render_cost_report(&record.state.usage, &record.meta.model, turns)
    );
    Ok(())
}

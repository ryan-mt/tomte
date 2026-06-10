//! `tomte cost` — print the cost receipt for a saved session: a per-model
//! breakdown plus normalized OpenAI/Anthropic subtotals (the cross-provider
//! receipt). The headless companion to the TUI `/cost`; reads the persisted
//! per-model `ModelUsage`, so it works after the session has ended. `--all`
//! merges every saved session for this project into one ledger.

use anyhow::{anyhow, Result};
use tomte_core::openai::InputItem;
use tomte_core::pricing::{merge_usage, render_cost_report};
use tomte_core::session;

pub async fn run(id: Option<String>, all: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir).map_err(|e| anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    if all {
        return run_all(&here);
    }

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

    println!(
        "{}",
        render_cost_report(
            &record.state.usage,
            &record.meta.model,
            user_turns(&record.history)
        )
    );
    Ok(())
}

/// The all-sessions ledger: load every saved session for this project, merge
/// the per-model usage, and render one combined report. A session file that
/// no longer loads is skipped (same posture as `session::list`).
fn run_all(here: &std::path::Path) -> Result<()> {
    let metas = session::list(here);
    if metas.is_empty() {
        return Err(anyhow!("no saved sessions for this project yet"));
    }
    let mut parts: Vec<Vec<tomte_core::session::ModelUsage>> = Vec::new();
    let mut turns = 0u64;
    for m in &metas {
        let Ok(record) = session::load(here, &m.id) else {
            continue;
        };
        turns = turns.saturating_add(user_turns(&record.history));
        parts.push(record.state.usage);
    }
    let merged = merge_usage(parts.iter().map(|v| v.as_slice()));
    println!(
        "All saved sessions for this project — {} session{}\n",
        parts.len(),
        if parts.len() == 1 { "" } else { "s" }
    );
    println!("{}", render_cost_report(&merged, &metas[0].model, turns));
    Ok(())
}

/// Approximate the user-visible turn count: the snapshot stores per-model
/// usage, not turns, so count user messages (one user message ≈ one turn).
fn user_turns(history: &[InputItem]) -> u64 {
    history
        .iter()
        .filter(|item| matches!(item, InputItem::Message { role, .. } if role == "user"))
        .count() as u64
}

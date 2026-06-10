//! `tomte sessions` — the saved-session ledger, headless. Bare it lists this
//! project's persisted sessions (the same store `tomte resume` / `--continue`
//! read), `sessions show [id]` prints one as a readable markdown transcript,
//! and `sessions prune` deletes old ones — dry-run by default, `--yes` to
//! actually delete. All selection/rendering logic lives in
//! `tomte_core::sessions_report` where it is unit-tested.

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use tomte_core::{session, sessions_report};

#[derive(Debug, Subcommand)]
pub enum SessionsAction {
    /// Print one session as a readable markdown transcript — messages in
    /// full, each tool call as a one-line note, tool outputs omitted.
    Show {
        /// Session id (from `tomte sessions`). Defaults to the newest.
        id: Option<String>,
        /// Emit the full session record as JSON instead (history, todos,
        /// per-model usage — everything the store carries).
        #[arg(long)]
        json: bool,
        /// Write to a file (e.g. transcript.md) instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
    /// Delete old sessions for this project. Dry-run by default — the plan is
    /// printed and nothing is touched; `--yes` performs the deletion. At
    /// least one rule (--keep / --older-than-days) is required.
    Prune {
        /// Keep only the newest N sessions; the rest are selected.
        #[arg(long)]
        keep: Option<usize>,
        /// Select sessions last updated more than N days ago.
        #[arg(long)]
        older_than_days: Option<u64>,
        /// Actually delete the selected sessions.
        #[arg(long)]
        yes: bool,
        /// Working directory (defaults to the current directory).
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
}

pub async fn run(
    action: Option<SessionsAction>,
    json: bool,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    match action {
        None => list(json, cwd),
        Some(SessionsAction::Show { id, json, out, cwd }) => show(id, json, out, cwd),
        Some(SessionsAction::Prune {
            keep,
            older_than_days,
            yes,
            cwd,
        }) => prune(keep, older_than_days, yes, cwd),
    }
}

fn enter(cwd: Option<std::path::PathBuf>) -> Result<std::path::PathBuf> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir).map_err(|e| anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    Ok(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}

fn list(json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    let here = enter(cwd)?;
    let metas = session::list(&here);
    if json {
        println!("{}", serde_json::to_string_pretty(&metas)?);
    } else {
        println!(
            "{}",
            sessions_report::render_list(&metas, session::now_ms())
        );
    }
    Ok(())
}

fn show(
    id: Option<String>,
    json: bool,
    out: Option<std::path::PathBuf>,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    let here = enter(cwd)?;
    let id = match id {
        Some(id) => id,
        None => session::list(&here)
            .first()
            .map(|m| m.id.clone())
            .ok_or_else(|| anyhow!("no saved sessions for this project yet"))?,
    };
    let record =
        session::load(&here, &id).map_err(|e| anyhow!("could not load session {id}: {e}"))?;
    let body = if json {
        serde_json::to_string_pretty(&record)?
    } else {
        sessions_report::render_transcript(&record)
    };
    match out {
        Some(path) => {
            std::fs::write(&path, &body).map_err(|e| anyhow!("--out {}: {e}", path.display()))?;
            println!("session {id} written to {}", path.display());
        }
        None => println!("{body}"),
    }
    Ok(())
}

fn prune(
    keep: Option<usize>,
    older_than_days: Option<u64>,
    yes: bool,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    if keep.is_none() && older_than_days.is_none() {
        bail!(
            "pass at least one rule: --keep <N> and/or --older-than-days <N> \
             (a bare prune selects nothing)"
        );
    }
    let here = enter(cwd)?;
    let metas = session::list(&here);
    if metas.is_empty() {
        println!("No saved sessions for this project — nothing to prune.");
        return Ok(());
    }
    let now = session::now_ms();
    let victims = sessions_report::plan_prune(&metas, keep, older_than_days, now);
    println!(
        "{}",
        sessions_report::render_prune_plan(&victims, metas.len(), !yes, now)
    );
    if !yes || victims.is_empty() {
        return Ok(());
    }
    let mut deleted = 0usize;
    let mut failed = 0usize;
    for m in &victims {
        match session::delete(&here, &m.id) {
            Ok(()) => deleted += 1,
            Err(e) => {
                failed += 1;
                eprintln!("could not delete {}: {e}", m.id);
            }
        }
    }
    println!(
        "deleted {deleted} session{} · {} kept",
        if deleted == 1 { "" } else { "s" },
        metas.len() - deleted
    );
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

//! `tomte rounds` — Night Rounds, the custodian's read-only inspection walk.
//!
//! Headless companion to nothing: rounds is CLI-first by design — it's the
//! command a nightly cron or CI job runs. It rebuilds the Repo Twin, diffs the
//! Pulse against the last walk, reconciles the decision trail, lists TODO marks
//! added since last rounds, and (unless `--no-proof`) re-runs the project's own
//! checks. Exits non-zero only when something is red — a decision whose
//! anchored line is gone/ambiguous, or a failed check — the morning gate.

use anyhow::{anyhow, Result};
use tomte_core::rounds;

pub async fn run(
    no_proof: bool,
    json: bool,
    out: Option<std::path::PathBuf>,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir).map_err(|e| anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let report = rounds::collect(&here, !no_proof).await;
    let body = if json {
        serde_json::to_string_pretty(&report)?
    } else {
        rounds::render(&report)
    };

    match &out {
        Some(path) => {
            std::fs::write(path, &body).map_err(|e| anyhow!("--out {}: {e}", path.display()))?;
            println!("rounds report written to {}", path.display());
        }
        None => println!("{body}"),
    }

    // Red findings close the morning gate so a nightly CI run can hold the
    // door; an amber walk (new TODOs, risers) still exits 0.
    if report.red() {
        std::process::exit(1);
    }
    Ok(())
}

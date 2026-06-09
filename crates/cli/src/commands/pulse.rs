//! `tomte pulse` — the hearth report: which files are most likely to break
//! next, scored from the Repo Twin's own indexes (change heat × import fan-in
//! × missing tests). Deterministic — rerun it, get the same card.

use anyhow::Result;
use tomte_core::repo_twin;

pub async fn run(rebuild: bool, json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let twin = if rebuild {
        repo_twin::rebuild(&here)?
    } else {
        repo_twin::load_or_build(&here)?
    };
    let report = repo_twin::pulse::pulse(&twin);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", repo_twin::pulse::render(&report));
    }
    Ok(())
}

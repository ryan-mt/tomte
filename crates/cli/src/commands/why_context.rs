//! `tomte why-context <seed>` — the Context X-Ray.
//!
//! Given a seed — a file, a `file:line` from a stack trace, or a symbol name —
//! it prints the files the Repo Twin says are relevant (each with the index it
//! came from) and the nearby files it leaves out (each with why it's
//! unreachable). The index builds on first use and is cached. `--json` emits the
//! full selection for scripting.

use anyhow::Result;
use tomte_core::repo_twin;

pub async fn run(seed: String, json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let twin = repo_twin::load_or_build(&here)?;
    let selection = repo_twin::select::why_context(&twin, &here, &seed);

    if json {
        println!("{}", serde_json::to_string_pretty(&selection)?);
    } else {
        println!("{}", repo_twin::select::render(&selection));
    }
    Ok(())
}

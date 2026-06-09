//! `tomte race "<task>"` — the Agent Tournament.
//!
//! Runs the task with several contestants (varying model / effort / style), each
//! in its own git worktree, then judges them by evidence — the project's own
//! tests, diff size, added coverage, and risky-command count — and prints the
//! winner. The judge is deterministic; an LLM is never the referee. `--apply`
//! applies the winning patch to the working tree.

use anyhow::Result;
use tomte_core::race::{self, RaceOptions};

pub async fn run(
    task: Vec<String>,
    agents: usize,
    models: Option<String>,
    apply: bool,
    json: bool,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    let task = task.join(" ");
    if task.trim().is_empty() {
        anyhow::bail!("provide a task, e.g. tomte race \"fix the checkout bug\" --agents 4");
    }
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let models: Vec<String> = models
        .map(|s| {
            s.split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let opts = RaceOptions {
        agents,
        models,
        apply,
    };

    let report = race::run_race(&here, &task, &opts).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", race::score::render(&report));
    }
    Ok(())
}

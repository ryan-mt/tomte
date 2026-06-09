//! `tomte race "<task>"` — the Agent Tournament.
//!
//! Runs the task with several contestants (varying model / effort / style), each
//! in its own git worktree, then judges them by evidence — the project's own
//! tests, diff size, added coverage, and risky-command count — and prints the
//! winner. The judge is deterministic; an LLM is never the referee. `--apply`
//! applies the winning patch to the working tree.

use anyhow::Result;
use tomte_core::race::{self, RaceEvent, RaceOptions};

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

    // Narrate progress on stderr — a race can run many minutes, and stdout must
    // stay clean for `--json | jq`-style piping.
    let t0 = std::time::Instant::now();
    let progress = move |ev: RaceEvent| {
        let t = t0.elapsed().as_secs();
        match ev {
            RaceEvent::UnconfinedPlatform => eprintln!(
                "⚠ no OS sandbox on this platform: contestants are isolated in git \
                 worktrees and dangerous commands stay blocked, but other shell side \
                 effects are not filesystem/network-confined"
            ),
            RaceEvent::Starting { contestants } => eprintln!(
                "🏁 {contestants} contestant(s) racing from HEAD in isolated worktrees — \
                 this runs the task AND the project's checks per contestant…"
            ),
            RaceEvent::WorktreeFailed { label, error } => {
                eprintln!("[{t:>4}s] {label}: {error}")
            }
            RaceEvent::AgentStarted { label, model } => {
                eprintln!("[{t:>4}s] {label} ({model}) started")
            }
            RaceEvent::AgentFinished {
                label,
                secs,
                error: None,
            } => eprintln!("[{t:>4}s] {label} finished its run in {secs}s"),
            RaceEvent::AgentFinished {
                label,
                secs,
                error: Some(e),
            } => eprintln!("[{t:>4}s] {label} failed after {secs}s: {e}"),
            RaceEvent::Verifying { label } => {
                eprintln!("[{t:>4}s] {label} verifying — running the project's own checks…")
            }
            RaceEvent::Verified {
                label,
                passed,
                failed,
            } => eprintln!("[{t:>4}s] {label} checks: {passed} passed, {failed} failed"),
        }
    };

    let report = race::run_race(&here, &task, &opts, &progress).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", race::score::render(&report));
    }
    Ok(())
}

//! `tomte handoff` — the shift report: one paste-ready markdown capsule (git
//! state, newest decisions + drift watch, twin summary, pulse top) collected
//! from real state so the next session — human or a different model — picks
//! the house up where this one left it.

use anyhow::Result;
use tomte_core::handoff;

pub async fn run(
    json: bool,
    out: Option<std::path::PathBuf>,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let capsule = handoff::collect(&here);
    let body = if json {
        serde_json::to_string_pretty(&capsule)?
    } else {
        handoff::render_markdown(&capsule)
    };

    match out {
        Some(path) => {
            std::fs::write(&path, &body)
                .map_err(|e| anyhow::anyhow!("--out {}: {e}", path.display()))?;
            println!("handoff written to {}", path.display());
        }
        None => println!("{body}"),
    }
    Ok(())
}

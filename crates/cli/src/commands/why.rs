//! `tomte why <loc>` / `tomte why --all` — read the project's decision trail:
//! why earlier changes were made, and by which model. The human-facing side of
//! Pillar 2; the agent writes the trail with the `record_decision` tool.

use anyhow::Result;
use tomte_core::decisions;

pub async fn run(
    loc: Option<String>,
    all: bool,
    reconcile: bool,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    if reconcile {
        let report = decisions::reconcile(&here);
        println!("{}", decisions::render_reconcile(&report));
        return Ok(());
    }

    match loc {
        Some(loc) if !all => {
            let records = decisions::for_loc_live(&here, &loc);
            println!("{}", decisions::render_for_loc(&records, &loc));
        }
        _ => {
            let records = decisions::load(&here);
            println!("{}", decisions::render_all(&records));
        }
    }
    Ok(())
}

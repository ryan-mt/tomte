//! `tomte why <loc>` / `tomte why --all` — read the project's decision trail:
//! why earlier changes were made, and by which model. The human-facing side of
//! Pillar 2; the agent writes the trail with the `record_decision` tool.
//! `--json` emits the same data machine-readably for scripting and CI.

use anyhow::Result;
use tomte_core::decisions;

pub async fn run(
    loc: Option<String>,
    all: bool,
    reconcile: bool,
    json: bool,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    if reconcile {
        let report = decisions::reconcile(&here);
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("{}", decisions::render_reconcile(&report));
        }
        return Ok(());
    }

    let records = match &loc {
        Some(loc) if !all => decisions::for_loc_live(&here, loc),
        _ => decisions::load(&here),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }
    match loc {
        Some(loc) if !all => println!("{}", decisions::render_for_loc(&records, &loc)),
        _ => println!("{}", decisions::render_all(&records)),
    }
    Ok(())
}

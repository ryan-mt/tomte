//! `tomte twin` — build and inspect the Repo Twin (Context X-Ray index).
//!
//! With no flags it loads the cached index (building it on first use) and prints
//! a one-glance summary of the five indexes. `--rebuild` forces a fresh scan;
//! `--json` emits the summary machine-readably for scripting/CI.

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

    if json {
        println!("{}", serde_json::to_string_pretty(&twin.summary())?);
        return Ok(());
    }

    println!("{}", twin.render_summary());
    Ok(())
}

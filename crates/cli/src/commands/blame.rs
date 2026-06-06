//! `tomte blame <file>` — the decision trail for one file, one decision per line
//! and greppable (`tomte blame src/auth.rs | grep argon2`). The pipeable,
//! file-scoped view of Pillar 2's trail; `tomte why <file:line>` zooms into a
//! single location and `tomte why --all` lists everything.

use anyhow::Result;
use tomte_core::decisions;

pub async fn run(file: String, cwd: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let records = decisions::for_file(&here, &file);
    println!("{}", decisions::render_blame(&records, &file));
    Ok(())
}

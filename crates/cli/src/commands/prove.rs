//! `tomte prove` — collect and print a Proof Capsule for the working tree.
//!
//! The headless companion to the in-session `/prove`. It runs the project's own
//! verification scripts (test / typecheck / lint / build) itself, records the
//! real exit codes, and prints the ✅/❌ card — then exits non-zero if anything
//! failed, so a commit hook or CI step can refuse work that isn't actually
//! verified. The proof is gathered by the CLI; the model is never in the loop.

use anyhow::{anyhow, Result};
use tomte_core::proof;

pub async fn run(json: bool, cwd: Option<std::path::PathBuf>) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir).map_err(|e| anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let capsule = proof::collect(&here).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&capsule)?);
    } else {
        println!("{}", capsule.render());
    }

    // A failed (or errored) check makes the run non-zero so it can gate a script;
    // a clean tree with nothing to verify still exits 0 (nothing failed).
    if !capsule.verified() {
        std::process::exit(1);
    }
    Ok(())
}

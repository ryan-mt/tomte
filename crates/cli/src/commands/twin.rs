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

    let s = twin.summary();
    println!("Repo Twin — {}", twin.root);
    if twin.truncated {
        println!("  (index truncated: the repo exceeds the file cap)");
    }
    println!("  files            {}", s.files);
    println!("    source         {}", s.source_files);
    println!("    tests          {}", s.test_files);
    println!(
        "  import edges     {} ({} resolved inside the repo)",
        s.import_edges, s.resolved_imports
    );
    println!("  symbols          {}", s.symbols);
    println!("  test→source map  {} edges", s.test_edges);
    println!("  git-tracked      {} files", s.tracked_by_git);
    println!("  convention docs  {}", s.rule_docs);
    println!();
    println!("Ask why a file/symbol is (or isn't) relevant:  tomte why-context <file|symbol>");
    Ok(())
}

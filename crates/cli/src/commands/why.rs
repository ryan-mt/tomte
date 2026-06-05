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
        println!("{}", render_reconcile(&report));
        return Ok(());
    }

    match loc {
        Some(loc) if !all => {
            let records = decisions::for_loc(&here, &loc);
            println!("{}", decisions::render_for_loc(&records, &loc));
        }
        _ => {
            let records = decisions::load(&here);
            println!("{}", decisions::render_all(&records));
        }
    }
    Ok(())
}

/// A calm, one-glance summary of a Drift Watch pass: what self-healed and what
/// now needs a human's eyes. Silent-on-a-tidy-house in spirit (Pillar 4).
fn render_reconcile(r: &decisions::ReconcileReport) -> String {
    if !r.changed() && r.stale() == 0 {
        return "decision trail is in order — every anchored decision still matches its code."
            .into();
    }
    let mut out = String::new();
    if r.changed() {
        out.push_str(&format!(
            "healed {} decision(s) that drifted:\n",
            r.moved.len()
        ));
        for (old, new) in &r.moved {
            out.push_str(&format!("  {old}  ->  {new}\n"));
        }
    }
    if r.stale() > 0 {
        out.push_str(&format!(
            "{} decision(s) no longer match their code — re-record or run `tomte why <loc>`:\n",
            r.stale()
        ));
        for loc in r.gone.iter().chain(r.ambiguous.iter()) {
            out.push_str(&format!("  {loc}\n"));
        }
    }
    out.trim_end().to_string()
}

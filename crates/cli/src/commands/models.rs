//! `tomte models` — the model lineup: every model tomte can drive, its context
//! window and thinking capabilities, which credentials are present, the active
//! model, and the failover chain an overload would walk. `--json` emits the
//! same report machine-readably.

use anyhow::Result;
use tomte_core::{config, models_report};

pub async fn run(json: bool) -> Result<()> {
    let cfg = config::load();
    let report = models_report::collect_current(&cfg);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", models_report::render(&report));
    }
    Ok(())
}

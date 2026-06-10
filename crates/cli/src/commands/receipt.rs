//! `tomte receipt` — the work receipt: one Markdown/HTML/JSON artifact that
//! proves a stretch of work — a fresh Proof Capsule (real exit codes), the
//! seal standing on HEAD, what the session actually ran and edited (from the
//! persisted session log), the cost receipt, and the newest decisions — so a
//! PR can carry evidence instead of a transcript.

use anyhow::Result;
use tomte_core::receipt;

pub async fn run(
    json: bool,
    html: bool,
    session: Option<String>,
    out: Option<std::path::PathBuf>,
    cwd: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(dir) = &cwd {
        std::env::set_current_dir(dir)
            .map_err(|e| anyhow::anyhow!("--cwd {}: {e}", dir.display()))?;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let r = receipt::collect(&here, session.as_deref()).await;
    let body = if json {
        serde_json::to_string_pretty(&r)?
    } else if html {
        receipt::render_html(&r)
    } else {
        receipt::render_markdown(&r)
    };

    match out {
        Some(path) => {
            std::fs::write(&path, &body)
                .map_err(|e| anyhow::anyhow!("--out {}: {e}", path.display()))?;
            println!("receipt written to {}", path.display());
        }
        None => println!("{body}"),
    }
    Ok(())
}

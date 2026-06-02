use anyhow::Result;
use opencli_core::doctor;

/// `opencli doctor` — print setup diagnostics and exit non-zero if any check
/// fails hard, so it can gate a setup script or CI step. Runs headless (no auth,
/// no TUI) on purpose: it's the command you reach for when the TUI won't even
/// start.
pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let report = doctor::diagnose(&cwd);
    println!("{}", report.render());
    if report.has_errors() {
        std::process::exit(1);
    }
    Ok(())
}

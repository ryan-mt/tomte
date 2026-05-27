use anyhow::Result;
use opencli_core::config;

pub async fn run(
    show: bool,
    set_model: Option<String>,
    set_reasoning: Option<String>,
) -> Result<()> {
    let mut cfg = config::load();
    let mut changed = false;
    if let Some(m) = set_model {
        cfg.model = m;
        changed = true;
    }
    if let Some(r) = set_reasoning {
        cfg.reasoning_effort = r;
        changed = true;
    }
    if changed {
        config::save(&cfg)?;
        println!("✅  Config updated");
    }
    if show || !changed {
        println!("{}", serde_json::to_string_pretty(&cfg)?);
        println!("\nFile: {}", config::config_file().display());
    }
    Ok(())
}

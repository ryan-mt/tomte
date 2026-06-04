use anyhow::Result;
use tomte_core::config;

pub async fn run(
    show: bool,
    set_model: Option<String>,
    set_reasoning: Option<String>,
) -> Result<()> {
    let mut cfg = config::load();
    let mut changed = false;
    if let Some(m) = set_model {
        cfg.model = config::normalize_model_name(&m);
        changed = true;
    }
    if let Some(r) = set_reasoning {
        let Some(r) = config::normalize_reasoning_effort(&r) else {
            anyhow::bail!(
                "invalid reasoning effort `{r}`; expected one of: {}",
                config::VALID_REASONING_EFFORTS.join(", ")
            );
        };
        cfg.reasoning_effort = r;
        changed = true;
    }
    if changed {
        config::save(&cfg)?;
        cfg = config::load();
        println!("✅  Config updated");
    }
    if show || !changed {
        println!(
            "{}",
            serde_json::to_string_pretty(&config::redacted_view(&cfg))?
        );
        println!("\nFile: {}", config::config_file().display());
    }
    Ok(())
}

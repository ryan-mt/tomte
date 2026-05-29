use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const CONFIG_DIR_NAME: &str = "opencli";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
    #[serde(default = "default_verbosity")]
    pub verbosity: String,
    /// Automatically compact the conversation when the context window nears
    /// its limit (~85%), replacing history with a model-generated summary so
    /// long sessions don't overflow or re-bill the full transcript each turn.
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    #[serde(default)]
    pub auto_approve_read: bool,
    #[serde(default)]
    pub auto_approve_write: bool,
}

fn default_model() -> String {
    "gpt-5.5".to_string()
}
fn default_reasoning_effort() -> String {
    "medium".to_string()
}
fn default_verbosity() -> String {
    "medium".to_string()
}
fn default_auto_compact() -> bool {
    true
}

/// Map legacy model names from earlier opencli versions to the real OpenAI
/// Responses API model IDs. Older `config.json`s shipped with placeholder
/// names like `gpt-5.5` that don't resolve at the API, so without this
/// migration the first turn after upgrade would 404. Idempotent.
pub fn migrate_legacy_model_name(name: &str) -> String {
    // gpt-5 / gpt-5.1 / gpt-5.2 were removed from the catalogue in favour
    // of gpt-5.3 / 5.4 / 5.5. Map old base ids onto the current default so
    // a returning user keeps a working model after the picker change.
    match name {
        "gpt-5" | "gpt-5.1" | "gpt-5.2" => "gpt-5.5".to_string(),
        other => other.to_string(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            reasoning_effort: default_reasoning_effort(),
            verbosity: default_verbosity(),
            auto_compact: true,
            auto_approve_read: true,
            auto_approve_write: false,
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR_NAME)
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> Config {
    let path = config_file();
    let mut cfg = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Config>(&s) {
            Ok(cfg) => cfg,
            Err(e) => {
                // Silently resetting to defaults on a corrupt file used to make
                // model/effort changes appear to vanish — log loudly so the
                // user sees something is wrong instead of debugging mystery
                // setting resets.
                tracing::warn!(
                    config = %path.display(),
                    error = %e,
                    "config.json parse failed; falling back to defaults"
                );
                Config::default()
            }
        },
        Err(_) => Config::default(),
    };
    // Auto-upgrade legacy placeholder model names from earlier opencli builds.
    let migrated = migrate_legacy_model_name(&cfg.model);
    if migrated != cfg.model {
        tracing::info!(
            old = %cfg.model,
            new = %migrated,
            "migrating legacy model name in config.json"
        );
        cfg.model = migrated;
    }
    cfg
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let persistable = persist_view(cfg);
    let text = serde_json::to_string_pretty(&persistable).unwrap();
    // Atomic write: a SIGKILL between truncate and write previously left
    // config.json empty, silently resetting all settings on next launch.
    let path = config_file();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// `max` is the heaviest adaptive-thinking tier on Anthropic and is
/// deliberately session-only — relaunching the CLI should not silently
/// re-engage the heaviest spend tier. Persist it as `xhigh` (next step
/// down). OpenAI models are untouched.
fn persist_view(cfg: &Config) -> Config {
    let mut out = cfg.clone();
    if out.reasoning_effort == "max"
        && crate::provider::Provider::from_model(&out.model) == crate::provider::Provider::Anthropic
    {
        out.reasoning_effort = "xhigh".to_string();
    }
    out
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn migrate_legacy_model_name_maps_all_legacy_aliases() {
        assert_eq!(migrate_legacy_model_name("gpt-5"), "gpt-5.5");
        assert_eq!(migrate_legacy_model_name("gpt-5.1"), "gpt-5.5");
        assert_eq!(migrate_legacy_model_name("gpt-5.2"), "gpt-5.5");
    }

    #[test]
    fn persist_view_downgrades_max_to_xhigh_for_anthropic() {
        let mut cfg = Config::default();
        cfg.model = "claude-opus-4-7".into();
        cfg.reasoning_effort = "max".into();
        let p = super::persist_view(&cfg);
        assert_eq!(p.reasoning_effort, "xhigh");
        assert_eq!(cfg.reasoning_effort, "max");
    }

    #[test]
    fn persist_view_leaves_openai_max_alone() {
        let mut cfg = Config::default();
        cfg.model = "gpt-5".into();
        cfg.reasoning_effort = "max".into();
        let p = super::persist_view(&cfg);
        assert_eq!(p.reasoning_effort, "max");
    }

    #[test]
    fn auto_compact_defaults_on() {
        assert!(Config::default().auto_compact);
        // A config.json predating the flag still deserializes with it enabled.
        let cfg: Config = serde_json::from_str(r#"{"model":"gpt-5.5"}"#).unwrap();
        assert!(cfg.auto_compact);
    }

    #[test]
    fn migrate_legacy_model_name_passes_through_new_names() {
        for name in [
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.3",
            "gpt-5-pro",
            "gpt-5-codex",
            "gpt-5-mini",
            "gpt-5-nano",
            "o3",
        ] {
            assert_eq!(migrate_legacy_model_name(name), name);
        }
    }
}

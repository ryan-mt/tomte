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

impl Default for Config {
    fn default() -> Self {
        Self {
            model: default_model(),
            reasoning_effort: default_reasoning_effort(),
            verbosity: default_verbosity(),
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
    match std::fs::read_to_string(&path) {
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
    }
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(cfg).unwrap();
    // Atomic write: a SIGKILL between truncate and write previously left
    // config.json empty, silently resetting all settings on next launch.
    let path = config_file();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

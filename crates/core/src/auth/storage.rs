use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    None,
    #[serde(alias = "chatgpt")]
    OpenaiOauth,
    #[serde(alias = "apikey", alias = "api_key")]
    OpenaiApiKey,
    AnthropicOauth,
    AnthropicApiKey,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthRecord {
    #[serde(default)]
    pub mode: AuthMode,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub tokens: Option<StoredTokens>,
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub anthropic_tokens: Option<StoredTokens>,
    #[serde(default)]
    pub last_refresh: Option<DateTime<Utc>>,
}

fn auth_file() -> std::path::PathBuf {
    config::config_dir().join("auth.json")
}

pub fn load_auth() -> Result<AuthRecord> {
    let path = auth_file();
    if !path.exists() {
        return Ok(AuthRecord::default());
    }
    let text = std::fs::read_to_string(&path)?;
    if text.trim().is_empty() {
        return Ok(AuthRecord::default());
    }
    let record: AuthRecord = serde_json::from_str(&text)?;
    Ok(record)
}

pub fn save_auth(record: &AuthRecord) -> Result<()> {
    let dir = config::config_dir();
    std::fs::create_dir_all(&dir)?;
    let path = auth_file();
    let text = serde_json::to_string_pretty(record)?;
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, text.as_bytes())?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

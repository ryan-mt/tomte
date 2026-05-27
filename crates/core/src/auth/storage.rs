use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    #[default]
    None,
    ChatGPT,
    ApiKey,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
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
    // Write to a sibling tempfile created with restrictive mode FIRST, then
    // atomic-rename into place. Previously the code did `std::fs::write` then
    // chmod, which (a) left a window where the file was world-readable under
    // a 022 umask and (b) silently swallowed chmod failure with `let _ =`,
    // potentially leaving tokens at 0644 forever.
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

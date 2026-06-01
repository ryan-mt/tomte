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
    // Unique temp name (not a fixed `auth.tmp`) so two concurrent writers — e.g.
    // both providers refreshing tokens at once — can't truncate and tear each
    // other's file, publishing a half-written auth.json that fails to parse and
    // logs the user out. The atomic rename still gives all-or-nothing semantics.
    let suffix = {
        use rand::RngCore;
        let mut b = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    let tmp = path.with_extension(format!("tmp.{suffix}"));
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

/// A specific stored credential to clear on logout, or `All` to clear every
/// stored credential at once. Maps 1:1 onto the four `AuthRecord` slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoutTarget {
    OpenAiOauth,
    OpenAiApiKey,
    AnthropicOauth,
    AnthropicApiKey,
    All,
}

impl LogoutTarget {
    /// Stable string key used to round-trip through the TUI picker.
    pub fn key(self) -> &'static str {
        match self {
            Self::OpenAiOauth => "openai_oauth",
            Self::OpenAiApiKey => "openai_apikey",
            Self::AnthropicOauth => "anthropic_oauth",
            Self::AnthropicApiKey => "anthropic_apikey",
            Self::All => "all",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "openai_oauth" => Self::OpenAiOauth,
            "openai_apikey" => Self::OpenAiApiKey,
            "anthropic_oauth" => Self::AnthropicOauth,
            "anthropic_apikey" => Self::AnthropicApiKey,
            "all" => Self::All,
            _ => return None,
        })
    }
}

fn tokens_filled(slot: &Option<StoredTokens>) -> bool {
    slot.as_ref().is_some_and(|t| !t.access_token.is_empty())
}
fn key_filled(slot: &Option<String>) -> bool {
    slot.as_ref().is_some_and(|k| !k.is_empty())
}

pub fn has_openai_oauth(r: &AuthRecord) -> bool {
    tokens_filled(&r.tokens)
}

pub fn has_openai_api_key(r: &AuthRecord) -> bool {
    key_filled(&r.api_key)
}

pub fn has_anthropic_oauth(r: &AuthRecord) -> bool {
    tokens_filled(&r.anthropic_tokens)
}

pub fn has_anthropic_api_key(r: &AuthRecord) -> bool {
    key_filled(&r.anthropic_api_key)
}

fn has_any_credential(r: &AuthRecord) -> bool {
    has_openai_oauth(r)
        || has_openai_api_key(r)
        || has_anthropic_oauth(r)
        || has_anthropic_api_key(r)
}

/// Whether `record.mode` still points at a credential that is actually stored.
fn mode_is_backed(r: &AuthRecord) -> bool {
    match r.mode {
        AuthMode::None => !has_any_credential(r),
        AuthMode::OpenaiOauth => has_openai_oauth(r),
        AuthMode::OpenaiApiKey => has_openai_api_key(r),
        AuthMode::AnthropicOauth => has_anthropic_oauth(r),
        AuthMode::AnthropicApiKey => has_anthropic_api_key(r),
    }
}

/// First remaining stored credential as a coherent active mode. OAuth before
/// API key, OpenAI before Anthropic (arbitrary but deterministic); `None` when
/// nothing is stored.
fn infer_mode(r: &AuthRecord) -> AuthMode {
    if has_openai_oauth(r) {
        AuthMode::OpenaiOauth
    } else if has_openai_api_key(r) {
        AuthMode::OpenaiApiKey
    } else if has_anthropic_oauth(r) {
        AuthMode::AnthropicOauth
    } else if has_anthropic_api_key(r) {
        AuthMode::AnthropicApiKey
    } else {
        AuthMode::None
    }
}

/// Active mode after repairing stale or legacy records in-memory.
pub fn effective_mode(r: &AuthRecord) -> AuthMode {
    if mode_is_backed(r) {
        r.mode
    } else {
        infer_mode(r)
    }
}

/// Clear one stored credential (or all). If the removal leaves `mode` pointing
/// at a now-empty slot, fall back to another stored credential (or `None`) so
/// the status line never claims a credential that was just removed. Only
/// rewrites `mode` when the active credential was the one removed — clearing an
/// inactive credential leaves the active mode untouched. Credentials supplied
/// via env vars are not stored here and are unaffected.
pub fn clear_credential(record: &mut AuthRecord, target: LogoutTarget) {
    match target {
        LogoutTarget::OpenAiOauth => record.tokens = None,
        LogoutTarget::OpenAiApiKey => record.api_key = None,
        LogoutTarget::AnthropicOauth => record.anthropic_tokens = None,
        LogoutTarget::AnthropicApiKey => record.anthropic_api_key = None,
        LogoutTarget::All => {
            record.tokens = None;
            record.api_key = None;
            record.anthropic_tokens = None;
            record.anthropic_api_key = None;
        }
    }
    if !mode_is_backed(record) {
        record.mode = effective_mode(record);
    }
}

pub fn activate_openai_api_key(record: &mut AuthRecord, key: String) {
    record.mode = AuthMode::OpenaiApiKey;
    record.api_key = Some(key);
}

pub fn activate_anthropic_api_key(record: &mut AuthRecord, key: String) {
    record.mode = AuthMode::AnthropicApiKey;
    record.anthropic_api_key = Some(key);
}

#[cfg(test)]
mod logout_tests {
    use super::*;

    fn tok() -> StoredTokens {
        StoredTokens {
            access_token: "x".into(),
            ..Default::default()
        }
    }

    #[test]
    fn clearing_active_credential_falls_back_to_remaining() {
        let mut r = AuthRecord {
            mode: AuthMode::OpenaiApiKey,
            api_key: Some("k".into()),
            anthropic_tokens: Some(tok()),
            ..Default::default()
        };
        clear_credential(&mut r, LogoutTarget::OpenAiApiKey);
        assert!(r.api_key.is_none());
        assert_eq!(r.mode, AuthMode::AnthropicOauth);
    }

    #[test]
    fn clearing_inactive_credential_keeps_active_mode() {
        let mut r = AuthRecord {
            mode: AuthMode::AnthropicOauth,
            anthropic_tokens: Some(tok()),
            api_key: Some("k".into()),
            ..Default::default()
        };
        clear_credential(&mut r, LogoutTarget::OpenAiApiKey);
        assert_eq!(r.mode, AuthMode::AnthropicOauth);
    }

    #[test]
    fn clearing_all_resets_to_none() {
        let mut r = AuthRecord {
            mode: AuthMode::OpenaiOauth,
            tokens: Some(tok()),
            anthropic_api_key: Some("k".into()),
            ..Default::default()
        };
        clear_credential(&mut r, LogoutTarget::All);
        assert_eq!(r.mode, AuthMode::None);
        assert!(r.tokens.is_none() && r.anthropic_api_key.is_none());
    }

    #[test]
    fn effective_mode_recovers_legacy_record_with_missing_mode() {
        let r = AuthRecord {
            mode: AuthMode::None,
            api_key: Some("k".into()),
            ..Default::default()
        };
        assert_eq!(effective_mode(&r), AuthMode::OpenaiApiKey);
    }

    #[test]
    fn effective_mode_ignores_empty_credential_slots() {
        let r = AuthRecord {
            mode: AuthMode::OpenaiApiKey,
            api_key: Some(String::new()),
            anthropic_api_key: Some("ak".into()),
            ..Default::default()
        };
        assert_eq!(effective_mode(&r), AuthMode::AnthropicApiKey);
    }

    #[test]
    fn activating_openai_api_key_preserves_oauth_token() {
        let mut r = AuthRecord {
            mode: AuthMode::OpenaiOauth,
            tokens: Some(tok()),
            ..Default::default()
        };

        activate_openai_api_key(&mut r, "sk-test".into());

        assert_eq!(r.mode, AuthMode::OpenaiApiKey);
        assert_eq!(r.api_key.as_deref(), Some("sk-test"));
        assert!(has_openai_oauth(&r));
    }

    #[test]
    fn activating_anthropic_api_key_preserves_oauth_token() {
        let mut r = AuthRecord {
            mode: AuthMode::AnthropicOauth,
            anthropic_tokens: Some(tok()),
            ..Default::default()
        };

        activate_anthropic_api_key(&mut r, "sk-ant-test".into());

        assert_eq!(r.mode, AuthMode::AnthropicApiKey);
        assert_eq!(r.anthropic_api_key.as_deref(), Some("sk-ant-test"));
        assert!(has_anthropic_oauth(&r));
    }
}

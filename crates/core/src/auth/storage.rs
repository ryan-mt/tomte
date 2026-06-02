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

#[cfg(any(test, not(unix)))]
fn non_unix_secure_auth_storage_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure auth storage requires owner-only file permissions on this platform",
    )
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

#[cfg(unix)]
pub fn save_auth(record: &AuthRecord) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = config::config_dir();
    config::create_dir_secure(&dir)?;
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
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)?;
    // fsync the parent directory so the rename is durable: a crash right after
    // rename can otherwise lose the new directory entry and revert auth.json to
    // the previous (stale/expired) credentials, forcing a spurious re-login.
    if let Ok(f) = std::fs::File::open(&dir) {
        let _ = f.sync_all();
    }
    Ok(())
}

/// Non-Unix platforms cannot yet enforce owner-only (`0o600`) permissions on the
/// credential file, so persistence fails explicitly rather than storing tokens
/// under inherited ACLs. (The real implementation is the Unix one above.)
#[cfg(not(unix))]
pub fn save_auth(_record: &AuthRecord) -> Result<()> {
    Err(non_unix_secure_auth_storage_error().into())
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

/// A stable per-account fingerprint for things that should persist for an
/// account and only change when the user switches AI accounts (e.g. the
/// `/buddy` companion). It is keyed on the durable account id / JWT subject /
/// API-key hash — never on the rotating access token — so it survives token
/// refreshes, and it is derived purely (nothing is stored), so deleting local
/// state can't re-roll it. Never returns a raw secret.
pub fn account_identity(record: &AuthRecord) -> String {
    match record.mode {
        AuthMode::None => "anonymous".to_string(),
        AuthMode::OpenaiApiKey => {
            format!(
                "openai-key:{}",
                short_hash(record.api_key.as_deref().unwrap_or(""))
            )
        }
        AuthMode::AnthropicApiKey => format!(
            "anthropic-key:{}",
            short_hash(record.anthropic_api_key.as_deref().unwrap_or(""))
        ),
        AuthMode::OpenaiOauth => format!("openai-oauth:{}", oauth_account(record.tokens.as_ref())),
        AuthMode::AnthropicOauth => {
            format!(
                "anthropic-oauth:{}",
                oauth_account(record.anthropic_tokens.as_ref())
            )
        }
    }
}

/// The durable account id for an OAuth credential: prefer the stored
/// `account_id`, then the JWT `sub` from the id/access token (stable across
/// refreshes), and only as a last resort a hash of the refresh token.
fn oauth_account(tokens: Option<&StoredTokens>) -> String {
    let Some(t) = tokens else {
        return "none".to_string();
    };
    if let Some(id) = t.account_id.as_deref().filter(|s| !s.is_empty()) {
        return id.to_string();
    }
    for jwt in [t.id_token.as_deref(), Some(t.access_token.as_str())] {
        if let Some(sub) = jwt.and_then(jwt_subject) {
            return format!("sub:{sub}");
        }
    }
    format!("rt:{}", short_hash(&t.refresh_token))
}

/// Extract the `sub` (or `account_id`) claim from a JWT payload without
/// verifying the signature — enough to fingerprint the account. Returns `None`
/// for anything that isn't a decodable JWT.
fn jwt_subject(token: &str) -> Option<String> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("sub")
        .or_else(|| json.get("account_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// FNV-1a (64-bit) of `s` as hex — a stable, non-reversible fingerprint so the
/// identity never embeds a raw credential.
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
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

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn api_key_identity_is_stable_distinct_and_secret_free() {
        let mut r = AuthRecord {
            mode: AuthMode::OpenaiApiKey,
            api_key: Some("sk-aaa".into()),
            ..Default::default()
        };
        let id = account_identity(&r);
        assert_eq!(id, account_identity(&r), "stable for the same account");
        assert!(!id.contains("sk-aaa"), "must not embed the raw key: {id}");
        r.api_key = Some("sk-bbb".into());
        assert_ne!(id, account_identity(&r), "changes when the account changes");
    }

    #[test]
    fn oauth_prefers_account_id() {
        let r = AuthRecord {
            mode: AuthMode::OpenaiOauth,
            tokens: Some(StoredTokens {
                account_id: Some("acct-7".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(account_identity(&r), "openai-oauth:acct-7");
    }

    #[test]
    fn oauth_falls_back_to_jwt_subject_across_token_rotation() {
        use base64::Engine;
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"user-42"}"#);
        // Different access tokens (rotation) but the same `sub` → same identity.
        let make = |access: &str| AuthRecord {
            mode: AuthMode::AnthropicOauth,
            anthropic_tokens: Some(StoredTokens {
                access_token: format!("h.{payload}.{access}"),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            account_identity(&make("sigA")),
            "anthropic-oauth:sub:user-42"
        );
        assert_eq!(
            account_identity(&make("sigA")),
            account_identity(&make("sigB"))
        );
    }

    #[test]
    fn no_credential_is_anonymous() {
        assert_eq!(account_identity(&AuthRecord::default()), "anonymous");
    }

    #[test]
    fn non_unix_secure_auth_storage_error_is_explicit() {
        let err = non_unix_secure_auth_storage_error();

        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(
            err.to_string()
                .contains("secure auth storage requires owner-only file permissions"),
            "{err}"
        );
    }
}

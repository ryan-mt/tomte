use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config;

mod identity;
pub use identity::account_identity;

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

#[derive(Clone, Default, Serialize, Deserialize)]
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

// Manual Debug: redact the token strings so they can't reach a log via `{:?}`.
// Non-secret fields (account id, expiry) stay visible for debugging.
impl std::fmt::Debug for StoredTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("account_id", &self.account_id)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
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

// Manual Debug: redact the API keys; the token fields are `StoredTokens`, whose
// own Debug already redacts.
impl std::fmt::Debug for AuthRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRecord")
            .field("mode", &self.mode)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("tokens", &self.tokens)
            .field(
                "anthropic_api_key",
                &self.anthropic_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("anthropic_tokens", &self.anthropic_tokens)
            .field("last_refresh", &self.last_refresh)
            .finish()
    }
}

fn auth_file() -> std::path::PathBuf {
    config::config_dir().join("auth.json")
}

#[cfg(any(test, not(any(unix, windows))))]
fn non_unix_secure_auth_storage_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure auth storage requires owner-only file permissions on this platform",
    )
}

/// Best-effort: tighten the credential file to owner-only (`0o600`) if it's
/// group- or world-accessible. `save_auth` always writes `0o600`, but a file
/// restored from a backup, copied with `cp -p`, or placed via `XDG_CONFIG_HOME`
/// redirection can land readable by other local users; `load_auth` closes that
/// on read, mirroring `config::create_dir_secure`'s directory repair.
#[cfg(unix)]
fn repair_owner_only(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

pub fn load_auth() -> Result<AuthRecord> {
    let path = auth_file();
    if !path.exists() {
        return Ok(AuthRecord::default());
    }
    #[cfg(unix)]
    repair_owner_only(&path);
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

/// Windows: persist the credential file restricted to the current user — the
/// analogue of the Unix `0o600`. The file lives under `%APPDATA%\tomte` (the
/// per-user profile, not world-readable), and we additionally strip inherited
/// ACEs and grant only the owner via `icacls`, matching the explicit
/// enforcement the Unix path gives. Fills the gap the old hard-error left, so
/// OAuth sign-in finally completes on Windows.
#[cfg(windows)]
pub fn save_auth(record: &AuthRecord) -> Result<()> {
    use std::io::Write;

    let dir = config::config_dir();
    config::create_dir_secure(&dir)?;
    // Tighten the directory to owner-only *with inheritance* so the temp file
    // created below is owner-only from birth, not just after the per-file icacls.
    // Closes the brief broad-ACL window on profiles whose %APPDATA% (or a custom
    // TOMTE_CONFIG_DIR) is not already owner-restricted.
    restrict_dir_to_owner_windows(&dir);
    let path = auth_file();
    let text = serde_json::to_string_pretty(record)?;
    // Unique temp name so two concurrent writers (e.g. both providers refreshing
    // at once) can't truncate and tear each other's file; the atomic rename
    // still gives all-or-nothing semantics.
    let suffix = {
        use rand::RngCore;
        let mut b = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    let tmp = path.with_extension(format!("tmp.{suffix}"));
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    // Tighten to owner-only BEFORE it becomes auth.json, so the credential file
    // is never momentarily broader than the user. Best-effort: on failure the
    // file is still protected by the per-user profile ACL it inherits — refusing
    // here would reinstate the very "can't sign in on Windows" bug this fixes.
    restrict_to_owner_windows(&tmp);
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Strip inherited ACEs from `path` and grant only the current user — the
/// Windows analogue of `chmod 600`, via the always-present `icacls`. Best-effort
/// (the file already inherits the per-user profile ACL if this can't run).
#[cfg(windows)]
pub(crate) fn restrict_to_owner_windows(path: &std::path::Path) {
    // Best-effort (a failure leaves the inherited per-user %APPDATA% ACL, which
    // already excludes other standard users), but no longer SILENT: a failed or
    // skipped tighten is logged so the degraded posture is observable instead of
    // an invisible no-op.
    let Some(user) = current_windows_user() else {
        tracing::warn!(
            "credential file kept its inherited %APPDATA% ACL: could not determine the \
             Windows user (USERNAME unset), so no explicit owner-only grant was applied"
        );
        return;
    };
    match std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:(R,W)"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::warn!(
            code = ?s.code(),
            "icacls could not restrict the credential file to the current user; \
             it kept its inherited %APPDATA% ACL"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "could not run icacls to restrict the credential file; \
             it kept its inherited %APPDATA% ACL"
        ),
    }
}

/// Strip inherited ACEs from the config directory and grant only the current
/// user, *with inheritance* (`(OI)(CI)`), so a file created inside is owner-only
/// from birth. Without this, the freshly created temp credential file carries
/// the directory's inherited ACL until the per-file `restrict_to_owner_windows`
/// runs — a brief window where it is broader than the user on a profile whose
/// `%APPDATA%` (or a custom `TOMTE_CONFIG_DIR`) is not already owner-restricted.
/// Best-effort and idempotent, matching the per-file grant; only the dir's own
/// ACL is changed, so pre-existing sibling files keep their permissions.
#[cfg(windows)]
pub(crate) fn restrict_dir_to_owner_windows(dir: &std::path::Path) {
    let Some(user) = current_windows_user() else {
        return; // restrict_to_owner_windows already warns about an unset USERNAME
    };
    let _ = std::process::Command::new("icacls")
        .arg(dir)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:(OI)(CI)(F)"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The current user as `DOMAIN\user` (or bare `user`) for an `icacls` grant.
#[cfg(windows)]
fn current_windows_user() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|s| !s.is_empty())?;
    match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => Some(format!("{domain}\\{user}")),
        _ => Some(user),
    }
}

/// Exotic non-Unix, non-Windows targets can't enforce owner-only permissions, so
/// persistence still fails explicitly rather than storing tokens unprotected.
#[cfg(not(any(unix, windows)))]
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

#[cfg(all(test, windows))]
mod windows_perm_tests {
    use super::*;

    #[test]
    fn restrict_to_owner_keeps_owner_access() {
        // Tightening the ACL must never lock the owner out of its own
        // credential file (that would log the user out on the next read).
        let path =
            std::env::temp_dir().join(format!("tomte-auth-win-{}.json", rand::random::<u64>()));
        std::fs::write(&path, "{}").unwrap();
        restrict_to_owner_windows(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{}",
            "owner must keep read access after the ACL is tightened"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn restrict_dir_to_owner_keeps_owner_access_and_lets_files_be_created() {
        // Hardening the directory must not lock the owner out: it must still be
        // able to create and read a file inside (the credential temp file).
        let dir = std::env::temp_dir().join(format!("tomte-auth-dir-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        restrict_dir_to_owner_windows(&dir);
        let file = dir.join("auth.json");
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{}",
            "owner must keep create+read access to a file in the hardened dir"
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(test, unix))]
mod perm_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn repair_owner_only_tightens_group_world_bits() {
        let path =
            std::env::temp_dir().join(format!("tomte-auth-perm-{}.json", rand::random::<u64>()));
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        repair_owner_only(&path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "a group/world-readable credential file must be tightened to 0o600"
        );
        // Idempotent on an already-tight file.
        repair_owner_only(&path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(&path);
    }
}

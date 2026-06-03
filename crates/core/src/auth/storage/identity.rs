//! Stable, secret-free account fingerprinting derived from an [`AuthRecord`].

use super::{AuthMode, AuthRecord, StoredTokens};

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
}

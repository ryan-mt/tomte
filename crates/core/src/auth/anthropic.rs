//! Anthropic Claude OAuth — kept in a dedicated module so the OpenAI flow in
//! [`super::oauth`] stays untouched. The two providers use entirely different
//! issuers, client ids, and redirect schemes; mixing them caused subtle bugs
//! during development, so the module boundary is load-bearing.
//!
//! ## ToS warning
//!
//! This flow signs the user in with their personal `claude.ai` (Claude Pro or
//! Claude Max) account and reuses the resulting OAuth token to call
//! `api.anthropic.com/v1/messages` directly. Anthropic's product terms
//! restrict programmatic use of subscription tokens to the official Claude
//! Code CLI; using them with a third-party tool *may* violate Anthropic's
//! Terms of Service. Surface [`TOS_WARNING`] in the UI before starting the
//! flow and require an explicit opt-in.

use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::Deserialize;

use super::pkce::{generate_pkce, random_state, Pkce};
use super::storage::{load_auth, save_auth, AuthMode, AuthRecord, StoredTokens};

/// User-visible warning shown before launching the Claude OAuth flow.
pub const TOS_WARNING: &str = "\
WARNING: This sign-in mode reuses your Claude Pro/Max subscription token to call
the Anthropic Messages API directly. Anthropic restricts subscription tokens to
the official Claude Code CLI; using them with a third-party tool MAY violate
Anthropic's Terms of Service. You assume full responsibility for this choice.";

/// `claude.ai` OAuth client for Claude Pro/Max subscription sign-in. These are
/// the public OAuth endpoints and client-id; values are taken from open-source
/// reference implementations.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
pub const MANUAL_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub const SCOPES: &str = "org:create_api_key user:profile user:inference";

#[derive(Clone, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

// Manual Debug so the freshly minted tokens can't reach a log via `{:?}`.
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

/// A manual (copy/paste) Anthropic login in progress. The Claude OAuth client
/// does not register a loopback redirect URI, so the official
/// `console.anthropic.com/oauth/code/callback` redirect is used instead:
/// claude.ai shows the user an authorization code which they paste back in.
pub struct ManualLogin {
    pub auth_url: String,
    pkce: Pkce,
    state: String,
}

pub fn build_authorize_url(redirect_uri: &str, pkce: &Pkce, state: &str) -> String {
    // `code=true` triggers the Claude Max upsell on the login page.
    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    let qs = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{}", AUTHORIZE_URL, qs)
}

/// Begin a manual Anthropic OAuth login: build the authorize URL (optionally
/// opening the browser) and return the handle needed to finish the flow once
/// the user pastes the code shown by claude.ai.
pub fn begin_manual_login(open_browser: bool) -> ManualLogin {
    let pkce = generate_pkce();
    let state = random_state();
    let auth_url = build_authorize_url(MANUAL_REDIRECT_URI, &pkce, &state);
    if open_browser {
        let _ = webbrowser::open(&auth_url);
    }
    ManualLogin {
        auth_url,
        pkce,
        state,
    }
}

/// Finish a manual Anthropic OAuth login with the code the user pasted from the
/// browser. The pasted value is usually `CODE#STATE`; the `#STATE` fragment is
/// stripped by [`exchange_code_for_tokens`].
pub async fn complete_manual_login(login: &ManualLogin, pasted_code: &str) -> Result<AuthRecord> {
    let code = pasted_code.trim();
    if code.is_empty() {
        return Err(anyhow!("no authorization code provided"));
    }
    // CSRF / authorization-code-injection guard: claude.ai echoes the state we
    // generated back as `CODE#STATE`. Verify it matches THIS login's state
    // before exchanging, so a code/state pair from a different (attacker-
    // initiated) authorization can't be pasted in. (The state was previously
    // generated but never checked — only forwarded into the token request.)
    check_returned_state(code, &login.state)?;
    let tokens =
        exchange_code_for_tokens(MANUAL_REDIRECT_URI, &login.pkce, code, &login.state).await?;
    let expires_at = tokens.expires_in.and_then(|sec| {
        // Guard a hostile/buggy out-of-range `expires_in`: chrono's
        // Duration::seconds and (DateTime + TimeDelta) both PANIC on
        // overflow. None here is treated as "needs refresh", not a crash.
        chrono::Duration::try_seconds(sec).and_then(|d| Utc::now().checked_add_signed(d))
    });

    // Start from a default record if auth.json is missing OR unreadable: a
    // fresh login is meant to overwrite whatever is there, so propagating a
    // parse error (`?`) on a corrupt file would needlessly lock the user out
    // of the self-healing re-login path.
    // Serialize the credential read-modify-write against concurrent token
    // refreshes, which take the same lock: a sibling provider's `ensure_fresh`
    // could otherwise land its freshly-rotated (single-use) refresh token between
    // our load and save, and our write-back of the pre-refresh snapshot would
    // revert it. Held just across the load→save, after the code exchange.
    let _refresh_guard = super::REFRESH_LOCK.lock().await;
    let mut record = load_auth().unwrap_or_default();
    record.mode = AuthMode::AnthropicOauth;
    record.anthropic_tokens = Some(StoredTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        id_token: None,
        account_id: None,
        expires_at,
    });
    record.last_refresh = Some(Utc::now());
    save_auth(&record)?;
    Ok(record)
}

/// Verify the OAuth `state` echoed back in a pasted `CODE#STATE` value against
/// the state generated for this login. claude.ai appends the state after a `#`
/// (and possibly more `&`-joined params). When a state is present it MUST match;
/// when the paste carries no `#state` we can't check it client-side and proceed
/// (the token request still binds our state server-side).
fn check_returned_state(pasted: &str, expected: &str) -> Result<()> {
    if let Some((_, rest)) = pasted.split_once('#') {
        let returned = rest.split('&').next().unwrap_or(rest);
        if !returned.is_empty() && returned != expected {
            return Err(anyhow!(
                "OAuth state mismatch — the pasted code is from a different login attempt. Start the login again."
            ));
        }
    }
    Ok(())
}

fn token_http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

pub async fn exchange_code_for_tokens(
    redirect_uri: &str,
    pkce: &Pkce,
    code: &str,
    state: &str,
) -> Result<TokenSet> {
    // Strip any URL fragments the user accidentally pasted with the code.
    let clean_code = code
        .split('#')
        .next()
        .and_then(|s| s.split('&').next())
        .unwrap_or(code);
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "code": clean_code,
        "redirect_uri": redirect_uri,
        "code_verifier": pkce.code_verifier,
        "state": state,
    });

    let resp = token_http()?
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("Anthropic token exchange failed {status}"));
    }
    let tokens: TokenSet =
        serde_json::from_str(&text).map_err(|e| anyhow!("parse Anthropic token response: {e}"))?;
    Ok(tokens)
}

pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenSet> {
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLIENT_ID,
    });
    let resp = token_http()?
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("Anthropic refresh failed {status}"));
    }
    let tokens: TokenSet =
        serde_json::from_str(&text).map_err(|e| anyhow!("parse refresh response: {e}"))?;
    Ok(tokens)
}

pub async fn ensure_fresh(record: &AuthRecord) -> Result<String> {
    let tokens = record
        .anthropic_tokens
        .as_ref()
        .ok_or_else(|| anyhow!("no Anthropic token available"))?;
    let needs_refresh = match tokens.expires_at {
        Some(t) => t < Utc::now() + chrono::Duration::minutes(2),
        None => true,
    };
    if !needs_refresh {
        return Ok(tokens.access_token.clone());
    }

    let _guard = super::REFRESH_LOCK.lock().await;
    // After acquiring the lock, re-load from disk in case a sibling caller
    // already refreshed while we were waiting. If their swap is still good,
    // return that access_token directly. Otherwise, still prefer their
    // (newer, unconsumed) refresh_token and base the save on their record so
    // we don't clobber fields the sibling already wrote with stale in-memory data.
    let (base_record, refresh_token) = match load_auth() {
        Ok(fresh_record) => {
            if let Some(fresh_tokens) = fresh_record.anthropic_tokens.as_ref() {
                let still_valid = match fresh_tokens.expires_at {
                    Some(t) => t > Utc::now() + chrono::Duration::minutes(2),
                    None => false,
                };
                if still_valid {
                    return Ok(fresh_tokens.access_token.clone());
                }
                // Disk has a newer (but expired) record — use its refresh_token
                // so we don't replay a token already consumed by the sibling.
                let rt = fresh_tokens.refresh_token.clone();
                (fresh_record, rt)
            } else {
                (record.clone(), tokens.refresh_token.clone())
            }
        }
        Err(_) => (record.clone(), tokens.refresh_token.clone()),
    };

    let refreshed = refresh_access_token(&refresh_token).await?;
    let expires_at = refreshed.expires_in.and_then(|sec| {
        // Guard a hostile/buggy out-of-range `expires_in`: chrono's
        // Duration::seconds and (DateTime + TimeDelta) both PANIC on
        // overflow. None here is treated as "needs refresh", not a crash.
        chrono::Duration::try_seconds(sec).and_then(|d| Utc::now().checked_add_signed(d))
    });
    // Re-load immediately before saving and merge ONLY our refreshed Anthropic
    // tokens into the CURRENT on-disk record. Writing back the pre-refresh
    // snapshot would clobber a sibling OpenAI refresh that landed during our
    // network round-trip — restoring its already-consumed (single-use) refresh
    // token and bricking that credential (the two providers hold separate refresh
    // locks, so they are not serialized against each other).
    let mut updated = match load_auth() {
        Ok(latest) => {
            if latest.anthropic_tokens.is_none() {
                // The Anthropic slot vanished from disk while we were on the
                // network — a logout (e.g. from another process) landed
                // mid-refresh. Honor it: the fresh access token still serves
                // the in-flight turn, but the credential is never re-persisted
                // (falling back to the pre-logout snapshot would silently
                // resurrect what the user just cleared).
                return Ok(refreshed.access_token);
            }
            latest
        }
        Err(_) => base_record,
    };
    if let Some(st) = updated.anthropic_tokens.as_mut() {
        st.access_token = refreshed.access_token.clone();
        st.refresh_token = refreshed.refresh_token;
        st.expires_at = expires_at;
    }
    updated.last_refresh = Some(Utc::now());
    // The network refresh already consumed the single-use refresh_token and
    // issued a replacement. If persisting it fails, don't bubble the error and
    // discard the freshly issued access_token — let the in-flight turn proceed on
    // it. (The rotated refresh_token is lost on a persistent disk failure, so a
    // later turn may need a re-login; that beats failing this turn outright.)
    if let Err(e) = save_auth(&updated) {
        tracing::warn!(
            "failed to persist refreshed Anthropic tokens: {e:#}; continuing on the \
             freshly issued access token (re-run `tomte login` if auth later fails)"
        );
    }
    Ok(refreshed.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returned_state_must_match_when_present() {
        // Matching state in the pasted `CODE#STATE` → accepted.
        assert!(check_returned_state("the-code#abc123", "abc123").is_ok());
        // Extra &-joined params after the state are tolerated.
        assert!(check_returned_state("the-code#abc123&foo=1", "abc123").is_ok());
        // A different state (attacker-injected code/state) → rejected.
        assert!(check_returned_state("the-code#evil", "abc123").is_err());
        // No `#state` in the paste → can't verify client-side, proceed.
        assert!(check_returned_state("just-the-code", "abc123").is_ok());
    }

    #[test]
    fn builds_authorize_url_with_required_params() {
        let pkce = Pkce {
            code_verifier: "verifier".into(),
            code_challenge: "challenge".into(),
        };
        let url = build_authorize_url("http://localhost:1456/callback", &pkce, "abc123");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("client_id=9d1c250a"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=org%3Acreate_api_key%20user%3Aprofile%20user%3Ainference"));
        assert!(url.contains("state=abc123"));
    }

    #[test]
    fn authorize_url_uses_manual_redirect_uri() {
        let pkce = Pkce {
            code_verifier: "verifier".into(),
            code_challenge: "challenge".into(),
        };
        let url = build_authorize_url(MANUAL_REDIRECT_URI, &pkce, "abc123");
        assert!(url.contains(
            "redirect_uri=https%3A%2F%2Fconsole.anthropic.com%2Foauth%2Fcode%2Fcallback"
        ));
    }
}

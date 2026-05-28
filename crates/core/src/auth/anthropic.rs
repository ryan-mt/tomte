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

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use once_cell::sync::Lazy;
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::pkce::{generate_pkce, random_state, Pkce};
use super::storage::{load_auth, save_auth, AuthMode, AuthRecord, StoredTokens};

/// User-visible warning shown before launching the Claude OAuth flow.
pub const TOS_WARNING: &str = "\
WARNING: This sign-in mode reuses your Claude Pro/Max subscription token to call
the Anthropic Messages API directly. Anthropic restricts subscription tokens to
the official Claude Code CLI; using them with a third-party tool MAY violate
Anthropic's Terms of Service. You assume full responsibility for this choice.";

/// `claude.ai` Claude Code OAuth client. Discovered by reverse-engineering the
/// public Claude Code CLI auth flow; values are taken from open-source
/// reference implementations.
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
pub const MANUAL_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub const DEFAULT_PORT: u16 = 1456;
pub const FALLBACK_PORT: u16 = 1458;
pub const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Serializes refresh swaps for the Anthropic refresh-token grant. Two
/// concurrent turns racing the refresh endpoint would otherwise burn the
/// single-use refresh_token and brick the credential.
static REFRESH_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

#[derive(Debug, Clone, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
}

pub struct PendingLogin {
    pub auth_url: String,
    pub completion: tokio::task::JoinHandle<Result<AuthRecord>>,
}

pub fn build_authorize_url(redirect_uri: &str, pkce: &Pkce, state: &str) -> String {
    // `code=true` triggers the Claude Max upsell on the login page; the same
    // parameter is used by the official Claude Code CLI.
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

/// Start a browser-based Anthropic OAuth flow. Returns immediately with the
/// authorize URL so a TUI can display it; `completion` resolves when the
/// callback fires.
pub async fn start_browser_login(open_browser: bool) -> Result<PendingLogin> {
    let pkce = generate_pkce();
    let state = random_state();

    let (_port, redirect_uri, code_rx) = spawn_callback_server(&state).await?;
    let auth_url = build_authorize_url(&redirect_uri, &pkce, &state);

    if open_browser {
        let _ = webbrowser::open(&auth_url);
    }

    let redirect = redirect_uri.clone();
    let url = auth_url.clone();
    let completion = tokio::spawn(async move {
        let code = tokio::time::timeout(Duration::from_secs(600), code_rx)
            .await
            .map_err(|_| anyhow!("login timed out after 10 minutes"))?
            .map_err(|_| anyhow!("callback channel closed"))??;

        let tokens = exchange_code_for_tokens(&redirect, &pkce, &code, &state).await?;
        let expires_at = tokens
            .expires_in
            .map(|sec| Utc::now() + chrono::Duration::seconds(sec));

        let mut record = load_auth()?;
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
    });

    Ok(PendingLogin {
        auth_url: url,
        completion,
    })
}

pub async fn login_with_browser(open_browser: bool) -> Result<AuthRecord> {
    let pending = start_browser_login(open_browser).await?;
    println!(
        "\n  Open the following URL to sign in with Claude:\n     {}\n",
        pending.auth_url
    );
    let record = pending
        .completion
        .await
        .map_err(|e| anyhow!("login task panicked: {e}"))??;
    println!("  Signed in with Claude.");
    Ok(record)
}

async fn spawn_callback_server(
    expected_state: &str,
) -> Result<(u16, String, oneshot::Receiver<Result<String>>)> {
    use axum::extract::{Query, State};
    use axum::response::Html;
    use axum::routing::get;
    use axum::Router;

    #[derive(Clone)]
    struct AppState {
        tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<Result<String>>>>>,
        expected_state: String,
    }

    async fn callback(
        State(st): State<AppState>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Html<String> {
        let result: Result<String> = (|| {
            let state = params.get("state").cloned().unwrap_or_default();
            if state != st.expected_state {
                return Err(anyhow!("OAuth state mismatch"));
            }
            if let Some(err) = params.get("error") {
                let desc = params.get("error_description").cloned().unwrap_or_default();
                return Err(anyhow!("OAuth error ({err}): {desc}"));
            }
            let code = params
                .get("code")
                .cloned()
                .ok_or_else(|| anyhow!("missing code in callback"))?;
            Ok(code)
        })();
        let ok = result.is_ok();
        let mut guard = st.tx.lock().await;
        if let Some(tx) = guard.take() {
            let _ = tx.send(result);
        }
        let body = if ok {
            include_str!("../assets/success.html")
        } else {
            include_str!("../assets/error.html")
        };
        Html(body.to_string())
    }

    let (tx, rx) = oneshot::channel();
    let state = AppState {
        tx: Arc::new(tokio::sync::Mutex::new(Some(tx))),
        expected_state: expected_state.to_string(),
    };
    let app = Router::new()
        .route("/callback", get(callback))
        .with_state(state);

    let (port, listener) = match try_bind(DEFAULT_PORT).await {
        Ok(v) => v,
        Err(_) => try_bind(FALLBACK_PORT)
            .await
            .context("Failed to bind Anthropic callback port")?,
    };
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let redirect_uri = format!("http://localhost:{port}/callback");
    Ok((port, redirect_uri, rx))
}

async fn try_bind(port: u16) -> Result<(u16, tokio::net::TcpListener)> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let port = listener.local_addr()?.port();
    Ok((port, listener))
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

    let _guard = REFRESH_LOCK.lock().await;
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
    let expires_at = refreshed
        .expires_in
        .map(|sec| Utc::now() + chrono::Duration::seconds(sec));
    let mut updated = base_record;
    if let Some(st) = updated.anthropic_tokens.as_mut() {
        st.access_token = refreshed.access_token.clone();
        st.refresh_token = refreshed.refresh_token;
        st.expires_at = expires_at;
    }
    updated.last_refresh = Some(Utc::now());
    save_auth(&updated)?;
    Ok(refreshed.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

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

/// Serializes refresh_token swaps across the process. Without this, two
/// concurrent turns both see the same near-expiry token, both POST to
/// /oauth/token, and the second `save_auth` overwrites the first with a
/// now-invalid refresh_token — bricking the credential on next expiry.
/// Holding this lock for the duration of the network round-trip is fine
/// because token refreshes are rare (every ~hour) and short (sub-second).
static REFRESH_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";
pub const DEFAULT_PORT: u16 = 1455;
pub const FALLBACK_PORT: u16 = 1457;
pub const SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

#[derive(Debug, Clone, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

pub struct OauthClient {
    pub client_id: String,
    pub issuer: String,
}

impl Default for OauthClient {
    fn default() -> Self {
        Self {
            client_id: CLIENT_ID.to_string(),
            issuer: ISSUER.to_string(),
        }
    }
}

pub fn build_authorize_url(
    client_id: &str,
    issuer: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPES),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("state", state),
    ];
    let qs = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}/oauth/authorize?{}", issuer.trim_end_matches('/'), qs)
}

/// A pending browser-based login: the authorize URL is ready immediately,
/// while `completion` resolves when the OAuth callback finishes (success or error).
pub struct PendingLogin {
    pub auth_url: String,
    pub completion: tokio::task::JoinHandle<Result<AuthRecord>>,
}

/// Start a browser-based OAuth flow.
///
/// Returns immediately with the authorize URL so callers (e.g. a TUI) can
/// display it in their own UI. The returned `completion` future resolves
/// once the user finishes the OAuth flow.
///
/// `open_browser` controls whether the system browser is launched automatically.
pub async fn start_browser_login(open_browser: bool) -> Result<PendingLogin> {
    let pkce = generate_pkce();
    let state = random_state();
    let client = OauthClient::default();

    let (_port, redirect_uri, code_rx) = spawn_callback_server(&state).await?;
    let auth_url = build_authorize_url(
        &client.client_id,
        &client.issuer,
        &redirect_uri,
        &pkce,
        &state,
    );

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

        let tokens = exchange_code_for_tokens(&client, &redirect, &pkce, &code).await?;
        let account_id = extract_account_id(tokens.id_token.as_deref());
        let expires_at = tokens
            .expires_in
            .map(|sec| Utc::now() + chrono::Duration::seconds(sec));

        let record = AuthRecord {
            mode: AuthMode::ChatGPT,
            api_key: None,
            tokens: Some(StoredTokens {
                access_token: tokens.access_token,
                refresh_token: tokens.refresh_token,
                id_token: tokens.id_token,
                account_id,
                expires_at,
            }),
            last_refresh: Some(Utc::now()),
        };
        save_auth(&record)?;
        Ok(record)
    });

    Ok(PendingLogin {
        auth_url: url,
        completion,
    })
}

/// Convenience: synchronous-style wrapper used by the non-TUI `opencli login` CLI.
/// Prints the URL to stdout, waits for completion.
pub async fn login_with_browser(open_browser: bool) -> Result<AuthRecord> {
    let pending = start_browser_login(open_browser).await?;
    println!(
        "\n  Open the following URL to sign in:\n     {}\n",
        pending.auth_url
    );
    let record = pending
        .completion
        .await
        .map_err(|e| anyhow!("login task panicked: {e}"))??;
    println!("  Signed in.");
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
                let friendly = friendly_oauth_error(err, &desc);
                return Err(anyhow!(friendly));
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
        .route("/auth/callback", get(callback))
        .with_state(state);

    let (port, listener) = match try_bind(DEFAULT_PORT).await {
        Ok(v) => v,
        Err(_) => try_bind(FALLBACK_PORT)
            .await
            .context("Failed to bind callback port")?,
    };
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    Ok((port, redirect_uri, rx))
}

async fn try_bind(port: u16) -> Result<(u16, tokio::net::TcpListener)> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let port = listener.local_addr()?.port();
    Ok((port, listener))
}

/// Build a reqwest client with a sane timeout and a strict no-redirect
/// policy for the token endpoint. The default policy follows 301/302 and
/// silently converts POST to GET, dropping the body and consuming the
/// authorization code with no chance to retry.
fn token_http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

pub async fn exchange_code_for_tokens(
    client: &OauthClient,
    redirect_uri: &str,
    pkce: &Pkce,
    code: &str,
) -> Result<TokenSet> {
    let http = token_http()?;
    let endpoint = format!("{}/oauth/token", client.issuer.trim_end_matches('/'));
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(code),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(&client.client_id),
        urlencoding::encode(&pkce.code_verifier),
    );
    let resp = http
        .post(&endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Some IdPs echo the submitted code/verifier back in the error body.
        // Don't propagate the raw text — it leaks credentials into logs.
        return Err(anyhow!("token exchange failed {status}"));
    }
    let tokens: TokenSet =
        serde_json::from_str(&text).map_err(|e| anyhow!("failed to parse token response: {e}"))?;
    Ok(tokens)
}

pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenSet> {
    let http = token_http()?;
    let endpoint = format!("{}/oauth/token", ISSUER);
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&scope={}",
        urlencoding::encode(refresh_token),
        urlencoding::encode(CLIENT_ID),
        urlencoding::encode(SCOPES),
    );
    let resp = http
        .post(&endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        // Bodies on refresh failures often echo refresh_token back. Drop them.
        return Err(anyhow!("refresh token failed {status}"));
    }
    let tokens: TokenSet = serde_json::from_str(&text)
        .map_err(|e| anyhow!("failed to parse refresh response: {e}"))?;
    Ok(tokens)
}

pub async fn ensure_fresh(record: &AuthRecord) -> Result<String> {
    let tokens = record
        .tokens
        .as_ref()
        .ok_or_else(|| anyhow!("no token available"))?;
    // A missing `expires_at` previously meant "no refresh needed", so a token
    // whose server-issued expiry was absent was never refreshed even after it
    // had actually expired. Force a refresh in that case so the API stops
    // returning 401s after the first hour or so.
    let needs_refresh = match tokens.expires_at {
        Some(t) => t < Utc::now() + chrono::Duration::minutes(2),
        None => true,
    };
    if !needs_refresh {
        return Ok(tokens.access_token.clone());
    }

    // Serialize concurrent refreshes. Two parallel turns would otherwise both
    // see the expiring token, both POST to /oauth/token, and the second
    // save_auth would clobber the first refresh_token swap with a refresh
    // token that is no longer valid (refresh tokens are single-use).
    let _guard = REFRESH_LOCK.lock().await;

    // After acquiring the lock, re-load from disk in case a sibling caller
    // already refreshed while we were waiting. If their swap is still good,
    // we just return that access_token and skip the network round-trip.
    if let Ok(fresh_record) = load_auth() {
        if let Some(fresh_tokens) = fresh_record.tokens.as_ref() {
            let still_valid = match fresh_tokens.expires_at {
                Some(t) => t > Utc::now() + chrono::Duration::minutes(2),
                None => false,
            };
            if still_valid {
                return Ok(fresh_tokens.access_token.clone());
            }
        }
    }

    let refreshed = refresh_access_token(&tokens.refresh_token).await?;
    let expires_at = refreshed
        .expires_in
        .map(|sec| Utc::now() + chrono::Duration::seconds(sec));
    let mut updated = record.clone();
    if let Some(st) = updated.tokens.as_mut() {
        st.access_token = refreshed.access_token.clone();
        st.refresh_token = refreshed.refresh_token;
        if refreshed.id_token.is_some() {
            st.id_token = refreshed.id_token;
        }
        st.expires_at = expires_at;
    }
    updated.last_refresh = Some(Utc::now());
    save_auth(&updated)?;
    Ok(refreshed.access_token)
}

fn friendly_oauth_error(code: &str, desc: &str) -> String {
    let d = desc.to_ascii_lowercase();
    if code == "access_denied" && d.contains("no_valid_organizations") {
        return "Your ChatGPT account has no valid organizations for this app. \
                Make sure you are signed into a ChatGPT Plus/Pro/Team/Enterprise \
                account that has Codex access, or use an OpenAI API key instead."
            .into();
    }
    if code == "access_denied" && d.contains("missing_codex_entitlement") {
        return "Your ChatGPT plan doesn't include Codex access. \
                Upgrade your plan or use an OpenAI API key instead."
            .into();
    }
    if !desc.is_empty() {
        format!("OAuth error ({code}): {desc}")
    } else {
        format!("OAuth error: {code}")
    }
}

fn extract_account_id(id_token: Option<&str>) -> Option<String> {
    use base64::Engine;
    let token = id_token?;
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

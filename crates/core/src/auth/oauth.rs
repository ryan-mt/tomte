use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::pkce::{generate_pkce, random_state, Pkce};
use super::storage::{load_auth, save_auth, AuthMode, AuthRecord, StoredTokens};

type ShutdownHandle = Arc<AsyncMutex<Option<oneshot::Sender<()>>>>;

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";
pub const DEFAULT_PORT: u16 = 1455;
pub const FALLBACK_PORT: u16 = 1457;
const CALLBACK_HOST: &str = "127.0.0.1";
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

    let (_port, redirect_uri, code_rx, shutdown) = spawn_callback_server(&state).await?;
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
        let code = match tokio::time::timeout(Duration::from_secs(600), code_rx).await {
            Ok(Ok(result)) => result?,
            Ok(Err(_)) => {
                signal_shutdown(&shutdown).await;
                return Err(anyhow!("callback channel closed"));
            }
            Err(_) => {
                signal_shutdown(&shutdown).await;
                return Err(anyhow!("login timed out after 10 minutes"));
            }
        };

        let tokens = exchange_code_for_tokens(&client, &redirect, &pkce, &code).await?;
        let account_id = extract_account_id(tokens.id_token.as_deref());
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
        let mut record = load_auth().unwrap_or_default();
        record.mode = AuthMode::OpenaiOauth;
        record.tokens = Some(StoredTokens {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            id_token: tokens.id_token,
            account_id,
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

/// Convenience: synchronous-style wrapper used by the non-TUI `tomte login` CLI.
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

/// Classify an OAuth loopback callback's query params. `None` means the request
/// is not our redirect — its `state` is missing or doesn't match our per-login
/// nonce (a browser prefetch/favicon probe, or a forged/stray request) — so it
/// must be ignored rather than allowed to abort a login still waiting for the
/// real callback. `Some(Ok(code))` / `Some(Err(_))` is our callback to act on.
fn classify_callback(
    params: &HashMap<String, String>,
    expected_state: &str,
) -> Option<Result<String>> {
    if params.get("state").map(String::as_str) != Some(expected_state) {
        return None;
    }
    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        return Some(Err(anyhow!(friendly_oauth_error(err, &desc))));
    }
    Some(
        params
            .get("code")
            .cloned()
            .ok_or_else(|| anyhow!("missing code in callback")),
    )
}

async fn spawn_callback_server(
    expected_state: &str,
) -> Result<(
    u16,
    String,
    oneshot::Receiver<Result<String>>,
    ShutdownHandle,
)> {
    use axum::extract::{Query, State};
    use axum::response::Html;
    use axum::routing::get;
    use axum::Router;

    #[derive(Clone)]
    struct AppState {
        tx: Arc<tokio::sync::Mutex<Option<oneshot::Sender<Result<String>>>>>,
        shutdown_tx: ShutdownHandle,
        expected_state: String,
    }

    async fn callback(
        State(st): State<AppState>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Html<String> {
        let Some(result) = classify_callback(&params, &st.expected_state) else {
            // Not our redirect (state missing/mismatched) — ignore it WITHOUT
            // consuming the result channel or shutting the server down, so the
            // genuine callback can still complete the login. The caller's login
            // timeout still releases the port if none ever arrives.
            return Html(include_str!("../assets/error.html").to_string());
        };
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
        // Signal shutdown *after* we have the response body ready.
        // axum will finish sending this response before the server stops.
        if let Some(stx) = st.shutdown_tx.lock().await.take() {
            let _ = stx.send(());
        }
        Html(body.to_string())
    }

    let (tx, rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shutdown_handle = Arc::new(AsyncMutex::new(Some(shutdown_tx)));
    let state = AppState {
        tx: Arc::new(tokio::sync::Mutex::new(Some(tx))),
        shutdown_tx: shutdown_handle.clone(),
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
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // Also honour the login timeout: a closed shutdown channel
                // (sender dropped on timeout) is treated as a shutdown signal
                // so the port is released even if no callback ever arrived.
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let redirect_uri = callback_redirect_uri(port);
    Ok((port, redirect_uri, rx, shutdown_handle))
}

async fn signal_shutdown(handle: &ShutdownHandle) {
    if let Some(tx) = handle.lock().await.take() {
        let _ = tx.send(());
    }
}

async fn try_bind(port: u16) -> Result<(u16, tokio::net::TcpListener)> {
    let addr: SocketAddr = format!("{CALLBACK_HOST}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let port = listener.local_addr()?.port();
    Ok((port, listener))
}

fn callback_redirect_uri(port: u16) -> String {
    // The OAuth client whitelists `http://localhost:<port>/auth/callback` by
    // exact string match, so the redirect_uri MUST say `localhost` — even
    // though the listener binds CALLBACK_HOST (127.0.0.1) for a stable IPv4
    // socket that `localhost` resolves to. Sending 127.0.0.1 here makes the
    // authorize request fail with `authorize_hydra_invalid_request`.
    format!("http://localhost:{port}/auth/callback")
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

    // Serialize concurrent refreshes on the shared process-wide lock (both
    // providers contend on it). Two parallel turns would otherwise both see the
    // expiring token, both POST to /oauth/token, and the second save_auth would
    // clobber the first refresh_token swap with a refresh token that is no
    // longer valid (refresh tokens are single-use).
    let _guard = super::REFRESH_LOCK.lock().await;

    // After acquiring the lock, re-load from disk in case a sibling caller
    // already refreshed while we were waiting. If their swap is still good,
    // return that access_token directly. Otherwise, still prefer their
    // (newer, unconsumed) refresh_token and base the save on their record.
    let (base_record, refresh_token_to_use) = match load_auth() {
        Ok(fresh_record) => {
            if let Some(fresh_tokens) = fresh_record.tokens.as_ref() {
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

    let refreshed = refresh_access_token(&refresh_token_to_use).await?;
    let expires_at = refreshed.expires_in.and_then(|sec| {
        // Guard a hostile/buggy out-of-range `expires_in`: chrono's
        // Duration::seconds and (DateTime + TimeDelta) both PANIC on
        // overflow. None here is treated as "needs refresh", not a crash.
        chrono::Duration::try_seconds(sec).and_then(|d| Utc::now().checked_add_signed(d))
    });
    // Re-load immediately before saving and merge ONLY our refreshed OpenAI
    // tokens into the CURRENT on-disk record. Writing back the pre-refresh
    // snapshot would clobber a sibling Anthropic refresh that landed during our
    // network round-trip — restoring its already-consumed (single-use) refresh
    // token and bricking that credential (the two providers hold separate refresh
    // locks, so they are not serialized against each other).
    let mut updated = match load_auth() {
        Ok(latest) if latest.tokens.is_some() => latest,
        _ => base_record,
    };
    if let Some(st) = updated.tokens.as_mut() {
        st.access_token = refreshed.access_token.clone();
        st.refresh_token = refreshed.refresh_token;
        if refreshed.id_token.is_some() {
            st.id_token = refreshed.id_token;
        }
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
            "failed to persist refreshed OpenAI tokens: {e:#}; continuing on the \
             freshly issued access token (re-run `tomte login` if auth later fails)"
        );
    }
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

#[cfg(test)]
mod tests {
    use super::{callback_redirect_uri, classify_callback};
    use std::collections::HashMap;

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn classify_callback_ignores_state_mismatch_but_acts_on_ours() {
        // Wrong or missing state -> not our redirect -> ignored (None), so a
        // stray request can't abort a login still waiting for the real callback.
        assert!(
            classify_callback(&params(&[("state", "wrong"), ("code", "c")]), "right").is_none()
        );
        assert!(classify_callback(&params(&[("code", "c")]), "right").is_none());
        // Matching state with a code -> our callback, yields the code.
        assert!(matches!(
            classify_callback(&params(&[("state", "right"), ("code", "abc")]), "right"),
            Some(Ok(c)) if c == "abc"
        ));
        // Matching state carrying an OAuth error -> resolve as Err (not ignored).
        assert!(matches!(
            classify_callback(
                &params(&[("state", "right"), ("error", "access_denied")]),
                "right"
            ),
            Some(Err(_))
        ));
        // Matching state but no code -> Err, not silently ignored.
        assert!(matches!(
            classify_callback(&params(&[("state", "right")]), "right"),
            Some(Err(_))
        ));
    }

    #[test]
    fn callback_redirect_uri_uses_localhost_to_match_oauth_whitelist() {
        // The OAuth client whitelists the `localhost` host literally; sending
        // 127.0.0.1 is rejected as authorize_hydra_invalid_request even though
        // the listener binds 127.0.0.1 (localhost resolves to it).
        assert_eq!(
            callback_redirect_uri(1455),
            "http://localhost:1455/auth/callback"
        );
    }
}

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use opencli_core::agent::{Agent, AgentEvent};
use opencli_core::auth::resolve_credential;
use opencli_core::config;
use opencli_core::openai::OpenAiClient;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    sessions: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<Agent>>>>>,
    /// Per-process bearer token. Required on every /api request to defeat
    /// drive-by browser CSRF from other localhost services or visited pages.
    auth_token: Arc<String>,
    /// Canonicalised cwd captured at startup. The WS `start` frame's `cwd`
    /// field is restricted to this directory (or a subdirectory) so a
    /// malicious WS client can't pivot the agent's working directory to
    /// `/etc` or `~/.ssh`.
    base_cwd: Arc<PathBuf>,
}

pub async fn serve(port: u16) -> Result<()> {
    let auth_token = generate_token();
    let base_cwd = std::env::current_dir()?
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Persist token to a 0600 file under the config dir so the bundled UI
    // (or a sibling tool) can read it without requiring the user to paste
    // it manually. Same-machine processes already share the user account,
    // so file ACLs are sufficient.
    if let Err(e) = write_token_file(&auth_token) {
        tracing::warn!(error = %e, "could not persist server token; UI may need it pasted manually");
    }

    let state = AppState {
        sessions: Arc::new(Mutex::new(Default::default())),
        auth_token: Arc::new(auth_token.clone()),
        base_cwd: Arc::new(base_cwd),
    };

    // Lock CORS down to same-origin loopback (prod) + Vite dev (5173).
    // Wildcard `Any` previously let any visited page issue authenticated
    // POSTs to the local server.
    let port_for_cors = port;
    let cors = CorsLayer::new()
        .allow_origin([
            format!("http://127.0.0.1:{port_for_cors}").parse::<HeaderValue>()?,
            format!("http://localhost:{port_for_cors}").parse::<HeaderValue>()?,
            "http://127.0.0.1:5173".parse::<HeaderValue>()?,
            "http://localhost:5173".parse::<HeaderValue>()?,
        ])
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE, "x-opencli-token".parse()?])
        .allow_credentials(true);

    let ui_dir = locate_ui_dist();

    // Routes that mutate or expose privileged data require the bearer token.
    let auth_state = state.clone();
    let protected = Router::new()
        .route("/api/login", post(api_login))
        .route("/api/logout", post(api_logout))
        .route("/api/config", get(api_get_config).post(api_set_config))
        .route("/api/chat", get(ws_chat))
        .with_state(state.clone())
        .route_layer(axum::middleware::from_fn_with_state(
            auth_state,
            require_token,
        ));

    // /api/status is left open so the UI can detect the server before it has
    // a token to present. The response carries no credentials — just a mode
    // string and model name — so leaving it unauthenticated is acceptable.
    let mut app = Router::new()
        .route("/api/status", get(api_status))
        .merge(protected)
        .layer(cors);

    if let Some(dir) = ui_dir {
        let index = dir.join("index.html");
        app = app
            .nest_service("/assets", ServeDir::new(dir.join("assets")))
            .fallback_service(ServeFile::new(index));
    } else {
        app = app.fallback(get(no_ui_fallback));
    }

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("✅  Server running at http://{addr}");
    println!("🔑  Auth token: {auth_token}");
    println!("    UI: open http://{addr}?token={auth_token} (or paste header `X-OpenCLI-Token`)");
    axum::serve(listener, app).await?;
    Ok(())
}

fn generate_token() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Write the bearer token to `<config_dir>/server-token` with mode 0600 on
/// Unix so co-tenant tools on the same machine can pick it up without
/// requiring the user to copy/paste.
fn write_token_file(token: &str) -> std::io::Result<()> {
    let dir = opencli_core::config::config_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("server-token");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(token.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, token)?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct TokenQuery {
    #[serde(default)]
    token: Option<String>,
}

/// Axum middleware: require the per-process bearer token on every /api
/// request. Accepts either an `X-OpenCLI-Token` header or a `?token=`
/// query parameter (the WS upgrade has no other practical channel).
async fn require_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let header_tok = headers
        .get("x-opencli-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let provided = header_tok.or(q.token);
    let expected = state.auth_token.as_str();
    let ok = provided
        .as_deref()
        .map(|t| constant_time_eq(t.as_bytes(), expected.as_bytes()))
        .unwrap_or(false);
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing or invalid X-OpenCLI-Token"})),
        )
            .into_response();
    }
    next.run(req).await
}

/// Constant-time byte-string comparison — finishes in time proportional to
/// the length without leaking the prefix match length via early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn locate_ui_dist() -> Option<PathBuf> {
    // 1. Env override
    if let Ok(v) = std::env::var("OPENCLI_UI_DIST") {
        let p = PathBuf::from(v);
        if p.join("index.html").exists() {
            return Some(p);
        }
    }
    // 2. Sibling of binary
    if let Ok(exe) = std::env::current_exe() {
        for hop in [1, 2, 3, 4] {
            if let Some(anc) = exe.ancestors().nth(hop) {
                let p = anc.join("ui").join("dist");
                if p.join("index.html").exists() {
                    return Some(p);
                }
            }
        }
    }
    // 3. CWD/ui/dist
    let p = std::env::current_dir().ok()?.join("ui").join("dist");
    if p.join("index.html").exists() {
        Some(p)
    } else {
        None
    }
}

async fn no_ui_fallback() -> Response {
    let html = r#"<!doctype html><html><head><meta charset="utf-8"><title>opencli</title>
    <style>body{font-family:sans-serif;background:#0a0a0a;color:#eee;padding:32px;max-width:720px;margin:auto}code{background:#222;padding:2px 6px;border-radius:4px}</style>
    </head><body><h1>opencli</h1>
    <p>The Web UI has not been built yet. Run:</p>
    <pre><code>cd ui && npm install && npm run build</code></pre>
    <p>Or run <code>npm run dev</code> for dev mode on :5173 — the API server stays on :7777.</p>
    </body></html>"#;
    (StatusCode::OK, [("content-type", "text/html")], html).into_response()
}

#[derive(Serialize)]
struct StatusResp {
    mode: String,
    account_id: Option<String>,
    model: String,
}

async fn api_status() -> Json<StatusResp> {
    let auth = opencli_core::auth::load_auth().unwrap_or_default();
    let cfg = config::load();
    let (mode, account_id) = match auth.mode {
        opencli_core::auth::AuthMode::ChatGPT => (
            "chatgpt".to_string(),
            auth.tokens.and_then(|t| t.account_id),
        ),
        opencli_core::auth::AuthMode::ApiKey => ("api_key".to_string(), None),
        opencli_core::auth::AuthMode::None => ("none".to_string(), None),
    };
    Json(StatusResp {
        mode,
        account_id,
        model: cfg.model,
    })
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    api_key: Option<String>,
}

async fn api_login(Json(body): Json<LoginBody>) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(k) = body.api_key {
        let record = opencli_core::auth::AuthRecord {
            mode: opencli_core::auth::AuthMode::ApiKey,
            api_key: Some(k),
            tokens: None,
            last_refresh: None,
        };
        opencli_core::auth::save_auth(&record).map_err(ApiError::from)?;
        return Ok(Json(serde_json::json!({"ok": true})));
    }
    tokio::spawn(async {
        // Surface OAuth failures via tracing instead of `let _ = …`. Without
        // this, the HTTP handler returns {ok: true, browser: true} while
        // the login silently dies and the user is left wondering why nothing
        // happened.
        if let Err(e) = opencli_core::auth::login_with_browser(true).await {
            tracing::warn!(error = %e, "browser login failed");
        }
    });
    Ok(Json(serde_json::json!({"ok": true, "browser": true})))
}

async fn api_logout() -> Json<serde_json::Value> {
    let path = config::config_dir().join("auth.json");
    let _ = std::fs::remove_file(&path);
    Json(serde_json::json!({"ok": true}))
}

async fn api_get_config() -> Json<config::Config> {
    Json(config::load())
}

async fn api_set_config(Json(cfg): Json<config::Config>) -> Result<Json<config::Config>, ApiError> {
    config::save(&cfg).map_err(|e| ApiError(anyhow::anyhow!(e)))?;
    Ok(Json(config::load()))
}

#[derive(Deserialize)]
struct ChatStart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

async fn ws_chat(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    // First message defines the session.
    let Some(Ok(first)) = socket.recv().await else {
        return;
    };
    let text = match first {
        Message::Text(t) => t,
        _ => return,
    };
    let start: ChatStart = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let _ = socket
                .send(Message::Text(json_err(format!("bad start: {e}"))))
                .await;
            return;
        }
    };
    if start.kind != "start" {
        let _ = socket
            .send(Message::Text(json_err("expected type=start".into())))
            .await;
        return;
    }
    let session_id = start.session_id.unwrap_or_else(nanoid_like);
    let prompt = start.prompt.unwrap_or_default();
    if prompt.trim().is_empty() {
        let _ = socket
            .send(Message::Text(json_err("empty prompt".into())))
            .await;
        return;
    }

    let agent_arc = {
        let mut sessions = state.sessions.lock().await;
        if let Some(a) = sessions.get(&session_id) {
            a.clone()
        } else {
            let credential = match resolve_credential().await {
                Ok(c) => c,
                Err(e) => {
                    let _ = socket.send(Message::Text(json_err(e.to_string()))).await;
                    return;
                }
            };
            let client = match OpenAiClient::new(credential) {
                Ok(c) => c,
                Err(e) => {
                    let _ = socket.send(Message::Text(json_err(e.to_string()))).await;
                    return;
                }
            };
            let mut cfg = config::load();
            if let Some(m) = start.model {
                cfg.model = m;
            }
            if let Some(r) = start.reasoning {
                cfg.reasoning_effort = r;
            }
            let mut agent = Agent::new(client, cfg);
            agent.require_approval = true;
            // cwd containment: canonicalize and require the path to be the
            // server's startup cwd or a subdirectory of it. Without this a
            // malicious WS client could send `{"cwd":"/"}` and pivot the
            // agent into reading /etc/passwd via the `read_file` tool.
            if let Some(cwd) = start.cwd {
                let p = std::path::PathBuf::from(&cwd);
                match p.canonicalize() {
                    Ok(canon) if canon.starts_with(state.base_cwd.as_path()) && canon.is_dir() => {
                        agent.cwd = canon;
                    }
                    _ => {
                        let _ = socket
                            .send(Message::Text(json_err(format!(
                                "cwd {cwd:?} is outside the server's base directory {} or does not exist",
                                state.base_cwd.display()
                            ))))
                            .await;
                        return;
                    }
                }
            }
            let arc = Arc::new(Mutex::new(agent));
            sessions.insert(session_id.clone(), arc.clone());
            arc
        }
    };

    {
        let mut agent = agent_arc.lock().await;
        agent.push_user_message(prompt);
    }

    // Snapshot of the agent's approval-channel Arc, captured BEFORE the
    // long-lived turn lock so WS approval_decision frames don't deadlock
    // waiting for run_turn to release the outer mutex.
    let approvals_handle = {
        let a = agent_arc.lock().await;
        a.pending_approvals.clone()
    };
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
    let agent_arc_clone = agent_arc.clone();
    let run_task = tokio::spawn(async move {
        let mut agent = agent_arc_clone.lock().await;
        agent.run_turn(tx).await
    });

    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                let payload = match serde_json::to_string(&ev) {
                    Ok(s) => s,
                    Err(e) => json_err(format!("serialize: {e}")),
                };
                if socket.send(Message::Text(payload)).await.is_err() { break; }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v.get("kind").and_then(|k| k.as_str()) == Some("approval_decision") {
                                let call_id = v.get("call_id").and_then(|c| c.as_str()).unwrap_or("").to_string();
                                let granted = v.get("granted").and_then(|g| g.as_bool()).unwrap_or(false);
                                if !call_id.is_empty() {
                                    let sender = {
                                        let mut map = approvals_handle.lock().await;
                                        map.remove(&call_id)
                                    };
                                    if let Some(s) = sender {
                                        let _ = s.send(granted);
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
    let _ = run_task.await;
    let _ = socket.send(Message::Close(None)).await;
}

fn json_err(msg: String) -> String {
    serde_json::json!({"kind": "Error", "message": msg}).to_string()
}

fn nanoid_like() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut b = [0u8; 9];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

struct ApiError(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": self.0.to_string()})),
        )
            .into_response()
    }
}

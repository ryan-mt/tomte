use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use rand::RngCore;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;

use crate::auth::Credential;

use super::models::ResponsesRequest;
use super::stream::StreamHandle;

/// Cap connect time so a black-holed DNS or unreachable host fails fast
/// rather than hanging the agent turn. Streaming responses can legitimately
/// be long-lived, so we deliberately leave the per-request `.timeout()`
/// unset and rely on `STREAM_IDLE_TIMEOUT` in the agent layer to catch
/// silent stream stalls.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Best-effort scrub of `sk-…` / `Bearer …` substrings from an error body
/// before it becomes part of an anyhow error string. The API has been
/// observed to echo the submitted Authorization header back inside 401
/// bodies; this prevents that echo from propagating into logs.
fn redact_auth_in(body: &str) -> String {
    let mut out = body.to_string();
    for token in ["sk-", "sk_proj_", "Bearer "] {
        while let Some(i) = out.find(token) {
            let tail = &out[i + token.len()..];
            let end = tail
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || *c == '"' || *c == '}')
                .map(|(j, _)| i + token.len() + j)
                .unwrap_or(out.len());
            out.replace_range(i..end, "<redacted>");
        }
    }
    out
}

const API_BASE: &str = "https://api.openai.com/v1";
const CHATGPT_BACKEND_BASE: &str = "https://chatgpt.com/backend-api/codex";

pub struct OpenAiClient {
    http: reqwest::Client,
    credential: Credential,
    session_id: String,
}

fn random_id() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

impl OpenAiClient {
    pub fn new(credential: Credential) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("opencli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            credential,
            session_id: random_id(),
        })
    }

    fn responses_endpoint(&self) -> String {
        if self.credential.is_chatgpt_subscription() {
            format!("{CHATGPT_BACKEND_BASE}/responses")
        } else {
            format!("{API_BASE}/responses")
        }
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.credential.auth_header_value())?,
        );
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("Accept", HeaderValue::from_static("text/event-stream"));
        if let Credential::OAuth { account_id: Some(id), .. } = &self.credential {
            h.insert("ChatGPT-Account-ID", HeaderValue::from_str(id)?);
        }
        if self.credential.is_chatgpt_subscription() {
            h.insert("OpenAI-Beta", HeaderValue::from_static("responses=v1"));
            h.insert("OAI-Product-Sku", HeaderValue::from_static("codex"));
            h.insert("originator", HeaderValue::from_static("opencli"));
            h.insert(
                "session_id",
                HeaderValue::from_str(&self.session_id)?,
            );
        }
        Ok(h)
    }

    /// Stream a Responses API request, returning a handle producing SSE events.
    pub async fn stream(&self, mut req: ResponsesRequest) -> Result<StreamHandle> {
        req.stream = true;
        self.apply_credential_defaults(&mut req);
        self.send_internal(req).await
    }

    /// Non-streaming variant.
    pub async fn create(&self, mut req: ResponsesRequest) -> Result<serde_json::Value> {
        req.stream = false;
        self.apply_credential_defaults(&mut req);
        let resp = self
            .http
            .post(self.responses_endpoint())
            .headers(self.headers()?)
            .json(&req)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("OpenAI {} {}", status, redact_auth_in(&text)));
        }
        serde_json::from_str(&text).with_context(|| format!("parse response: {text}"))
    }

    fn apply_credential_defaults(&self, req: &mut ResponsesRequest) {
        // ChatGPT backend rejects requests unless `store: false` is explicitly set.
        // For the public API (`api.openai.com`), the default behavior (server-side
        // storage) is fine; we leave `store` unset.
        if self.credential.is_chatgpt_subscription() && req.store.is_none() {
            req.store = Some(false);
        }
    }

    async fn send_internal(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        let resp = self
            .http
            .post(self.responses_endpoint())
            .headers(self.headers()?)
            .json(&req)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => format!("(failed to read error body: {e})"),
            };
            return Err(anyhow!("OpenAI {} {}", status, text));
        }
        Ok(StreamHandle::from_response(resp))
    }
}

/// Helper: convenient way to call a non-streaming endpoint with arbitrary body.
pub async fn raw_post<B: Serialize>(
    credential: &Credential,
    path: &str,
    body: &B,
) -> Result<serde_json::Value> {
    let http = reqwest::Client::new();
    let url = if credential.is_chatgpt_subscription() {
        format!("{CHATGPT_BACKEND_BASE}/{}", path.trim_start_matches('/'))
    } else {
        format!("{API_BASE}/{}", path.trim_start_matches('/'))
    };
    let mut req = http
        .post(&url)
        .header(AUTHORIZATION, credential.auth_header_value())
        .header(CONTENT_TYPE, "application/json")
        .json(body);
    if let Credential::OAuth { account_id: Some(id), .. } = credential {
        req = req.header("ChatGPT-Account-ID", id.clone());
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("OpenAI {} {}", status, text));
    }
    serde_json::from_str(&text).with_context(|| format!("parse: {text}"))
}

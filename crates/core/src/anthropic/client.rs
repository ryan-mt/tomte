//! HTTP client for the Anthropic Messages API. Accepts the OpenAI-shaped
//! [`ResponsesRequest`] for compatibility with the existing agent loop; the
//! actual on-wire format is built by [`super::translate`].

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

use crate::auth::Credential;
use crate::openai::stream::StreamHandle;
use crate::openai::ResponsesRequest;
use crate::provider::Provider;

use super::models::MessagesRequest;
use super::stream::handle_from_response;
use super::translate::to_messages_request;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const API_BASE: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Beta-feature flags required for OAuth subscription tokens to be accepted
/// by the Messages API. These match the values the official Claude Code CLI
/// sends; they are undocumented and may change without notice.
const OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20";

/// Required first line of the system prompt when authenticating with an
/// OAuth subscription token (except for Haiku models). Anthropic validates
/// this server-side; missing it produces a generic 400.
const OAUTH_IDENTITY_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

pub struct AnthropicClient {
    http: reqwest::Client,
    credential: Credential,
}

impl AnthropicClient {
    pub fn new(credential: Credential) -> Result<Self> {
        if credential.provider() != Provider::Anthropic {
            return Err(anyhow!(
                "AnthropicClient expects an Anthropic credential, got {}",
                credential.provider()
            ));
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("opencli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Ok(Self { http, credential })
    }

    fn endpoint(&self) -> String {
        format!("{API_BASE}/messages")
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("Accept", HeaderValue::from_static("text/event-stream"));
        h.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        match &self.credential {
            Credential::ApiKey { key, .. } => {
                h.insert("x-api-key", HeaderValue::from_str(key)?);
            }
            Credential::OAuth { access_token, .. } => {
                h.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
                h.insert("anthropic-beta", HeaderValue::from_static(OAUTH_BETA));
                h.insert(
                    "anthropic-dangerous-direct-browser-access",
                    HeaderValue::from_static("true"),
                );
                h.insert("x-app", HeaderValue::from_static("cli"));
            }
        }
        Ok(h)
    }

    pub async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        let mut body = to_messages_request(&req);
        body.stream = true;
        self.apply_oauth_identity(&mut body);
        let resp = self
            .http
            .post(self.endpoint())
            .headers(self.headers()?)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => format!("(failed to read error body: {e})"),
            };
            return Err(anyhow!("Anthropic {} {}", status, text));
        }
        Ok(handle_from_response(resp))
    }

    pub async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value> {
        let mut body = to_messages_request(&req);
        body.stream = false;
        self.apply_oauth_identity(&mut body);
        let resp = self
            .http
            .post(self.endpoint())
            .headers(self.headers()?)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("Anthropic {} {}", status, text));
        }
        serde_json::from_str(&text).map_err(|e| anyhow!("parse Anthropic response: {e}: {text}"))
    }

    /// Prepend the Claude Code identity line to the system prompt when using
    /// OAuth subscription tokens, since Anthropic rejects the request without
    /// it on non-Haiku models.
    fn apply_oauth_identity(&self, body: &mut MessagesRequest) {
        if !self.credential.is_anthropic_oauth() {
            return;
        }
        use super::models::SystemBlock;
        let identity = SystemBlock::Text {
            text: OAUTH_IDENTITY_PROMPT.to_string(),
        };
        match body.system.as_mut() {
            Some(blocks) => {
                let already_present = matches!(
                    blocks.first(),
                    Some(SystemBlock::Text { text }) if text.starts_with(OAUTH_IDENTITY_PROMPT)
                );
                if !already_present {
                    blocks.insert(0, identity);
                }
            }
            None => {
                body.system = Some(vec![identity]);
            }
        }
    }
}

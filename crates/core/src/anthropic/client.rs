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

/// Beta flag that unlocks the 1M-token context window on the Claude API. Sent
/// only for models that actually have a 1M window (see `model_supports_1m`);
/// requesting it for a 200K model is rejected, and it is harmless for the GA-1M
/// models. Mirrors how the official Claude Code CLI enables 1M.
const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";

/// Required first line of the system prompt when authenticating with an
/// OAuth subscription token (except for Haiku models). Anthropic validates
/// this server-side; missing it produces a generic 400.
const OAUTH_IDENTITY_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Compute the `anthropic-beta` header value, or `None` when no betas apply.
/// OAuth always carries the claude-code/oauth betas; the 1M context beta is
/// appended on top for any 1M-window model. On the API-key path the only beta
/// we ever send is the 1M one, and only for 1M models.
fn anthropic_beta_value(is_oauth: bool, model: &str) -> Option<String> {
    let mut betas: Vec<&str> = Vec::new();
    if is_oauth {
        betas.push(OAUTH_BETA);
    }
    if crate::agent::model_supports_1m(model) {
        betas.push(CONTEXT_1M_BETA);
    }
    if betas.is_empty() {
        None
    } else {
        Some(betas.join(","))
    }
}

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

    fn headers(&self, model: &str) -> Result<HeaderMap> {
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
                h.insert(
                    "anthropic-dangerous-direct-browser-access",
                    HeaderValue::from_static("true"),
                );
                h.insert("x-app", HeaderValue::from_static("cli"));
            }
        }
        // anthropic-beta: OAuth needs the claude-code/oauth flags; the 1M context
        // beta is added on top for any 1M-window model (and is the only beta the
        // API-key path ever sends). Gated on the model so we never request 1M for
        // a 200K model, which the API would reject.
        if let Some(beta) = anthropic_beta_value(self.credential.is_anthropic_oauth(), model) {
            h.insert("anthropic-beta", HeaderValue::from_str(&beta)?);
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
            .headers(self.headers(&body.model)?)
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
            .headers(self.headers(&body.model)?)
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
            cache_control: None,
        };
        match body.system.as_mut() {
            Some(blocks) => {
                let already_present = matches!(
                    blocks.first(),
                    Some(SystemBlock::Text { text, .. }) if text.starts_with(OAUTH_IDENTITY_PROMPT)
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

#[cfg(test)]
mod beta_header_tests {
    use super::{anthropic_beta_value, CONTEXT_1M_BETA, OAUTH_BETA};

    #[test]
    fn oauth_1m_model_appends_context_1m_beta() {
        let v = anthropic_beta_value(true, "claude-opus-4-8").unwrap();
        assert_eq!(v, format!("{OAUTH_BETA},{CONTEXT_1M_BETA}"));
    }

    #[test]
    fn oauth_200k_model_keeps_only_oauth_betas() {
        let v = anthropic_beta_value(true, "claude-sonnet-4-5").unwrap();
        assert_eq!(v, OAUTH_BETA);
    }

    #[test]
    fn api_key_1m_model_sends_only_context_1m_beta() {
        let v = anthropic_beta_value(false, "claude-sonnet-4-6").unwrap();
        assert_eq!(v, CONTEXT_1M_BETA);
    }

    #[test]
    fn api_key_200k_model_sends_no_beta() {
        assert!(anthropic_beta_value(false, "claude-haiku-4-5").is_none());
    }
}

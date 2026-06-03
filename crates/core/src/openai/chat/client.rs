//! The OpenAI-compatible Chat Completions `ProviderClient`. Split out of
//! `chat`; logic unchanged.

use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};

use crate::client::ProviderClient;
use crate::openai::models::ResponsesRequest;
use crate::openai::stream::StreamHandle;
use crate::provider::Provider;

use super::request::chat_request_body;
use super::stream::handle_chat_response;

/// HTTP client for an OpenAI-compatible Chat Completions provider configured in
/// `config.providers`. Implements the shared [`ProviderClient`] so the agent
/// loop drives it identically to the built-in providers.
pub struct ChatCompletionsClient {
    http: reqwest::Client,
    provider_id: String,
    base_url: String,
    api_key: String,
    forward_reasoning_effort: bool,
}

impl ChatCompletionsClient {
    pub fn new(
        provider_id: String,
        base_url: String,
        api_key: String,
        forward_reasoning_effort: bool,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            http,
            provider_id,
            base_url,
            api_key,
            forward_reasoning_effort,
        })
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Strip the `<id>/` routing prefix so the upstream sees its native model id.
    fn wire_model(&self, model: &str) -> String {
        let prefix = format!("{}/", self.provider_id);
        model.strip_prefix(&prefix).unwrap_or(model).to_string()
    }

    async fn send(&self, mut req: ResponsesRequest, stream: bool) -> Result<reqwest::Response> {
        req.model = self.wire_model(&req.model);
        req.stream = stream;
        let body = chat_request_body(&req, self.forward_reasoning_effort);
        let mut builder = self
            .http
            .post(self.endpoint())
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if !self.api_key.is_empty() {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", self.api_key));
        }
        Ok(crate::retry::send_with_retry(builder).await?)
    }
}

#[async_trait]
impl ProviderClient for ChatCompletionsClient {
    fn provider(&self) -> Provider {
        // Reported as OpenAI since the wire protocol is OpenAI-compatible; the
        // value is informational only (nothing routes on it).
        Provider::OpenAi
    }

    async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        let resp = self.send(req, true).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "{} {} {}",
                self.provider_id,
                status,
                crate::sensitive::redact_auth_in(&text)
            ));
        }
        Ok(handle_chat_response(resp))
    }

    async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value> {
        let resp = self.send(req, false).await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "{} {} {}",
                self.provider_id,
                status,
                crate::sensitive::redact_auth_in(&text)
            ));
        }
        serde_json::from_str(&text).map_err(|e| anyhow!("parse Chat Completions response: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_endpoint_trims_slash_and_strips_model_prefix() {
        let c = ChatCompletionsClient::new(
            "groq".into(),
            "https://api.groq.com/openai/v1/".into(),
            "k".into(),
            false,
        )
        .unwrap();
        assert_eq!(
            c.endpoint(),
            "https://api.groq.com/openai/v1/chat/completions"
        );
        assert_eq!(c.wire_model("groq/llama-3.3-70b"), "llama-3.3-70b");
        // A bare id (no provider prefix) is left untouched.
        assert_eq!(c.wire_model("llama-3.3-70b"), "llama-3.3-70b");
    }
}

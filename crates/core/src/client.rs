//! Provider-agnostic LLM client. The agent loop talks to [`LlmClient`] and
//! never has to know whether it's hitting OpenAI or Anthropic. Construction
//! picks the concrete provider client; everything after dispatches through the
//! [`ProviderClient`] trait object, so adding a provider only touches
//! [`LlmClient::new`] plus the new provider module — not a match arm per method.

use anyhow::Result;
use async_trait::async_trait;

use crate::anthropic::AnthropicClient;
use crate::auth::Credential;
use crate::config::Config;
use crate::openai::chat::ChatCompletionsClient;
use crate::openai::stream::StreamHandle;
use crate::openai::{OpenAiClient, ResponsesRequest};
use crate::provider::Provider;

/// One LLM provider's HTTP client behind a uniform interface. Each implementor
/// translates the shared [`ResponsesRequest`] IR into its own wire format.
#[async_trait]
pub trait ProviderClient: Send + Sync {
    fn provider(&self) -> Provider;
    async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle>;
    async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value>;
}

/// Provider-agnostic handle the agent loop holds. Wraps the concrete provider
/// client as a trait object so call sites never match on the provider.
pub struct LlmClient {
    inner: Box<dyn ProviderClient>,
}

impl LlmClient {
    /// Build a client appropriate for the credential's provider. This match is
    /// the single place a new provider must be wired in.
    pub fn new(credential: Credential) -> Result<Self> {
        let inner: Box<dyn ProviderClient> = match credential.provider() {
            Provider::OpenAi => Box::new(OpenAiClient::new(credential)?),
            Provider::Anthropic => Box::new(AnthropicClient::new(credential)?),
        };
        Ok(Self { inner })
    }

    /// Build a client from the active config. A `<id>/<model>` whose `<id>` is
    /// declared in `config.providers` routes to the OpenAI-compatible Chat
    /// Completions adapter; everything else uses the built-in OpenAI/Anthropic
    /// path, resolving the stored credential for the detected provider.
    pub async fn for_config(cfg: &Config) -> Result<Self> {
        if let Some((prefix, _)) = cfg.model.split_once('/') {
            if let Some(pc) = cfg.providers.get(prefix) {
                let client = ChatCompletionsClient::new(
                    prefix.to_string(),
                    pc.base_url.clone(),
                    pc.resolve_api_key(),
                )?;
                return Ok(Self {
                    inner: Box::new(client),
                });
            }
        }
        let provider = Provider::from_model(&cfg.model);
        let credential = crate::auth::resolve_credential(provider).await?;
        Self::new(credential)
    }

    pub fn provider(&self) -> Provider {
        self.inner.provider()
    }

    pub async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        self.inner.stream(req).await
    }

    pub async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value> {
        self.inner.create(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ProviderConfig};

    #[tokio::test]
    async fn for_config_routes_declared_provider_to_chat_adapter() {
        // A `<id>/<model>` whose id is in `providers` builds without touching
        // auth.json or the network (the Chat Completions client is local).
        let cfg = Config {
            model: "groq/llama-3.3-70b".to_string(),
            providers: std::collections::HashMap::from([(
                "groq".to_string(),
                ProviderConfig {
                    base_url: "https://api.groq.com/openai/v1".to_string(),
                    api_key: Some("sk-test".to_string()),
                    api_key_env: None,
                    context_limit: None,
                },
            )]),
            ..Config::default()
        };
        let client = LlmClient::for_config(&cfg).await.unwrap();
        // The Chat Completions adapter reports the OpenAI-compatible protocol.
        assert_eq!(client.provider(), Provider::OpenAi);
    }
}

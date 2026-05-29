//! Provider-agnostic LLM client. The agent loop talks to [`LlmClient`] and
//! never has to know whether it's hitting OpenAI or Anthropic. Construction
//! picks the concrete provider client; everything after dispatches through the
//! [`ProviderClient`] trait object, so adding a provider only touches
//! [`LlmClient::new`] plus the new provider module — not a match arm per method.

use anyhow::Result;
use async_trait::async_trait;

use crate::anthropic::AnthropicClient;
use crate::auth::Credential;
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

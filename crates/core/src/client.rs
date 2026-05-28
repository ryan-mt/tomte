//! Provider-agnostic LLM client. The agent loop talks to this enum and never
//! has to know whether it's hitting OpenAI or Anthropic; each variant wraps
//! the provider-specific HTTP client and translates the request shape if
//! needed.

use anyhow::Result;

use crate::anthropic::AnthropicClient;
use crate::auth::Credential;
use crate::openai::stream::StreamHandle;
use crate::openai::{OpenAiClient, ResponsesRequest};
use crate::provider::Provider;

pub enum LlmClient {
    OpenAi(OpenAiClient),
    Anthropic(AnthropicClient),
}

impl LlmClient {
    /// Build a client appropriate for the credential's provider.
    pub fn new(credential: Credential) -> Result<Self> {
        match credential.provider() {
            Provider::OpenAi => Ok(Self::OpenAi(OpenAiClient::new(credential)?)),
            Provider::Anthropic => Ok(Self::Anthropic(AnthropicClient::new(credential)?)),
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            Self::OpenAi(_) => Provider::OpenAi,
            Self::Anthropic(_) => Provider::Anthropic,
        }
    }

    pub async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        match self {
            Self::OpenAi(c) => c.stream(req).await,
            Self::Anthropic(c) => c.stream(req).await,
        }
    }

    pub async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value> {
        match self {
            Self::OpenAi(c) => c.create(req).await,
            Self::Anthropic(c) => c.create(req).await,
        }
    }
}

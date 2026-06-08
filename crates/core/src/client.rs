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
            if let Some(pc) = cfg
                .providers
                .get(prefix)
                .cloned()
                .or_else(|| crate::config::builtin_provider(prefix))
            {
                let client = ChatCompletionsClient::new(
                    prefix.to_string(),
                    pc.base_url.clone(),
                    pc.resolve_api_key(),
                    pc.forward_reasoning_effort,
                )?;
                return Ok(Self {
                    inner: Box::new(client),
                });
            }
        }
        let provider = Provider::from_model(&cfg.model);
        let credential = crate::auth::resolve_credential(provider).await?;
        if let Some(msg) = chatgpt_oauth_model_rejection(&credential, &cfg.model) {
            anyhow::bail!(msg);
        }
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

/// On a ChatGPT/Codex subscription the OpenAI backend accepts only a narrow
/// model set (see [`crate::catalog::openai_chatgpt_oauth_models`]); any other id
/// 400s at request time. Return a clear, actionable error for such a model so
/// the failure names the supported set instead of surfacing a raw provider 400.
/// API-key credentials are unaffected — they keep the full public catalogue.
fn chatgpt_oauth_model_rejection(credential: &Credential, model: &str) -> Option<String> {
    if credential.is_chatgpt_subscription()
        && !crate::catalog::openai_chatgpt_oauth_models().contains(&model)
    {
        let supported = crate::catalog::openai_chatgpt_oauth_models().join(", ");
        Some(format!(
            "model `{model}` isn't available on a ChatGPT/Codex subscription. \
             Supported with this sign-in: {supported}. Switch with /model, or sign in \
             with an OpenAI API key to use the full catalogue."
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ProviderConfig};

    #[test]
    fn chatgpt_oauth_rejects_api_key_only_models() {
        let oauth = Credential::OAuth {
            provider: Provider::OpenAi,
            access_token: "t".into(),
            account_id: None,
        };
        // An allowlisted model passes the guard.
        assert!(chatgpt_oauth_model_rejection(&oauth, "gpt-5.5").is_none());
        // A non-allowlisted model is rejected, naming the model and the set.
        let msg = chatgpt_oauth_model_rejection(&oauth, "gpt-5-nano").expect("should reject");
        assert!(msg.contains("gpt-5-nano"), "{msg}");
        assert!(msg.contains("gpt-5.5"), "{msg}");
    }

    #[test]
    fn api_key_credential_keeps_full_catalogue() {
        let api = Credential::ApiKey {
            provider: Provider::OpenAi,
            key: "sk-x".into(),
        };
        assert!(chatgpt_oauth_model_rejection(&api, "gpt-5-nano").is_none());
        assert!(chatgpt_oauth_model_rejection(&api, "gpt-5.4-mini").is_none());
    }

    #[test]
    fn anthropic_oauth_is_unaffected() {
        let anth = Credential::OAuth {
            provider: Provider::Anthropic,
            access_token: "t".into(),
            account_id: None,
        };
        assert!(chatgpt_oauth_model_rejection(&anth, "claude-opus-4-8").is_none());
    }

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
                    forward_reasoning_effort: false,
                },
            )]),
            ..Config::default()
        };
        let client = LlmClient::for_config(&cfg).await.unwrap();
        // The Chat Completions adapter reports the OpenAI-compatible protocol.
        assert_eq!(client.provider(), Provider::OpenAi);
    }

    #[tokio::test]
    async fn for_config_routes_builtin_provider_without_declaration() {
        // `groq/<model>` with NO `providers` entry still routes to the Chat
        // adapter via the built-in preset — works out of the box, no network.
        let cfg = Config {
            model: "groq/llama-3.3-70b".to_string(),
            ..Config::default()
        };
        let client = LlmClient::for_config(&cfg).await.unwrap();
        assert_eq!(client.provider(), Provider::OpenAi);
    }
}

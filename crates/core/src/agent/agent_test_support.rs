//! Shared helpers for the agent unit-test groups.

use std::collections::HashMap;

use crate::config::ProviderConfig;

use super::*;

pub(super) async fn session_test_agent() -> Agent {
    let config = Config {
        model: "local/test-model".to_string(),
        providers: HashMap::from([(
            "local".to_string(),
            ProviderConfig {
                base_url: "http://localhost/v1".to_string(),
                api_key: Some("sk-test".to_string()),
                api_key_env: None,
                context_limit: None,
                forward_reasoning_effort: false,
            },
        )]),
        ..Config::default()
    };
    let client = LlmClient::for_config(&config).await.unwrap();
    Agent::new(client, config)
}

/// A configured `local/<model>` provider (built offline, no credential
/// lookup) used as a fallback target in failover tests.
pub(super) fn local_provider_map() -> HashMap<String, ProviderConfig> {
    HashMap::from([(
        "local".to_string(),
        ProviderConfig {
            base_url: "http://localhost/v1".to_string(),
            api_key: Some("sk-test".to_string()),
            api_key_env: None,
            context_limit: None,
            forward_reasoning_effort: false,
        },
    )])
}

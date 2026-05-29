pub mod anthropic;
pub mod oauth;
pub mod pkce;
pub mod storage;

pub use oauth::{
    login_with_browser, refresh_access_token, start_browser_login, OauthClient, PendingLogin,
    TokenSet,
};
pub use storage::{
    clear_credential, effective_mode, has_anthropic_api_key, has_anthropic_oauth,
    has_openai_api_key, has_openai_oauth, load_auth, save_auth, AuthMode, AuthRecord, LogoutTarget,
};

use anyhow::Result;

use crate::provider::Provider;

pub fn effective_mode_with_env(record: &AuthRecord) -> AuthMode {
    effective_mode_with_env_values(
        record,
        std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
    )
}

fn effective_mode_with_env_values(
    record: &AuthRecord,
    openai_api_key: String,
    anthropic_api_key: String,
) -> AuthMode {
    if !openai_api_key.is_empty() {
        AuthMode::OpenaiApiKey
    } else if !anthropic_api_key.is_empty() {
        AuthMode::AnthropicApiKey
    } else {
        effective_mode(record)
    }
}

#[derive(Debug, Clone)]
pub enum Credential {
    OAuth {
        provider: Provider,
        access_token: String,
        account_id: Option<String>,
    },
    ApiKey {
        provider: Provider,
        key: String,
    },
}

impl Credential {
    pub fn auth_header_value(&self) -> String {
        match self {
            Self::OAuth { access_token, .. } => format!("Bearer {}", access_token),
            Self::ApiKey { key, .. } => format!("Bearer {}", key),
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            Self::OAuth { provider, .. } | Self::ApiKey { provider, .. } => *provider,
        }
    }

    pub fn is_chatgpt_subscription(&self) -> bool {
        matches!(
            self,
            Self::OAuth {
                provider: Provider::OpenAi,
                ..
            }
        )
    }

    pub fn is_anthropic_oauth(&self) -> bool {
        matches!(
            self,
            Self::OAuth {
                provider: Provider::Anthropic,
                ..
            }
        )
    }
}

pub async fn resolve_credential(provider: Provider) -> Result<Credential> {
    let env_var = match provider {
        Provider::OpenAi => "OPENAI_API_KEY",
        Provider::Anthropic => "ANTHROPIC_API_KEY",
    };
    if let Ok(key) = std::env::var(env_var) {
        if !key.is_empty() {
            return Ok(Credential::ApiKey { provider, key });
        }
    }
    let record = load_auth()?;
    match provider {
        Provider::OpenAi => resolve_openai(&record).await,
        Provider::Anthropic => resolve_anthropic(&record).await,
    }
}

async fn resolve_openai(record: &AuthRecord) -> Result<Credential> {
    if matches!(record.mode, AuthMode::OpenaiApiKey) {
        if let Some(key) = record.api_key.clone().filter(|k| !k.is_empty()) {
            return Ok(Credential::ApiKey {
                provider: Provider::OpenAi,
                key,
            });
        }
    }
    if matches!(record.mode, AuthMode::OpenaiOauth) {
        if let Some(tokens) = record
            .tokens
            .as_ref()
            .filter(|t| !t.access_token.is_empty())
        {
            let access = oauth::ensure_fresh(record).await?;
            return Ok(Credential::OAuth {
                provider: Provider::OpenAi,
                access_token: access,
                account_id: tokens.account_id.clone(),
            });
        }
    }
    if let Some(tokens) = record.tokens.as_ref() {
        if !tokens.access_token.is_empty() {
            let access = oauth::ensure_fresh(record).await?;
            return Ok(Credential::OAuth {
                provider: Provider::OpenAi,
                access_token: access,
                account_id: tokens.account_id.clone(),
            });
        }
    }
    if let Some(key) = record.api_key.clone() {
        if !key.is_empty() {
            return Ok(Credential::ApiKey {
                provider: Provider::OpenAi,
                key,
            });
        }
    }
    Err(anyhow::anyhow!(
        "Not signed in for OpenAI. Run `opencli login` or set OPENAI_API_KEY."
    ))
}

async fn resolve_anthropic(record: &AuthRecord) -> Result<Credential> {
    if matches!(record.mode, AuthMode::AnthropicApiKey) {
        if let Some(key) = record.anthropic_api_key.clone().filter(|k| !k.is_empty()) {
            return Ok(Credential::ApiKey {
                provider: Provider::Anthropic,
                key,
            });
        }
    }
    if matches!(record.mode, AuthMode::AnthropicOauth)
        && record
            .anthropic_tokens
            .as_ref()
            .is_some_and(|t| !t.access_token.is_empty())
    {
        let access = anthropic::ensure_fresh(record).await?;
        return Ok(Credential::OAuth {
            provider: Provider::Anthropic,
            access_token: access,
            account_id: None,
        });
    }
    if let Some(tokens) = record.anthropic_tokens.as_ref() {
        if !tokens.access_token.is_empty() {
            let access = anthropic::ensure_fresh(record).await?;
            return Ok(Credential::OAuth {
                provider: Provider::Anthropic,
                access_token: access,
                account_id: None,
            });
        }
    }
    if let Some(key) = record.anthropic_api_key.clone() {
        if !key.is_empty() {
            return Ok(Credential::ApiKey {
                provider: Provider::Anthropic,
                key,
            });
        }
    }
    Err(anyhow::anyhow!(
        "Not signed in for Anthropic. Run `opencli login --provider anthropic` or set ANTHROPIC_API_KEY."
    ))
}

/// Inspect the on-disk auth record (plus env vars) and report which providers
/// the user is currently authenticated against. Used by the CLI's `status`
/// and `login` commands to decide which model catalogues to surface.
///
/// Returns an empty vec when no credential is configured for any provider —
/// in that state the UI must hide the model picker until the user signs in.
pub fn signed_in_providers() -> Vec<Provider> {
    let mut out = Vec::new();
    let record = load_auth().unwrap_or_default();
    let openai_env = std::env::var("OPENAI_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let anthropic_env = std::env::var("ANTHROPIC_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let openai_stored = has_openai_oauth(&record) || has_openai_api_key(&record);
    let anthropic_stored = has_anthropic_oauth(&record) || has_anthropic_api_key(&record);
    if openai_env || openai_stored {
        out.push(Provider::OpenAi);
    }
    if anthropic_env || anthropic_stored {
        out.push(Provider::Anthropic);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::storage::StoredTokens;
    use chrono::Utc;

    fn fresh_tokens() -> StoredTokens {
        StoredTokens {
            access_token: "oauth-access".to_string(),
            refresh_token: "oauth-refresh".to_string(),
            id_token: None,
            account_id: Some("acct".to_string()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        }
    }

    #[tokio::test]
    async fn openai_resolution_respects_active_api_key_mode() {
        let record = AuthRecord {
            mode: AuthMode::OpenaiApiKey,
            tokens: Some(fresh_tokens()),
            api_key: Some("api-key".to_string()),
            ..Default::default()
        };
        let credential = resolve_openai(&record).await.unwrap();
        match credential {
            Credential::ApiKey { provider, key } => {
                assert_eq!(provider, Provider::OpenAi);
                assert_eq!(key, "api-key");
            }
            other => panic!("expected api key credential, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn anthropic_resolution_respects_active_api_key_mode() {
        let record = AuthRecord {
            mode: AuthMode::AnthropicApiKey,
            anthropic_tokens: Some(fresh_tokens()),
            anthropic_api_key: Some("anthropic-key".to_string()),
            ..Default::default()
        };
        let credential = resolve_anthropic(&record).await.unwrap();
        match credential {
            Credential::ApiKey { provider, key } => {
                assert_eq!(provider, Provider::Anthropic);
                assert_eq!(key, "anthropic-key");
            }
            other => panic!("expected api key credential, got {other:?}"),
        }
    }

    #[test]
    fn effective_mode_with_env_values_reports_env_credentials() {
        let record = AuthRecord::default();
        assert_eq!(
            effective_mode_with_env_values(&record, "sk-env".to_string(), String::new()),
            AuthMode::OpenaiApiKey
        );
        assert_eq!(
            effective_mode_with_env_values(&record, String::new(), "sk-ant-env".to_string()),
            AuthMode::AnthropicApiKey
        );
    }
}

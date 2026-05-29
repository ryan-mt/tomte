pub mod anthropic;
pub mod oauth;
pub mod pkce;
pub mod storage;

pub use oauth::{
    login_with_browser, refresh_access_token, start_browser_login, OauthClient, PendingLogin,
    TokenSet,
};
pub use storage::{clear_credential, load_auth, save_auth, AuthMode, AuthRecord, LogoutTarget};

use anyhow::Result;

use crate::provider::Provider;

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
    let openai_stored = record
        .tokens
        .as_ref()
        .map(|t| !t.access_token.is_empty())
        .unwrap_or(false)
        || record
            .api_key
            .as_ref()
            .map(|k| !k.is_empty())
            .unwrap_or(false);
    let anthropic_stored = record
        .anthropic_tokens
        .as_ref()
        .map(|t| !t.access_token.is_empty())
        .unwrap_or(false)
        || record
            .anthropic_api_key
            .as_ref()
            .map(|k| !k.is_empty())
            .unwrap_or(false);
    if openai_env || openai_stored {
        out.push(Provider::OpenAi);
    }
    if anthropic_env || anthropic_stored {
        out.push(Provider::Anthropic);
    }
    out
}

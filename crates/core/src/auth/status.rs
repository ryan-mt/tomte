//! Read-only views over the stored credentials (plus env vars) for status,
//! login, and doctor surfaces. Reports presence/source only — never secrets.

use super::storage::{
    has_anthropic_api_key, has_anthropic_oauth, has_openai_api_key, has_openai_oauth, load_auth,
    AuthMode, AuthRecord,
};
use crate::provider::Provider;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedInModelCatalog {
    pub provider: Provider,
    pub models: &'static [&'static str],
}

/// Model catalogues the current credentials can actually use. OpenAI API keys
/// can use the full public OpenAI catalogue, but ChatGPT/Codex OAuth accepts a
/// narrower model set; surfacing API-key-only models for OAuth leads to a 400
/// before the first token streams.
pub fn signed_in_model_catalogs() -> Vec<SignedInModelCatalog> {
    let record = load_auth().unwrap_or_default();
    let openai_env = std::env::var("OPENAI_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let anthropic_env = std::env::var("ANTHROPIC_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    signed_in_model_catalogs_with_env(&record, openai_env, anthropic_env)
}

fn signed_in_model_catalogs_with_env(
    record: &AuthRecord,
    has_openai_env: bool,
    has_anthropic_env: bool,
) -> Vec<SignedInModelCatalog> {
    let mut out = Vec::new();
    let has_stored_openai_api_key = has_openai_api_key(record);
    let has_stored_openai_oauth = has_openai_oauth(record);
    let openai_uses_api_key = has_openai_env
        || (matches!(record.mode, AuthMode::OpenaiApiKey) && has_stored_openai_api_key)
        || (has_stored_openai_api_key && !has_stored_openai_oauth);
    if openai_uses_api_key {
        out.push(SignedInModelCatalog {
            provider: Provider::OpenAi,
            models: crate::catalog::available_models(Provider::OpenAi),
        });
    } else if has_stored_openai_oauth {
        out.push(SignedInModelCatalog {
            provider: Provider::OpenAi,
            models: crate::catalog::openai_chatgpt_oauth_models(),
        });
    }

    if has_anthropic_env || has_anthropic_oauth(record) || has_anthropic_api_key(record) {
        out.push(SignedInModelCatalog {
            provider: Provider::Anthropic,
            models: crate::catalog::available_models(Provider::Anthropic),
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPresence {
    Stored,
    Env,
    Missing,
}

impl CredentialPresence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::Env => "env",
            Self::Missing => "not configured",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialCoverage {
    pub openai_oauth: CredentialPresence,
    pub openai_api_key: CredentialPresence,
    pub anthropic_oauth: CredentialPresence,
    pub anthropic_api_key: CredentialPresence,
}

/// Safe credential matrix for status surfaces. This deliberately reports only
/// presence/source, never token/key contents.
pub fn credential_coverage() -> CredentialCoverage {
    let record = load_auth().unwrap_or_default();
    let openai_env = std::env::var("OPENAI_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let anthropic_env = std::env::var("ANTHROPIC_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    credential_coverage_with_env(&record, openai_env, anthropic_env)
}

fn credential_coverage_with_env(
    record: &AuthRecord,
    has_openai_env: bool,
    has_anthropic_env: bool,
) -> CredentialCoverage {
    CredentialCoverage {
        openai_oauth: if has_openai_oauth(record) {
            CredentialPresence::Stored
        } else {
            CredentialPresence::Missing
        },
        openai_api_key: if has_openai_env {
            CredentialPresence::Env
        } else if has_openai_api_key(record) {
            CredentialPresence::Stored
        } else {
            CredentialPresence::Missing
        },
        anthropic_oauth: if has_anthropic_oauth(record) {
            CredentialPresence::Stored
        } else {
            CredentialPresence::Missing
        },
        anthropic_api_key: if has_anthropic_env {
            CredentialPresence::Env
        } else if has_anthropic_api_key(record) {
            CredentialPresence::Stored
        } else {
            CredentialPresence::Missing
        },
    }
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

    #[test]
    fn model_catalogs_limit_openai_oauth_to_codex_backend_models() {
        let record = AuthRecord {
            tokens: Some(fresh_tokens()),
            ..Default::default()
        };

        let catalogs = signed_in_model_catalogs_with_env(&record, false, false);
        let openai = catalogs
            .iter()
            .find(|catalog| catalog.provider == Provider::OpenAi)
            .expect("openai catalog");

        assert_eq!(openai.models, crate::catalog::openai_chatgpt_oauth_models());
        assert!(!openai.models.contains(&"gpt-5.4-mini"));
    }

    #[test]
    fn model_catalogs_keep_full_openai_list_when_api_key_exists() {
        let record = AuthRecord {
            mode: AuthMode::OpenaiApiKey,
            tokens: Some(fresh_tokens()),
            api_key: Some("sk-test".into()),
            ..Default::default()
        };

        let catalogs = signed_in_model_catalogs_with_env(&record, false, false);
        let openai = catalogs
            .iter()
            .find(|catalog| catalog.provider == Provider::OpenAi)
            .expect("openai catalog");

        assert_eq!(
            openai.models,
            crate::catalog::available_models(Provider::OpenAi)
        );
        assert!(openai.models.contains(&"gpt-5.4-mini"));
    }

    #[test]
    fn model_catalogs_match_openai_oauth_when_api_key_is_not_active() {
        let record = AuthRecord {
            mode: AuthMode::AnthropicOauth,
            tokens: Some(fresh_tokens()),
            api_key: Some("sk-test".into()),
            ..Default::default()
        };

        let catalogs = signed_in_model_catalogs_with_env(&record, false, false);
        let openai = catalogs
            .iter()
            .find(|catalog| catalog.provider == Provider::OpenAi)
            .expect("openai catalog");

        assert_eq!(openai.models, crate::catalog::openai_chatgpt_oauth_models());
        assert!(!openai.models.contains(&"gpt-5.4-mini"));
    }

    #[test]
    fn model_catalogs_env_openai_api_key_overrides_oauth_catalog() {
        let record = AuthRecord {
            mode: AuthMode::OpenaiOauth,
            tokens: Some(fresh_tokens()),
            ..Default::default()
        };

        let catalogs = signed_in_model_catalogs_with_env(&record, true, false);
        let openai = catalogs
            .iter()
            .find(|catalog| catalog.provider == Provider::OpenAi)
            .expect("openai catalog");

        assert_eq!(
            openai.models,
            crate::catalog::available_models(Provider::OpenAi)
        );
        assert!(openai.models.contains(&"gpt-5.4-mini"));
    }

    #[test]
    fn credential_coverage_reports_presence_without_secret_values() {
        let record = AuthRecord {
            tokens: Some(fresh_tokens()),
            anthropic_api_key: Some("sk-ant-secret".into()),
            ..Default::default()
        };

        let coverage = credential_coverage_with_env(&record, true, false);

        assert_eq!(coverage.openai_oauth, CredentialPresence::Stored);
        assert_eq!(coverage.openai_api_key, CredentialPresence::Env);
        assert_eq!(coverage.anthropic_oauth, CredentialPresence::Missing);
        assert_eq!(coverage.anthropic_api_key, CredentialPresence::Stored);
        assert_eq!(coverage.openai_api_key.label(), "env");
        assert_eq!(coverage.anthropic_oauth.label(), "not configured");
    }
}

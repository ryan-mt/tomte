pub mod oauth;
pub mod pkce;
pub mod storage;

pub use oauth::{
    login_with_browser, refresh_access_token, start_browser_login, OauthClient, PendingLogin,
    TokenSet,
};
pub use storage::{load_auth, save_auth, AuthRecord, AuthMode};

use anyhow::Result;

/// Auth resolution: prefer OAuth tokens, fall back to API key (env or stored).
#[derive(Debug, Clone)]
pub enum Credential {
    OAuth { access_token: String, account_id: Option<String> },
    ApiKey(String),
}

impl Credential {
    pub fn auth_header_value(&self) -> String {
        match self {
            Self::OAuth { access_token, .. } => format!("Bearer {}", access_token),
            Self::ApiKey(k) => format!("Bearer {}", k),
        }
    }

    pub fn is_chatgpt_subscription(&self) -> bool {
        matches!(self, Self::OAuth { .. })
    }
}

pub async fn resolve_credential() -> Result<Credential> {
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Ok(Credential::ApiKey(key));
        }
    }
    let record = load_auth()?;
    match record.mode {
        AuthMode::ChatGPT => {
            let tokens = record
                .tokens
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("ChatGPT sign-in is incomplete. Run `opencli login`."))?;
            let access = oauth::ensure_fresh(&record).await?;
            Ok(Credential::OAuth {
                access_token: access,
                account_id: tokens.account_id.clone(),
            })
        }
        AuthMode::ApiKey => record
            .api_key
            .map(Credential::ApiKey)
            .ok_or_else(|| anyhow::anyhow!("No API key configured. Run `opencli login --api-key`.")),
        AuthMode::None => Err(anyhow::anyhow!(
            "Not signed in. Run `opencli login` or set OPENAI_API_KEY."
        )),
    }
}

use serde::{Deserialize, Serialize};

/// Which upstream LLM provider a credential or model belongs to.
///
/// The active provider for a turn is derived from the configured model name
/// (see [`Provider::from_model`]). Each provider has its own API endpoints,
/// auth scheme, and on-the-wire request/response shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    OpenAi,
    Anthropic,
}

impl Provider {
    /// Best-effort detection from a model id. Anything that begins with
    /// `claude` routes to Anthropic; everything else falls back to OpenAI
    /// so unknown ids stay compatible with the existing OpenAI path.
    pub fn from_model(model: &str) -> Self {
        if model.trim().to_ascii_lowercase().starts_with("claude") {
            Self::Anthropic
        } else {
            Self::OpenAi
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_anthropic_from_claude_prefix() {
        assert_eq!(Provider::from_model("claude-opus-4-5"), Provider::Anthropic);
        assert_eq!(Provider::from_model("claude-sonnet-4-5"), Provider::Anthropic);
        assert_eq!(Provider::from_model("Claude-Haiku-4-5"), Provider::Anthropic);
    }

    #[test]
    fn detects_openai_otherwise() {
        assert_eq!(Provider::from_model("gpt-5"), Provider::OpenAi);
        assert_eq!(Provider::from_model("gpt-5-codex"), Provider::OpenAi);
        assert_eq!(Provider::from_model("o3"), Provider::OpenAi);
        assert_eq!(Provider::from_model(""), Provider::OpenAi);
    }
}

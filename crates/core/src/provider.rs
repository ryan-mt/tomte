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

    /// Parse a provider id — the left side of a `provider/model` spec.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }

    /// Resolve a model spec into its provider and the bare model id used on the
    /// wire. Accepts an explicit `provider/model` form (e.g.
    /// `anthropic/claude-opus-4-8`): when the prefix names a known provider it
    /// is authoritative and stripped from the id. A bare id — no slash, or an
    /// unrecognised prefix — falls back to [`Provider::from_model`] heuristics
    /// and is returned unchanged. Lets the configured `model` be either form
    /// without breaking the existing bare-id configs.
    pub fn parse_model(spec: &str) -> (Self, String) {
        if let Some((prefix, rest)) = spec.split_once('/') {
            if let Some(provider) = Self::from_name(prefix) {
                if !rest.is_empty() {
                    return (provider, rest.to_string());
                }
            }
        }
        (Self::from_model(spec), spec.to_string())
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

    /// Catalogue of model ids known to be available for this provider, in
    /// the recommended display order (best general default first). The CLI
    /// surfaces this list after sign-in so the user knows what to pick. Backed
    /// by the model catalogue (the single source of truth); see
    /// [`crate::catalog`].
    pub fn available_models(&self) -> &'static [&'static str] {
        crate::catalog::available_models(*self)
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
        assert_eq!(
            Provider::from_model("claude-sonnet-4-5"),
            Provider::Anthropic
        );
        assert_eq!(
            Provider::from_model("Claude-Haiku-4-5"),
            Provider::Anthropic
        );
    }

    #[test]
    fn detects_openai_otherwise() {
        assert_eq!(Provider::from_model("gpt-5"), Provider::OpenAi);
        assert_eq!(Provider::from_model("o3"), Provider::OpenAi);
        assert_eq!(Provider::from_model(""), Provider::OpenAi);
    }

    #[test]
    fn parse_model_honours_explicit_provider_prefix() {
        assert_eq!(
            Provider::parse_model("anthropic/claude-opus-4-8"),
            (Provider::Anthropic, "claude-opus-4-8".to_string())
        );
        assert_eq!(
            Provider::parse_model("openai/gpt-5.5"),
            (Provider::OpenAi, "gpt-5.5".to_string())
        );
    }

    #[test]
    fn parse_model_falls_back_for_bare_and_unknown_prefixes() {
        // Bare id keeps working via the heuristic, returned unchanged.
        assert_eq!(
            Provider::parse_model("claude-opus-4-8"),
            (Provider::Anthropic, "claude-opus-4-8".to_string())
        );
        assert_eq!(
            Provider::parse_model("gpt-5.5"),
            (Provider::OpenAi, "gpt-5.5".to_string())
        );
        // Unknown prefix (e.g. a path-like id) is not treated as a provider.
        assert_eq!(
            Provider::parse_model("lmstudio/google/gemma"),
            (Provider::OpenAi, "lmstudio/google/gemma".to_string())
        );
    }

    #[test]
    fn available_models_are_non_empty() {
        assert!(!Provider::OpenAi.available_models().is_empty());
        assert!(!Provider::Anthropic.available_models().is_empty());
    }

    #[test]
    fn anthropic_models_start_with_claude() {
        for m in Provider::Anthropic.available_models() {
            assert!(m.starts_with("claude"), "{m} should start with claude");
        }
    }
}

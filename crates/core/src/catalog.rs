//! Data-driven model catalogue: the single source of truth for per-model facts
//! (which provider owns a model, its context-window size, whether it is eligible
//! for the Anthropic 1M-context beta, and whether it supports adaptive extended
//! thinking).
//!
//! Before this module these facts were duplicated across call sites — the 1M set
//! lived in both `agent::model_supports_1m` and `translate.rs`, the per-provider
//! model lists in `provider.rs`, and the context-window rules in `agent`. They
//! now live here once.
//!
//! Capability queries accept *any* model id. A known id is read straight from
//! [`catalog`]; an unknown id — a dated snapshot like `claude-opus-4-8-20260101`
//! or a variant like `gpt-5.5-codex` — falls back to family matching on the id
//! substring, preserving the behaviour the per-call-site checks had before.

use once_cell::sync::Lazy;

use crate::provider::Provider;

/// Static facts about a known model.
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub id: &'static str,
    pub provider: Provider,
    /// Context-window size in tokens (drives the local warn/auto-compact
    /// threshold).
    pub context_limit: u64,
    /// Eligible for Anthropic's `context-1m-2025-08-07` beta header. Always
    /// `false` for OpenAI models — their window size is carried by
    /// `context_limit`; this flag only gates the Anthropic beta.
    pub supports_1m: bool,
    /// Supports Anthropic adaptive extended thinking (`{"type":"adaptive"}`).
    /// Always `false` for OpenAI models, whose reasoning is configured via the
    /// Responses `reasoning.effort` field instead.
    pub supports_adaptive_thinking: bool,
}

const fn openai(id: &'static str, context_limit: u64) -> ModelInfo {
    ModelInfo {
        id,
        provider: Provider::OpenAi,
        context_limit,
        supports_1m: false,
        supports_adaptive_thinking: false,
    }
}

const fn anthropic(
    id: &'static str,
    context_limit: u64,
    supports_1m: bool,
    supports_adaptive_thinking: bool,
) -> ModelInfo {
    ModelInfo {
        id,
        provider: Provider::Anthropic,
        context_limit,
        supports_1m,
        supports_adaptive_thinking,
    }
}

/// Every model opencli surfaces after sign-in, in display order (best general
/// default first) within each provider. Facts verified against the published
/// model docs (May 2026); ids not listed here fall back to the `family_*` rules.
const MODELS: &[ModelInfo] = &[
    // ---- OpenAI ----
    openai("gpt-5.5", 1_050_000),
    openai("gpt-5.4", 1_000_000),
    openai("gpt-5.3", 400_000),
    openai("gpt-5-pro", 400_000),
    openai("gpt-5-mini", 400_000),
    openai("gpt-5-nano", 200_000),
    // ---- Anthropic ----
    anthropic("claude-opus-4-8", 1_000_000, true, true),
    anthropic("claude-opus-4-7", 1_000_000, true, true),
    anthropic("claude-opus-4-6", 1_000_000, true, true),
    anthropic("claude-opus-4-5", 200_000, false, false),
    anthropic("claude-sonnet-4-6", 1_000_000, true, true),
    anthropic("claude-sonnet-4-5", 200_000, false, false),
    anthropic("claude-haiku-4-5", 200_000, false, false),
];

/// The full catalogue, in declaration order.
pub fn catalog() -> &'static [ModelInfo] {
    MODELS
}

/// Exact lookup of a known model id. Returns `None` for variants/snapshots —
/// callers that need a value for those should use the capability functions,
/// which fall back to family matching.
pub fn lookup(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

static OPENAI_IDS: Lazy<Vec<&'static str>> = Lazy::new(|| ids_for(Provider::OpenAi));
static ANTHROPIC_IDS: Lazy<Vec<&'static str>> = Lazy::new(|| ids_for(Provider::Anthropic));
static OPENAI_CHATGPT_OAUTH_IDS: Lazy<Vec<&'static str>> = Lazy::new(|| {
    MODELS
        .iter()
        .filter(|m| m.provider == Provider::OpenAi && matches!(m.id, "gpt-5.5" | "gpt-5.4"))
        .map(|m| m.id)
        .collect()
});

fn ids_for(provider: Provider) -> Vec<&'static str> {
    MODELS
        .iter()
        .filter(|m| m.provider == provider)
        .map(|m| m.id)
        .collect()
}

/// Catalogue of model ids for a provider, in display order. Backs
/// [`Provider::available_models`].
pub fn available_models(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::OpenAi => OPENAI_IDS.as_slice(),
        Provider::Anthropic => ANTHROPIC_IDS.as_slice(),
    }
}

/// OpenAI models accepted by the ChatGPT/Codex OAuth backend. The public API-key
/// catalogue is broader (mini/nano/pro), but the subscription backend rejects
/// those ids with a request-level 400. Keep the OAuth picker/status list to ids
/// verified against that backend so users don't select a dead model.
pub fn openai_chatgpt_oauth_models() -> &'static [&'static str] {
    OPENAI_CHATGPT_OAUTH_IDS.as_slice()
}

/// Context-window size (tokens) for any model id. Known ids read the catalogue;
/// unknown ids fall back to family matching.
pub fn context_limit(model: &str) -> u64 {
    if let Some(m) = lookup(model) {
        return m.context_limit;
    }
    family_context_limit(model)
}

/// Whether a model is eligible for the Anthropic 1M-context beta header.
pub fn supports_1m(model: &str) -> bool {
    if let Some(m) = lookup(model) {
        return m.supports_1m;
    }
    family_supports_1m(model)
}

/// Whether a model supports Anthropic adaptive extended thinking.
pub fn supports_adaptive_thinking(model: &str) -> bool {
    if let Some(m) = lookup(model) {
        return m.supports_adaptive_thinking;
    }
    family_supports_adaptive_thinking(model)
}

/// Whether a model accepts the `xhigh` adaptive effort tier (the level between
/// `high` and `max`). Today only Opus 4.7+; other adaptive models clamp `xhigh`
/// down to `high`. Version-gated so future Opus ids (4.9, 5.x) inherit it
/// without a catalog edit; Sonnet and Haiku never get `xhigh`.
pub fn supports_xhigh(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if !m.contains("opus") {
        return false;
    }
    matches!(claude_version(&m), Some((major, minor)) if major > 4 || (major == 4 && minor >= 7))
}

/// Whether a model uses the legacy budget-based extended-thinking shape
/// (`thinking:{type:"enabled", budget_tokens:N}`) rather than adaptive. These
/// are the pre-adaptive thinking Claude models — Opus/Sonnet below 4.6, plus
/// Haiku (which supports extended but never adaptive). Adaptive-capable models
/// (4.6+) also accept the legacy shape, but `translate.rs` prefers adaptive for
/// them, so this only needs to identify the legacy-only set. Unknown future ids
/// (4.6+) are deliberately NOT matched: they route to adaptive, because Opus
/// 4.7+ reject `type:"enabled"` with a 400.
pub fn supports_extended_thinking(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if !m.starts_with("claude") {
        return false;
    }
    if m.contains("haiku") {
        return true;
    }
    matches!(claude_version(&m), Some((4, minor)) if minor <= 5)
}

// ---- Family fallbacks (single home for the id-substring rules) ----
//
// Per the Claude API docs (May 2026): Opus 4.6/4.7/4.8, Sonnet 4.6 and the
// Mythos preview ship a 1M window; Sonnet 4.5/4, Opus 4.5 and Haiku are 200K.
// For OpenAI: gpt-5.5 → 1.05M, gpt-5.4 → 1M, mini → 400K, nano → 200K; every
// other gpt-5* → 400K.

fn family_supports_1m(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("opus-4-8")
        || m.contains("opus-4-7")
        || m.contains("opus-4-6")
        || m.contains("sonnet-4-6")
        || m.contains("mythos")
}

fn family_context_limit(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") || m.contains("mythos") {
        return if family_supports_1m(&m) {
            1_000_000
        } else {
            200_000
        };
    }
    if m.contains("nano") {
        return 200_000;
    }
    if m.contains("mini") {
        return 400_000;
    }
    if m.contains("gpt-5.5") {
        return 1_050_000;
    }
    if m.contains("gpt-5.4") {
        return 1_000_000;
    }
    400_000
}

fn family_supports_adaptive_thinking(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if !(m.starts_with("claude") || m.contains("mythos")) {
        return false;
    }
    // The Mythos preview is adaptive; Haiku never is.
    if m.contains("mythos") {
        return true;
    }
    if m.contains("haiku") {
        return false;
    }
    // Adaptive thinking landed in Claude 4.6. Version-gated so future ids
    // (4.9, 5.x) stay adaptive — the forward-compatible shape — instead of
    // falling through to the deprecated `type:"enabled"` form that 4.7+ reject.
    matches!(claude_version(&m), Some((major, minor)) if major > 4 || (major == 4 && minor >= 6))
}

/// Parse the `(major, minor)` version from a `claude-<tier>-<major>-<minor>` id
/// — the shape every Claude model opencli surfaces uses (a trailing date
/// snapshot is ignored). Returns `None` for ids that don't fit, so callers fall
/// back to a safe default rather than guessing.
fn claude_version(model_lc: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = model_lc.split('-').collect();
    let tier = parts
        .iter()
        .position(|p| matches!(*p, "opus" | "sonnet" | "haiku"))?;
    let major = parts.get(tier + 1)?.parse().ok()?;
    let minor = parts.get(tier + 2)?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every catalogued model's stored facts must equal what the family
    /// fallbacks would compute, so the two paths can never silently diverge.
    #[test]
    fn table_agrees_with_family_fallbacks() {
        for m in catalog() {
            assert_eq!(
                m.context_limit,
                family_context_limit(m.id),
                "{} context_limit",
                m.id
            );
            assert_eq!(
                m.supports_1m,
                family_supports_1m(m.id),
                "{} supports_1m",
                m.id
            );
            assert_eq!(
                m.supports_adaptive_thinking,
                family_supports_adaptive_thinking(m.id),
                "{} adaptive",
                m.id
            );
        }
    }

    #[test]
    fn available_models_are_grouped_by_provider() {
        let openai = available_models(Provider::OpenAi);
        let anthropic = available_models(Provider::Anthropic);
        assert!(!openai.is_empty());
        assert!(!anthropic.is_empty());
        assert!(openai.iter().all(|id| !id.starts_with("claude")));
        assert!(anthropic.iter().all(|id| id.starts_with("claude")));
        assert_eq!(openai[0], "gpt-5.5");
        assert_eq!(anthropic[0], "claude-opus-4-8");
    }

    #[test]
    fn chatgpt_oauth_models_exclude_api_key_only_openai_models() {
        let models = openai_chatgpt_oauth_models();
        assert_eq!(models, ["gpt-5.5", "gpt-5.4"]);
        for unsupported in ["gpt-5-pro", "gpt-5-mini", "gpt-5-nano", "gpt-5.3"] {
            assert!(
                !models.contains(&unsupported),
                "{unsupported} should not be suggested for ChatGPT OAuth"
            );
        }
    }

    #[test]
    fn unknown_variants_fall_back_to_family_matching() {
        // Dated snapshot of a known 1M Anthropic model.
        assert_eq!(context_limit("claude-opus-4-8-20260101"), 1_000_000);
        assert!(supports_1m("claude-opus-4-8-20260101"));
        assert!(supports_adaptive_thinking("claude-opus-4-8-20260101"));
        // OpenAI variant keeps its window but is never 1M-beta/adaptive.
        assert_eq!(context_limit("gpt-5.5-codex"), 1_050_000);
        assert!(!supports_1m("gpt-5.5-codex"));
        assert!(!supports_adaptive_thinking("gpt-5.5-codex"));
        // Mythos preview is not in the display catalogue but is a known family.
        assert_eq!(context_limit("claude-mythos-preview"), 1_000_000);
        assert!(supports_1m("claude-mythos-preview"));
    }

    #[test]
    fn lookup_finds_known_and_misses_unknown() {
        assert!(lookup("claude-opus-4-8").is_some());
        assert!(lookup("claude-opus-4-8-20260101").is_none());
    }

    #[test]
    fn future_claude_versions_prefer_adaptive_over_legacy() {
        // Un-cataloged future ids must route to adaptive, never the deprecated
        // legacy shape (Opus 4.7+ reject `type:"enabled"` with a 400).
        for id in ["claude-opus-4-9", "claude-opus-5-0", "claude-sonnet-5-0"] {
            assert!(supports_adaptive_thinking(id), "{id} should be adaptive");
            assert!(!supports_extended_thinking(id), "{id} should not be legacy");
        }
        // Pre-adaptive thinking models still use the budget shape.
        assert!(supports_extended_thinking("claude-opus-4-5"));
        assert!(supports_extended_thinking("claude-sonnet-4-5"));
        assert!(supports_extended_thinking("claude-haiku-4-5"));
        assert!(!supports_adaptive_thinking("claude-haiku-4-5"));
    }

    #[test]
    fn xhigh_is_opus_4_7_plus_only() {
        for id in [
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-4-9", // future
            "claude-opus-5-0", // future
        ] {
            assert!(supports_xhigh(id), "{id} should support xhigh");
        }
        for id in ["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"] {
            assert!(!supports_xhigh(id), "{id} should clamp xhigh to high");
        }
    }
}

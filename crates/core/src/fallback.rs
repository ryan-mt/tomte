//! Provider-agnostic failover primitives.
//!
//! These are the pure building blocks for falling over to another model when
//! the active one is rate-limited or its provider is overloaded. They are
//! deliberately model- and provider-agnostic: nothing here knows about a
//! specific model id — a fallback entry is opaque, and the only thing we
//! classify is the *shape* of a provider error string.
//!
//! These are consumed by the turn loop's `try_fail_over` (see `agent/turn.rs`),
//! which wraps them in the two guards that make reactive failover safe: it
//! builds each candidate via [`crate::client::LlmClient::for_config`] and skips
//! one with no usable credential (so falling over never turns a clear rate-limit
//! into a confusing auth error), and it classifies fatal/overflow errors
//! *before* overload — via [`is_quota_or_overload`] plus
//! [`crate::agent::is_context_overflow_message`] — so a 400/401/refusal or a
//! context overflow is never mistaken for "try a different model". It runs on
//! the pre-stream send error and on a mid-stream `Failed`/`Error` event, but
//! only until answer text has started streaming — past that point the error
//! surfaces instead, so a fail-over can never replay output already shown.

use crate::config::Config;

/// Whether a provider error string indicates a transient rate-limit / quota /
/// overload condition — the only class that warrants failing over to a
/// different model. Matched on the lowercased error text because the native
/// clients surface these as `anyhow` errors carrying the provider's message,
/// not typed variants.
///
/// Kept deliberately tight: only phrasings that unambiguously mean "this
/// provider is over capacity right now", so a fatal `400`/`401`/`404` (bad
/// request, auth, model-not-found) or a content/refusal error is not misread as
/// overload. Callers that act on this must still check fatal/overflow classes
/// first; this predicate only answers "is it overload-shaped?".
pub fn is_quota_or_overload(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "429",
        "rate limit",
        "rate_limit",
        "too many requests",
        "overloaded",
        "insufficient_quota",
        "quota exceeded",
        "exceeded your current quota",
        "capacity",
        "service unavailable",
        "503",
    ];
    NEEDLES.iter().any(|needle| e.contains(needle))
}

/// Built-in same-provider ladders for [`default_fallbacks`], ordered by
/// descending capability tier: `(tier substring, fallback id)`. Conservative by
/// design on two axes:
/// - every id is accepted by every auth mode of its provider (the ChatGPT
///   subscription OAuth backend rejects mini/nano/pro ids with a 400, so the
///   OpenAI ladder stays on the two ids it takes);
/// - the Anthropic ladder stops at Sonnet — auto-dropping a coding session to
///   Haiku would trade a visible overload error for silently weaker edits.
const ANTHROPIC_LADDER: &[(&str, &str)] = &[
    ("fable", "claude-fable-5"),
    ("opus", "claude-opus-4-8"),
    ("sonnet", "claude-sonnet-4-6"),
];
const OPENAI_LADDER: &[(&str, &str)] = &[("gpt-5.5", "gpt-5.5"), ("gpt-5.4", "gpt-5.4")];

/// The built-in failover chain for `model` when the user configured none:
/// same-provider entries at the same or LOWER capability tier than the active
/// model, so reactive failover never moves a session onto a more expensive
/// model uninvited. Anchored on the model's tier (`fable`/`opus`/`sonnet`,
/// `gpt-5.5`/`gpt-5.4`); a model whose tier is not on the ladder — Haiku,
/// `gpt-5.2`, a local or third-party endpoint — gets NO default chain: tomte
/// never reroutes a model it can't place onto a provider the user didn't pick.
/// Entries that are a prefix of the active id are skipped, so a dated snapshot
/// (`claude-opus-4-8-20260101`) is not "rescued" by its own base id.
pub fn default_fallbacks(model: &str) -> Vec<String> {
    let (_, bare) = crate::provider::Provider::parse_model(model);
    let m = bare.trim().to_ascii_lowercase();
    let ladder: &[(&str, &str)] = if m.starts_with("claude") {
        ANTHROPIC_LADDER
    } else if m.starts_with("gpt-") {
        OPENAI_LADDER
    } else {
        return Vec::new();
    };
    // Anchor at the LOWEST tier the id mentions (rposition: the ladder is
    // ordered top tier first). An id carrying two tier words — say a
    // hypothetical `claude-sonnet-opus-distill` — must anchor at sonnet, not
    // opus: anchoring high would offer a pricier model than the one the user
    // picked, breaking the "sideways or down, never up" promise.
    let Some(tier) = ladder.iter().rposition(|(t, _)| m.contains(t)) else {
        return Vec::new();
    };
    ladder[tier..]
        .iter()
        .filter(|(_, id)| !m.starts_with(id))
        .map(|(_, id)| (*id).to_string())
        .collect()
}

/// The next fallback model to try, given the configured
/// [`Config::fallback_models`] and the models already attempted this turn.
/// A non-empty configured list is authoritative and its entries are treated as
/// opaque specs — no provider/model parsing happens here, so the chain works
/// across providers and local endpoints alike. With the list empty (the
/// default) and [`Config::auto_fallback`] on, the built-in same-provider
/// ladder from [`default_fallbacks`] is used instead. Returns the first entry
/// not present in `tried`, or `None` when the chain is exhausted (or
/// `auto_fallback` is off with nothing configured).
pub fn next_fallback(cfg: &Config, tried: &[String]) -> Option<String> {
    if !cfg.fallback_models.is_empty() {
        return cfg
            .fallback_models
            .iter()
            .find(|m| !tried.iter().any(|t| t == *m))
            .cloned();
    }
    if !cfg.auto_fallback {
        return None;
    }
    default_fallbacks(&cfg.model)
        .into_iter()
        .find(|m| !tried.iter().any(|t| t == m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_overload_and_quota_phrasings() {
        for s in [
            "HTTP 429 Too Many Requests",
            "anthropic error: overloaded_error: Overloaded",
            "OpenAI error: rate_limit_exceeded",
            "You exceeded your current quota (insufficient_quota)",
            "503 Service Unavailable",
            "the model is at capacity right now",
        ] {
            assert!(is_quota_or_overload(s), "should be overload: {s}");
        }
    }

    #[test]
    fn does_not_classify_fatal_errors_as_overload() {
        for s in [
            "400 Bad Request: invalid 'input[0].id'",
            "401 Unauthorized: invalid api key",
            "404 model not found: gpt-9",
            "stream ended before completion",
            "refusal: the model declined to answer",
        ] {
            assert!(!is_quota_or_overload(s), "should NOT be overload: {s}");
        }
    }

    #[test]
    fn next_fallback_skips_tried_and_exhausts_in_order() {
        let cfg = Config {
            fallback_models: vec!["claude-opus-4-8".into(), "groq/llama-3.3-70b".into()],
            ..Config::default()
        };
        assert_eq!(
            next_fallback(&cfg, &[]),
            Some("claude-opus-4-8".into()),
            "first untried entry, in order"
        );
        assert_eq!(
            next_fallback(&cfg, &["claude-opus-4-8".into()]),
            Some("groq/llama-3.3-70b".into()),
            "skips an already-tried entry"
        );
        assert_eq!(
            next_fallback(
                &cfg,
                &["claude-opus-4-8".into(), "groq/llama-3.3-70b".into()],
            ),
            None,
            "exhausted"
        );
    }

    // The built-in ladder only ever moves sideways or DOWN in capability/price:
    // failover must never silently upgrade the bill.
    #[test]
    fn default_ladder_never_upgrades() {
        assert_eq!(
            default_fallbacks("claude-fable-5"),
            vec!["claude-opus-4-8".to_string(), "claude-sonnet-4-6".into()]
        );
        assert_eq!(
            default_fallbacks("claude-opus-4-8"),
            vec!["claude-sonnet-4-6".to_string()]
        );
        // Sonnet has nothing below it on the ladder (Haiku is deliberately
        // excluded — silently weaker edits are worse than a visible error).
        assert!(default_fallbacks("claude-sonnet-4-6").is_empty());
        assert_eq!(default_fallbacks("gpt-5.5"), vec!["gpt-5.4".to_string()]);
        assert!(default_fallbacks("gpt-5.4").is_empty());
        // A mini variant must not be "upgraded" to its full-size base id.
        assert!(default_fallbacks("gpt-5.4-mini").is_empty());
    }

    #[test]
    fn default_ladder_handles_specs_snapshots_and_variants() {
        // A provider-prefixed spec is parsed, not treated as an unknown id.
        assert_eq!(
            default_fallbacks("anthropic/claude-fable-5"),
            vec!["claude-opus-4-8".to_string(), "claude-sonnet-4-6".into()]
        );
        // A dated snapshot skips its own base id rather than "failing over" to
        // the same overloaded model.
        assert_eq!(
            default_fallbacks("claude-opus-4-8-20260101"),
            vec!["claude-sonnet-4-6".to_string()]
        );
        // A same-tier sibling is allowed (separate id, same price tier).
        assert_eq!(
            default_fallbacks("claude-opus-4-7"),
            vec!["claude-opus-4-8".to_string(), "claude-sonnet-4-6".into()]
        );
        assert_eq!(
            default_fallbacks("gpt-5.5-codex"),
            vec!["gpt-5.4".to_string()]
        );
    }

    // An id mentioning TWO tier words must anchor at the lowest one — anchoring
    // high would offer a pricier model than the one the user picked.
    #[test]
    fn default_ladder_multi_tier_id_anchors_at_the_lowest_tier() {
        assert_eq!(
            default_fallbacks("claude-sonnet-opus-distill"),
            vec!["claude-sonnet-4-6".to_string()],
            "must anchor at sonnet, never walk up to opus"
        );
        assert_eq!(
            default_fallbacks("claude-fable-sonnet-blend"),
            vec!["claude-sonnet-4-6".to_string()]
        );
    }

    // Models tomte can't place on a ladder get NO default chain — never reroute
    // a local/third-party/unplaceable model to a provider the user didn't pick.
    #[test]
    fn default_ladder_unknown_or_unplaceable_is_empty() {
        for model in [
            "groq/llama-3.3-70b",
            "o3",
            "gpt-5.2",
            "gpt-5",
            "claude-haiku-4-5",
            "",
        ] {
            assert!(
                default_fallbacks(model).is_empty(),
                "{model} must get no default chain"
            );
        }
    }

    #[test]
    fn next_fallback_uses_ladder_when_unconfigured() {
        let cfg = Config {
            model: "claude-fable-5".into(),
            ..Config::default()
        };
        let started = vec!["claude-fable-5".to_string()];
        assert_eq!(
            next_fallback(&cfg, &started),
            Some("claude-opus-4-8".into()),
            "empty fallback_models + auto_fallback → the built-in ladder"
        );
        assert_eq!(
            next_fallback(&cfg, &["claude-fable-5".into(), "claude-opus-4-8".into()]),
            Some("claude-sonnet-4-6".into())
        );
        assert_eq!(
            next_fallback(
                &cfg,
                &[
                    "claude-fable-5".into(),
                    "claude-opus-4-8".into(),
                    "claude-sonnet-4-6".into(),
                ]
            ),
            None,
            "ladder exhausted"
        );

        // Opt-out restores the old fail-fast behavior.
        let off = Config {
            auto_fallback: false,
            ..cfg.clone()
        };
        assert_eq!(next_fallback(&off, &started), None);

        // A configured list is authoritative — the ladder never mixes in.
        let configured = Config {
            fallback_models: vec!["groq/llama-3.3-70b".into()],
            ..cfg
        };
        assert_eq!(
            next_fallback(&configured, &started),
            Some("groq/llama-3.3-70b".into())
        );
    }

    #[test]
    fn next_fallback_empty_list_without_auto_is_none() {
        let cfg = Config {
            model: "local/primary".into(),
            ..Config::default()
        };
        // Auto is on by default, but an unplaceable model still yields nothing.
        assert_eq!(next_fallback(&cfg, &[]), None);
    }
}

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

/// The next fallback model to try, given the configured
/// [`Config::fallback_models`] and the models already attempted this turn.
/// Returns the first configured fallback not present in `tried`, or `None` when
/// the list is empty or exhausted. Entries are treated as opaque specs — no
/// provider/model parsing happens here, so the chain works across providers and
/// local endpoints alike.
pub fn next_fallback(cfg: &Config, tried: &[String]) -> Option<String> {
    cfg.fallback_models
        .iter()
        .find(|m| !tried.iter().any(|t| t == *m))
        .cloned()
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

    #[test]
    fn next_fallback_empty_list_is_none() {
        let cfg = Config::default();
        assert_eq!(next_fallback(&cfg, &[]), None);
    }
}

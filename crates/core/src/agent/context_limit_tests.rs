//! Agent tests (context_limit_tests), split out of `agent`.

use super::{context_window_label, model_context_limit, model_supports_1m};

#[test]
fn context_window_labels_are_human_readable() {
    assert_eq!(context_window_label("claude-opus-4-8"), "1M");
    assert_eq!(context_window_label("claude-sonnet-4-5"), "200K");
    assert_eq!(context_window_label("gpt-5.5"), "1.05M");
    assert_eq!(context_window_label("gpt-5-mini"), "400K");
}

#[test]
fn model_supports_1m_matches_docs() {
    // 1M-window models (gates both the limit table and the context-1m beta).
    for m in [
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-mythos-preview",
    ] {
        assert!(model_supports_1m(m), "{m} should support 1M");
    }
    // 200K models must NOT trigger the 1M beta.
    for m in [
        "claude-opus-4-5",
        "claude-sonnet-4-5",
        "claude-sonnet-4",
        "claude-haiku-4-5",
        "gpt-5.5",
    ] {
        assert!(!model_supports_1m(m), "{m} should not support 1M");
    }
}

#[test]
fn anthropic_context_windows_match_docs() {
    // Opus 4.6+ and Sonnet 4.6 → 1M.
    assert_eq!(model_context_limit("claude-opus-4-8"), 1_000_000);
    assert_eq!(model_context_limit("claude-opus-4-7"), 1_000_000);
    assert_eq!(model_context_limit("claude-opus-4-6"), 1_000_000);
    assert_eq!(model_context_limit("claude-sonnet-4-6"), 1_000_000);
    assert_eq!(model_context_limit("claude-mythos-preview"), 1_000_000);
    // Opus 4.5, Sonnet 4.5 and Haiku → 200K.
    assert_eq!(model_context_limit("claude-opus-4-5"), 200_000);
    assert_eq!(model_context_limit("claude-sonnet-4-5"), 200_000);
    assert_eq!(model_context_limit("claude-haiku-4-5"), 200_000);
}

#[test]
fn openai_context_windows() {
    assert_eq!(model_context_limit("gpt-5.5"), 1_050_000);
    assert_eq!(model_context_limit("gpt-5.4"), 1_000_000);
    assert_eq!(model_context_limit("gpt-5.3"), 400_000);
    assert_eq!(model_context_limit("gpt-5-mini"), 400_000);
    assert_eq!(model_context_limit("gpt-5-nano"), 200_000);
}

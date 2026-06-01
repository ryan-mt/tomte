/// Normalize streamed tool-argument fragments before they reach provider-neutral
/// buffering. Some compatible providers emit an empty placeholder (`{}`, `[]`,
/// or `null`) immediately before the real JSON object. Treat those placeholders
/// as absent, and recover only when the suffix clearly starts another JSON
/// payload so malformed text is not silently rewritten.
pub(crate) fn normalize_argument_fragment(value: &str) -> Option<&str> {
    let value = strip_empty_argument_prefix(value);
    if is_empty_argument_fragment(value) {
        None
    } else {
        Some(value)
    }
}

/// Decide what a streamed tool-argument accumulator should append for one
/// fragment. While the buffer is still empty, a leading empty-args placeholder
/// (`{}`/`[]`/`null`, possibly prefixing the real object) is normalized away via
/// [`normalize_argument_fragment`]; once mid-object every fragment is kept
/// VERBATIM, so a bare `null`/`{}`/`[]` that is the real value of a field (e.g.
/// the streamed value of `"limit": null`) is not dropped and the accumulated
/// JSON stays valid. Returns `None` when the fragment should be skipped.
///
/// This is the single source of truth for the leading-placeholder rule, shared
/// by every streaming arg accumulator (OpenAI Responses, OpenAI-compatible Chat
/// Completions, and Anthropic) so they cannot drift apart — a past divergence
/// here silently corrupted Chat Completions tool calls.
pub(crate) fn accumulate_argument_fragment(buffer_is_empty: bool, fragment: &str) -> Option<&str> {
    let fragment = if buffer_is_empty {
        normalize_argument_fragment(fragment)?
    } else {
        fragment
    };
    if fragment.is_empty() {
        None
    } else {
        Some(fragment)
    }
}

pub(crate) fn is_empty_argument_fragment(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty() || trimmed == "{}" || trimmed == "[]" || trimmed.eq_ignore_ascii_case("null")
}

fn strip_empty_argument_prefix(value: &str) -> &str {
    let trimmed = value.trim_start();
    for prefix in ["{}", "[]"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let rest = rest.trim_start();
            if looks_like_json_payload(rest) {
                return rest;
            }
        }
    }
    if trimmed
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("null"))
    {
        let rest = trimmed[4..].trim_start();
        if looks_like_json_payload(rest) {
            return rest;
        }
    }
    value
}

fn looks_like_json_payload(value: &str) -> bool {
    value.starts_with('{') || value.starts_with('[')
}

#[cfg(test)]
mod tests {
    use super::{
        accumulate_argument_fragment, is_empty_argument_fragment, normalize_argument_fragment,
    };

    #[test]
    fn accumulate_drops_leading_placeholder_keeps_midstream_verbatim() {
        // Empty buffer: a leading placeholder is dropped, a payload after it recovered.
        assert_eq!(accumulate_argument_fragment(true, "{}"), None);
        assert_eq!(accumulate_argument_fragment(true, "null"), None);
        assert_eq!(accumulate_argument_fragment(true, ""), None);
        assert_eq!(
            accumulate_argument_fragment(true, r#"{} {"a":1}"#),
            Some(r#"{"a":1}"#)
        );
        // Mid-stream (buffer non-empty): a bare null/{} is the real field value
        // and must be kept VERBATIM, not dropped.
        assert_eq!(accumulate_argument_fragment(false, "null"), Some("null"));
        assert_eq!(accumulate_argument_fragment(false, "{}"), Some("{}"));
        // A truly empty mid-stream fragment is still skipped.
        assert_eq!(accumulate_argument_fragment(false, ""), None);
    }

    #[test]
    fn treats_empty_placeholders_as_absent() {
        assert_eq!(normalize_argument_fragment(""), None);
        assert_eq!(normalize_argument_fragment(" {} "), None);
        assert_eq!(normalize_argument_fragment("[]"), None);
        assert_eq!(normalize_argument_fragment("null"), None);
        assert_eq!(normalize_argument_fragment("NULL"), None);
    }

    #[test]
    fn recovers_json_payload_after_empty_prefix() {
        assert_eq!(
            normalize_argument_fragment(r#"{} {"path":"Cargo.toml"}"#),
            Some(r#"{"path":"Cargo.toml"}"#)
        );
        assert_eq!(
            normalize_argument_fragment(r#"[]{"path":"Cargo.toml"}"#),
            Some(r#"{"path":"Cargo.toml"}"#)
        );
        assert_eq!(
            normalize_argument_fragment(r#"null {"path":"Cargo.toml"}"#),
            Some(r#"{"path":"Cargo.toml"}"#)
        );
    }

    #[test]
    fn leaves_non_json_suffixes_untouched() {
        assert_eq!(
            normalize_argument_fragment("{} not-json"),
            Some("{} not-json")
        );
        assert_eq!(normalize_argument_fragment("nullish"), Some("nullish"));
        assert!(!is_empty_argument_fragment("{} not-json"));
    }
}

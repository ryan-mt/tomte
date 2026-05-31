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
    use super::{is_empty_argument_fragment, normalize_argument_fragment};

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

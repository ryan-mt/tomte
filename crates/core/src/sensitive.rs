pub(crate) const ERROR_EXCERPT_CHARS: usize = 160;

pub(crate) fn redact_auth_in(body: &str) -> String {
    let mut out = body.to_string();
    for token in ["sk-ant-", "sk_proj_", "sk-proj-", "sk-", "Bearer "] {
        let mut search_from = 0;
        while let Some(rel) = out[search_from..].find(token) {
            let start = search_from + rel;
            // Only treat the prefix as a real token start at a word boundary, so
            // benign substrings (`disk-usage`, `risk-free`, `ask-them`) aren't
            // mangled in error output. A real token always sits after a quote,
            // `=`/`:`, whitespace, or the string start — never glued onto a
            // preceding identifier character.
            let preceded_by_ident = out[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
            if preceded_by_ident {
                search_from = start + token.len();
                continue;
            }
            let tail = &out[start + token.len()..];
            let end = tail
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || matches!(*c, '"' | '\'' | '}' | ']' | ','))
                .map(|(offset, _)| start + token.len() + offset)
                .unwrap_or(out.len());
            out.replace_range(start..end, "<redacted>");
            search_from = start + "<redacted>".len();
        }
    }
    out
}

pub(crate) fn error_excerpt(body: &str) -> String {
    let redacted = redact_auth_in(body);
    let total_chars = redacted.chars().count();
    if total_chars <= ERROR_EXCERPT_CHARS {
        return redacted;
    }

    let excerpt: String = redacted.chars().take(ERROR_EXCERPT_CHARS).collect();
    format!(
        "{excerpt}... [truncated {} chars]",
        total_chars - ERROR_EXCERPT_CHARS
    )
}

/// Like [`error_excerpt`], but first redacts `secret` (a known configured
/// provider key) by exact match. The prefix heuristic in [`redact_auth_in`] only
/// catches `sk-`/`Bearer`-style tokens, so a custom OpenAI-compatible provider's
/// bare key (hex, `xai-…`, `gsk_…`) would otherwise survive into a logged error
/// body. A short/empty `secret` is ignored to avoid over-redacting noise.
pub(crate) fn error_excerpt_redacting(body: &str, secret: &str) -> String {
    if secret.len() >= 8 {
        error_excerpt(&body.replace(secret, "<redacted>"))
    } else {
        error_excerpt(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_excerpt_redacts_exact_configured_key() {
        // A custom provider key with no recognized prefix is missed by the
        // heuristic but caught by exact match.
        let key = "xai-prefixless-secret-9f8e7d6c5b4a";
        let body = format!(r#"{{"error":"bad auth: {key}"}}"#);
        assert!(
            redact_auth_in(&body).contains(key),
            "heuristic alone misses it"
        );
        let red = error_excerpt_redacting(&body, key);
        assert!(!red.contains(key), "{red}");
        assert!(red.contains("<redacted>"), "{red}");
        // A too-short / empty key is ignored (no over-redaction).
        assert_eq!(error_excerpt_redacting("hello world", ""), "hello world");
    }

    #[test]
    fn redact_auth_in_respects_word_boundaries() {
        // Real tokens (after a quote / `=` / space / string start) are redacted.
        assert!(!redact_auth_in(r#"{"k":"sk-ant-api03-xyz"}"#).contains("api03"));
        assert!(!redact_auth_in("Authorization: Bearer abc123def456").contains("abc123def456"));
        assert!(!redact_auth_in("key=sk-proj-secret9").contains("secret9"));
        // Benign substrings that merely share a prefix are left intact.
        assert_eq!(
            redact_auth_in("model gpt-5 disk-usage was high"),
            "model gpt-5 disk-usage was high"
        );
        assert_eq!(
            redact_auth_in("risk-free and brisk-walking"),
            "risk-free and brisk-walking"
        );
        assert_eq!(redact_auth_in("ask-them later"), "ask-them later");
    }
}

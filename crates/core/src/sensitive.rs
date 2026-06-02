pub(crate) const ERROR_EXCERPT_CHARS: usize = 160;

pub(crate) fn redact_auth_in(body: &str) -> String {
    let mut out = body.to_string();
    for token in ["sk-ant-", "sk_proj_", "sk-proj-", "sk-", "Bearer "] {
        while let Some(start) = out.find(token) {
            let tail = &out[start + token.len()..];
            let end = tail
                .char_indices()
                .find(|(_, c)| c.is_whitespace() || matches!(*c, '"' | '\'' | '}' | ']' | ','))
                .map(|(offset, _)| start + token.len() + offset)
                .unwrap_or(out.len());
            out.replace_range(start..end, "<redacted>");
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

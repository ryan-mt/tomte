//! Field-level parsing of Anthropic `content_block_delta` payloads: classifying
//! the delta kind and pulling text / thinking / signature / tool-argument
//! fragments out of the many shapes the API (and its aliases) emit. Pure and
//! network-free so the stream loop in [`super`] stays small and these are unit
//! tested directly.

use serde_json::Value;

use crate::tool_args::accumulate_argument_fragment;

const ANTHROPIC_TOOL_ARGUMENT_MAX_BYTES: usize = 2 * 1024 * 1024;
const ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES: usize = ANTHROPIC_TOOL_ARGUMENT_MAX_BYTES + 1;

pub(super) fn append_tool_args_capped(
    buf: &mut String,
    truncated: &mut bool,
    fragment: &str,
) -> Option<String> {
    // Drop a leading empty-args placeholder, keep mid-object fragments verbatim
    // (so a bare `"limit": null` survives) — shared rule, see
    // `accumulate_argument_fragment`.
    let fragment = accumulate_argument_fragment(buf.is_empty(), fragment)?;
    if *truncated {
        return None;
    }
    let remaining = ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES.saturating_sub(buf.len());
    if remaining == 0 {
        *truncated = true;
        return None;
    }
    if fragment.len() <= remaining {
        buf.push_str(fragment);
        return Some(fragment.to_string());
    }

    let mut cut = remaining;
    while cut > 0 && !fragment.is_char_boundary(cut) {
        cut -= 1;
    }
    let appended = if cut == 0 {
        " ".repeat(remaining)
    } else {
        fragment[..cut].to_string()
    };
    buf.push_str(&appended);
    *truncated = true;
    Some(appended)
}

pub(super) fn non_empty_tool_input(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        // Raw JSON-text fragment: keep it verbatim (see `append_tool_args_capped`).
        // Normalizing here would drop a bare `null`/`{}`/`[]` chunk mid-stream.
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string(other).ok(),
    }
}

pub(super) fn parse_anthropic_sse_error(
    error: impl std::fmt::Display,
    data: &str,
) -> anyhow::Error {
    anyhow::anyhow!(
        "parse Anthropic SSE: {error}; data: {}",
        crate::sensitive::error_excerpt(data)
    )
}

pub(super) fn partial_json_delta(delta: Option<&Value>) -> String {
    const KEYS: &[&str] = &[
        "partial_json",
        "partialJson",
        "input_json_delta",
        "inputJsonDelta",
        "arguments_delta",
        "argumentsDelta",
        "arguments",
    ];
    let Some(delta) = delta else {
        return String::new();
    };
    KEYS.iter()
        .find_map(|key| non_empty_tool_input(delta.get(*key)?))
        .unwrap_or_default()
}

fn string_delta_field(delta: Option<&Value>, keys: &[&str]) -> String {
    let Some(delta) = delta else {
        return String::new();
    };
    keys.iter()
        .find_map(|key| delta.get(*key)?.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

pub(super) fn text_delta(delta: Option<&Value>) -> String {
    string_delta_field(delta, &["text", "content", "delta"])
}

pub(super) fn thinking_delta(delta: Option<&Value>) -> String {
    string_delta_field(
        delta,
        &[
            "thinking",
            "reasoning",
            "reasoning_text",
            "reasoningText",
            "text",
            "content",
        ],
    )
}

pub(super) fn signature_delta(delta: Option<&Value>) -> String {
    string_delta_field(delta, &["signature", "signature_delta", "signatureDelta"])
}

pub(super) fn content_block_delta_type(
    delta: Option<&Value>,
    block_kind: Option<&str>,
) -> &'static str {
    let raw_type = delta
        .and_then(|d| d.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match raw_type {
        "text_delta" | "text" | "content_delta" => return "text_delta",
        "input_json_delta" | "tool_input_delta" | "arguments_delta" => {
            return "input_json_delta";
        }
        "thinking_delta" | "reasoning_delta" => return "thinking_delta",
        "signature_delta" => return "signature_delta",
        _ => {}
    }

    if !partial_json_delta(delta).is_empty() {
        return "input_json_delta";
    }
    if !signature_delta(delta).is_empty() {
        return "signature_delta";
    }
    if (block_kind == Some("thinking") || block_kind == Some("redacted_thinking"))
        && !thinking_delta(delta).is_empty()
    {
        return "thinking_delta";
    }
    if !text_delta(delta).is_empty() {
        return "text_delta";
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_anthropic_sse_error_redacts_and_caps_event_data() {
        let data = format!(
            "{{\"error\":\"bad key sk-ant-api03-secret and Bearer oauth-secret\",\"padding\":\"{}\"",
            "x".repeat(512)
        );

        let err = parse_anthropic_sse_error("expected value", &data);
        let message = err.to_string();

        assert!(!message.contains("sk-ant-api03-secret"), "{message}");
        assert!(!message.contains("oauth-secret"), "{message}");
        assert!(!message.contains(&"x".repeat(256)), "{message}");
        assert!(message.contains("<redacted>"), "{message}");
        assert!(message.contains("truncated"), "{message}");
        assert!(message.len() < 360, "{message}");
    }

    #[test]
    fn input_json_delta_concatenates_bare_null_verbatim() {
        // Regression: Claude streams `"limit": null` and the `null` value arrives
        // as its own input_json_delta chunk. It must be kept, not dropped as an
        // "empty placeholder", or the accumulated args become invalid JSON.
        let mut buf = String::new();
        let mut truncated = false;
        for frag in [
            r#"{"path":"sample.py","limit":"#,
            "null",
            r#","offset":null}"#,
        ] {
            append_tool_args_capped(&mut buf, &mut truncated, frag);
        }
        assert_eq!(buf, r#"{"path":"sample.py","limit":null,"offset":null}"#);
        // And it parses cleanly with limit == null.
        let v: serde_json::Value = serde_json::from_str(&buf).unwrap();
        assert!(v["limit"].is_null());
        assert_eq!(v["path"], "sample.py");
    }

    #[test]
    fn non_empty_tool_input_keeps_bare_null_string_fragment() {
        assert_eq!(
            non_empty_tool_input(&serde_json::json!("null")),
            Some("null".to_string())
        );
        // A truly empty string fragment is still skipped.
        assert_eq!(non_empty_tool_input(&serde_json::json!("")), None);
    }

    #[test]
    fn anthropic_tool_argument_buffer_is_capped_before_agent() {
        let mut buf = String::new();
        let mut truncated = false;

        let appended = append_tool_args_capped(
            &mut buf,
            &mut truncated,
            &"x".repeat(ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES + 8192),
        )
        .unwrap();

        assert_eq!(buf.len(), ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES);
        assert_eq!(appended.len(), ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES);
        assert!(truncated);
    }

    #[test]
    fn anthropic_tool_argument_cap_preserves_utf8_boundary() {
        let mut buf = "x".repeat(ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES - 1);
        let mut truncated = false;

        let appended = append_tool_args_capped(&mut buf, &mut truncated, "é").unwrap();

        assert_eq!(buf.len(), ANTHROPIC_TOOL_ARGUMENT_BUFFER_BYTES);
        assert_eq!(appended, " ");
        assert!(buf.is_char_boundary(buf.len()));
        assert!(truncated);
    }
}

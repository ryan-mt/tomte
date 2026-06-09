//! JSON value/string extraction helpers for Responses-API stream events.

use serde_json::Value;

pub(super) fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_value(value.get(*key)))
}

pub(super) fn first_argument_string(value: &Value, keys: &[&str]) -> Option<String> {
    first_argument_value(value, keys)
        .or_else(|| {
            value
                .get("function")
                .and_then(|f| first_argument_value(f, keys))
        })
        .or_else(|| {
            value
                .get("tool")
                .and_then(|t| first_argument_value(t, keys))
        })
        .or_else(|| {
            value
                .get("item")
                .and_then(|item| first_argument_string(item, keys))
        })
        .or_else(|| {
            value
                .get("output_item")
                .and_then(|item| first_argument_string(item, keys))
        })
}

pub(super) fn first_text_string(value: &Value, keys: &[&str]) -> Option<String> {
    first_text_value(value, keys)
        .or_else(|| {
            value
                .get("item")
                .and_then(|item| first_text_string(item, keys))
        })
        .or_else(|| {
            value
                .get("output_item")
                .and_then(|item| first_text_string(item, keys))
        })
}

pub(super) fn first_text_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| text_string_value(value.get(*key)))
}

pub(super) fn text_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(piece) = text_string_value(Some(part)) {
                    out.push_str(&piece);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        Value::Object(obj) => {
            for key in ["text", "content", "output_text", "outputText", "delta"] {
                if let Some(text) = text_string_value(obj.get(key)) {
                    return Some(text);
                }
            }
            None
        }
        Value::Number(_) | Value::Bool(_) => None,
    }
}

pub(super) fn first_argument_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| argument_string_value(value.get(*key)))
}

pub(super) fn argument_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        // Raw streamed JSON-text fragment: keep it verbatim, exactly like the
        // Chat path's `chat_argument_string_value`. A bare `null`/`{}`/`[]`
        // arriving mid-stream is the real value of a field (e.g. the streamed
        // value of `"limit": null`); normalizing here would corrupt args into
        // `"limit": ,`. The LEADING empty-args placeholder is dropped downstream
        // by `ToolArgsBuffer::push` while its buffer is still empty.
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

pub(super) fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn argument_string_value_keeps_real_values_and_drops_empty_placeholders() {
        // None / null / "" / empty container are placeholders → dropped, so a
        // leading empty-args fragment can't poison the accumulated tool args.
        assert_eq!(argument_string_value(None), None);
        assert_eq!(argument_string_value(Some(&json!(null))), None);
        assert_eq!(argument_string_value(Some(&json!(""))), None);
        assert_eq!(argument_string_value(Some(&json!([]))), None);
        assert_eq!(argument_string_value(Some(&json!({}))), None);
        // A raw JSON-text fragment is kept verbatim (e.g. the streamed `null`
        // value of `"limit": null` — normalizing it would corrupt the args).
        assert_eq!(
            argument_string_value(Some(&json!("\"limit\":null"))).as_deref(),
            Some("\"limit\":null")
        );
        // Scalars stringify; non-empty containers serialize back to JSON.
        assert_eq!(argument_string_value(Some(&json!(5))).as_deref(), Some("5"));
        assert_eq!(
            argument_string_value(Some(&json!(true))).as_deref(),
            Some("true")
        );
        assert_eq!(
            argument_string_value(Some(&json!([1, 2]))).as_deref(),
            Some("[1,2]")
        );
        assert_eq!(
            argument_string_value(Some(&json!({"a":1}))).as_deref(),
            Some("{\"a\":1}")
        );
    }

    #[test]
    fn text_string_value_flattens_parts_and_picks_known_keys() {
        assert_eq!(text_string_value(Some(&json!("hi"))).as_deref(), Some("hi"));
        assert_eq!(text_string_value(Some(&json!(""))), None);
        assert_eq!(text_string_value(Some(&json!(null))), None);
        assert_eq!(text_string_value(Some(&json!(5))), None);
        // Array of parts concatenates; nested {text|content|delta} are picked.
        assert_eq!(
            text_string_value(Some(&json!(["a", "b"]))).as_deref(),
            Some("ab")
        );
        assert_eq!(
            text_string_value(Some(&json!({"text":"x"}))).as_deref(),
            Some("x")
        );
        assert_eq!(
            text_string_value(Some(&json!({"delta":"d"}))).as_deref(),
            Some("d")
        );
        assert_eq!(
            text_string_value(Some(&json!([{"text":"a"},{"text":"b"}]))).as_deref(),
            Some("ab")
        );
        // An object with no recognized text key yields nothing.
        assert_eq!(text_string_value(Some(&json!({"other":"z"}))), None);
    }
}

//! Lenient value extraction across the many provider tool-call shapes
//! (camelCase aliases, object vs. string args, ...). Split out of `chat`.

use std::collections::BTreeMap;

use serde_json::Value;

use super::accumulator::ToolAcc;

pub(super) fn tool_call_values(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    }
}

fn chat_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

fn chat_value_from_keys(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| chat_string_value(value.get(*key)))
}

fn chat_argument_value_from_keys(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| chat_argument_string_value(value.get(*key)))
}

fn chat_argument_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        // Raw streamed JSON-text fragment: keep it verbatim. A bare
        // `null`/`{}`/`[]` arriving mid-stream is the real value of a field (e.g.
        // the streamed value of `"limit": null`); the LEADING empty-args
        // placeholder is dropped instead by `append_tool_args_capped` while its
        // buffer is still empty. Normalizing here would corrupt args into
        // `"limit": ,` (it sits ahead of that accumulator on the stream path).
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

pub(super) fn chat_content_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                if let Some(piece) = chat_content_text_part(part) {
                    text.push_str(&piece);
                }
            }
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        Value::Object(_) => chat_content_text_part(value?),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn chat_content_text_part(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(obj) => {
            for key in ["text", "content", "output_text", "outputText", "delta"] {
                if let Some(text) = chat_content_text(obj.get(key)) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

pub(super) fn chat_reasoning_text(message_part: &Value) -> Option<String> {
    [
        "reasoning_content",
        "reasoningContent",
        "reasoning_text",
        "reasoningText",
        "thinking_content",
        "thinkingContent",
        "reasoning",
        "thinking",
    ]
    .iter()
    .find_map(|key| chat_content_text(message_part.get(*key)))
}

pub(super) fn chat_tool_name(function: Option<&Value>, tool_call: &Value) -> Option<String> {
    const NAME_KEYS: &[&str] = &[
        "name",
        "tool_name",
        "toolName",
        "function_name",
        "functionName",
        "recipient_name",
        "recipientName",
    ];
    function
        .and_then(|f| chat_value_from_keys(f, NAME_KEYS))
        .or_else(|| chat_value_from_keys(tool_call, NAME_KEYS))
        .or_else(|| {
            tool_call
                .get("tool")
                .and_then(|t| chat_value_from_keys(t, NAME_KEYS))
        })
}

pub(super) fn chat_argument_value(function: Option<&Value>, tool_call: &Value) -> Option<String> {
    const ARG_KEYS: &[&str] = &[
        "arguments",
        "arguments_json",
        "argumentsJson",
        "args",
        "input",
        "input_json",
        "inputJson",
        "tool_input",
        "toolInput",
        "parameters",
        "parameters_json",
        "parametersJson",
        "partial_json",
        "partialJson",
        "input_json_delta",
        "inputJsonDelta",
    ];
    function
        .and_then(|f| chat_argument_value_from_keys(f, ARG_KEYS))
        .or_else(|| chat_argument_value_from_keys(tool_call, ARG_KEYS))
        .or_else(|| {
            tool_call
                .get("tool")
                .and_then(|t| chat_argument_value_from_keys(t, ARG_KEYS))
        })
}

pub(super) fn chat_tool_call_id(tool_call: &Value) -> Option<String> {
    chat_value_from_keys(
        tool_call,
        &[
            "id",
            "call_id",
            "callId",
            "tool_call_id",
            "toolCallId",
            "tool_use_id",
            "toolUseId",
        ],
    )
}

pub(super) fn chat_tool_call_index(
    tools: &BTreeMap<u32, ToolAcc>,
    tool_call: &Value,
    fallback_position: usize,
) -> u32 {
    // An out-of-range index would silently truncate under `as u32` and could
    // collide with a legitimate slot, so drop through to id/position resolution
    // when it does not fit — exactly as a missing index does.
    if let Some(index) = tool_call
        .get("index")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
    {
        return index;
    }
    if let Some(id) = chat_tool_call_id(tool_call) {
        if let Some((index, _)) = tools.iter().find(|(_, acc)| acc.id == id) {
            return *index;
        }
        let mut index = fallback_position as u32;
        while tools.contains_key(&index) {
            index = index.saturating_add(1);
        }
        return index;
    }
    // No index and no id: a bare continuation fragment. A single-element
    // continuation chunk lands here at position 0; if exactly one tool call is
    // in flight it can only belong to that call, so route to it rather than
    // trusting chunk-local position — which would split the call onto a fresh
    // slot 0 if its established index is non-zero. (When one chunk carries
    // several anonymous calls, the later ones arrive at a non-zero fallback
    // position and keep their own distinct slots.)
    if fallback_position == 0 {
        if let Some((index, _)) = tools.iter().next() {
            if tools.len() == 1 {
                return *index;
            }
        }
    }
    fallback_position as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_tool_helpers_accept_camel_case_aliases() {
        let tool_call = json!({
            "callId": "call_123",
            "toolName": "Read",
            "toolInput": {"filePath": "Cargo.toml"}
        });

        assert_eq!(chat_tool_call_id(&tool_call).as_deref(), Some("call_123"));
        assert_eq!(chat_tool_name(None, &tool_call).as_deref(), Some("Read"));
        assert_eq!(
            chat_argument_value(None, &tool_call).as_deref(),
            Some(r#"{"filePath":"Cargo.toml"}"#)
        );
    }

    #[test]
    fn chat_tool_id_accepts_anthropic_tool_use_alias() {
        let tool_call = json!({
            "tool_use_id": "toolu_123",
            "function": {"name": "read_file", "arguments": {"path": "Cargo.toml"}}
        });

        assert_eq!(chat_tool_call_id(&tool_call).as_deref(), Some("toolu_123"));
    }

    #[test]
    fn tool_call_index_routes_lone_continuation_and_rejects_huge_index() {
        let acc = || ToolAcc {
            id: "call_a".into(),
            name: Some("read_file".into()),
            args: String::new(),
            args_truncated: false,
            added: true,
            emitted_args_len: 0,
        };
        // One call established at a non-zero index. A bare continuation fragment
        // (no `index`, no `id`) arriving at position 0 must route to that call,
        // not split onto a fresh slot 0.
        let mut tools: BTreeMap<u32, ToolAcc> = BTreeMap::new();
        tools.insert(5, acc());
        let cont = json!({"function": {"arguments": "{\"path\":"}});
        assert_eq!(chat_tool_call_index(&tools, &cont, 0), 5);

        // An out-of-range `index` must not truncate via `as u32` into a colliding
        // slot; with no id it falls through to the position fallback.
        let huge = json!({ "index": 4_294_967_296u64 });
        assert_eq!(chat_tool_call_index(&tools, &huge, 3), 3);

        // A valid in-range index is honored unchanged.
        let ok = json!({ "index": 2 });
        assert_eq!(chat_tool_call_index(&tools, &ok, 0), 2);
    }
}

//! Split out of `agent`; logic unchanged.

use super::*;

#[derive(Debug, Clone, Default)]
pub(super) struct ToolArgsBuffer {
    pub(super) text: String,
    pub(super) too_large: bool,
}

impl ToolArgsBuffer {
    pub(super) fn push<'a>(&mut self, chunk: &'a str) -> Option<&'a str> {
        // Drop a leading empty-args placeholder, keep mid-object fragments
        // verbatim (so a bare `"limit": null` survives) — shared rule, see
        // `accumulate_argument_fragment`.
        let chunk = accumulate_argument_fragment(self.text.is_empty(), chunk)?;
        if self.too_large {
            return None;
        }
        if self.text.len().saturating_add(chunk.len()) > MAX_TOOL_ARGUMENT_BYTES {
            self.text.clear();
            self.too_large = true;
            return None;
        }
        self.text.push_str(chunk);
        Some(chunk)
    }

    pub(super) fn replace_if_non_empty(&mut self, value: String) {
        let Some(value) = normalize_argument_fragment(&value) else {
            return;
        };
        if self.too_large {
            return;
        }
        if value.len() > MAX_TOOL_ARGUMENT_BYTES {
            self.text.clear();
            self.too_large = true;
            return;
        }
        self.text.clear();
        self.text.push_str(value);
    }

    pub(super) fn merge_inline(&mut self, value: &str) {
        let Some(value) = normalize_argument_fragment(value) else {
            return;
        };
        if self.too_large {
            return;
        }
        if self.text.is_empty() || value.starts_with(&self.text) {
            self.replace_if_non_empty(value.to_string());
        } else {
            self.push(value);
        }
    }

    pub(super) fn merge_from(&mut self, other: ToolArgsBuffer) {
        if self.too_large {
            return;
        }
        if other.too_large {
            self.text.clear();
            self.too_large = true;
            return;
        }
        self.merge_inline(&other.text);
    }

    #[cfg(test)]
    pub(super) fn history_text(&self) -> String {
        if self.too_large {
            "{}".to_string()
        } else {
            self.text.clone()
        }
    }
}

pub(super) fn is_function_call_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call" | "tool_call" | "function" | "tool_use")
    ) || item.get("item").is_some_and(is_function_call_item)
        || item.get("output_item").is_some_and(is_function_call_item)
}

pub(super) fn string_field(item: &Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) fn string_field_any(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| string_field(item, key))
}

pub(super) fn tool_name_from_item(item: &Value) -> String {
    const TOOL_NAME_KEYS: &[&str] = &[
        "name",
        "tool_name",
        "toolName",
        "function_name",
        "functionName",
        "recipient_name",
        "recipientName",
    ];
    string_field_any(item, TOOL_NAME_KEYS)
        .or_else(|| {
            item.get("function")
                .and_then(|f| string_field_any(f, TOOL_NAME_KEYS))
        })
        .or_else(|| {
            item.get("tool")
                .and_then(|t| string_field_any(t, TOOL_NAME_KEYS))
        })
        .or_else(|| {
            item.get("item")
                .map(tool_name_from_item)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            item.get("output_item")
                .map(tool_name_from_item)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
}

pub(super) fn arguments_from_item(item: &Value) -> Option<String> {
    const ARGUMENT_KEYS: &[&str] = &[
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
        "arguments_delta",
        "argumentsDelta",
    ];
    let value = first_value_from_item(item, ARGUMENT_KEYS)
        .or_else(|| {
            item.get("function")
                .and_then(|f| first_value_from_item(f, ARGUMENT_KEYS))
        })
        .or_else(|| {
            item.get("tool")
                .and_then(|t| first_value_from_item(t, ARGUMENT_KEYS))
        });
    if let Some(arguments) = value.and_then(value_to_arguments) {
        return Some(arguments);
    }
    item.get("item")
        .and_then(arguments_from_item)
        .or_else(|| item.get("output_item").and_then(arguments_from_item))
}

pub(super) fn first_value_from_item<'a>(item: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| item.get(*key))
}

pub(super) fn value_to_arguments(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => normalize_argument_fragment(s).map(str::to_string),
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        other => serde_json::to_string(other).ok(),
    }
}

pub(super) fn function_call_refs(item: &Value) -> Option<(String, String)> {
    if let Some(nested) = item
        .get("item")
        .and_then(function_call_refs)
        .or_else(|| item.get("output_item").and_then(function_call_refs))
    {
        return Some(nested);
    }

    let call_id = string_field(item, "call_id")
        .or_else(|| string_field(item, "callId"))
        .or_else(|| string_field(item, "tool_call_id"))
        .or_else(|| string_field(item, "toolCallId"))
        .or_else(|| string_field(item, "tool_use_id"))
        .or_else(|| string_field(item, "toolUseId"))
        .or_else(|| string_field(item, "id"))
        .unwrap_or_default();
    let item_id = string_field(item, "id")
        .or_else(|| string_field(item, "item_id"))
        .or_else(|| string_field(item, "itemId"))
        .unwrap_or_else(|| call_id.clone());
    function_call_ids(&call_id, &item_id)
}

pub(super) fn function_call_ids(call_id: &str, item_id: &str) -> Option<(String, String)> {
    if call_id.is_empty() && item_id.is_empty() {
        return None;
    }
    let item_id = item_id.to_string();
    let call_id = if call_id.is_empty() {
        item_id.clone()
    } else {
        call_id.to_string()
    };
    Some((call_id, item_id))
}

/// Whether another orphan argument fragment may be accumulated for `item_id`.
/// Bounds both the number of distinct buffers and their aggregate bytes so a
/// malformed stream (endless unique ids, or endless fragments for one id)
/// can't pin memory. An existing id stays writable up to the byte cap; a new
/// id also needs a free slot under the count cap.
pub(super) fn orphan_args_has_room(
    buffers: &std::collections::HashMap<String, ToolArgsBuffer>,
    item_id: &str,
) -> bool {
    let total: usize = buffers.values().map(|b| b.text.len()).sum();
    total < MAX_ORPHAN_ARG_TOTAL_BYTES
        && (buffers.contains_key(item_id) || buffers.len() < MAX_ORPHAN_ARG_BUFFERS)
}

pub(super) fn take_orphan_args(
    buffers: &mut std::collections::HashMap<String, ToolArgsBuffer>,
    call_id: &str,
    item_id: &str,
) -> ToolArgsBuffer {
    let mut args = ToolArgsBuffer::default();
    if !call_id.is_empty() {
        if let Some(orphan) = buffers.remove(call_id) {
            args.merge_from(orphan);
        }
    }
    if item_id != call_id && !item_id.is_empty() {
        if let Some(orphan) = buffers.remove(item_id) {
            args.merge_from(orphan);
        }
    }
    args
}

pub(super) fn display_tool_name(name: &str) -> &str {
    // Trim first so a whitespace-only name reads `<missing>` in error text
    // instead of a backtick-wrapped run of spaces (the execution path trims
    // before matching, so this is display-only parity).
    let name = name.trim();
    if name.is_empty() {
        "<missing>"
    } else {
        name
    }
}

pub(super) fn history_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.len() > 64
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        "_invalid_tool_name".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn history_tool_name_for_registry(
    registry: &crate::tools::Registry,
    name: &str,
) -> String {
    registry
        .find(name)
        .map(|tool| tool.name().to_string())
        .unwrap_or_else(|| history_tool_name(name))
}

/// Marker substituted for an old tool output that microcompaction cleared.
pub(super) const CLEARED_TOOL_OUTPUT_MARKER: &str =
    "[older tool output cleared to conserve context — re-run the tool if you need this again]";
/// Most-recent tool outputs kept verbatim during microcompaction.
pub(super) const MICROCOMPACT_KEEP_RECENT: usize = 6;
/// Only clear outputs larger than this; tiny ones aren't worth a cache miss.
pub(super) const MICROCOMPACT_MIN_OUTPUT_BYTES: usize = 1024;
/// Context-occupancy percent at which microcompaction engages — below the 85%
/// full-summary fallback so it sheds bulk first and far more cheaply.
pub(super) const MICROCOMPACT_PCT: u64 = 75;

/// Replace the `output` of stale `FunctionCallOutput` items with a short marker,
/// keeping the most recent `keep_recent` intact and only touching outputs larger
/// than `min_bytes`. Structure is preserved (every `function_call` keeps its
/// paired output item, so both the OpenAI and Anthropic wires stay valid); only
/// the bulky, already-acted-on text is shed. Returns the number cleared.
pub(super) fn clear_stale_tool_outputs(
    history: &mut [InputItem],
    keep_recent: usize,
    min_bytes: usize,
) -> usize {
    let positions: Vec<usize> = history
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            matches!(item, InputItem::FunctionCallOutput { .. }).then_some(idx)
        })
        .collect();
    let clearable = positions.len().saturating_sub(keep_recent);
    let mut cleared = 0;
    for &idx in positions.iter().take(clearable) {
        if let InputItem::FunctionCallOutput { output, media, .. } = &mut history[idx] {
            // Attached media (base64 images/PDFs) is the real token bulk, so a
            // short-text result with media still counts as clearable —
            // otherwise image-heavy sessions shed nothing and ride into
            // hard overflow.
            if (output.len() > min_bytes || !media.is_empty())
                && output != CLEARED_TOOL_OUTPUT_MARKER
            {
                *output = CLEARED_TOOL_OUTPUT_MARKER.to_string();
                // Drop any attached media too — the marker replaces the whole
                // result, and a stale image only burns context/tokens.
                media.clear();
                cleared += 1;
            }
        }
    }
    cleared
}

// One completed tool call from a tool phase, ready to be written to history.
pub(super) struct CompletedCall {
    pub call_id: String,
    pub raw_name: String,
    pub output: String,
    pub is_error: bool,
    pub media: Vec<crate::openai::ToolMedia>,
    pub canonical_args: Option<String>,
}

/// Append one tool phase's calls to history in wire-safe order: every
/// `function_call` first, then every `function_call_output`, then any
/// schema-mismatch notes. Grouping matters on the Anthropic translate:
/// interleaving the pairs per call split one step into several assistant
/// messages, and the second-plus started with a bare `tool_use` — rejected
/// with a 400 whenever thinking is enabled. The outputs must also precede the
/// mismatch notes so `tool_result` blocks stay first in the user message.
pub(super) fn append_step_history(
    history: &mut Vec<InputItem>,
    registry: &crate::tools::Registry,
    completed: Vec<CompletedCall>,
) {
    for call in &completed {
        if let Some(arguments) = &call.canonical_args {
            history.push(InputItem::FunctionCall {
                call_id: call.call_id.clone(),
                name: history_tool_name_for_registry(registry, &call.raw_name),
                arguments: arguments.clone(),
            });
        }
    }
    let mut mismatched = Vec::new();
    for call in completed {
        if call.canonical_args.is_some() {
            history.push(InputItem::FunctionCallOutput {
                call_id: call.call_id,
                output: call.output,
                error: call.is_error,
                media: call.media,
            });
        } else {
            mismatched.push(call);
        }
    }
    for call in mismatched {
        history.push(InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::InputText {
                text: safe_tool_error_message(&call.raw_name, &call.output),
            }],
        });
    }
}

pub(super) fn safe_tool_error_message(raw_name: &str, output: &str) -> String {
    let name = raw_name.trim();
    let name = if name.is_empty() { "<missing>" } else { name };
    let name = safe_system_reminder_text(name, SAFE_TOOL_HISTORY_NAME_CHARS);
    let output = safe_system_reminder_text(output.trim(), SAFE_TOOL_HISTORY_ERROR_CHARS);
    format!(
        "<system-reminder>tomte could not execute tool `{name}`. The tool call was not recorded as a function_call because it does not match the active tool schema. Error: {output}</system-reminder>"
    )
}

pub(super) fn safe_system_reminder_text(input: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut truncated = false;
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            truncated = true;
            break;
        }
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' | '\r' | '\t' => out.push(ch),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    if truncated {
        out.push_str("...");
    }
    out
}

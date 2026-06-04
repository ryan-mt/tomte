//! Split out of `agent`; logic unchanged.

use super::*;

pub(super) fn canonical_edit_item(item: &Value) -> Option<Value> {
    let obj = item.as_object()?;
    let mut out = serde_json::Map::new();
    insert_first(
        &mut out,
        obj,
        "old_string",
        &["old_string", "oldString", "old_text", "oldText"],
    );
    insert_first(
        &mut out,
        obj,
        "new_string",
        &["new_string", "newString", "new_text", "newText"],
    );
    insert_bool_or_default(
        &mut out,
        obj,
        "replace_all",
        &["replace_all", "replaceAll"],
        false,
    );
    Some(Value::Object(out))
}

pub(super) fn canonical_todo_item(item: &Value) -> Option<Value> {
    let obj = item.as_object()?;
    let mut out = serde_json::Map::new();
    insert_first(&mut out, obj, "content", &["content"]);
    if let Some(status) = first_value(obj, &["status"]) {
        let value = status
            .as_str()
            .and_then(TodoStatus::parse)
            .map(todo_status_label)
            .map(|status| Value::String(status.to_string()))
            .unwrap_or_else(|| status.clone());
        out.insert("status".to_string(), value);
    }
    insert_first(&mut out, obj, "activeForm", &["activeForm", "active_form"]);
    Some(Value::Object(out))
}

pub(super) fn canonical_question_item(item: &Value) -> Option<Value> {
    let obj = item.as_object()?;
    let mut out = serde_json::Map::new();
    insert_first(&mut out, obj, "question", &["question", "prompt", "text"]);
    insert_first(&mut out, obj, "header", &["header", "title"]);
    let options = first_value(obj, &["options", "choices"])
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| canonical_question_option(item).unwrap_or_else(|| item.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    out.insert("options".to_string(), Value::Array(options));
    insert_bool_or_null(
        &mut out,
        obj,
        "multi_select",
        &["multi_select", "multiSelect"],
    );
    Some(Value::Object(out))
}

pub(super) fn canonical_question_option(item: &Value) -> Option<Value> {
    if let Some(label) = item.as_str() {
        let mut out = serde_json::Map::new();
        out.insert("label".to_string(), Value::String(label.to_string()));
        out.insert("description".to_string(), Value::String(label.to_string()));
        return Some(Value::Object(out));
    }
    let obj = item.as_object()?;
    let mut out = serde_json::Map::new();
    insert_first(&mut out, obj, "label", &["label", "value", "name", "title"]);
    if !out.contains_key("label") {
        insert_first(
            &mut out,
            obj,
            "label",
            &["description", "detail", "details", "text"],
        );
    }
    insert_first(
        &mut out,
        obj,
        "description",
        &["description", "detail", "details", "text"],
    );
    if !out.contains_key("description") {
        if let Some(label) = out.get("label").cloned() {
            out.insert("description".to_string(), label);
        }
    }
    Some(Value::Object(out))
}

pub(super) fn canonical_question_items(obj: &serde_json::Map<String, Value>) -> Vec<Value> {
    if let Some(items) = obj.get("questions").and_then(Value::as_array) {
        return items
            .iter()
            .map(|item| canonical_question_item(item).unwrap_or_else(|| item.clone()))
            .collect();
    }
    let has_question = first_value(obj, &["question", "prompt", "text"]).is_some();
    let has_options = first_value(obj, &["options", "choices"]).is_some();
    if has_question && has_options {
        let item = Value::Object(obj.clone());
        return vec![canonical_question_item(&item).unwrap_or(item)];
    }
    Vec::new()
}

pub(super) fn first_value<'a>(
    obj: &'a serde_json::Map<String, Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    keys.iter().find_map(|key| obj.get(*key))
}

pub(super) fn insert_first(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    if let Some(value) = first_value(obj, aliases) {
        out.insert(key.to_string(), value.clone());
    }
}

pub(super) fn insert_dispatch_subagent_type(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
) {
    if let Some(value) = first_value(
        obj,
        &[
            "subagent_type",
            "subagentType",
            "agent_type",
            "agentType",
            "agent",
            "type",
        ],
    ) {
        out.insert("subagent_type".to_string(), value.clone());
    } else {
        out.insert(
            "subagent_type".to_string(),
            Value::String("general-purpose".to_string()),
        );
    }
}

pub(super) fn insert_dispatch_plan_mode_required(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
) {
    let explicit = first_value(
        obj,
        &[
            "plan_mode_required",
            "planModeRequired",
            "plan_required",
            "planRequired",
        ],
    )
    .and_then(normalized_bool);
    let value = explicit
        .or_else(|| {
            first_value(
                obj,
                &["mode", "permission_mode", "permissionMode", "spawnMode"],
            )
            .and_then(normalized_dispatch_plan_mode)
        })
        .unwrap_or(Value::Null);
    out.insert("plan_mode_required".to_string(), value);
}

pub(super) fn insert_or_null(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    out.insert(
        key.to_string(),
        first_value(obj, aliases).cloned().unwrap_or(Value::Null),
    );
}

pub(super) fn insert_string_or_null(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    out.insert(
        key.to_string(),
        first_value(obj, aliases)
            .and_then(normalized_string)
            .unwrap_or(Value::Null),
    );
}

pub(super) fn insert_source_if_present(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    if let Some(value) = first_value(obj, aliases) {
        out.insert(
            key.to_string(),
            normalized_source_string(value).unwrap_or_else(|| value.clone()),
        );
    }
}

pub(super) fn insert_number_or_null(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    out.insert(
        key.to_string(),
        first_value(obj, aliases)
            .and_then(normalized_u64)
            .unwrap_or(Value::Null),
    );
}

pub(super) fn insert_bool_or_null(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    out.insert(
        key.to_string(),
        first_value(obj, aliases)
            .and_then(normalized_bool)
            .unwrap_or(Value::Null),
    );
}

pub(super) fn insert_bool_or_default(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
    default: bool,
) {
    out.insert(
        key.to_string(),
        first_value(obj, aliases)
            .and_then(normalized_bool)
            .unwrap_or(Value::Bool(default)),
    );
}

pub(super) fn insert_string_vec_or_null(
    out: &mut serde_json::Map<String, Value>,
    obj: &serde_json::Map<String, Value>,
    key: &str,
    aliases: &[&str],
) {
    out.insert(
        key.to_string(),
        first_value(obj, aliases)
            .and_then(normalized_string_vec)
            .unwrap_or(Value::Null),
    );
}

pub(super) fn normalized_string(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Number(n) => Some(Value::String(n.to_string())),
        _ => None,
    }
}

pub(super) fn normalized_source_string(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                let s = item.as_str()?;
                out.push_str(s);
            }
            Some(Value::String(out))
        }
        _ => None,
    }
}

pub(super) fn normalized_bool(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(b) => Some(Value::Bool(*b)),
        Value::Number(n) => match n.as_u64()? {
            0 => Some(Value::Bool(false)),
            1 => Some(Value::Bool(true)),
            _ => None,
        },
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(Value::Bool(true)),
            "false" | "0" | "no" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn normalized_dispatch_plan_mode(value: &Value) -> Option<Value> {
    let s = value.as_str()?;
    let normalized = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    match normalized.as_str() {
        "plan" | "plan_mode" | "planning" | "read_only" | "readonly" => Some(Value::Bool(true)),
        "default" | "auto" | "edit" | "edits" | "write" => Some(Value::Bool(false)),
        _ => None,
    }
}

pub(super) fn normalized_goal_status(value: &Value) -> Option<Value> {
    let s = value.as_str()?;
    let normalized = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let status = match normalized.as_str() {
        "in_progress" | "inprogress" | "progress" | "continue" | "continuing" | "working" => {
            "in_progress"
        }
        "complete" | "completed" | "done" | "success" | "succeeded" => "complete",
        "blocked" | "stuck" | "needs_input" | "needs_user_input" | "waiting_for_user" => "blocked",
        _ => return None,
    };
    Some(Value::String(status.to_string()))
}

pub(super) fn normalized_grep_output_mode(value: &Value) -> Option<Value> {
    let s = value.as_str()?;
    let normalized = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let mode = match normalized.as_str() {
        "" | "null" | "content" | "match" | "matches" | "lines" => "content",
        "files_with_matches" | "fileswithmatches" | "files" | "paths" | "filenames"
        | "files_only" | "filesonly" | "paths_only" | "pathsonly" => "files_with_matches",
        "count" | "counts" | "count_matches" | "countmatches" => "count",
        _ => return None,
    };
    Some(Value::String(mode.to_string()))
}

pub(super) fn normalized_glob_sort(value: &Value) -> Option<Value> {
    let s = value.as_str()?;
    let normalized = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let sort = match normalized.as_str() {
        "" | "null" | "name" | "names" | "alpha" | "alphabetical" | "alphabetic" | "filename"
        | "file_name" | "path" | "paths" => "name",
        "mtime" | "modified" | "modified_time" | "modtime" | "time" | "recent" | "recently"
        | "newest" | "date" => "mtime",
        _ => return None,
    };
    Some(Value::String(sort.to_string()))
}

pub(super) fn normalized_string_vec(value: &Value) -> Option<Value> {
    match value {
        Value::Array(items) => {
            let strings = items
                .iter()
                .filter_map(|item| item.as_str().map(str::trim))
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect::<Vec<_>>();
            Some(if strings.is_empty() {
                Value::Null
            } else {
                Value::Array(strings)
            })
        }
        Value::String(s) => {
            let strings = s
                .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect::<Vec<_>>();
            Some(if strings.is_empty() {
                Value::Null
            } else {
                Value::Array(strings)
            })
        }
        Value::Null => Some(Value::Null),
        _ => None,
    }
}

pub(super) fn normalized_u64(value: &Value) -> Option<Value> {
    match value {
        Value::Number(n) => n.as_u64().map(|n| json!(n)),
        Value::String(s) => s.trim().parse::<u64>().ok().map(|n| json!(n)),
        _ => None,
    }
}

pub(super) fn json_type_label(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Opt-in wire diagnostic (`TOMTE_DEBUG_WIRE=1`). Lets the user confirm the
/// reasoning effort they selected is actually carried to the provider and that
/// the model spent reasoning tokens — provider-agnostic, so it works the same
/// for OpenAI, Anthropic, and any future provider on the shared agent loop.
pub(super) fn wire_debug_enabled() -> bool {
    std::env::var_os("TOMTE_DEBUG_WIRE").is_some()
}

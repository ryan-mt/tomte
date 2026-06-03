//! Actionable feedback for malformed tool calls.
//!
//! Tool arguments are deserialized leniently by serde (see `parse_args` and the
//! `deserialize_*` helpers in the parent module), which stays the single source
//! of truth for whether a call is accepted. When serde *does* reject a call, the
//! raw message ("invalid type: string …, expected u64") rarely tells the model
//! the full shape it should have sent. [`ArgSchemaError`] marks such a failure so
//! the agent can recognize it (not a runtime error like "file not found") and
//! append [`schema_hint`] — a compact, model-facing summary of the tool's
//! expected arguments — so the model can self-correct within the same turn.

use serde_json::Value;
use std::fmt;

/// A tool's arguments failed to deserialize into its typed parameters. Carried
/// through `anyhow` so the agent can `downcast_ref` it and attach a schema hint;
/// its `Display` is kept stable so logs/transcripts read the same as before.
#[derive(Debug, Clone)]
pub struct ArgSchemaError {
    pub tool: String,
    pub detail: String,
}

impl fmt::Display for ArgSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tool `{}` argument schema mismatch: {}",
            self.tool, self.detail
        )
    }
}

impl std::error::Error for ArgSchemaError {}

/// A short, model-facing summary of a tool's expected arguments, derived from
/// its JSON Schema (the *original* `parameters_schema()`, not the strict-mode
/// rewrite). Returns an empty string when the schema documents no properties, so
/// the caller can skip appending a blank block.
///
/// "Required" is inferred from the type rather than the schema's `required`
/// list: opencli lists every property in `required` and represents optional
/// fields as a nullable type (`["integer","null"]`), so a property is treated as
/// optional when its type admits `null` (or it carries a `default`).
pub fn schema_hint(tool: &str, schema: &Value) -> String {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    if props.is_empty() {
        return String::new();
    }

    let mut lines = vec![format!("Expected arguments for `{tool}` (a JSON object):")];
    for (name, prop) in props {
        let ty = prop_type_label(prop);
        let req = if prop_is_optional(prop) {
            "optional"
        } else {
            "required"
        };
        let mut line = format!("  - {name} ({ty}, {req})");
        if let Some(desc) = prop.get("description").and_then(Value::as_str) {
            let desc = clean_desc(desc);
            if !desc.is_empty() {
                line.push_str(": ");
                line.push_str(&desc);
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Human-readable type for one schema property: enum values win, then arrays
/// (`array of <item>`), then a plain or union (`integer | null`) type.
fn prop_type_label(prop: &Value) -> String {
    if let Some(values) = prop.get("enum").and_then(Value::as_array) {
        let vals: Vec<String> = values.iter().map(stringify_scalar).collect();
        if !vals.is_empty() {
            return format!("one of: {}", vals.join(", "));
        }
    }
    if let Some(items) = prop.get("items") {
        return format!("array of {}", prop_type_label(items));
    }
    match prop.get("type") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(types)) => {
            let parts: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
            if parts.is_empty() {
                "any".to_string()
            } else {
                parts.join(" | ")
            }
        }
        _ => {
            for key in ["anyOf", "oneOf"] {
                if let Some(arr) = prop.get(key).and_then(Value::as_array) {
                    let parts: Vec<String> = arr.iter().map(prop_type_label).collect();
                    if !parts.is_empty() {
                        return parts.join(" | ");
                    }
                }
            }
            "any".to_string()
        }
    }
}

/// A property is optional when its type admits `null` (how opencli encodes
/// optional fields) or it declares a `default`.
fn prop_is_optional(prop: &Value) -> bool {
    if prop.get("default").is_some() {
        return true;
    }
    match prop.get("type") {
        Some(Value::Array(types)) => types.iter().any(|t| t.as_str() == Some("null")),
        _ => ["anyOf", "oneOf"].iter().any(|key| {
            prop.get(key).and_then(Value::as_array).is_some_and(|arr| {
                arr.iter()
                    .any(|s| s.get("type").and_then(Value::as_str) == Some("null"))
            })
        }),
    }
}

fn stringify_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Collapse whitespace and cap length so a long description stays a single tidy
/// bullet line.
fn clean_desc(desc: &str) -> String {
    let normalized = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 160;
    if normalized.chars().count() <= MAX {
        return normalized;
    }
    let truncated: String = normalized.chars().take(MAX).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arg_schema_error_display_is_stable() {
        let e = ArgSchemaError {
            tool: "read_file".to_string(),
            detail: "missing field `path`".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "tool `read_file` argument schema mismatch: missing field `path`"
        );
    }

    #[test]
    fn arg_schema_error_round_trips_through_anyhow() {
        // The agent relies on downcasting to tell an argument error apart from a
        // runtime error, so the type must survive the `anyhow` boundary.
        let err: anyhow::Error = anyhow::Error::new(ArgSchemaError {
            tool: "grep".to_string(),
            detail: "x".to_string(),
        });
        assert!(err.downcast_ref::<ArgSchemaError>().is_some());
    }

    #[test]
    fn hint_renders_required_optional_types_and_descriptions() {
        // Mirrors read_file: a required string and two nullable (optional) ints.
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path inside the working directory."},
                "offset": {"type": ["integer", "null"], "description": "Zero-indexed starting line."},
                "limit": {"type": ["integer", "null"]}
            },
            "required": ["path", "offset", "limit"]
        });
        let hint = schema_hint("read_file", &schema);
        assert!(hint.starts_with("Expected arguments for `read_file` (a JSON object):"));
        assert!(
            hint.contains("path (string, required): Relative path inside the working directory."),
            "got: {hint}"
        );
        // `offset`/`limit` are in `required` but nullable → reported as optional.
        assert!(
            hint.contains("offset (integer | null, optional)"),
            "got: {hint}"
        );
        assert!(
            hint.contains("limit (integer | null, optional)"),
            "got: {hint}"
        );
    }

    #[test]
    fn hint_renders_enums_and_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["status", "tags"]
        });
        let hint = schema_hint("todo_write", &schema);
        assert!(
            hint.contains("status (one of: pending, in_progress, completed, required)"),
            "got: {hint}"
        );
        assert!(
            hint.contains("tags (array of string, required)"),
            "got: {hint}"
        );
    }

    #[test]
    fn hint_is_empty_for_schema_without_properties() {
        assert!(schema_hint("noop", &json!({"type": "object"})).is_empty());
        assert!(schema_hint("noop", &json!({"type": "object", "properties": {}})).is_empty());
    }

    #[test]
    fn parse_args_failure_is_arg_schema_error() {
        #[derive(serde::Deserialize, Debug)]
        #[allow(dead_code)]
        struct Demo {
            path: String,
        }
        // Wrong type for `path` → serde rejects → wrapped as ArgSchemaError with
        // the stable Display logs/transcripts expect.
        let err = crate::tools::parse_args::<Demo>("demo", json!({"path": 5})).unwrap_err();
        assert!(err.downcast_ref::<ArgSchemaError>().is_some());
        assert!(err
            .to_string()
            .contains("tool `demo` argument schema mismatch"));
    }

    #[test]
    fn optional_via_default_is_detected() {
        let schema = json!({
            "type": "object",
            "properties": {"verbose": {"type": "boolean", "default": false}},
            "required": ["verbose"]
        });
        assert!(schema_hint("x", &schema).contains("verbose (boolean, optional)"));
    }
}

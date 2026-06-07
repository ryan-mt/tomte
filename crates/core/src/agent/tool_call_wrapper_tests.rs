//! Tool-call wrapper / function-call-id extraction tests.

use super::*;
use std::collections::HashMap;

#[test]
fn wrapper_does_not_strip_empty_prefix_before_non_json_suffix() {
    let item = json!({
        "type": "tool_call",
        "id": "call_123",
        "function": {
            "name": "read_file",
            "arguments": "{} not-json"
        }
    });

    assert_eq!(arguments_from_item(&item).as_deref(), Some("{} not-json"));
}

#[test]
fn wrapper_does_not_strip_null_prefix_inside_regular_text() {
    let item = json!({
        "type": "tool_call",
        "id": "call_123",
        "function": {
            "name": "read_file",
            "arguments": "nullish"
        }
    });

    assert_eq!(arguments_from_item(&item).as_deref(), Some("nullish"));
}

#[test]
fn accepts_anthropic_style_tool_use_item_shape() {
    let item = json!({
        "type": "tool_use",
        "id": "call_123",
        "name": "read_file",
        "input": {"path": "Cargo.toml"}
    });

    assert!(super::is_function_call_item(&item));
    assert_eq!(tool_name_from_item(&item), "read_file");
    assert_eq!(
        function_call_refs(&item),
        Some(("call_123".to_string(), "call_123".to_string()))
    );
    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn accepts_anthropic_style_tool_use_id_alias() {
    let item = json!({
        "type": "tool_use",
        "tool_use_id": "toolu_123",
        "itemId": "item_123",
        "name": "read_file",
        "input": {"path": "Cargo.toml"}
    });

    assert_eq!(
        function_call_refs(&item),
        Some(("toolu_123".to_string(), "item_123".to_string()))
    );
}

#[test]
fn accepts_namespaced_recipient_and_nested_tool_args() {
    let item = json!({
        "type": "tool_call",
        "id": "call_123",
        "recipient_name": "functions.Read",
        "tool": {
            "args": {"path": "Cargo.toml"}
        }
    });

    assert_eq!(tool_name_from_item(&item), "functions.Read");
    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn tool_args_buffer_keeps_delta_buffer_when_done_is_empty() {
    let mut args = ToolArgsBuffer::default();
    assert_eq!(args.push(r#"{"path":"#), Some(r#"{"path":"#));
    assert_eq!(args.push(r#""Cargo.toml"}"#), Some(r#""Cargo.toml"}"#));
    args.replace_if_non_empty(String::new());

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_treats_empty_object_as_placeholder_before_delta() {
    let mut args = ToolArgsBuffer::default();
    args.merge_inline("{}");
    assert_eq!(
        args.push(r#"{"path":"Cargo.toml"}"#),
        Some(r#"{"path":"Cargo.toml"}"#)
    );

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_reports_only_accepted_delta() {
    let mut args = ToolArgsBuffer::default();

    assert_eq!(args.push("{}"), None);
    assert_eq!(
        args.push(r#"{}{"path":"Cargo.toml"}"#),
        Some(r#"{"path":"Cargo.toml"}"#)
    );

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_keeps_delta_buffer_when_done_is_empty_object() {
    let mut args = ToolArgsBuffer::default();
    assert_eq!(
        args.push(r#"{"path":"Cargo.toml"}"#),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
    args.replace_if_non_empty("{}".to_string());

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_strips_empty_prefix_from_done_arguments() {
    let mut args = ToolArgsBuffer::default();
    assert_eq!(
        args.push(r#"{"path":"Cargo.toml"}"#),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
    args.replace_if_non_empty(r#"{}{"path":"Cargo.toml"}"#.to_string());

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_strips_null_prefix_from_inline_arguments() {
    let mut args = ToolArgsBuffer::default();
    args.merge_inline(r#"null {"path":"Cargo.toml"}"#);

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_keeps_non_json_suffix_after_empty_prefix() {
    let mut args = ToolArgsBuffer::default();
    args.merge_inline("{} not-json");

    assert_eq!(args.text, "{} not-json");
    assert!(!args.too_large);
}

#[test]
fn tool_args_buffer_marks_oversized_payloads() {
    let mut args = ToolArgsBuffer::default();
    assert_eq!(args.push(&"x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1)), None);

    assert!(args.too_large);
    assert!(args.text.is_empty());
    assert_eq!(args.history_text(), "{}");
}

#[test]
fn output_item_added_recovers_orphan_args_by_call_id() {
    let mut buffers = HashMap::new();
    assert_eq!(
        buffers
            .entry("call_123".to_string())
            .or_insert_with(ToolArgsBuffer::default)
            .push(r#"{"path":"Cargo.toml"}"#),
        Some(r#"{"path":"Cargo.toml"}"#)
    );

    let args = take_orphan_args(&mut buffers, "call_123", "item_123");

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(buffers.is_empty());
}

#[test]
fn output_item_added_recovers_orphan_args_by_item_id() {
    let mut buffers = HashMap::new();
    assert_eq!(
        buffers
            .entry("item_123".to_string())
            .or_insert_with(ToolArgsBuffer::default)
            .push(r#"{"path":"Cargo.toml"}"#),
        Some(r#"{"path":"Cargo.toml"}"#)
    );

    let args = take_orphan_args(&mut buffers, "call_123", "item_123");

    assert_eq!(args.text, r#"{"path":"Cargo.toml"}"#);
    assert!(buffers.is_empty());
}

#[test]
fn history_tool_name_rejects_empty_or_non_portable_names() {
    assert_eq!(history_tool_name(" read_file "), "read_file");
    assert_eq!(history_tool_name(""), "_invalid_tool_name");
    assert_eq!(history_tool_name("bad name"), "_invalid_tool_name");
}

#[test]
fn history_tool_name_canonicalizes_known_provider_aliases() {
    let registry = crate::tools::Registry::standard();

    assert_eq!(
        history_tool_name_for_registry(&registry, " Read "),
        "read_file"
    );
    assert_eq!(
        history_tool_name_for_registry(&registry, "Agent"),
        "dispatch_agent"
    );
    assert_eq!(
        history_tool_name_for_registry(&registry, "functions.Read"),
        "read_file"
    );
    assert_eq!(
        history_tool_name_for_registry(&registry, "update_goal"),
        "goal_update"
    );
    assert_eq!(
        history_tool_name_for_registry(&registry, "bad name"),
        "_invalid_tool_name"
    );
}

#[test]
fn history_tool_arguments_canonicalize_claude_style_file_args() {
    let raw = history_tool_arguments(
        "edit_file",
        &json!({
            "file_path": "/repo/src/main.rs",
            "old_string": "old",
            "new_string": "new"
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["path"], "/repo/src/main.rs");
    assert_eq!(value["old_string"], "old");
    assert_eq!(value["new_string"], "new");
    assert_eq!(value["replace_all"], false);
    assert!(value.get("file_path").is_none());
}

#[test]
fn history_tool_arguments_canonicalize_notebook_path_aliases() {
    let raw = history_tool_arguments(
        "notebook_edit",
        &json!({
            "path": "notebooks/demo.ipynb",
            "source": ["print(42)\n"],
            "index": 0,
            "type": null,
            "mode": "replace"
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["notebook_path"], "notebooks/demo.ipynb");
    assert_eq!(value["new_source"], "print(42)\n");
    assert_eq!(value["cell_id"], "0");
    assert_eq!(value["cell_type"], Value::Null);
    assert_eq!(value["edit_mode"], "replace");
    assert!(value.get("path").is_none());
    assert!(value.get("source").is_none());
    assert!(value.get("index").is_none());
    assert!(value.get("mode").is_none());
}

#[test]
fn history_tool_arguments_canonicalize_claude_style_bash_args() {
    let raw = history_tool_arguments(
        "run_shell",
        &json!({
            "cmd": "cargo test",
            "timeout": "1000",
            "run_in_background": "true",
            "description": "Run tests",
            "dangerouslyDisableSandbox": true
        }),
    );
    let value: Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(value["command"], "cargo test");
    assert_eq!(value["timeout_ms"], 1000);
    assert_eq!(value["run_in_background"], true);
    assert_eq!(value["dangerous_override"], Value::Null);
    assert!(value.get("cmd").is_none());
    assert!(value.get("description").is_none());
    assert!(value.get("dangerouslyDisableSandbox").is_none());
}

#[test]
fn history_tool_arguments_canonicalize_bash_id_aliases() {
    let output_raw = history_tool_arguments(
        "bash_output",
        &json!({
            "bashId": "bash_123",
            "id": "wrong"
        }),
    );
    let output_value: Value = serde_json::from_str(&output_raw).unwrap();

    assert_eq!(output_value["bash_id"], "bash_123");
    assert!(output_value.get("bashId").is_none());
    assert!(output_value.get("id").is_none());

    let kill_raw = history_tool_arguments(
        "kill_shell",
        &json!({
            "shell_id": "bash_456"
        }),
    );
    let kill_value: Value = serde_json::from_str(&kill_raw).unwrap();

    assert_eq!(kill_value["bash_id"], "bash_456");
    assert!(kill_value.get("shell_id").is_none());
}

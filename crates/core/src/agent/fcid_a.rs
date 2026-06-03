//! Function-call-id tests (fcid_a), split out of `agent`.

use super::*;

#[test]
fn args_buffer_keeps_bare_null_value_mid_stream() {
    // Regression: a streamed `"limit": null` whose `null` arrives as its own
    // delta chunk must NOT be dropped as an empty placeholder.
    let mut buf = ToolArgsBuffer::default();
    for chunk in [r#"{"path":"a.py","limit":"#, "null", r#","offset":null}"#] {
        buf.push(chunk);
    }
    assert_eq!(buf.text, r#"{"path":"a.py","limit":null,"offset":null}"#);
    let v: serde_json::Value = serde_json::from_str(&buf.text).unwrap();
    assert!(v["limit"].is_null());
}

#[test]
fn orphan_args_bounded_by_count_and_total_bytes() {
    use std::collections::HashMap;
    let mut buffers: HashMap<String, ToolArgsBuffer> = HashMap::new();
    // Empty map: room for a brand-new id.
    assert!(orphan_args_has_room(&buffers, "a"));

    // Fill to the count cap with tiny buffers.
    for i in 0..MAX_ORPHAN_ARG_BUFFERS {
        buffers.insert(format!("id{i}"), ToolArgsBuffer::default());
    }
    // A new id is refused at the count cap; an existing id still has room.
    assert!(!orphan_args_has_room(&buffers, "new"));
    assert!(orphan_args_has_room(&buffers, "id0"));

    // Blow the aggregate byte cap with one fat buffer; now even an existing
    // id is refused, so a single endless-fragment id can't pin memory.
    let fat = ToolArgsBuffer {
        text: "x".repeat(MAX_ORPHAN_ARG_TOTAL_BYTES),
        ..Default::default()
    };
    buffers.insert("id0".to_string(), fat);
    assert!(!orphan_args_has_room(&buffers, "id0"));
    assert!(!orphan_args_has_room(&buffers, "new"));
}

#[test]
fn args_buffer_drops_only_leading_placeholder() {
    // A leading `{}` placeholder is dropped; the real object that follows is
    // kept. (Mirrors a provider that prefixes args with an empty object.)
    let mut buf = ToolArgsBuffer::default();
    buf.push("{}");
    buf.push(r#"{"path":"a.py"}"#);
    assert_eq!(buf.text, r#"{"path":"a.py"}"#);
}

#[tokio::test]
async fn stream_truncation_skips_incomplete_tool_calls() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut partial_args = ToolArgsBuffer::default();
    partial_args.push(r#"{"command":"cargo""#);
    let mut pending = vec![
        PendingCall {
            call_id: "call_complete".into(),
            item_id: "item_complete".into(),
            name: "read_file".into(),
            args: ToolArgsBuffer::default(),
            args_done_emitted: true,
        },
        PendingCall {
            call_id: "call_partial".into(),
            item_id: "item_partial".into(),
            name: "run_shell".into(),
            args: partial_args,
            args_done_emitted: false,
        },
    ];

    skip_incomplete_tool_calls_after_truncation(
        &mut pending,
        &tx,
        "SSE stream ended before a terminal event",
    )
    .await;

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, "call_complete");
    match rx.recv().await.expect("skip event") {
        AgentEvent::ToolResult {
            call_id,
            output,
            error,
        } => {
            assert_eq!(call_id, "call_partial");
            assert!(error);
            assert!(
                output.contains("skipped this incomplete tool call"),
                "{output}"
            );
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn uses_item_id_when_call_id_is_missing() {
    assert_eq!(
        function_call_ids("", "item_123"),
        Some(("item_123".to_string(), "item_123".to_string()))
    );
}

#[test]
fn keeps_provider_call_id_when_present() {
    assert_eq!(
        function_call_ids("call_123", "item_123"),
        Some(("call_123".to_string(), "item_123".to_string()))
    );
}

#[test]
fn rejects_events_with_no_usable_id() {
    assert_eq!(function_call_ids("", ""), None);
}

#[test]
fn extracts_nested_function_call_refs_and_arguments() {
    let item = json!({
        "type": "tool_call",
        "id": "item_123",
        "tool_call_id": "call_123",
        "function": {
            "name": " Read ",
            "arguments": {"path": "Cargo.toml"}
        }
    });

    assert_eq!(tool_name_from_item(&item), "Read");
    assert_eq!(
        function_call_refs(&item),
        Some(("call_123".to_string(), "item_123".to_string()))
    );
    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn accepts_camel_case_tool_call_item_shape() {
    let item = json!({
        "type": "tool_call",
        "callId": "call_123",
        "itemId": "item_123",
        "toolName": "Read",
        "toolInput": {"filePath": "Cargo.toml"}
    });

    assert_eq!(tool_name_from_item(&item), "Read");
    assert_eq!(
        function_call_refs(&item),
        Some(("call_123".to_string(), "item_123".to_string()))
    );
    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"filePath":"Cargo.toml"}"#)
    );
}

#[test]
fn accepts_inline_input_arguments() {
    let item = json!({
        "type": "function_call",
        "id": "item_123",
        "name": "run_shell",
        "input": {"cmd": "cargo test"}
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"cmd":"cargo test"}"#)
    );
}

#[test]
fn accepts_parameters_as_tool_arguments() {
    let item = json!({
        "type": "tool_call",
        "id": "call_123",
        "function": {
            "name": "read_file",
            "parameters": {"path": "Cargo.toml"}
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn accepts_nested_provider_name_and_partial_json_aliases() {
    let item = json!({
        "type": "tool_call",
        "id": "call_123",
        "function": {
            "recipient_name": "functions.Read",
            "partialJson": {"path": "Cargo.toml"}
        }
    });

    assert_eq!(tool_name_from_item(&item), "functions.Read");
    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn accepts_output_item_wrapped_tool_call_shape() {
    let item = json!({
        "output_item": {
            "type": "tool_call",
            "id": "call_123",
            "function": {
                "recipient_name": "functions.Read",
                "partialJson": {"path": "Cargo.toml"}
            }
        }
    });

    assert!(super::is_function_call_item(&item));
    assert_eq!(tool_name_from_item(&item), "functions.Read");
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
fn wrapped_tool_call_refs_prefer_inner_ids() {
    let item = json!({
        "id": "wrapper_evt_1",
        "output_item": {
            "type": "tool_call",
            "id": "item_123",
            "call_id": "call_123",
            "function": {
                "name": "read_file",
                "arguments": {"path": "Cargo.toml"}
            }
        }
    });

    assert_eq!(
        function_call_refs(&item),
        Some(("call_123".to_string(), "item_123".to_string()))
    );
}

#[test]
fn wrapper_empty_arguments_fall_back_to_nested_tool_arguments() {
    let item = json!({
        "arguments": "",
        "output_item": {
            "type": "tool_call",
            "id": "call_123",
            "function": {
                "name": "read_file",
                "arguments": {"path": "Cargo.toml"}
            }
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn wrapper_blank_arguments_fall_back_to_nested_tool_arguments() {
    let item = json!({
        "arguments": "   \n\t",
        "output_item": {
            "type": "tool_call",
            "id": "call_123",
            "function": {
                "name": "read_file",
                "arguments": {"path": "Cargo.toml"}
            }
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn wrapper_empty_object_arguments_fall_back_to_nested_tool_arguments() {
    let item = json!({
        "arguments": {},
        "output_item": {
            "type": "tool_call",
            "id": "call_123",
            "function": {
                "name": "read_file",
                "arguments": {"path": "Cargo.toml"}
            }
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn wrapper_empty_array_arguments_fall_back_to_nested_tool_arguments() {
    let item = json!({
        "arguments": [],
        "output_item": {
            "type": "tool_call",
            "id": "call_123",
            "function": {
                "name": "read_file",
                "arguments": {"path": "Cargo.toml"}
            }
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn wrapper_null_string_arguments_fall_back_to_nested_tool_arguments() {
    let item = json!({
        "arguments": "null",
        "output_item": {
            "type": "tool_call",
            "id": "call_123",
            "function": {
                "name": "read_file",
                "arguments": {"path": "Cargo.toml"}
            }
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

#[test]
fn wrapper_concatenated_empty_prefix_arguments_are_recovered() {
    let item = json!({
        "type": "tool_call",
        "id": "call_123",
        "function": {
            "name": "read_file",
            "arguments": "{} {\"path\":\"Cargo.toml\"}"
        }
    });

    assert_eq!(
        arguments_from_item(&item).as_deref(),
        Some(r#"{"path":"Cargo.toml"}"#)
    );
}

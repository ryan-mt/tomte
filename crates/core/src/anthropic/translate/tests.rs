use super::*;
use crate::openai::models::{InputItem, MessageContent, ToolFunctionDef};

#[test]
fn cache_breakpoints_set_on_system_and_last_message() {
    let req = ResponsesRequest::new(
        "claude-opus-4-8",
        vec![InputItem::Message {
            role: "user".into(),
            content: vec![MessageContent::InputText { text: "hi".into() }],
        }],
    )
    .with_instructions("you are a helpful agent");
    let out = to_messages_request(&req);

    // The system prompt carries a cache breakpoint (which also caches the
    // tool definitions, since tools precede system in the cache prefix).
    let sys_cc = match &out.system.as_ref().expect("system present")[0] {
        SystemBlock::Text { cache_control, .. } => cache_control,
    };
    assert!(
        sys_cc.is_some(),
        "system block should be a cache breakpoint"
    );

    // The last block of the last message carries a breakpoint too, so the
    // conversation history is cached and not re-billed in full next turn.
    let last = out.messages.last().unwrap().content.last().unwrap();
    let msg_cc = match last {
        ContentBlock::Text { cache_control, .. } => cache_control,
        other => panic!("expected text block, got {other:?}"),
    };
    assert!(
        msg_cc.is_some(),
        "last message block should be a breakpoint"
    );

    // And it serializes in the Anthropic wire shape.
    let json = serde_json::to_string(&out).unwrap();
    assert!(
        json.contains(r#""cache_control":{"type":"ephemeral"}"#),
        "serialized request should carry cache_control: {json}"
    );
}

#[test]
fn translates_simple_user_message() {
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![InputItem::Message {
            role: "user".into(),
            content: vec![MessageContent::text("hi")],
        }],
    );
    let out = to_messages_request(&req);
    assert_eq!(out.model, "claude-opus-4-5");
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "user");
}

#[test]
fn unrecognized_claude_id_defaults_to_adaptive_not_no_thinking() {
    // A Claude-family id whose version `claude_version()` can't parse (an
    // alias or a future naming shape) must still get a thinking block —
    // sending a frontier Claude model no-thinking would silently disable
    // reasoning. `claude-opus-experimental` is a synthetic stand-in for any
    // such unrecognized id (it is not a real model).
    let plan = map_effort("claude-opus-experimental", Some("high"));
    assert!(
        matches!(plan.thinking, Some(ThinkingConfig::Adaptive)),
        "unrecognized Claude id should default to adaptive thinking"
    );
    assert_eq!(
        plan.output_config
            .expect("adaptive carries output_config")
            .effort,
        "high"
    );
}

#[test]
fn unrecognized_non_claude_id_stays_no_thinking() {
    // The Claude-family default must not leak to other providers: a non-Claude
    // id with no catalog thinking support stays no-thinking.
    let plan = map_effort("some-unknown-model", Some("high"));
    assert!(
        plan.thinking.is_none(),
        "a non-Claude id must stay no-thinking"
    );
}

#[test]
fn coalesces_adjacent_same_role_items() {
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![
            InputItem::Message {
                role: "user".into(),
                content: vec![MessageContent::text("a")],
            },
            InputItem::Message {
                role: "user".into(),
                content: vec![MessageContent::text("b")],
            },
        ],
    );
    let out = to_messages_request(&req);
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].content.len(), 2);
}

#[test]
fn function_call_becomes_assistant_tool_use() {
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![InputItem::FunctionCall {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: "{\"x\":1}".into(),
        }],
    );
    let out = to_messages_request(&req);
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "assistant");
    match &out.messages[0].content[0] {
        ContentBlock::ToolUse {
            id, name, input, ..
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "echo");
            assert_eq!(input["x"], 1);
        }
        _ => panic!("expected ToolUse"),
    }
}

#[test]
fn grouped_multi_tool_step_stays_one_assistant_message_with_leading_thinking() {
    // The history a multi-tool step writes (thinking, then all function_calls,
    // then all outputs) must fold into exactly assistant[thinking, tool_use,
    // tool_use] + user[tool_result, tool_result]. If the assistant turn split,
    // the second message would open with a bare tool_use — Anthropic rejects
    // that whenever thinking is enabled.
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![
            InputItem::Reasoning {
                id: String::new(),
                summary: Vec::new(),
                thinking: Some("plan both reads".into()),
                signature: Some("sig".into()),
                redacted_thinking: None,
            },
            InputItem::FunctionCall {
                call_id: "call_a".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"a.rs\"}".into(),
            },
            InputItem::FunctionCall {
                call_id: "call_b".into(),
                name: "read_file".into(),
                arguments: "{\"path\":\"b.rs\"}".into(),
            },
            InputItem::FunctionCallOutput {
                call_id: "call_a".into(),
                output: "a".into(),
                error: false,
                media: Vec::new(),
            },
            InputItem::FunctionCallOutput {
                call_id: "call_b".into(),
                output: "b".into(),
                error: false,
                media: Vec::new(),
            },
        ],
    );
    let out = to_messages_request(&req);
    assert_eq!(out.messages.len(), 2);
    assert_eq!(out.messages[0].role, "assistant");
    assert!(
        matches!(&out.messages[0].content[0], ContentBlock::Thinking { .. }),
        "assistant message must start with the thinking block"
    );
    assert!(matches!(
        &out.messages[0].content[1],
        ContentBlock::ToolUse { id, .. } if id == "call_a"
    ));
    assert!(matches!(
        &out.messages[0].content[2],
        ContentBlock::ToolUse { id, .. } if id == "call_b"
    ));
    assert_eq!(out.messages[1].role, "user");
    assert!(matches!(
        &out.messages[1].content[0],
        ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_a"
    ));
    assert!(matches!(
        &out.messages[1].content[1],
        ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_b"
    ));
}

#[test]
fn function_call_output_becomes_user_tool_result() {
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![InputItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: "42".into(),
            error: false,
            media: Vec::new(),
        }],
    );
    let out = to_messages_request(&req);
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "user");
    match &out.messages[0].content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => {
            assert_eq!(tool_use_id, "call_1");
            assert_eq!(content, &Value::String("42".into()));
            assert_eq!(is_error, &None);
        }
        _ => panic!("expected ToolResult"),
    }
}

#[test]
fn errored_function_call_output_marks_anthropic_tool_result_error() {
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![InputItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: "Error: missing file".into(),
            error: true,
            media: Vec::new(),
        }],
    );
    let out = to_messages_request(&req);
    match &out.messages[0].content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => {
            assert_eq!(tool_use_id, "call_1");
            assert_eq!(content, &Value::String("Error: missing file".into()));
            assert_eq!(is_error, &Some(true));
        }
        _ => panic!("expected ToolResult"),
    }
}

#[test]
fn function_call_output_with_media_emits_image_block() {
    let req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![InputItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: "see image".into(),
            error: false,
            media: vec![crate::openai::ToolMedia {
                media_type: "image/png".into(),
                data_base64: "iVBORw0KGgo=".into(),
            }],
        }],
    );
    let out = to_messages_request(&req);
    match &out.messages[0].content[0] {
        ContentBlock::ToolResult { content, .. } => {
            let arr = content.as_array().expect("media → content array");
            assert!(
                arr.iter()
                    .any(|b| b["type"] == "text" && b["text"] == "see image"),
                "expected a text block: {arr:?}"
            );
            let img = arr
                .iter()
                .find(|b| b["type"] == "image")
                .expect("expected an image block");
            assert_eq!(img["source"]["type"], "base64");
            assert_eq!(img["source"]["media_type"], "image/png");
            assert_eq!(img["source"]["data"], "iVBORw0KGgo=");
        }
        _ => panic!("expected ToolResult"),
    }
}

#[test]
fn forwards_function_tool_definitions() {
    let mut req = ResponsesRequest::new(
        "claude-opus-4-5",
        vec![InputItem::Message {
            role: "user".into(),
            content: vec![MessageContent::text("hi")],
        }],
    );
    req.tools = vec![Tool::Function(ToolFunctionDef {
        name: "search".into(),
        description: "search docs".into(),
        parameters: serde_json::json!({"type":"object"}),
        strict: true,
    })];
    let out = to_messages_request(&req);
    assert_eq!(out.tools.len(), 1);
    assert_eq!(out.tools[0].name, "search");
}

#[test]
fn high_effort_emits_adaptive_thinking_and_output_config() {
    let req = ResponsesRequest::new(
        "claude-opus-4-7",
        vec![InputItem::Message {
            role: "user".into(),
            content: vec![MessageContent::text("hi")],
        }],
    )
    .with_reasoning("high");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Adaptive)));
    assert_eq!(out.output_config.as_ref().unwrap().effort, "high");
}

#[test]
fn adaptive_thinking_serializes_without_effort_field() {
    let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("high");
    let out = to_messages_request(&req);
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["thinking"], serde_json::json!({"type": "adaptive"}));
    assert_eq!(json["output_config"]["effort"], "high");
}

#[test]
fn max_effort_lifts_max_tokens() {
    let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("max");
    let out = to_messages_request(&req);
    assert_eq!(out.output_config.as_ref().unwrap().effort, "max");
    assert!(out.max_tokens > 32_000);
}

#[test]
fn xhigh_passes_through_only_for_opus_4_7() {
    let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("xhigh");
    let out = to_messages_request(&req);
    assert_eq!(out.output_config.as_ref().unwrap().effort, "xhigh");

    let req = ResponsesRequest::new("claude-sonnet-4-6", vec![]).with_reasoning("xhigh");
    let out = to_messages_request(&req);
    assert_eq!(out.output_config.as_ref().unwrap().effort, "high");
}

#[test]
fn opus_4_8_gets_adaptive_thinking_and_xhigh() {
    let req = ResponsesRequest::new("claude-opus-4-8", vec![]).with_reasoning("high");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Adaptive)));
    assert_eq!(out.output_config.as_ref().unwrap().effort, "high");

    let req = ResponsesRequest::new("claude-opus-4-8", vec![]).with_reasoning("xhigh");
    let out = to_messages_request(&req);
    assert_eq!(out.output_config.as_ref().unwrap().effort, "xhigh");
    // xhigh is the top tier, so it must get the lifted ceiling like `max`.
    assert!(out.max_tokens > 32_000);
}

#[test]
fn fable_5_gets_adaptive_xhigh_and_omits_thinking_when_off() {
    // Fable 5's published surface (GA 2026-06-09): adaptive-only thinking,
    // xhigh honoured. An explicit `thinking:{"type":"disabled"}` is a 400 on
    // Fable, so with no effort the `thinking` field must be omitted entirely
    // (None serializes to no field) — adaptive is then always-on server-side.
    let req = ResponsesRequest::new("claude-fable-5", vec![]).with_reasoning("xhigh");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Adaptive)));
    assert_eq!(out.output_config.as_ref().unwrap().effort, "xhigh");
    assert!(out.max_tokens > 32_000);

    let req = ResponsesRequest::new("claude-fable-5", vec![]).with_reasoning("none");
    let out = to_messages_request(&req);
    assert!(out.thinking.is_none(), "no effort must omit `thinking`");
    let json = serde_json::to_value(&out).unwrap();
    assert!(
        json.get("thinking").is_none(),
        "the wire request must not carry a `thinking` field at all"
    );

    // Never the legacy budget shape — Fable rejects type:"enabled" with a 400.
    let req = ResponsesRequest::new("claude-fable-5", vec![]).with_reasoning("medium");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Adaptive)));
}

#[test]
fn future_opus_gets_adaptive_xhigh_not_legacy() {
    // A future Opus id not yet in the catalog must use adaptive + xhigh,
    // never the deprecated type:enabled shape (which 4.7+ reject with a 400).
    let req = ResponsesRequest::new("claude-opus-4-9", vec![]).with_reasoning("xhigh");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Adaptive)));
    assert_eq!(out.output_config.as_ref().unwrap().effort, "xhigh");
    assert!(out.max_tokens > 32_000);
}

#[test]
fn future_claude_never_uses_legacy_enabled() {
    for model in ["claude-opus-5-0", "claude-sonnet-5-0"] {
        let req = ResponsesRequest::new(model, vec![]).with_reasoning("high");
        let out = to_messages_request(&req);
        assert!(
            matches!(out.thinking, Some(ThinkingConfig::Adaptive)),
            "{model} must use adaptive, not the legacy enabled shape"
        );
    }
}

#[test]
fn minimal_disables_thinking() {
    let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("minimal");
    let out = to_messages_request(&req);
    assert!(out.thinking.is_none());
    assert!(out.output_config.is_none());
}

#[test]
fn haiku_gets_extended_thinking() {
    // Haiku 4.5 supports legacy extended thinking (docs: Extended = Yes,
    // Adaptive = No), so it gets a budget-based config rather than nothing.
    let req = ResponsesRequest::new("claude-haiku-4-5", vec![]).with_reasoning("medium");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Enabled { .. })));
    // Extended thinking has no output_config.effort (that field is adaptive-only).
    assert!(out.output_config.is_none());
}

#[test]
fn pre_4_6_models_use_extended_not_adaptive() {
    // Sonnet 4.5 / Opus 4.5 don't support adaptive; they get the legacy
    // budget-based extended-thinking shape instead of being silently dropped.
    let req = ResponsesRequest::new("claude-sonnet-4-5", vec![]).with_reasoning("high");
    let out = to_messages_request(&req);
    match out.thinking {
        Some(ThinkingConfig::Enabled { budget_tokens }) => {
            assert!(budget_tokens > 0 && budget_tokens < out.max_tokens);
        }
        other => panic!("expected Enabled thinking, got {other:?}"),
    }
    assert!(out.output_config.is_none());
}

#[test]
fn none_effort_disables_thinking_on_legacy_models() {
    let req = ResponsesRequest::new("claude-opus-4-5", vec![]).with_reasoning("none");
    let out = to_messages_request(&req);
    assert!(out.thinking.is_none());
}

#[test]
fn opus_4_5_extended_budget_stays_under_max_tokens() {
    let req = ResponsesRequest::new("claude-opus-4-5", vec![]).with_reasoning("max");
    let out = to_messages_request(&req);
    match out.thinking {
        Some(ThinkingConfig::Enabled { budget_tokens }) => {
            assert!(
                budget_tokens < out.max_tokens,
                "budget must stay below max_tokens"
            );
        }
        other => panic!("expected Enabled thinking, got {other:?}"),
    }
}

#[test]
fn ultracode_maps_to_xhigh_on_opus_4_8() {
    let req = ResponsesRequest::new("claude-opus-4-8", vec![]).with_reasoning("ultracode");
    let out = to_messages_request(&req);
    assert!(matches!(out.thinking, Some(ThinkingConfig::Adaptive)));
    assert_eq!(out.output_config.as_ref().unwrap().effort, "xhigh");
}

#[test]
fn reasoning_item_emits_thinking_block_before_tool_use() {
    let req = ResponsesRequest::new(
        "claude-opus-4-8",
        vec![
            InputItem::Reasoning {
                id: String::new(),
                summary: vec![],
                thinking: Some("pondering".into()),
                signature: Some("sig-abc".into()),
                redacted_thinking: None,
            },
            InputItem::FunctionCall {
                call_id: "call_1".into(),
                name: "echo".into(),
                arguments: "{}".into(),
            },
        ],
    );
    let out = to_messages_request(&req);
    // Both land in one assistant message: thinking block first, then tool_use.
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "assistant");
    match &out.messages[0].content[0] {
        ContentBlock::Thinking {
            thinking,
            signature,
        } => {
            assert_eq!(thinking, "pondering");
            assert_eq!(signature, "sig-abc");
        }
        other => panic!("expected thinking block first, got {other:?}"),
    }
    assert!(matches!(
        out.messages[0].content[1],
        ContentBlock::ToolUse { .. }
    ));
}

#[test]
fn reasoning_without_signature_is_dropped() {
    // An OpenAI-origin reasoning item (no signature) yields no thinking block.
    let req = ResponsesRequest::new(
        "claude-opus-4-8",
        vec![InputItem::Reasoning {
            id: "r1".into(),
            summary: vec![],
            thinking: None,
            signature: None,
            redacted_thinking: None,
        }],
    );
    let out = to_messages_request(&req);
    assert!(out.messages.is_empty());
}

#[test]
fn redacted_reasoning_emits_redacted_thinking_block_before_tool_use() {
    // A redacted_thinking block (no signature, only opaque data) must be
    // replayed verbatim ahead of the tool_use, or Anthropic rejects the
    // turn for a broken thinking/tool_use chain.
    let req = ResponsesRequest::new(
        "claude-opus-4-8",
        vec![
            InputItem::Reasoning {
                id: String::new(),
                summary: vec![],
                thinking: None,
                signature: None,
                redacted_thinking: Some("enc-xyz".into()),
            },
            InputItem::FunctionCall {
                call_id: "call_1".into(),
                name: "echo".into(),
                arguments: "{}".into(),
            },
        ],
    );
    let out = to_messages_request(&req);
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "assistant");
    match &out.messages[0].content[0] {
        ContentBlock::RedactedThinking { data } => assert_eq!(data, "enc-xyz"),
        other => panic!("expected redacted_thinking block first, got {other:?}"),
    }
    assert!(matches!(
        out.messages[0].content[1],
        ContentBlock::ToolUse { .. }
    ));
    // And it serializes in the Anthropic wire shape.
    let json = serde_json::to_string(&out).unwrap();
    assert!(
        json.contains(r#""type":"redacted_thinking","data":"enc-xyz""#),
        "got: {json}"
    );
}

#[test]
fn lifts_instructions_into_system_block() {
    let req = ResponsesRequest::new("claude-opus-4-5", vec![]).with_instructions("you are claude");
    let out = to_messages_request(&req);
    assert!(out.system.is_some());
    let s = out.system.unwrap();
    match &s[0] {
        SystemBlock::Text { text, .. } => assert_eq!(text, "you are claude"),
    }
}

//! Translate OpenAI Responses API shapes into Anthropic Messages shapes.
//!
//! The agent loop is written against OpenAI's `ResponsesRequest` / `InputItem`
//! types. To support Claude without rewriting the agent, the Anthropic client
//! accepts the OpenAI shape and converts it here. `reasoning_effort` is
//! mapped onto Anthropic's adaptive-thinking `effort`; `verbosity` is
//! dropped (no Anthropic equivalent).

use serde_json::Value;

use crate::openai::models::{InputItem, MessageContent, ResponsesRequest, Tool};

use super::models::{
    AnthropicMessage, CacheControl, ContentBlock, ImageSource, MessagesRequest, OutputConfig,
    SystemBlock, ThinkingConfig, ToolDef,
};

/// Default cap when no extended thinking is requested. Anthropic requires
/// `max_tokens` to be set; 32k leaves headroom for long tool-using turns.
const DEFAULT_MAX_TOKENS: u32 = 32_000;
/// Lifted ceiling for `max` effort, which can deliberate for many tokens and
/// still needs room to emit a final answer.
const MAX_EFFORT_MAX_TOKENS: u32 = 64_000;

struct EffortPlan {
    thinking: Option<ThinkingConfig>,
    output_config: Option<OutputConfig>,
    max_tokens: u32,
}

impl EffortPlan {
    fn no_thinking() -> Self {
        Self {
            thinking: None,
            output_config: None,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

/// Map a reasoning-effort label onto the Anthropic request shape.
///
/// Two thinking shapes exist:
///   - Adaptive (Opus 4.6/4.7/4.8, Sonnet 4.6): `thinking:{"type":"adaptive"}`
///     with the level in `output_config.effort`. Sending `effort` inside
///     `thinking` is rejected as an unknown field on Opus 4.7/4.8.
///   - Legacy extended (Opus 4.5, Sonnet 4.5, Haiku 4.5): the deprecated
///     `{"type":"enabled","budget_tokens":N}` shape — these models don't accept
///     adaptive. Opus 4.7/4.8 reject `type:enabled` (400), so they only ever use
///     adaptive.
///
/// `none`/`minimal`/`disabled` engage no thinking on either shape. `xhigh`
/// (and Claude Code's `ultracode`, which is xhigh on the wire) is only honoured
/// on Opus 4.7/4.8 and clamps to `high` elsewhere.
fn map_effort(model: &str, effort: Option<&str>) -> EffortPlan {
    let Some(raw) = effort else {
        return EffortPlan::no_thinking();
    };
    // Not valid adaptive efforts and we don't engage a budget for them.
    if matches!(raw, "none" | "minimal" | "disabled") {
        return EffortPlan::no_thinking();
    }

    if crate::catalog::supports_adaptive_thinking(model) {
        let eff = match raw {
            "low" | "medium" | "high" | "max" => raw,
            "xhigh" | "ultracode" if crate::catalog::supports_xhigh(model) => "xhigh",
            "xhigh" | "ultracode" => "high",
            _ => "high",
        };
        return EffortPlan {
            thinking: Some(ThinkingConfig::Adaptive),
            output_config: Some(OutputConfig {
                effort: eff.to_string(),
            }),
            // `xhigh`/`max` are the top deliberation tiers; give them the lifted
            // ceiling so thinking tokens don't eat the 32k budget and truncate.
            max_tokens: if eff == "max" || eff == "xhigh" {
                MAX_EFFORT_MAX_TOKENS
            } else {
                DEFAULT_MAX_TOKENS
            },
        };
    }

    if crate::catalog::supports_extended_thinking(model) {
        // Legacy budget-based thinking. budget_tokens must stay below max_tokens
        // (32k here), so every tier leaves room for the response. There is no
        // `output_config.effort` for this shape.
        let budget_tokens = match raw {
            "low" => 4_096,
            "medium" => 8_192,
            "high" | "xhigh" | "ultracode" => 16_384,
            "max" => 24_576,
            _ => 8_192,
        };
        return EffortPlan {
            thinking: Some(ThinkingConfig::Enabled { budget_tokens }),
            output_config: None,
            max_tokens: DEFAULT_MAX_TOKENS,
        };
    }

    EffortPlan::no_thinking()
}

pub fn to_messages_request(req: &ResponsesRequest) -> MessagesRequest {
    let mut messages: Vec<AnthropicMessage> = Vec::new();
    let mut pending_role: Option<String> = None;
    let mut pending_blocks: Vec<ContentBlock> = Vec::new();
    let flush = |role: &mut Option<String>,
                 blocks: &mut Vec<ContentBlock>,
                 out: &mut Vec<AnthropicMessage>| {
        if let Some(r) = role.take() {
            if !blocks.is_empty() {
                out.push(AnthropicMessage {
                    role: r,
                    content: std::mem::take(blocks),
                });
            }
        }
    };

    for item in &req.input {
        match item {
            InputItem::Message { role, content } => {
                let target_role = if role == "assistant" {
                    "assistant".to_string()
                } else {
                    "user".to_string()
                };
                if pending_role.as_deref() != Some(target_role.as_str()) {
                    flush(&mut pending_role, &mut pending_blocks, &mut messages);
                    pending_role = Some(target_role.clone());
                }
                for c in content {
                    if let Some(b) = message_content_to_block(c) {
                        pending_blocks.push(b);
                    }
                }
            }
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                if pending_role.as_deref() != Some("assistant") {
                    flush(&mut pending_role, &mut pending_blocks, &mut messages);
                    pending_role = Some("assistant".to_string());
                }
                let input: Value = serde_json::from_str(arguments)
                    .unwrap_or_else(|_| Value::Object(Default::default()));
                pending_blocks.push(ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input,
                    cache_control: None,
                });
            }
            InputItem::FunctionCallOutput {
                call_id,
                output,
                error,
            } => {
                if pending_role.as_deref() != Some("user") {
                    flush(&mut pending_role, &mut pending_blocks, &mut messages);
                    pending_role = Some("user".to_string());
                }
                pending_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: Value::String(output.clone()),
                    is_error: if *error { Some(true) } else { None },
                    cache_control: None,
                });
            }
            InputItem::Reasoning {
                thinking,
                signature,
                ..
            } => {
                // Replay the signed thinking block at the head of the assistant
                // message so the tool_use that follows verifies and the reasoning
                // chain is preserved. Skipped when no signature was captured
                // (e.g. an OpenAI-origin reasoning item carried in history).
                if let Some(sig) = signature {
                    if pending_role.as_deref() != Some("assistant") {
                        flush(&mut pending_role, &mut pending_blocks, &mut messages);
                        pending_role = Some("assistant".to_string());
                    }
                    pending_blocks.push(ContentBlock::Thinking {
                        thinking: thinking.clone().unwrap_or_default(),
                        signature: sig.clone(),
                    });
                }
            }
        }
    }
    flush(&mut pending_role, &mut pending_blocks, &mut messages);

    let system = req
        .instructions
        .as_ref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            vec![SystemBlock::Text {
                text: s.clone(),
                cache_control: None,
            }]
        });

    let tools = req.tools.iter().filter_map(tool_to_anthropic).collect();

    let effort = req.reasoning.as_ref().and_then(|r| r.effort.as_deref());
    let plan = map_effort(&req.model, effort);

    let mut request = MessagesRequest {
        model: req.model.clone(),
        max_tokens: plan.max_tokens,
        messages,
        system,
        tools,
        stream: req.stream,
        temperature: None,
        top_p: None,
        thinking: plan.thinking,
        output_config: plan.output_config,
    };
    apply_cache_breakpoints(&mut request);
    request
}

/// Add Anthropic prompt-cache breakpoints so the stable request prefix isn't
/// re-billed at full price every turn. Two ephemeral breakpoints:
///   1. the last system block — caches the system prompt and (since the cache
///      prefix is tools → system → messages) the tool definitions before it;
///   2. the last block of the last message — caches the conversation history,
///      so each subsequent turn only pays full price for the newest content.
///
/// Below the per-model minimum cacheable size Anthropic silently ignores the
/// markers, so setting them unconditionally is safe for short conversations.
fn apply_cache_breakpoints(request: &mut MessagesRequest) {
    match request.system.as_mut().and_then(|s| s.last_mut()) {
        Some(SystemBlock::Text { cache_control, .. }) => {
            *cache_control = Some(CacheControl::ephemeral());
        }
        None => {
            // No system prompt — cache the tool definitions directly instead.
            if let Some(last_tool) = request.tools.last_mut() {
                last_tool.cache_control = Some(CacheControl::ephemeral());
            }
        }
    }
    if let Some(block) = request
        .messages
        .last_mut()
        .and_then(|m| m.content.last_mut())
    {
        let cc = Some(CacheControl::ephemeral());
        match block {
            ContentBlock::Text { cache_control, .. }
            | ContentBlock::Image { cache_control, .. }
            | ContentBlock::ToolUse { cache_control, .. }
            | ContentBlock::ToolResult { cache_control, .. } => *cache_control = cc,
            // Thinking blocks carry no cache_control and are never the last
            // block in a message (text/tool_use always follow), so skip them.
            ContentBlock::Thinking { .. } => {}
        }
    }
}

fn message_content_to_block(c: &MessageContent) -> Option<ContentBlock> {
    match c {
        MessageContent::InputText { text } | MessageContent::OutputText { text } => {
            if text.is_empty() {
                None
            } else {
                Some(ContentBlock::Text {
                    text: text.clone(),
                    cache_control: None,
                })
            }
        }
        MessageContent::InputImage { image_url, .. } => {
            if let Some(rest) = image_url.strip_prefix("data:") {
                if let Some((media, data)) = rest.split_once(";base64,") {
                    return Some(ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: media.to_string(),
                            data: data.to_string(),
                        },
                        cache_control: None,
                    });
                }
            }
            Some(ContentBlock::Image {
                source: ImageSource::Url {
                    url: image_url.clone(),
                },
                cache_control: None,
            })
        }
    }
}

fn tool_to_anthropic(t: &Tool) -> Option<ToolDef> {
    match t {
        Tool::Function(f) => Some(ToolDef {
            name: f.name.clone(),
            description: f.description.clone(),
            input_schema: f.parameters.clone(),
            cache_control: None,
        }),
        Tool::WebSearch | Tool::CodeInterpreter => None,
    }
}

#[cfg(test)]
mod tests {
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
    fn function_call_output_becomes_user_tool_result() {
        let req = ResponsesRequest::new(
            "claude-opus-4-5",
            vec![InputItem::FunctionCallOutput {
                call_id: "call_1".into(),
                output: "42".into(),
                error: false,
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
            }],
        );
        let out = to_messages_request(&req);
        assert!(out.messages.is_empty());
    }

    #[test]
    fn lifts_instructions_into_system_block() {
        let req =
            ResponsesRequest::new("claude-opus-4-5", vec![]).with_instructions("you are claude");
        let out = to_messages_request(&req);
        assert!(out.system.is_some());
        let s = out.system.unwrap();
        match &s[0] {
            SystemBlock::Text { text, .. } => assert_eq!(text, "you are claude"),
        }
    }
}

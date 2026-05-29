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
/// Anthropic separates the thinking *mode* from the effort *level*: thinking
/// is `{"type":"adaptive"}` and the level lives in `output_config.effort`.
/// Sending `effort` inside `thinking` is rejected as an unknown field on
/// Opus 4.7/4.8. Haiku and pre-4.6 models don't support adaptive thinking; we
/// drop the `thinking` field entirely for them rather than fall back to the
/// deprecated `{"type":"enabled","budget_tokens":N}` shape. `xhigh` is only
/// honoured on Opus 4.7/4.8 and clamps down to `high` elsewhere.
fn map_effort(model: &str, effort: Option<&str>) -> EffortPlan {
    let model_lc = model.to_ascii_lowercase();
    if model_lc.contains("haiku") {
        return EffortPlan::no_thinking();
    }
    let is_adaptive_capable = crate::catalog::supports_adaptive_thinking(model);
    if !is_adaptive_capable {
        return EffortPlan::no_thinking();
    }

    let Some(raw) = effort else {
        return EffortPlan::no_thinking();
    };
    let level: Option<&str> = match raw {
        "none" | "minimal" | "disabled" => None,
        "low" | "medium" | "high" | "max" => Some(raw),
        "xhigh" => {
            if model_lc.contains("opus-4-8") || model_lc.contains("opus-4-7") {
                Some("xhigh")
            } else {
                Some("high")
            }
        }
        _ => Some("high"),
    };
    match level {
        None => EffortPlan::no_thinking(),
        Some(eff) => EffortPlan {
            thinking: Some(ThinkingConfig::Adaptive),
            output_config: Some(OutputConfig {
                effort: eff.to_string(),
            }),
            // `xhigh` is the highest deliberation tier (Opus 4.7/4.8); like
            // `max` it must get the lifted ceiling, otherwise adaptive
            // thinking tokens eat the 32k budget and truncate the answer.
            max_tokens: if eff == "max" || eff == "xhigh" {
                MAX_EFFORT_MAX_TOKENS
            } else {
                DEFAULT_MAX_TOKENS
            },
        },
    }
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
            InputItem::FunctionCallOutput { call_id, output } => {
                if pending_role.as_deref() != Some("user") {
                    flush(&mut pending_role, &mut pending_blocks, &mut messages);
                    pending_role = Some("user".to_string());
                }
                pending_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: Value::String(output.clone()),
                    is_error: None,
                    cache_control: None,
                });
            }
            InputItem::Reasoning { .. } => {
                // Anthropic does not accept replayed reasoning blocks.
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
            }],
        );
        let out = to_messages_request(&req);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].role, "user");
        match &out.messages[0].content[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                assert_eq!(tool_use_id, "call_1");
                assert_eq!(content, &Value::String("42".into()));
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
    fn minimal_disables_thinking() {
        let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("minimal");
        let out = to_messages_request(&req);
        assert!(out.thinking.is_none());
        assert!(out.output_config.is_none());
    }

    #[test]
    fn haiku_never_gets_thinking() {
        let req = ResponsesRequest::new("claude-haiku-4-5", vec![]).with_reasoning("max");
        let out = to_messages_request(&req);
        assert!(out.thinking.is_none());
        assert!(out.output_config.is_none());
    }

    #[test]
    fn pre_4_6_models_skip_adaptive() {
        let req = ResponsesRequest::new("claude-sonnet-4-5", vec![]).with_reasoning("high");
        let out = to_messages_request(&req);
        assert!(out.thinking.is_none());
        assert!(out.output_config.is_none());
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

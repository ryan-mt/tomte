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
    AnthropicMessage, ContentBlock, ImageSource, MessagesRequest, SystemBlock, ThinkingConfig,
    ToolDef,
};

/// Default cap when no extended thinking is requested. Anthropic requires
/// `max_tokens` to be set; 32k leaves headroom for long tool-using turns.
const DEFAULT_MAX_TOKENS: u32 = 32_000;
/// Lifted ceiling for `max` effort, which can deliberate for many tokens and
/// still needs room to emit a final answer.
const MAX_EFFORT_MAX_TOKENS: u32 = 64_000;

/// Map a reasoning-effort label onto Anthropic adaptive-thinking shape.
/// Newer Claude 4 models accept `{"type":"adaptive","effort":"<level>"}`
/// (manual `budget_tokens` is deprecated). Haiku doesn't support thinking.
///
/// Effort values per docs:
///   - low/medium/high  → all adaptive-capable Claude 4 models
///   - xhigh            → Opus 4.7 only (between high and max)
///   - max              → Opus 4.6+, Sonnet 4.6+, Opus 4.7
fn map_effort(model: &str, effort: Option<&str>) -> (Option<ThinkingConfig>, u32) {
    let model_lc = model.to_ascii_lowercase();
    if model_lc.contains("haiku") {
        return (None, DEFAULT_MAX_TOKENS);
    }
    let Some(raw) = effort else {
        return (None, DEFAULT_MAX_TOKENS);
    };
    // Normalise: OpenAI "none"/"minimal" disable thinking for Anthropic too.
    let normalised: Option<&str> = match raw {
        "none" | "minimal" | "disabled" => None,
        "low" | "medium" | "high" | "max" => Some(raw),
        "xhigh" => {
            if model_lc.contains("opus-4-7") {
                Some("xhigh")
            } else {
                Some("high")
            }
        }
        _ => Some("high"),
    };
    match normalised {
        None => (None, DEFAULT_MAX_TOKENS),
        Some(level) => {
            let max_tokens = if level == "max" {
                MAX_EFFORT_MAX_TOKENS
            } else {
                DEFAULT_MAX_TOKENS
            };
            (
                Some(ThinkingConfig::Adaptive {
                    effort: level.to_string(),
                }),
                max_tokens,
            )
        }
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
        .map(|s| vec![SystemBlock::Text { text: s.clone() }]);

    let tools = req.tools.iter().filter_map(tool_to_anthropic).collect();

    let effort = req.reasoning.as_ref().and_then(|r| r.effort.as_deref());
    let (thinking, max_tokens) = map_effort(&req.model, effort);

    MessagesRequest {
        model: req.model.clone(),
        max_tokens,
        messages,
        system,
        tools,
        stream: req.stream,
        temperature: None,
        top_p: None,
        thinking,
    }
}

fn message_content_to_block(c: &MessageContent) -> Option<ContentBlock> {
    match c {
        MessageContent::InputText { text } | MessageContent::OutputText { text } => {
            if text.is_empty() {
                None
            } else {
                Some(ContentBlock::Text { text: text.clone() })
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
                    });
                }
            }
            Some(ContentBlock::Image {
                source: ImageSource::Url {
                    url: image_url.clone(),
                },
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
        }),
        Tool::WebSearch | Tool::CodeInterpreter => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::models::{InputItem, MessageContent, ToolFunctionDef};

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
            ContentBlock::ToolUse { id, name, input } => {
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
    fn high_effort_emits_adaptive_thinking() {
        let req = ResponsesRequest::new(
            "claude-opus-4-7",
            vec![InputItem::Message {
                role: "user".into(),
                content: vec![MessageContent::text("hi")],
            }],
        )
        .with_reasoning("high");
        let out = to_messages_request(&req);
        let ThinkingConfig::Adaptive { effort } =
            out.thinking.expect("high must enable adaptive thinking");
        assert_eq!(effort, "high");
    }

    #[test]
    fn max_effort_lifts_max_tokens() {
        let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("max");
        let out = to_messages_request(&req);
        let ThinkingConfig::Adaptive { effort } = out.thinking.unwrap();
        assert_eq!(effort, "max");
        assert!(out.max_tokens > 32_000, "max effort needs lifted ceiling");
    }

    #[test]
    fn xhigh_passes_through_only_for_opus_4_7() {
        let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("xhigh");
        let out = to_messages_request(&req);
        let ThinkingConfig::Adaptive { effort } = out.thinking.unwrap();
        assert_eq!(effort, "xhigh", "Opus 4.7 must accept xhigh");

        let req = ResponsesRequest::new("claude-sonnet-4-6", vec![]).with_reasoning("xhigh");
        let out = to_messages_request(&req);
        let ThinkingConfig::Adaptive { effort } = out.thinking.unwrap();
        assert_eq!(effort, "high", "non-Opus-4.7 must downgrade xhigh to high");
    }

    #[test]
    fn minimal_disables_thinking() {
        let req = ResponsesRequest::new("claude-opus-4-7", vec![]).with_reasoning("minimal");
        let out = to_messages_request(&req);
        assert!(out.thinking.is_none(), "minimal must skip thinking");
    }

    #[test]
    fn haiku_never_gets_thinking() {
        let req = ResponsesRequest::new("claude-haiku-4-5", vec![]).with_reasoning("max");
        let out = to_messages_request(&req);
        assert!(out.thinking.is_none(), "haiku does not support thinking");
    }

    #[test]
    fn lifts_instructions_into_system_block() {
        let req = ResponsesRequest::new("claude-opus-4-5", vec![])
            .with_instructions("you are claude");
        let out = to_messages_request(&req);
        assert!(out.system.is_some());
        let s = out.system.unwrap();
        match &s[0] {
            SystemBlock::Text { text } => assert_eq!(text, "you are claude"),
        }
    }
}

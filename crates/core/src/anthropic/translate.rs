//! Translate OpenAI Responses API shapes into Anthropic Messages shapes.
//!
//! The agent loop is written against OpenAI's `ResponsesRequest` / `InputItem`
//! types. To support Claude without rewriting the agent, the Anthropic client
//! accepts the OpenAI shape and converts it here. The translation is lossy
//! in one direction (Anthropic has no `reasoning_effort` / `verbosity`), so
//! those fields are simply dropped.

use serde_json::Value;

use crate::openai::models::{InputItem, MessageContent, ResponsesRequest, Tool};

use super::models::{
    AnthropicMessage, ContentBlock, ImageSource, MessagesRequest, SystemBlock, ToolDef,
};

/// Maximum tokens to ask Claude to generate per turn. Anthropic requires
/// `max_tokens` to be set; 32k is a generous default that leaves headroom
/// for long tool-using turns without truncating real responses.
const DEFAULT_MAX_TOKENS: u32 = 32_000;

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

    MessagesRequest {
        model: req.model.clone(),
        max_tokens: DEFAULT_MAX_TOKENS,
        messages,
        system,
        tools,
        stream: req.stream,
        temperature: None,
        top_p: None,
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

//! Translate the shared Responses IR into a Chat Completions request body.
//! Split out of `chat`; logic unchanged.

use serde_json::{json, Map, Value};

use crate::openai::models::{InputItem, MessageContent, ResponsesRequest, Tool, ToolChoice};

/// Build a Chat Completions request body from the shared IR. Reasoning/verbosity
/// and the Responses-only fields are intentionally dropped: most compatible
/// endpoints reject unknown fields, and tool calls + text are what the agent
/// loop needs.
pub fn translate_chat_request(req: &ResponsesRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = &req.instructions {
        if !instructions.is_empty() {
            messages.push(json!({"role": "system", "content": instructions}));
        }
    }
    for item in &req.input {
        match item {
            InputItem::Message { role, content } => {
                let role = match role.as_str() {
                    "assistant" => "assistant",
                    "system" | "developer" => "system",
                    _ => "user",
                };
                messages.push(json!({"role": role, "content": message_content(content)}));
            }
            InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let call = json!({
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments},
                });
                // A tool call belongs on an assistant message's `tool_calls`.
                // Attach to the open assistant message when the model emitted
                // text first; otherwise open a content-less assistant message.
                if let Some(last) = messages.last_mut() {
                    if last.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        let arr = last
                            .as_object_mut()
                            .expect("assistant message is an object")
                            .entry("tool_calls")
                            .or_insert_with(|| Value::Array(Vec::new()));
                        if let Some(a) = arr.as_array_mut() {
                            a.push(call);
                        }
                        continue;
                    }
                }
                messages.push(json!({
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [call],
                }));
            }
            InputItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
            }
            // Reasoning items have no Chat Completions input representation.
            InputItem::Reasoning { .. } => {}
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), json!(messages));
    body.insert("stream".into(), json!(req.stream));
    if req.stream {
        // Ask for usage in the final chunk so token accounting works.
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let tools: Vec<Value> = req.tools.iter().filter_map(tool_to_chat).collect();
    if !tools.is_empty() {
        body.insert("tools".into(), json!(tools));
    }
    if let Some(tc) = &req.tool_choice {
        body.insert("tool_choice".into(), tool_choice_to_chat(tc));
    }
    Value::Object(body)
}

/// Map the shared IR reasoning effort onto a Chat Completions `reasoning_effort`
/// value. Returns `None` for levels with no standard representation, so the
/// field is omitted rather than risking a 400 upstream. `xhigh`/`max`/
/// `ultracode` clamp to `high`, the top tier Chat Completions defines.
fn chat_reasoning_effort(effort: &str) -> Option<&'static str> {
    match effort {
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" | "max" | "ultracode" => Some("high"),
        _ => None, // none / disabled / unknown — let the provider default
    }
}

/// Build the wire body for a Chat Completions request, injecting
/// `reasoning_effort` when the provider opts in (`forward_reasoning_effort`) and
/// the selected effort maps to a standard value. Shared by `send` and its test
/// so the test exercises the real injection rather than a parallel copy.
pub(super) fn chat_request_body(req: &ResponsesRequest, forward_reasoning_effort: bool) -> Value {
    let mut body = translate_chat_request(req);
    if forward_reasoning_effort {
        if let Some(effort) = req
            .reasoning
            .as_ref()
            .and_then(|r| r.effort.as_deref())
            .and_then(chat_reasoning_effort)
        {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("reasoning_effort".into(), json!(effort));
            }
        }
    }
    body
}

/// Collapse all-text content to a plain string (what every compatible endpoint
/// accepts); switch to the typed parts array only when an image is present.
fn message_content(content: &[MessageContent]) -> Value {
    let has_image = content
        .iter()
        .any(|c| matches!(c, MessageContent::InputImage { .. }));
    if !has_image {
        let mut s = String::new();
        for c in content {
            if let MessageContent::InputText { text } | MessageContent::OutputText { text } = c {
                s.push_str(text);
            }
        }
        return Value::String(s);
    }
    let parts: Vec<Value> = content
        .iter()
        .map(|c| match c {
            MessageContent::InputText { text } | MessageContent::OutputText { text } => {
                json!({"type": "text", "text": text})
            }
            MessageContent::InputImage { image_url, .. } => {
                json!({"type": "image_url", "image_url": {"url": image_url}})
            }
        })
        .collect();
    Value::Array(parts)
}

fn tool_to_chat(tool: &Tool) -> Option<Value> {
    match tool {
        Tool::Function(f) => Some(json!({
            "type": "function",
            "function": {
                "name": f.name,
                "description": f.description,
                "parameters": f.parameters,
                "strict": f.strict,
            }
        })),
        // OpenAI Responses built-ins have no Chat Completions equivalent.
        Tool::WebSearch | Tool::CodeInterpreter => None,
    }
}

fn tool_choice_to_chat(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Mode(m) => json!(m),
        ToolChoice::Specific { name, .. } => {
            json!({"type": "function", "function": {"name": name}})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::models::ToolFunctionDef;

    #[test]
    fn translate_groups_tool_calls_and_results() {
        let req = ResponsesRequest::new(
            "llama-3.3-70b",
            vec![
                InputItem::Message {
                    role: "user".into(),
                    content: vec![MessageContent::text("read it")],
                },
                InputItem::Message {
                    role: "assistant".into(),
                    content: vec![MessageContent::OutputText {
                        text: "sure".into(),
                    }],
                },
                InputItem::FunctionCall {
                    call_id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{\"path\":\"x\"}".into(),
                },
                InputItem::FunctionCallOutput {
                    call_id: "call_1".into(),
                    output: "contents".into(),
                    error: false,
                    media: Vec::new(),
                },
            ],
        )
        .with_instructions("be brief")
        .with_tools(vec![Tool::Function(ToolFunctionDef {
            name: "read_file".into(),
            description: "read a file".into(),
            parameters: json!({"type": "object"}),
            strict: true,
        })]);

        let body = translate_chat_request(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "be brief");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "read it");
        // The function call must attach to the assistant message, not stand alone.
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "sure");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "read_file");
        // The output becomes a `tool` message keyed by the call id.
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
        assert_eq!(msgs[3]["content"], "contents");
        // Tools and streaming options are present.
        assert_eq!(body["tools"][0]["function"]["name"], "read_file");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn translate_opens_assistant_message_for_leading_tool_call() {
        // A tool call with no preceding assistant text opens its own message.
        let req = ResponsesRequest::new(
            "m",
            vec![InputItem::FunctionCall {
                call_id: "c1".into(),
                name: "f".into(),
                arguments: "{}".into(),
            }],
        );
        let body = translate_chat_request(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "assistant");
        assert!(msgs[0]["content"].is_null());
        assert_eq!(msgs[0]["tool_calls"][0]["id"], "c1");
    }

    #[test]
    fn forwards_reasoning_effort_only_when_enabled_and_mappable() {
        use crate::openai::models::ReasoningConfig;
        let req = |effort: &str| ResponsesRequest {
            reasoning: Some(ReasoningConfig {
                effort: Some(effort.to_string()),
                summary: None,
            }),
            ..ResponsesRequest::new("m", vec![])
        };
        // Drives the REAL body-builder that `send` uses, not a parallel copy.
        let body_effort = |forward: bool, effort: &str| -> Option<String> {
            chat_request_body(&req(effort), forward)
                .get("reasoning_effort")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };

        // Disabled: never forwarded, even for a valid level.
        assert_eq!(body_effort(false, "high"), None);
        // Enabled: standard levels pass through; xhigh clamps to high.
        assert_eq!(body_effort(true, "medium").as_deref(), Some("medium"));
        assert_eq!(body_effort(true, "xhigh").as_deref(), Some("high"));
        // Enabled but unmappable extreme: omitted rather than risking a 400.
        assert_eq!(body_effort(true, "none"), None);
    }
}

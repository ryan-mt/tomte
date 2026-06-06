use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tool definition for the Responses API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Tool {
    Function(ToolFunctionDef),
    #[serde(rename = "web_search")]
    WebSearch,
    #[serde(rename = "code_interpreter")]
    CodeInterpreter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(default = "default_strict")]
    pub strict: bool,
}

fn default_strict() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(String), // "auto" | "none" | "required"
    Specific { r#type: String, name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>, // "low" | "medium" | "high" | "xhigh"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>, // "low" | "medium" | "high"
}

/// One item in the input array. Can be a message, function_call_output, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputItem {
    #[serde(rename = "message")]
    Message {
        role: String, // "user" | "assistant" | "system" | "developer"
        content: Vec<MessageContent>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        call_id: String,
        output: String,
        /// Internal-only marker so provider adapters that support explicit tool
        /// errors (Anthropic) can preserve the distinction. Skipped on the
        /// OpenAI Responses wire because function_call_output does not accept
        /// an `error` field.
        #[serde(default, skip_serializing)]
        error: bool,
    },
    /// Provider-specific, opaque continuity item. A reasoning block can only be
    /// replayed to the provider that produced it. Because history is a shared IR
    /// that can cross providers (a `/model` switch, a resumed session), EVERY
    /// native provider must drop foreign reasoning at its IR→wire boundary:
    /// Anthropic keeps only signed blocks (`anthropic/translate.rs`), the OpenAI
    /// Responses path keeps only items with a real reasoning id and no signature
    /// (`openai/client.rs::strip_unsendable_reasoning`), and the Chat Completions
    /// adapter drops them entirely (`openai/chat.rs`). New providers must do the
    /// same, or they will reject the foreign id (e.g. OpenAI `400 input[N].id`).
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<Value>,
        /// Anthropic thinking plaintext, kept to replay the block across the
        /// tool loop. Empty when the model's `display` is `omitted` (4.7/4.8).
        /// Persisted (so a resumed session keeps it); the OpenAI Responses wire
        /// drops Anthropic-origin reasoning whole via `strip_unsendable_reasoning`
        /// and OpenAI-origin items leave it `None`, so it never reaches that wire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        /// Anthropic thinking-block signature (opaque encrypted reasoning).
        /// Required ahead of a replayed `tool_use`, so it must survive a session
        /// resume. Anthropic-origin items (signature set) are stripped before the
        /// OpenAI wire; OpenAI-origin items leave it `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Anthropic `redacted_thinking` data: reasoning the safety system
        /// encrypted, with no plaintext or signature. Replayed verbatim ahead
        /// of a `tool_use` (see `anthropic/translate.rs`); dropping it makes the
        /// API reject the follow-up turn — so it must survive a resume too.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted_thinking: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageContent {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "input_image")]
    InputImage {
        image_url: String,
        #[serde(default)]
        detail: Option<String>,
    },
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

impl MessageContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self::InputText { text: s.into() }
    }
}

/// Full Responses API request body.
#[derive(Debug, Clone, Serialize)]
pub struct ResponsesRequest {
    pub model: String,
    pub input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResponsesResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub output: Vec<Value>,
    #[serde(default)]
    pub usage: Option<Value>,
}

/// Convenience builder.
impl ResponsesRequest {
    pub fn new(model: impl Into<String>, input: Vec<InputItem>) -> Self {
        Self {
            model: model.into(),
            input,
            instructions: None,
            tools: Vec::new(),
            tool_choice: None,
            reasoning: None,
            text: None,
            previous_response_id: None,
            parallel_tool_calls: None,
            stream: true,
            store: None,
        }
    }

    pub fn with_instructions(mut self, ins: impl Into<String>) -> Self {
        self.instructions = Some(ins.into());
        self
    }
    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.parallel_tool_calls = Some(true);
        self.tools = tools;
        self
    }
    pub fn with_reasoning(mut self, effort: impl Into<String>) -> Self {
        self.reasoning = Some(ReasoningConfig {
            effort: Some(effort.into()),
            summary: Some("auto".to_string()),
        });
        self
    }
    pub fn with_verbosity(mut self, v: impl Into<String>) -> Self {
        self.text = Some(TextConfig {
            verbosity: Some(v.into()),
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_call_output_error_is_internal_only_on_responses_wire() {
        let req = ResponsesRequest::new(
            "gpt-5",
            vec![InputItem::FunctionCallOutput {
                call_id: "call_1".into(),
                output: "Error: failed".into(),
                error: true,
            }],
        );
        let wire = serde_json::to_value(&req).unwrap();
        assert_eq!(wire["input"][0]["type"], "function_call_output");
        assert_eq!(wire["input"][0]["call_id"], "call_1");
        assert_eq!(wire["input"][0]["output"], "Error: failed");
        assert!(wire["input"][0].get("error").is_none());
    }

    #[test]
    fn function_call_output_error_defaults_false_when_missing() {
        let item: InputItem = serde_json::from_value(serde_json::json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "ok"
        }))
        .unwrap();
        match item {
            InputItem::FunctionCallOutput { error, .. } => assert!(!error),
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }
}

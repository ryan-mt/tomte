//! Wire types for the Anthropic Messages API (`POST /v1/messages`).
//!
//! Mirrors the official API surface: a `MessagesRequest` is what we POST, and
//! `MessagesResponse` is the non-streaming response shape. Streaming uses
//! [`super::stream`] which produces incremental events.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One message in the `messages` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String, // "user" | "assistant"
    pub content: Vec<ContentBlock>,
}

/// Marks an Anthropic prompt-cache breakpoint. The request prefix up to and
/// including the block carrying this is cached and re-read at ~10% cost on
/// later turns. Only `{"type":"ephemeral"}` exists today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            kind: "ephemeral".to_string(),
        }
    }
}

/// A single block inside a message's `content` array. Text, tool invocations,
/// and tool results are all blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum SystemBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<SystemBlock>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// `effort` lives at the top level under `output_config`, NOT inside
    /// `thinking`. Putting it inside `thinking` is rejected as an unknown
    /// field on Opus 4.7.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
}

/// Anthropic thinking config. Newer Claude 4 models (Opus 4.6+, Sonnet 4.6+,
/// Opus 4.7, Opus 4.8) accept `{"type":"adaptive"}`; the effort level is sent
/// separately via `output_config.effort`. The legacy
/// `{"type":"enabled","budget_tokens":N}` form is deprecated on Opus 4.6 /
/// Sonnet 4.6 and rejected on Opus 4.7/4.8. Haiku does not support thinking.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    Adaptive,
    Enabled { budget_tokens: u32 },
}

/// Top-level guidance for response generation. Today we only carry `effort`,
/// which steers adaptive thinking depth.
///
/// Effort values per docs:
///   - low / medium / high → all adaptive-capable Claude 4 models
///   - xhigh → Opus 4.7 / Opus 4.8 only (between high and max)
///   - max  → Opus 4.6+, Sonnet 4.6+, Opus 4.7, Opus 4.8
#[derive(Debug, Clone, Serialize)]
pub struct OutputConfig {
    pub effort: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessagesResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub content: Vec<Value>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

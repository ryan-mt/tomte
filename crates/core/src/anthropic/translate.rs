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

    /// The forward-compatible adaptive-thinking shape (`thinking:{type:adaptive}`
    /// with the level in `output_config.effort`). Used for every adaptive-capable
    /// Claude model, and as the safe default for an uncatalogued Claude-family id
    /// whose version can't be parsed — sending such a frontier model no-thinking
    /// would silently disable reasoning, and the legacy `type:enabled` shape is
    /// rejected (400) by Opus 4.7+.
    fn adaptive(model: &str, raw: &str) -> Self {
        let eff = match raw {
            "low" | "medium" | "high" | "max" => raw,
            "xhigh" | "ultracode" if crate::catalog::supports_xhigh(model) => "xhigh",
            "xhigh" | "ultracode" => "high",
            _ => "high",
        };
        Self {
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
        return EffortPlan::adaptive(model, raw);
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

    // Neither catalog shape matched. For a Claude-family id this only happens
    // when `claude_version()` can't parse the id (an alias, or a future/reshaped
    // naming we don't recognize yet) — it does NOT mean thinking is unwanted, so
    // default to the forward-compatible adaptive shape rather than silently
    // disabling reasoning on what may be a frontier model. (Opus 4.7+ reject the
    // legacy `type:"enabled"` form, so adaptive is the safe default for anything
    // newer than the catalog knows.) Non-Claude providers keep no-thinking.
    if model.to_ascii_lowercase().starts_with("claude") {
        return EffortPlan::adaptive(model, raw);
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
                redacted_thinking,
                ..
            } => {
                // Replay the reasoning block at the head of the assistant message
                // so the tool_use that follows verifies and the reasoning chain is
                // preserved: a signed thinking block, or a redacted_thinking block
                // (opaque encrypted data, no signature). Skipped when neither was
                // captured (e.g. an OpenAI-origin reasoning item in history).
                let block = if let Some(sig) = signature {
                    Some(ContentBlock::Thinking {
                        thinking: thinking.clone().unwrap_or_default(),
                        signature: sig.clone(),
                    })
                } else {
                    redacted_thinking
                        .clone()
                        .map(|data| ContentBlock::RedactedThinking { data })
                };
                if let Some(block) = block {
                    if pending_role.as_deref() != Some("assistant") {
                        flush(&mut pending_role, &mut pending_blocks, &mut messages);
                        pending_role = Some("assistant".to_string());
                    }
                    pending_blocks.push(block);
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
            ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {}
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
mod tests;

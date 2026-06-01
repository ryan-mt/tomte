//! OpenAI-compatible Chat Completions adapter. Lets opencli talk to any
//! provider that implements `/v1/chat/completions` (Groq, OpenRouter, DeepSeek,
//! Together, local Ollama / LM Studio, …) by translating the shared
//! [`ResponsesRequest`] IR into a Chat Completions request and bridging its SSE
//! stream back onto the shared [`ResponseStreamEvent`] shape — the same IR the
//! OpenAI Responses and Anthropic paths use, so the agent loop is unchanged.
//!
//! This is the wire layer only (translation + streaming). Client construction,
//! config, auth, and routing live in the provider/config plumbing.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use super::models::{InputItem, MessageContent, ResponsesRequest, Tool, ToolChoice};
use super::stream::{ResponseStreamEvent, StreamHandle};
use crate::client::ProviderClient;
use crate::provider::Provider;
use crate::tool_args::normalize_argument_fragment;

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

/// Normalize Chat Completions usage (`prompt_tokens`/`completion_tokens`) into
/// the `input_tokens`/`output_tokens` shape the agent's accounting expects.
fn normalize_usage(usage: &Value) -> Value {
    let get = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    json!({
        "input_tokens": get("prompt_tokens"),
        "output_tokens": get("completion_tokens"),
        "total_tokens": get("total_tokens"),
    })
}

struct ToolAcc {
    id: String,
    name: Option<String>,
    args: String,
    args_truncated: bool,
    added: bool,
    emitted_args_len: usize,
}

const CHAT_TOOL_ARGUMENT_MAX_BYTES: usize = 2 * 1024 * 1024;
const CHAT_TOOL_ARGUMENT_BUFFER_BYTES: usize = CHAT_TOOL_ARGUMENT_MAX_BYTES + 1;

async fn apply_tool_delta(
    tools: &mut BTreeMap<u32, ToolAcc>,
    index: u32,
    id: Option<String>,
    name: Option<String>,
    args_fragment: Option<String>,
    tx: &mpsc::Sender<anyhow::Result<ResponseStreamEvent>>,
) {
    let acc = tools.entry(index).or_insert_with(|| ToolAcc {
        id: id.clone().unwrap_or_else(|| format!("call_{index}")),
        name: None,
        args: String::new(),
        args_truncated: false,
        added: false,
        emitted_args_len: 0,
    });
    if !acc.added {
        if let Some(id) = id {
            acc.id = id;
        }
    }
    if acc.name.is_none() {
        acc.name = name;
    }
    if let Some(frag) = args_fragment {
        append_tool_args_capped(acc, &frag);
    }
    if !acc.added {
        if let Some(name) = acc.name.as_deref() {
            let item = json!({
                "type": "function_call",
                "id": acc.id.clone(),
                "call_id": acc.id.clone(),
                "name": name,
                "arguments": "",
            });
            let _ = tx
                .send(Ok(ResponseStreamEvent::OutputItemAdded {
                    item,
                    output_index: index,
                }))
                .await;
            acc.added = true;
        }
    }
    if acc.added && acc.args.len() > acc.emitted_args_len {
        let delta = acc.args[acc.emitted_args_len..].to_string();
        acc.emitted_args_len = acc.args.len();
        let _ = tx
            .send(Ok(ResponseStreamEvent::FunctionCallArgsDelta {
                item_id: acc.id.clone(),
                delta,
            }))
            .await;
    }
}

fn append_tool_args_capped(acc: &mut ToolAcc, fragment: &str) {
    let Some(fragment) = normalize_argument_fragment(fragment) else {
        return;
    };
    if acc.args_truncated {
        return;
    }
    let remaining = CHAT_TOOL_ARGUMENT_BUFFER_BYTES.saturating_sub(acc.args.len());
    if remaining == 0 {
        acc.args_truncated = true;
        return;
    }
    if fragment.len() <= remaining {
        acc.args.push_str(fragment);
        return;
    }
    let mut cut = remaining;
    while cut > 0 && !fragment.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 {
        acc.args.push_str(&" ".repeat(remaining));
    } else {
        acc.args.push_str(&fragment[..cut]);
    }
    acc.args_truncated = true;
}

fn tool_call_values(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    }
}

fn chat_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

fn chat_value_from_keys(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| chat_string_value(value.get(*key)))
}

fn chat_argument_value_from_keys(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| chat_argument_string_value(value.get(*key)))
}

fn chat_argument_string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(s) => normalize_argument_fragment(s).map(str::to_string),
        Value::Array(arr) if arr.is_empty() => None,
        Value::Object(map) if map.is_empty() => None,
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(_) | Value::Object(_) => value.and_then(|v| serde_json::to_string(v).ok()),
    }
}

fn chat_tool_name(function: Option<&Value>, tool_call: &Value) -> Option<String> {
    const NAME_KEYS: &[&str] = &[
        "name",
        "tool_name",
        "toolName",
        "function_name",
        "functionName",
        "recipient_name",
        "recipientName",
    ];
    function
        .and_then(|f| chat_value_from_keys(f, NAME_KEYS))
        .or_else(|| chat_value_from_keys(tool_call, NAME_KEYS))
        .or_else(|| {
            tool_call
                .get("tool")
                .and_then(|t| chat_value_from_keys(t, NAME_KEYS))
        })
}

fn chat_argument_value(function: Option<&Value>, tool_call: &Value) -> Option<String> {
    const ARG_KEYS: &[&str] = &[
        "arguments",
        "arguments_json",
        "argumentsJson",
        "args",
        "input",
        "input_json",
        "inputJson",
        "tool_input",
        "toolInput",
        "parameters",
        "parameters_json",
        "parametersJson",
        "partial_json",
        "partialJson",
        "input_json_delta",
        "inputJsonDelta",
    ];
    function
        .and_then(|f| chat_argument_value_from_keys(f, ARG_KEYS))
        .or_else(|| chat_argument_value_from_keys(tool_call, ARG_KEYS))
        .or_else(|| {
            tool_call
                .get("tool")
                .and_then(|t| chat_argument_value_from_keys(t, ARG_KEYS))
        })
}

fn chat_tool_call_id(tool_call: &Value) -> Option<String> {
    chat_value_from_keys(
        tool_call,
        &[
            "id",
            "call_id",
            "callId",
            "tool_call_id",
            "toolCallId",
            "tool_use_id",
            "toolUseId",
        ],
    )
}

/// Bridge a Chat Completions SSE response onto a `StreamHandle` carrying
/// `ResponseStreamEvent`, consumed by the agent loop exactly like the other
/// providers.
pub fn handle_chat_response(resp: reqwest::Response) -> StreamHandle {
    let (tx, rx) = mpsc::channel::<anyhow::Result<ResponseStreamEvent>>(256);
    let tx_supervisor = tx.clone();
    tokio::spawn(async move {
        let pump = tokio::spawn(async move {
            let mut stream = resp.bytes_stream().eventsource();
            // Tool calls accumulate by their streamed `index`.
            let mut tools: BTreeMap<u32, ToolAcc> = BTreeMap::new();
            let mut content_buf = String::new();
            let mut usage: Option<Value> = None;
            let mut response_id: Option<String> = None;
            let mut response_model: Option<String> = None;
            let mut done = false;
            let mut saw_finish = false;
            let mut had_error = false;

            while let Some(item) = stream.next().await {
                let ev = match item {
                    Ok(ev) => ev,
                    Err(e) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!("Chat Completions SSE transport: {e}")))
                            .await;
                        return;
                    }
                };
                if ev.data == "[DONE]" {
                    done = true;
                    break;
                }
                let chunk: Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx
                            .send(Err(anyhow::anyhow!(
                                "parse Chat Completions SSE: {e}: {}",
                                ev.data
                            )))
                            .await;
                        continue;
                    }
                };
                if let Some(err) = chunk.get("error") {
                    had_error = true;
                    let msg = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Chat Completions stream error")
                        .to_string();
                    let _ = tx
                        .send(Ok(ResponseStreamEvent::Error { message: msg }))
                        .await;
                    break;
                }
                if response_id.is_none() {
                    response_id = chunk.get("id").and_then(|v| v.as_str()).map(String::from);
                }
                if response_model.is_none() {
                    response_model = chunk
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if let Some(u) = chunk.get("usage") {
                    if u.is_object() {
                        usage = Some(normalize_usage(u));
                    }
                }
                let Some(choice) = chunk
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                else {
                    continue;
                };
                if choice
                    .get("finish_reason")
                    .map(|v| !v.is_null())
                    .unwrap_or(false)
                {
                    saw_finish = true;
                }
                let message_part = choice.get("delta").or_else(|| choice.get("message"));
                if let Some(text) = message_part
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                {
                    if !text.is_empty() {
                        content_buf.push_str(text);
                        let _ = tx
                            .send(Ok(ResponseStreamEvent::OutputTextDelta {
                                delta: text.to_string(),
                                item_id: None,
                            }))
                            .await;
                    }
                }
                if let Some(calls) = message_part.and_then(|d| d.get("tool_calls")) {
                    for tc in tool_call_values(calls) {
                        let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let function = tc.get("function");
                        apply_tool_delta(
                            &mut tools,
                            index,
                            chat_tool_call_id(tc),
                            chat_tool_name(function, tc),
                            chat_argument_value(function, tc),
                            &tx,
                        )
                        .await;
                    }
                }
                if let Some(function_call) = message_part.and_then(|d| d.get("function_call")) {
                    apply_tool_delta(
                        &mut tools,
                        0,
                        None,
                        chat_tool_name(Some(function_call), function_call),
                        chat_argument_value(Some(function_call), function_call),
                        &tx,
                    )
                    .await;
                }
            }

            if had_error {
                return;
            }
            if !done && !saw_finish {
                let _ = tx
                    .send(Err(anyhow::anyhow!(
                        "Chat Completions stream ended before completion"
                    )))
                    .await;
                return;
            }
            // Finalize: close each tool call, then the text, then complete.
            for acc in tools.into_values() {
                if !acc.added {
                    let item = json!({
                        "type": "function_call",
                        "id": acc.id,
                        "call_id": acc.id,
                        "name": acc.name.unwrap_or_default(),
                        "arguments": "",
                    });
                    let _ = tx
                        .send(Ok(ResponseStreamEvent::OutputItemAdded {
                            item,
                            output_index: 0,
                        }))
                        .await;
                }
                let _ = tx
                    .send(Ok(ResponseStreamEvent::FunctionCallArgsDone {
                        item_id: acc.id,
                        arguments: acc.args,
                    }))
                    .await;
            }
            if !content_buf.is_empty() {
                let _ = tx
                    .send(Ok(ResponseStreamEvent::OutputTextDone {
                        text: content_buf,
                        item_id: None,
                    }))
                    .await;
            }
            let response = json!({
                "id": response_id,
                "model": response_model,
                "usage": usage,
            });
            let _ = tx
                .send(Ok(ResponseStreamEvent::Completed { response }))
                .await;
        });
        if let Err(e) = pump.await {
            if e.is_panic() {
                let _ = tx_supervisor
                    .send(Err(anyhow::anyhow!(
                        "Chat Completions SSE pump panicked: {e}"
                    )))
                    .await;
            }
        }
    });
    StreamHandle { rx }
}

/// HTTP client for an OpenAI-compatible Chat Completions provider configured in
/// `config.providers`. Implements the shared [`ProviderClient`] so the agent
/// loop drives it identically to the built-in providers.
pub struct ChatCompletionsClient {
    http: reqwest::Client,
    provider_id: String,
    base_url: String,
    api_key: String,
}

impl ChatCompletionsClient {
    pub fn new(provider_id: String, base_url: String, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            http,
            provider_id,
            base_url,
            api_key,
        })
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Strip the `<id>/` routing prefix so the upstream sees its native model id.
    fn wire_model(&self, model: &str) -> String {
        let prefix = format!("{}/", self.provider_id);
        model.strip_prefix(&prefix).unwrap_or(model).to_string()
    }

    async fn send(&self, mut req: ResponsesRequest, stream: bool) -> Result<reqwest::Response> {
        req.model = self.wire_model(&req.model);
        req.stream = stream;
        let body = translate_chat_request(&req);
        let mut builder = self
            .http
            .post(self.endpoint())
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if !self.api_key.is_empty() {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", self.api_key));
        }
        Ok(crate::retry::send_with_retry(builder).await?)
    }
}

#[async_trait]
impl ProviderClient for ChatCompletionsClient {
    fn provider(&self) -> Provider {
        // Reported as OpenAI since the wire protocol is OpenAI-compatible; the
        // value is informational only (nothing routes on it).
        Provider::OpenAi
    }

    async fn stream(&self, req: ResponsesRequest) -> Result<StreamHandle> {
        let resp = self.send(req, true).await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("{} {} {}", self.provider_id, status, text));
        }
        Ok(handle_chat_response(resp))
    }

    async fn create(&self, req: ResponsesRequest) -> Result<serde_json::Value> {
        let resp = self.send(req, false).await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow!("{} {} {}", self.provider_id, status, text));
        }
        serde_json::from_str(&text).map_err(|e| anyhow!("parse Chat Completions response: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::models::{MessageContent, ToolFunctionDef};
    use axum::{
        response::sse::{Event, Sse},
        routing::get,
        Router,
    };
    use futures_util::stream;
    use std::{convert::Infallible, time::Duration};
    use tokio::net::TcpListener;

    #[test]
    fn chat_tool_helpers_accept_camel_case_aliases() {
        let tool_call = json!({
            "callId": "call_123",
            "toolName": "Read",
            "toolInput": {"filePath": "Cargo.toml"}
        });

        assert_eq!(chat_tool_call_id(&tool_call).as_deref(), Some("call_123"));
        assert_eq!(chat_tool_name(None, &tool_call).as_deref(), Some("Read"));
        assert_eq!(
            chat_argument_value(None, &tool_call).as_deref(),
            Some(r#"{"filePath":"Cargo.toml"}"#)
        );
    }

    #[test]
    fn chat_tool_id_accepts_anthropic_tool_use_alias() {
        let tool_call = json!({
            "tool_use_id": "toolu_123",
            "function": {"name": "read_file", "arguments": {"path": "Cargo.toml"}}
        });

        assert_eq!(chat_tool_call_id(&tool_call).as_deref(), Some("toolu_123"));
    }

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

    #[tokio::test]
    async fn stream_maps_content_tool_calls_and_usage() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","model":"m","choices":[{"index":0,"delta":{"content":"Hi"}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"x\"}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut text = String::new();
        let mut added_name = None;
        let mut args = String::new();
        let mut completed_usage = None;
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            match ev.unwrap() {
                ResponseStreamEvent::OutputTextDelta { delta, .. } => text.push_str(&delta),
                ResponseStreamEvent::OutputItemAdded { item, .. } => {
                    added_name = item.get("name").and_then(|v| v.as_str()).map(String::from);
                }
                ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
                ResponseStreamEvent::Completed { response } => {
                    completed_usage = response.get("usage").cloned();
                }
                _ => {}
            }
        }
        server.abort();

        assert_eq!(text, "Hi");
        assert_eq!(added_name.as_deref(), Some("read_file"));
        assert_eq!(args, "{\"path\":\"x\"}");
        let usage = completed_usage.unwrap();
        assert_eq!(usage["input_tokens"], 10);
        assert_eq!(usage["output_tokens"], 5);
        assert_eq!(usage["total_tokens"], 15);
    }

    #[tokio::test]
    async fn chat_tool_argument_buffer_is_capped_before_agent() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut tools = BTreeMap::new();

        apply_tool_delta(
            &mut tools,
            0,
            Some("call_x".to_string()),
            Some("read_file".to_string()),
            Some("x".repeat(CHAT_TOOL_ARGUMENT_BUFFER_BYTES + 8192)),
            &tx,
        )
        .await;

        let acc = tools.get(&0).unwrap();
        assert_eq!(acc.args.len(), CHAT_TOOL_ARGUMENT_BUFFER_BYTES);
        assert!(acc.args_truncated);

        let _started = rx.recv().await.unwrap().unwrap();
        match rx.recv().await.unwrap().unwrap() {
            ResponseStreamEvent::FunctionCallArgsDelta { delta, .. } => {
                assert_eq!(delta.len(), CHAT_TOOL_ARGUMENT_BUFFER_BYTES);
            }
            other => panic!("expected FunctionCallArgsDelta, got {other:?}"),
        }
    }

    #[test]
    fn chat_tool_argument_cap_preserves_utf8_boundary() {
        let mut acc = ToolAcc {
            id: "call_x".into(),
            name: Some("read_file".into()),
            args: "x".repeat(CHAT_TOOL_ARGUMENT_BUFFER_BYTES - 1),
            args_truncated: false,
            added: true,
            emitted_args_len: 0,
        };

        append_tool_args_capped(&mut acc, "é");

        assert_eq!(acc.args.len(), CHAT_TOOL_ARGUMENT_BUFFER_BYTES);
        assert!(acc.args.is_char_boundary(acc.args.len()));
        assert!(acc.args_truncated);
    }

    #[tokio::test]
    async fn stream_waits_for_tool_name_split_across_deltas() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"arguments":""}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"x\"}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut added_name = None;
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            match ev.unwrap() {
                ResponseStreamEvent::OutputItemAdded { item, .. } => {
                    added_name = item.get("name").and_then(|v| v.as_str()).map(String::from);
                }
                ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
                _ => {}
            }
        }
        server.abort();

        assert_eq!(added_name.as_deref(), Some("read_file"));
        assert_eq!(args, "{\"path\":\"x\"}");
    }

    #[tokio::test]
    async fn stream_maps_legacy_function_call_deltas() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"function_call":{"name":"read_file","arguments":""}}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"function_call":{"arguments":"{\"path\":\"x\"}"}}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"function_call"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut added_name = None;
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            match ev.unwrap() {
                ResponseStreamEvent::OutputItemAdded { item, .. } => {
                    added_name = item.get("name").and_then(|v| v.as_str()).map(String::from);
                }
                ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
                _ => {}
            }
        }
        server.abort();

        assert_eq!(added_name.as_deref(), Some("read_file"));
        assert_eq!(args, "{\"path\":\"x\"}");
    }

    #[tokio::test]
    async fn stream_accepts_tool_call_object_and_object_arguments() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":{"path":"x"}}}}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            if let ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } = ev.unwrap() {
                args = arguments;
            }
        }
        server.abort();

        assert_eq!(args, r#"{"path":"x"}"#);
    }

    #[tokio::test]
    async fn stream_accepts_parameters_as_tool_arguments() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","parameters":{"path":"Cargo.toml"}}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            if let ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } = ev.unwrap() {
                args = arguments;
            }
        }
        server.abort();

        assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
    }

    #[tokio::test]
    async fn stream_accepts_message_shape_chunks() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"message":{"content":"Hi","tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":{"path":"Cargo.toml"}}}]},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut text = String::new();
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            match ev.unwrap() {
                ResponseStreamEvent::OutputTextDelta { delta, .. } => text.push_str(&delta),
                ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
                _ => {}
            }
        }
        server.abort();

        assert_eq!(text, "Hi");
        assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
    }

    #[tokio::test]
    async fn stream_accepts_provider_tool_name_and_partial_json_aliases() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"recipient_name":"functions.Read","partialJson":"{\"path\":\"Cargo.toml\"}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut name = String::new();
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            match ev.unwrap() {
                ResponseStreamEvent::OutputItemAdded { item, .. } => {
                    name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                }
                ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } => args = arguments,
                _ => {}
            }
        }
        server.abort();

        assert_eq!(name, "functions.Read");
        assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
    }

    #[tokio::test]
    async fn stream_ignores_empty_arg_placeholder_before_real_tool_arguments() {
        let app = Router::new().route(
            "/",
            get(|| async {
                let chunks = vec![
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","type":"function","function":{"name":"read_file","arguments":"{}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"Cargo.toml\"}"}}]}}]}"#,
                    r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
                ];
                let events: Vec<Result<Event, Infallible>> = chunks
                    .into_iter()
                    .map(|c| Ok(Event::default().data(c)))
                    .chain(std::iter::once(Ok(Event::default().data("[DONE]"))))
                    .collect();
                Sse::new(stream::iter(events))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut handle = handle_chat_response(resp);
        let mut args = String::new();
        while let Some(ev) = tokio::time::timeout(Duration::from_secs(1), handle.rx.recv())
            .await
            .unwrap()
        {
            if let ResponseStreamEvent::FunctionCallArgsDone { arguments, .. } = ev.unwrap() {
                args = arguments;
            }
        }
        server.abort();

        assert_eq!(args, r#"{"path":"Cargo.toml"}"#);
    }

    #[test]
    fn client_endpoint_trims_slash_and_strips_model_prefix() {
        let c = ChatCompletionsClient::new(
            "groq".into(),
            "https://api.groq.com/openai/v1/".into(),
            "k".into(),
        )
        .unwrap();
        assert_eq!(
            c.endpoint(),
            "https://api.groq.com/openai/v1/chat/completions"
        );
        assert_eq!(c.wire_model("groq/llama-3.3-70b"), "llama-3.3-70b");
        // A bare id (no provider prefix) is left untouched.
        assert_eq!(c.wire_model("llama-3.3-70b"), "llama-3.3-70b");
    }
}

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
            InputItem::FunctionCallOutput { call_id, output } => {
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
    args: String,
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
                    let _ = tx.send(Ok(ResponseStreamEvent::Error { message: msg })).await;
                    break;
                }
                if response_id.is_none() {
                    response_id = chunk.get("id").and_then(|v| v.as_str()).map(String::from);
                }
                if response_model.is_none() {
                    response_model = chunk.get("model").and_then(|v| v.as_str()).map(String::from);
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
                let delta = choice.get("delta");
                if let Some(text) = delta.and_then(|d| d.get("content")).and_then(|v| v.as_str()) {
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
                if let Some(calls) = delta.and_then(|d| d.get("tool_calls")).and_then(|v| v.as_array())
                {
                    for tc in calls {
                        let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str());
                        let id = tc.get("id").and_then(|v| v.as_str());
                        // Register the call the first time we see its index,
                        // synthesizing an id if the provider streamed only an index.
                        let mut added: Option<Value> = None;
                        if let std::collections::btree_map::Entry::Vacant(slot) =
                            tools.entry(index)
                        {
                            let call_id = id
                                .map(String::from)
                                .unwrap_or_else(|| format!("call_{index}"));
                            added = Some(json!({
                                "type": "function_call",
                                "id": call_id,
                                "call_id": call_id,
                                "name": name.unwrap_or(""),
                                "arguments": "",
                            }));
                            slot.insert(ToolAcc {
                                id: call_id,
                                args: String::new(),
                            });
                        }
                        if let Some(item) = added {
                            let _ = tx
                                .send(Ok(ResponseStreamEvent::OutputItemAdded {
                                    item,
                                    output_index: index,
                                }))
                                .await;
                        }
                        if let Some(frag) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                        {
                            if !frag.is_empty() {
                                if let Some(acc) = tools.get_mut(&index) {
                                    acc.args.push_str(frag);
                                    let _ = tx
                                        .send(Ok(ResponseStreamEvent::FunctionCallArgsDelta {
                                            item_id: acc.id.clone(),
                                            delta: frag.to_string(),
                                        }))
                                        .await;
                                }
                            }
                        }
                    }
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
                    .send(Err(anyhow::anyhow!("Chat Completions SSE pump panicked: {e}")))
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
        Ok(builder.send().await?)
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

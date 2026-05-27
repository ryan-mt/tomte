use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

/// Abort the recv loop if the upstream SSE stream falls silent for this long.
/// Catches network hangs where the server stops emitting events without
/// closing the channel — previously this left the UI stuck on "Reasoning…"
/// forever with no way to recover.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

use crate::config::Config;
use crate::openai::{
    InputItem, MessageContent, OpenAiClient, ResponseStreamEvent, ResponsesRequest,
};
use crate::tools::{ApprovalMode, Registry, SessionState, ToolContext};

/// Streaming event surfaced to the UI/CLI layer.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind")]
pub enum AgentEvent {
    AssistantTextDelta { text: String },
    AssistantTextDone { text: String },
    ReasoningDelta { text: String },
    ReasoningDone { text: String },
    ToolCallStarted { name: String, call_id: String },
    ToolCallArgsDelta { call_id: String, delta: String },
    ToolCallArgsDone { call_id: String, arguments: String },
    ToolResult { call_id: String, output: String, error: bool },
    Usage { input_tokens: u64, output_tokens: u64, total_tokens: u64 },
    TurnComplete,
    Error { message: String },
}

pub struct Agent {
    pub client: OpenAiClient,
    pub registry: Registry,
    pub config: Config,
    pub cwd: std::path::PathBuf,
    pub approval: ApprovalMode,
    pub history: Vec<InputItem>,
    pub system_prompt: String,
    /// Mutable per-session state shared across tool calls (todo list, etc.).
    pub session: Arc<Mutex<SessionState>>,
    /// Lifecycle hooks loaded from `~/.config/opencli/settings.json`. Pre-tool
    /// hooks can block a tool call by exiting with code 2; the model receives
    /// the hook's stdout as the block reason.
    pub hooks: Arc<crate::hooks::HookSet>,
}

impl Agent {
    pub fn new(client: OpenAiClient, config: Config) -> Self {
        Self {
            client,
            registry: Registry::standard(),
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            approval: ApprovalMode::OnRequest,
            history: Vec::new(),
            config,
            system_prompt: default_system_prompt(),
            session: Arc::new(Mutex::new(SessionState::default())),
            hooks: Arc::new(crate::hooks::load()),
        }
    }

    /// Spawn the MCP servers listed in `settings.json` and register every
    /// discovered tool into this agent's `Registry` under `mcp__<server>__<tool>`.
    /// Best-effort: a misconfigured server logs a warning but does not abort.
    pub async fn load_mcp(&mut self) -> Result<()> {
        let clients = crate::mcp::spawn_all().await;
        for client in clients {
            for info in client.tools.clone() {
                let adapter = crate::mcp::McpToolAdapter::new(client.clone(), info);
                self.registry.add(Box::new(adapter));
            }
        }
        Ok(())
    }

    pub fn push_user_message(&mut self, text: impl Into<String>) {
        self.history.push(InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(text)],
        });
    }

    /// Push a user message with text + image attachments (paths read from disk).
    pub fn push_user_message_with_images(
        &mut self,
        text: String,
        image_paths: &[std::path::PathBuf],
    ) {
        let mut content = vec![MessageContent::text(text)];
        for path in image_paths {
            if let Ok(bytes) = std::fs::read(path) {
                use base64::Engine;
                let mime = guess_mime(path);
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                content.push(MessageContent::InputImage {
                    image_url: format!("data:{};base64,{}", mime, b64),
                    detail: None,
                });
            }
        }
        self.history.push(InputItem::Message {
            role: "user".to_string(),
            content,
        });
    }

    /// Drive one full turn: send the current history, process tool calls until
    /// the model produces final assistant text. Emits events through `tx`.
    pub async fn run_turn(&mut self, tx: mpsc::Sender<AgentEvent>) -> Result<()> {
        loop {
            let request = ResponsesRequest::new(self.config.model.clone(), self.history.clone())
                .with_instructions(self.system_prompt.clone())
                .with_tools(self.registry.definitions())
                .with_reasoning(self.config.reasoning_effort.clone())
                .with_verbosity(self.config.verbosity.clone());
            let mut handle = self.client.stream(request).await?;

            let mut pending_calls: Vec<PendingCall> = Vec::new();
            let mut final_text = String::new();
            let mut reasoning_text = String::new();

            loop {
                let recv = tokio::time::timeout(STREAM_IDLE_TIMEOUT, handle.rx.recv()).await;
                let ev = match recv {
                    Err(_) => {
                        // No event for STREAM_IDLE_TIMEOUT — the upstream
                        // stream is stuck. Surface as an error and bail so
                        // the UI unsticks and the user can retry.
                        let msg = format!(
                            "stream idle for {}s — connection may be stale, try again",
                            STREAM_IDLE_TIMEOUT.as_secs()
                        );
                        let _ = tx
                            .send(AgentEvent::Error { message: msg.clone() })
                            .await;
                        return Err(anyhow::anyhow!(msg));
                    }
                    Ok(None) => break,
                    Ok(Some(Err(e))) => {
                        let _ = tx
                            .send(AgentEvent::Error {
                                message: e.to_string(),
                            })
                            .await;
                        return Err(e);
                    }
                    Ok(Some(Ok(v))) => v,
                };
                match ev {
                    ResponseStreamEvent::OutputItemAdded { item, .. } => {
                        if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                            let call_id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let item_id = item
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Some models send the complete arguments inline on the
                            // OutputItemAdded item; capture them as an initial buffer.
                            let args_buf = item
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            pending_calls.push(PendingCall {
                                call_id: call_id.clone(),
                                item_id,
                                name: name.clone(),
                                args_buf,
                            });
                            let _ = tx
                                .send(AgentEvent::ToolCallStarted { name, call_id })
                                .await;
                        }
                    }
                    ResponseStreamEvent::OutputItemDone { item, .. } => {
                        if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                            let call_id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let item_id = item
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = item
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if let Some(pc) = pending_calls
                                .iter_mut()
                                .find(|p| p.call_id == call_id || p.item_id == item_id)
                            {
                                if !arguments.is_empty() {
                                    pc.args_buf = arguments.clone();
                                }
                            }
                        }
                    }
                    ResponseStreamEvent::OutputTextDelta { delta, .. } => {
                        final_text.push_str(&delta);
                        let _ = tx
                            .send(AgentEvent::AssistantTextDelta { text: delta })
                            .await;
                    }
                    ResponseStreamEvent::OutputTextDone { text, .. } => {
                        let _ = tx
                            .send(AgentEvent::AssistantTextDone { text: text.clone() })
                            .await;
                        final_text = text;
                    }
                    ResponseStreamEvent::FunctionCallArgsDelta { item_id, delta } => {
                        // Stream delta event references the output-item id, which
                        // may differ from the function call_id. Match by either.
                        let call_id = pending_calls
                            .iter_mut()
                            .find(|p| p.item_id == item_id || p.call_id == item_id)
                            .map(|pc| {
                                pc.args_buf.push_str(&delta);
                                pc.call_id.clone()
                            })
                            .unwrap_or_else(|| item_id.clone());
                        let _ = tx
                            .send(AgentEvent::ToolCallArgsDelta { call_id, delta })
                            .await;
                    }
                    ResponseStreamEvent::FunctionCallArgsDone { item_id, arguments } => {
                        let call_id = pending_calls
                            .iter_mut()
                            .find(|p| p.item_id == item_id || p.call_id == item_id)
                            .map(|pc| {
                                pc.args_buf = arguments.clone();
                                pc.call_id.clone()
                            })
                            .unwrap_or_else(|| item_id.clone());
                        let _ = tx
                            .send(AgentEvent::ToolCallArgsDone {
                                call_id,
                                arguments,
                            })
                            .await;
                    }
                    ResponseStreamEvent::ReasoningDelta { delta } => {
                        reasoning_text.push_str(&delta);
                        let _ = tx.send(AgentEvent::ReasoningDelta { text: delta }).await;
                    }
                    ResponseStreamEvent::ReasoningDone { text } => {
                        let _ = tx
                            .send(AgentEvent::ReasoningDone { text: text.clone() })
                            .await;
                        reasoning_text = text;
                    }
                    ResponseStreamEvent::Completed { response } => {
                        emit_usage(&response, &tx).await;
                        break;
                    }
                    ResponseStreamEvent::Failed { response } => {
                        // Previously handled identically to Completed, which
                        // masked content-filter / quota / 5xx errors as a
                        // successful empty turn. Surface them instead.
                        emit_usage(&response, &tx).await;
                        let message = response
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("response.failed (no message)")
                            .to_string();
                        let _ = tx
                            .send(AgentEvent::Error { message: message.clone() })
                            .await;
                        return Err(anyhow::anyhow!("response.failed: {message}"));
                    }
                    ResponseStreamEvent::Error { message } => {
                        let _ = tx.send(AgentEvent::Error { message: message.clone() }).await;
                        return Err(anyhow::anyhow!(message));
                    }
                    crate::openai::stream::ResponseStreamEvent::Other { kind } => {
                        tracing::debug!(event = %kind, "unknown SSE event");
                    }
                    _ => {}
                }
            }

            // Append any function calls + their outputs to history, then loop again.
            if pending_calls.is_empty() {
                if !final_text.is_empty() {
                    self.history.push(InputItem::Message {
                        role: "assistant".to_string(),
                        content: vec![MessageContent::OutputText { text: final_text }],
                    });
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                return Ok(());
            }

            let ctx = ToolContext {
                cwd: self.cwd.clone(),
                approval: self.approval,
                session: self.session.clone(),
            };

            // Push every function_call to history first so the request that
            // carries the outputs back to the model is well-formed regardless
            // of completion order.
            for pc in &pending_calls {
                self.history.push(InputItem::FunctionCall {
                    call_id: pc.call_id.clone(),
                    name: pc.name.clone(),
                    arguments: pc.args_buf.clone(),
                });
            }

            // Split into runnable tasks vs pre-computed errors (malformed JSON,
            // unknown tool name) so the executable set can be driven in parallel
            // and the error set can be surfaced as tool errors without blocking
            // the rest.
            let mut runnable: Vec<(String, Value, &dyn crate::tools::BuiltinTool)> = Vec::new();
            let mut precomputed: Vec<(String, String, bool)> = Vec::new();
            for pc in &pending_calls {
                let parsed: std::result::Result<Value, _> = if pc.args_buf.trim().is_empty() {
                    Ok(Value::Object(Default::default()))
                } else {
                    serde_json::from_str(&pc.args_buf)
                };
                match parsed {
                    Err(e) => precomputed.push((
                        pc.call_id.clone(),
                        format!("Error: tool args are not valid JSON: {e}"),
                        true,
                    )),
                    Ok(args) => match self.registry.find(&pc.name) {
                        Some(t) => {
                            // Plan mode is read-only: any tool that mutates
                            // state (`write_file`, `edit_file`, `run_shell`,
                            // `todo_write`, …) is rejected up-front so the
                            // model can adjust its plan instead of attempting
                            // the call and producing a confusing failure.
                            if self.approval == ApprovalMode::Plan && !t.is_read_only() {
                                precomputed.push((
                                    pc.call_id.clone(),
                                    format!(
                                        "Error: tool `{}` is blocked in plan mode (read-only). Switch out of plan mode to execute writes/shell.",
                                        pc.name
                                    ),
                                    true,
                                ));
                            } else {
                                // PreToolUse hooks (from settings.json) can
                                // block a tool call by exiting 2. We surface
                                // the hook's stdout as the error so the model
                                // sees a useful reason.
                                match self.hooks.fire_pre(&pc.name, &args).await {
                                    crate::hooks::HookDecision::Block(reason) => {
                                        precomputed.push((
                                            pc.call_id.clone(),
                                            format!("Error: blocked by PreToolUse hook: {reason}"),
                                            true,
                                        ));
                                    }
                                    crate::hooks::HookDecision::Allow => {
                                        runnable.push((pc.call_id.clone(), args, t));
                                    }
                                }
                            }
                        }
                        None => precomputed.push((
                            pc.call_id.clone(),
                            format!("Error: unknown tool: {}", pc.name),
                            true,
                        )),
                    },
                }
            }

            // Execute tools concurrently. The model emits parallel tool calls
            // when they are independent (eg. reading several files); running
            // them sequentially here would force them to serialize and
            // dominate the turn latency. `join_all` polls the futures on the
            // current task — true concurrency for I/O-bound tools.
            let futures = runnable.into_iter().map(|(call_id, args, tool)| {
                let ctx = ctx.clone();
                let tx = tx.clone();
                async move {
                    let res = tool.execute(args, &ctx).await;
                    let (output, is_err) = match res {
                        Ok(s) => (s, false),
                        Err(e) => (format!("Error: {e}"), true),
                    };
                    let _ = tx
                        .send(AgentEvent::ToolResult {
                            call_id: call_id.clone(),
                            output: output.clone(),
                            error: is_err,
                        })
                        .await;
                    (call_id, output, is_err)
                }
            });
            let mut results: Vec<(String, String, bool)> =
                futures::future::join_all(futures).await;

            // Surface precomputed errors via the same event stream so the UI
            // and model see them identically to runtime errors.
            for (call_id, output, _) in &precomputed {
                let _ = tx
                    .send(AgentEvent::ToolResult {
                        call_id: call_id.clone(),
                        output: output.clone(),
                        error: true,
                    })
                    .await;
            }
            results.extend(precomputed);

            // Append outputs to history in the original call order so the
            // model sees a deterministic transcript even when completion
            // order shuffled.
            let mut by_id: std::collections::HashMap<String, String> =
                results.into_iter().map(|(id, out, _)| (id, out)).collect();
            for pc in &pending_calls {
                if let Some(output) = by_id.remove(&pc.call_id) {
                    self.history.push(InputItem::FunctionCallOutput {
                        call_id: pc.call_id.clone(),
                        output,
                    });
                }
            }
            // continue loop to send tool outputs back
        }
    }
}

struct PendingCall {
    call_id: String,
    item_id: String,
    name: String,
    args_buf: String,
}

async fn emit_usage(response: &Value, tx: &mpsc::Sender<AgentEvent>) {
    if let Some(usage) = response.get("usage") {
        let i = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let o = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let t = usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(i + o);
        let _ = tx
            .send(AgentEvent::Usage {
                input_tokens: i,
                output_tokens: o,
                total_tokens: t,
            })
            .await;
    }
}

fn guess_mime(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn default_system_prompt() -> String {
    r#"You are opencli, a CLI coding agent. You operate inside the user's repository on their machine. You have direct tools for reading, searching, editing, and running code — use them. Do not describe what you would do; do it.

# Tool philosophy
- Act through tools, then summarize. Never speculate about file contents — read them.
- Make tool calls in parallel whenever the calls are independent. Reading three files, or grepping for two separate patterns, should be issued as a single batch so they run concurrently.
- Prefer the narrowest tool that answers the question: `grep` for "where is X used", `glob` for "which files match", `read_file` for "what does this file say", `list_dir` only when you need a directory snapshot.
- Always read a file before editing it. The edit tool requires the exact existing text; guessing the contents wastes a turn.

# Editing code
- `edit_file` for surgical changes inside an existing file. Provide enough surrounding context in `old_string` to make it unique.
- `write_file` only when creating a new file or completely replacing one. Never use it as a substitute for `edit_file` on an existing file — you will silently lose unrelated content.
- Match the existing style of the file (indentation, naming, error handling) even if you would personally write it differently.
- Do not add comments unless the user asked. Do not add documentation, examples, or "improvements" the user did not request.
- Touch only what the task requires. If you notice unrelated issues, mention them in your reply rather than fixing them silently.

# Running commands
- Use `run_shell` for builds, tests, formatters, version checks, and any one-shot command the user implies. The default timeout is 120 seconds; raise `timeout_ms` for slow builds.
- The shell sandbox strips secret-like environment variables (anything matching TOKEN/SECRET/KEY/etc.) from the child process. Do not rely on those being present.
- Never run destructive commands (`rm -rf`, force-push, dropping tables, etc.) unless the user explicitly asked.

# Communicating with the user
- Be concise. Prefer terse, direct answers over walls of explanation.
- Cite specific locations as `path:line` so the user can jump to them.
- When you finish, give a one or two sentence summary of what changed and what remains. Do not restate the diff.

# When you are unsure
- If a request is ambiguous, ask one focused question rather than guessing.
- If a simpler approach exists than the one the user proposed, say so before implementing.
"#
    .to_string()
}

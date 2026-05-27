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

/// Per-tool execution hard cap. A misbehaving tool (e.g. an MCP server that
/// stops responding) used to be able to wedge the whole turn indefinitely.
/// `run_shell` already enforces its own foreground timeout internally; this
/// outer cap is generous enough to clear it (default 120s) plus margin.
const TOOL_HARD_TIMEOUT: Duration = Duration::from_secs(180);

use crate::config::Config;
use crate::openai::{
    InputItem, MessageContent, OpenAiClient, ResponseStreamEvent, ResponsesRequest,
};
use crate::tools::{ApprovalMode, Registry, SessionState, TodoItem, ToolContext};

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
    /// Snapshot of the session todo list. Emitted after every tool batch so
    /// the UI can render `/todos` and the status line without locking the
    /// agent mutex itself. Replaces the entire client-side cache each time.
    TodosSnapshot { todos: Vec<TodoItem> },
    Usage { input_tokens: u64, output_tokens: u64, total_tokens: u64 },
    TurnComplete,
    Error { message: String },
    ContextWarning { used: u64, limit: u64 },
    ApprovalRequest { call_id: String, tool_name: String, args_json: String, diff_preview: Option<String> },
    ApprovalGranted { call_id: String },
    ApprovalDenied { call_id: String },
}

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

pub fn model_context_limit(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("nano") { 200_000 }
    else if m.contains("mini") { 400_000 }
    else { 1_000_000 }
}

pub struct Agent {
    pub client: OpenAiClient,
    pub registry: Registry,
    pub config: Config,
    pub cwd: std::path::PathBuf,
    pub approval: ApprovalMode,
    pub history: Vec<InputItem>,
    pub system_prompt: String,
    /// Unique id for the current chat session. Stable across the lifetime of
    /// an `Agent` so persisted records can be updated in place rather than
    /// duplicated on every turn.
    pub session_id: String,
    /// Wall-clock epoch (ms) when this session was first opened. Carried
    /// through restores so a resumed session keeps its original birthtime.
    pub session_created_ms: u64,
    /// Mutable per-session state shared across tool calls (todo list, etc.).
    pub session: Arc<Mutex<SessionState>>,
    /// Lifecycle hooks loaded from `~/.config/opencli/settings.json`. Pre-tool
    /// hooks can block a tool call by exiting with code 2; the model receives
    /// the hook's stdout as the block reason.
    pub hooks: Arc<crate::hooks::HookSet>,
    pub pending_approvals: Arc<Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    pub require_approval: bool,
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
            session_id: crate::session::new_session_id(),
            session_created_ms: crate::session::now_ms(),
            pending_approvals: Arc::new(Mutex::new(std::collections::HashMap::new())),
            require_approval: false,
        }
    }

    pub async fn respond_approval(&self, call_id: &str, granted: bool) {
        let sender = { let mut map = self.pending_approvals.lock().await; map.remove(call_id) };
        if let Some(s) = sender { let _ = s.send(granted); }
    }

    /// Replace this agent's history and identity from a stored session so
    /// `/resume` can pick up exactly where the previous run left off.
    pub fn restore_from(&mut self, record: crate::session::SessionRecord) {
        self.history = record.history;
        self.session_id = record.meta.id;
        self.session_created_ms = record.meta.created_at_ms;
    }

    /// Append the project's `CLAUDE.md` (and the global one at
    /// `~/.config/opencli/CLAUDE.md`) to the system prompt so the model
    /// always sees the user's project notes and personal conventions.
    /// Idempotent — call after `cwd` is set; reading is best-effort and
    /// silently no-ops if either file is missing.
    pub fn apply_project_memory(&mut self) {
        let cfg_dir = crate::config::config_dir();
        let globals = [cfg_dir.join("CLAUDE.md"), cfg_dir.join("AGENTS.md")];
        let projects = [self.cwd.join("CLAUDE.md"), self.cwd.join("AGENTS.md")];
        let read_first = |paths: &[std::path::PathBuf]| -> Option<(std::path::PathBuf, String)> {
            for p in paths {
                if let Ok(text) = std::fs::read_to_string(p) {
                    let t = text.trim();
                    if !t.is_empty() { return Some((p.clone(), t.to_string())); }
                }
            }
            None
        };
        let mut additions = String::new();
        if let Some((path, text)) = read_first(&globals) {
            additions.push_str(&format!("

# Global memory ({})

", path.display()));
            additions.push_str(&text);
        }
        if let Some((path, text)) = read_first(&projects) {
            additions.push_str(&format!("

# Project memory ({})

", path.display()));
            additions.push_str(&text);
        }
        if !additions.is_empty() {
            self.system_prompt.push_str(&additions);
        }
    }

    /// Build a `SessionRecord` snapshot of the current conversation. Cheap
    /// enough to call after every turn — the history is just `InputItem`s.
    pub fn to_session_record(&self) -> crate::session::SessionRecord {
        crate::session::SessionRecord {
            meta: crate::session::SessionMeta {
                id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                model: self.config.model.clone(),
                created_at_ms: self.session_created_ms,
                updated_at_ms: crate::session::now_ms(),
                message_count: self.history.len(),
                preview: crate::session::derive_preview(&self.history),
            },
            history: self.history.clone(),
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
                        emit_usage(&response, &tx, &self.config.model).await;
                        break;
                    }
                    ResponseStreamEvent::Failed { response } => {
                        // Previously handled identically to Completed, which
                        // masked content-filter / quota / 5xx errors as a
                        // successful empty turn. Surface them instead.
                        emit_usage(&response, &tx, &self.config.model).await;
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

            // History pushes deferred until after outputs computed (cancel-safety).

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
                    Err(e) => {
                        // Surface enough detail that the model can self-correct
                        // on the next turn: which tool, which byte offset, what
                        // the parser actually saw at the failure point.
                        let preview = if pc.args_buf.len() > 200 {
                            format!("{}…", &pc.args_buf[..200])
                        } else {
                            pc.args_buf.clone()
                        };
                        tracing::warn!(
                            tool = %pc.name,
                            call_id = %pc.call_id,
                            error = %e,
                            "tool.args.invalid_json"
                        );
                        precomputed.push((
                            pc.call_id.clone(),
                            format!(
                                "Error: tool `{}` arguments are not valid JSON ({e}). Received: {preview}",
                                pc.name
                            ),
                            true,
                        ));
                    }
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
                                        let needs_gate = self.require_approval
                                            && matches!(self.approval, ApprovalMode::OnRequest | ApprovalMode::Manual)
                                            && !t.is_read_only();
                                        if needs_gate {
                                            let diff_preview = t.compute_preview(&args, &ctx).await;
                                            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<bool>();
                                            self.pending_approvals.lock().await.insert(pc.call_id.clone(), resp_tx);
                                            let _ = tx.send(AgentEvent::ApprovalRequest {
                                                call_id: pc.call_id.clone(),
                                                tool_name: pc.name.clone(),
                                                args_json: pc.args_buf.clone(),
                                                diff_preview,
                                            }).await;
                                            let granted = match tokio::time::timeout(APPROVAL_TIMEOUT, resp_rx).await {
                                                Ok(Ok(g)) => g,
                                                _ => { self.pending_approvals.lock().await.remove(&pc.call_id); false }
                                            };
                                            let _ = tx.send(if granted {
                                                AgentEvent::ApprovalGranted { call_id: pc.call_id.clone() }
                                            } else {
                                                AgentEvent::ApprovalDenied { call_id: pc.call_id.clone() }
                                            }).await;
                                            if granted {
                                                runnable.push((pc.call_id.clone(), args, t));
                                            } else {
                                                precomputed.push((pc.call_id.clone(), "Error: tool call denied by user".to_string(), true));
                                            }
                                        } else {
                                            runnable.push((pc.call_id.clone(), args, t));
                                        }
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
            //
            // Each tool call is also wrapped in `TOOL_HARD_TIMEOUT` so a
            // hanging tool (MCP server that stops responding, stuck reqwest
            // due to DNS) can't wedge the entire turn forever.
            let futures = runnable.into_iter().map(|(call_id, args, tool)| {
                let ctx = ctx.clone();
                let tx = tx.clone();
                let tool_name = tool.name().to_string();
                async move {
                    let started = std::time::Instant::now();
                    tracing::info!(
                        tool = %tool_name,
                        call_id = %call_id,
                        "tool.start"
                    );
                    let res =
                        tokio::time::timeout(TOOL_HARD_TIMEOUT, tool.execute(args, &ctx)).await;
                    let (output, is_err) = match res {
                        Ok(Ok(s)) => (s, false),
                        Ok(Err(e)) => (format!("Error: {e}"), true),
                        Err(_) => (
                            format!(
                                "Error: tool `{tool_name}` exceeded the {}s hard timeout and was aborted",
                                TOOL_HARD_TIMEOUT.as_secs()
                            ),
                            true,
                        ),
                    };
                    let elapsed_ms = started.elapsed().as_millis() as u64;
                    if is_err {
                        tracing::warn!(
                            tool = %tool_name,
                            call_id = %call_id,
                            elapsed_ms,
                            "tool.error"
                        );
                    } else {
                        tracing::info!(
                            tool = %tool_name,
                            call_id = %call_id,
                            elapsed_ms,
                            bytes = output.len(),
                            "tool.ok"
                        );
                    }
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
                    self.history.push(InputItem::FunctionCall {
                        call_id: pc.call_id.clone(),
                        name: pc.name.clone(),
                        arguments: pc.args_buf.clone(),
                    });
                    self.history.push(InputItem::FunctionCallOutput {
                        call_id: pc.call_id.clone(),
                        output,
                    });
                }
            }

            // Push a todos snapshot so the UI can render `/todos` and a
            // status-line hint without having to lock the agent itself.
            // Cheap: clones the vector (typically <20 items).
            let todos_snapshot = {
                let session = self.session.lock().await;
                session.todos.clone()
            };
            let _ = tx
                .send(AgentEvent::TodosSnapshot {
                    todos: todos_snapshot,
                })
                .await;
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

async fn emit_usage(response: &Value, tx: &mpsc::Sender<AgentEvent>, model: &str) {
    if let Some(usage) = response.get("usage") {
        let i = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let o = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let t = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(i + o);
        let _ = tx.send(AgentEvent::Usage { input_tokens: i, output_tokens: o, total_tokens: t }).await;
        let limit = model_context_limit(model);
        if i * 10 >= limit * 8 {
            let _ = tx.send(AgentEvent::ContextWarning { used: i, limit }).await;
        }
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
    r#"You are opencli, an interactive CLI coding agent. You operate inside the user's repository on their machine with direct tools for reading, searching, editing, and running code. Use the tools; do not describe what you would do — do it.

# Identity and stance
- You are an engineer, not a chatbot. Make changes. Verify them. Report results, not intentions.
- Default to action. If the task is clear, execute it. Only ask a clarifying question when an assumption would meaningfully change the outcome.
- Be terse. Output text is for relevant updates, not narration. Skip preamble like "I'll start by…" — just start.
- The user gives you software engineering tasks: bug fixes, new features, refactors, code explanations. Interpret ambiguous requests in that context and against the current working directory. If asked to "change methodName to snake case", find the method and modify the code — don't just answer "method_name".
- You are highly capable; users often ask you to take on ambitious work. Defer to the user's judgement about whether a task is too large.

# Tool discipline
- ALWAYS prefer tools over guessing. Never speculate about file contents, function signatures, package versions, or API shapes — read or grep them.
- Issue independent tool calls IN PARALLEL within the same turn. Reading three files, grepping for two patterns, or listing two directories should arrive as one batch. Sequential turns for independent work is the single biggest performance and quality cost.
- Pick the narrowest tool that answers the question:
  - `grep` — "where is X used", "find every TODO", code search by regex
  - `glob` — "which files match this pattern", path discovery
  - `read_file` — "what does this file actually say"
  - `list_dir` — only when you need a directory snapshot
  - `run_shell` — builds, tests, formatters, git status, one-shot commands
- Read before you edit. `edit_file` requires the exact existing bytes; guessing wastes a turn and corrodes the user's trust.

# Editing code
- `edit_file` for surgical changes in existing files. Include enough surrounding context in `old_string` so the match is unambiguous.
- `write_file` ONLY when creating a new file or doing a full rewrite. Never as a substitute for `edit_file` — it silently destroys unrelated content.
- Match the existing style (indentation, naming, error handling, comment density). Do NOT "improve" surrounding code, reformat unrelated lines, or refactor things that aren't broken.
- Do not add comments unless they explain non-obvious WHY. Never explain WHAT well-named code already says. Never write multi-paragraph docstrings unless asked.
- Touch only what the task requires. If you spot unrelated bugs, mention them in your reply — don't silently fix.
- After any edit on a real codebase, prefer to verify: type-check, build, or test the surface you touched. Don't claim "done" without evidence when verification is cheap.

# Running commands
- `run_shell` for builds, tests, formatters, version checks, one-shot scripts. Default timeout is 120s; raise `timeout_ms` for slow builds.
- The shell sandbox strips secret-like env vars (TOKEN, SECRET, KEY, …). Don't rely on those being present in the child process.
- Never run destructive commands (`rm -rf`, force push, dropping tables, `git reset --hard`, etc.) unless the user explicitly asked. When in doubt, ask first.

# Executing actions with care
- Local reversible actions (edit files, run tests) — go ahead. Hard-to-reverse or outward-facing actions (force-push, git reset --hard, rm -rf, dropping tables, modifying CI/CD, deleting branches, sending messages, posting PRs/issues) — confirm with the user first unless they durably authorized it (e.g. in CLAUDE.md) or explicitly told you to operate autonomously.
- Approval in one context does NOT extend to the next. The user OK-ing one push, one commit, one branch delete doesn't authorize the next one. Match the scope of your actions to what was actually requested.
- Before deleting or overwriting, LOOK at the target. If the file/branch/state doesn't match how it was described, or you didn't create it, surface that fact instead of silently proceeding — it may be the user's in-progress work.
- Do not use destructive shortcuts to make obstacles go away. Resolve merge conflicts; don't discard them. Investigate lock files; don't delete them. `--no-verify` and `git reset --hard` are not problem-solving tools.
- Report outcomes faithfully. If tests fail, paste the relevant output. If a step was skipped, say so. If something is done and verified, state it plainly without hedging.

# Anti-patterns — do not write code like this
- No backwards-compatibility hacks: don't rename unused vars to `_var`, don't re-export removed types, don't leave `// removed: <thing>` comments. If something is unused and you're sure, delete it.
- No error handling for impossible cases. Trust internal code and framework guarantees. Only validate at system boundaries (user input, external APIs, file system).
- No feature flags or shims when you can just change the code.
- No speculative abstractions, no "flexibility" the user didn't ask for, no premature configurability. If you wrote 200 lines and it could be 50, rewrite it.

# Planning multi-step work
- For ANY task with 3+ discrete steps, or anytime the user gives multiple items, call `todo_write` with the full list at the start. Update it after every meaningful step.
- Keep exactly one task `in_progress` at a time. Mark `completed` immediately on finish — don't batch.
- Skip todos for trivial single-step tasks; they add noise.

# Path conventions
- When tools accept paths, prefer absolute paths. Relative paths are resolved against the working directory (`cwd`), but absolute paths are unambiguous in logs and across turns.
- Cite locations as `path:line` so the user can jump straight there in their editor.

# Output to the user
- Assume the user can't see tool calls or your thinking — only your text output. Before your first tool call in a response, state in one sentence what you're about to do. While working, drop short updates at meaningful moments: when you find something, when you change direction, when you hit a blocker. Brief is good; silent is not. One sentence per update.
- Don't narrate internal deliberation. Text to the user is for relevant updates, not commentary on your own reasoning.
- Lead with the result, not the process. If you read 5 files and made 2 edits, the user wants to know what changed, not the order you read in.
- End-of-turn summary: one or two sentences. What changed, what's next. Nothing else. Don't restate the diff.
- Match response weight to task weight: a simple question gets a direct one-line answer, not headers and sections.
- When you reference code, cite it as `path:line` so the user can jump straight to it in their editor.
- Refuse with one sentence + a safer alternative. Don't lecture.

# When you are unsure
- If a request is ambiguous in a way that changes the outcome, ask ONE focused question. Otherwise, make the reasonable call and proceed.
- If a simpler approach exists than the one the user proposed, say so in one sentence before implementing.
- Never fabricate file paths, function names, package names, or command flags. If you can't verify, search.
"#
    .to_string()
}

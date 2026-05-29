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

use crate::client::LlmClient;
use crate::config::Config;
use crate::openai::{InputItem, MessageContent, ResponseStreamEvent, ResponsesRequest};
use crate::tools::{ApprovalMode, Registry, SessionState, TodoItem, ToolContext};

/// Streaming event surfaced to the UI/CLI layer.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind")]
pub enum AgentEvent {
    AssistantTextDelta {
        text: String,
    },
    AssistantTextDone {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ReasoningDone {
        text: String,
    },
    ToolCallStarted {
        name: String,
        call_id: String,
    },
    ToolCallArgsDelta {
        call_id: String,
        delta: String,
    },
    ToolCallArgsDone {
        call_id: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        output: String,
        error: bool,
    },
    /// Snapshot of the session todo list. Emitted after every tool batch so
    /// the UI can render `/todos` and the status line without locking the
    /// agent mutex itself. Replaces the entire client-side cache each time.
    TodosSnapshot {
        todos: Vec<TodoItem>,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    TurnComplete,
    Error {
        message: String,
    },
    ContextWarning {
        used: u64,
        limit: u64,
    },
    /// Stronger than `ContextWarning`: input has crossed 85% of the model's
    /// context window. The UI auto-compacts when `config.auto_compact` is on,
    /// otherwise it urges the user to run `/compact`.
    AutoCompactSuggested {
        used: u64,
        limit: u64,
    },
    /// A background compaction finished. Sent by the TUI's compaction task over
    /// the same channel so the main loop can stop the progress bar and report
    /// the outcome. `error` is `None` on success (`original_len` is the item
    /// count collapsed into one summary) or `Some(reason)` on failure/no-op.
    CompactDone {
        original_len: u64,
        error: Option<String>,
    },
    ApprovalRequest {
        call_id: String,
        tool_name: String,
        args_json: String,
        diff_preview: Option<String>,
    },
    ApprovalGranted {
        call_id: String,
    },
    ApprovalDenied {
        call_id: String,
    },
}

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Tools that touch only session-local state (no FS, no shell). Safe to run
/// without per-call approval even when `require_approval` is on.
const ALWAYS_AUTO_TOOLS: &[&str] = &["todo_write"];

/// File-mutation tools auto-approved in "accept edits" mode.
const EDIT_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "multi_edit",
    "undo_last_edit",
    "notebook_edit",
];

/// Below this many history items, compaction is a no-op: too short to be worth
/// a summarization round-trip (and the result wouldn't free meaningful space).
const COMPACT_MIN_ITEMS: usize = 4;

/// Instruction appended to the history to elicit a compact, self-contained
/// summary that becomes the new conversation baseline.
const COMPACT_PROMPT: &str =
    "Summarize the conversation so far into a single self-contained block: \
                              what we worked on, what files / decisions matter going forward, and \
                              where we left off. Keep it under 30 lines. After this, treat the \
                              summary as the canonical context — earlier messages are gone.";

/// Context-window size (tokens) per model, used to warn before a turn
/// overflows. Verified against published model docs (May 2026):
///   - Anthropic: Opus 4.6 / 4.7 / 4.8 and Sonnet 4.6 ship a 1M window. Opus
///     4.5, Sonnet 4.5 / 4 and all Haiku are 200K. (Sonnet 4.5's 1M is a
///     header-gated beta we don't request, so 200K is the effective limit.)
///   - OpenAI: gpt-5.5 → 1.05M, gpt-5.4 → 1M, mini → 400K, nano → 200K;
///     everything else (gpt-5 / 5.2 / 5.3 / -pro / -codex) → 400K.
pub fn model_context_limit(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude") {
        let one_million = m.contains("opus-4-8")
            || m.contains("opus-4-7")
            || m.contains("opus-4-6")
            || m.contains("sonnet-4-6");
        return if one_million { 1_000_000 } else { 200_000 };
    }
    if m.contains("nano") {
        return 200_000;
    }
    if m.contains("mini") {
        return 400_000;
    }
    if m.contains("gpt-5.5") {
        return 1_050_000;
    }
    if m.contains("gpt-5.4") {
        return 1_000_000;
    }
    400_000
}

/// Build the post-compaction history: a single user message carrying the
/// summary as the new canonical context. A plain text message with no
/// `call_id` can never leave an orphaned function_call/output pair, which is
/// why full replacement (rather than mid-history truncation) is the safe,
/// provider-agnostic strategy. Role `user` because `translate.rs` coalesces
/// non-assistant roles to Anthropic's `user`, and a conversation that opens
/// with a user turn is valid for every provider.
fn compacted_history(summary: &str) -> Vec<InputItem> {
    vec![InputItem::Message {
        role: "user".to_string(),
        content: vec![MessageContent::text(format!(
            "[Conversation summary — earlier history was compacted to save context]\n\n{summary}"
        ))],
    }]
}

/// Whether a history of this many items is worth compacting. Pulled out so the
/// threshold is unit-testable without constructing an Agent or a network call.
fn should_compact(history_len: usize) -> bool {
    history_len > COMPACT_MIN_ITEMS
}

pub struct Agent {
    pub client: LlmClient,
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
    pub pending_approvals:
        Arc<Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    pub require_approval: bool,
    /// When true, file-edit tools auto-approve even though `require_approval`
    /// is on. Powers "accept edits" mode in the TUI; shell still prompts.
    pub auto_approve_edits: bool,
}

impl Agent {
    pub fn new(client: LlmClient, config: Config) -> Self {
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
            auto_approve_edits: false,
        }
    }

    pub async fn respond_approval(&self, call_id: &str, granted: bool) {
        let sender = {
            let mut map = self.pending_approvals.lock().await;
            map.remove(call_id)
        };
        if let Some(s) = sender {
            let _ = s.send(granted);
        }
    }

    /// Roll back the most recent file edit on the agent's session undo stack.
    /// Equivalent to the `undo_last_edit` tool but callable directly from the
    /// host (e.g. a `/undo` slash command) without round-tripping the model.
    pub async fn undo_last_edit(&self) -> anyhow::Result<String> {
        use anyhow::{anyhow, Context};
        let entry = {
            let mut session = self.session.lock().await;
            session.undo_stack.pop_back()
        };
        let entry = entry.ok_or_else(|| anyhow!("no edits to undo"))?;
        // Mirrors the TOCTOU guard in the `undo_last_edit` tool: refuse to
        // overwrite a file that has been touched since we edited it, so a
        // user's manual changes can't be silently destroyed by /undo.
        if let Some(expected) = entry.post_edit_mtime {
            let current = std::fs::metadata(&entry.path)
                .and_then(|m| m.modified())
                .ok();
            if current != Some(expected) {
                return Err(anyhow!(
                    "refusing to undo {}: file has been modified since the edit",
                    entry.path.display()
                ));
            }
        }
        match entry.original_content {
            Some(content) => {
                tokio::fs::write(&entry.path, content)
                    .await
                    .with_context(|| format!("restore {}", entry.path.display()))?;
                Ok(format!("Restored {}", entry.path.display()))
            }
            None => {
                tokio::fs::remove_file(&entry.path)
                    .await
                    .with_context(|| format!("remove {}", entry.path.display()))?;
                Ok(format!(
                    "Removed (was a new file): {}",
                    entry.path.display()
                ))
            }
        }
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

        let read_first = |paths: &[std::path::PathBuf]| -> Option<(std::path::PathBuf, String)> {
            for p in paths {
                if let Ok(text) = std::fs::read_to_string(p) {
                    let t = text.trim();
                    if !t.is_empty() {
                        return Some((p.clone(), t.to_string()));
                    }
                }
            }
            None
        };

        // Walk up from cwd to filesystem root collecting every CLAUDE.md / AGENTS.md.
        // Bottom-up collection, then reverse so the broadest ancestor comes first
        // and the most-specific (closest to cwd) is appended LAST in the prompt.
        let mut found: Vec<(std::path::PathBuf, String)> = Vec::new();
        let mut cur = self.cwd.clone();
        loop {
            let here = [cur.join("CLAUDE.md"), cur.join("AGENTS.md")];
            if let Some(hit) = read_first(&here) {
                if !globals.iter().any(|g| g == &hit.0) {
                    found.push(hit);
                }
            }
            match cur.parent() {
                Some(parent) if parent != cur => cur = parent.to_path_buf(),
                _ => break,
            }
        }
        found.reverse();

        let mut additions = String::new();
        if let Some((path, text)) = read_first(&globals) {
            additions.push_str(&format!("\n\n# Global memory ({})\n\n", path.display()));
            additions.push_str(&text);
        }
        for (path, text) in &found {
            additions.push_str(&format!("\n\n# Project memory ({})\n\n", path.display()));
            additions.push_str(text);
        }
        if !additions.is_empty() {
            self.system_prompt.push_str(&additions);
        }
    }

    /// Discover every installed skill (opencli + Claude Code + Codex + project)
    /// and append a compact manifest to the system prompt so the model knows
    /// what playbooks exist and can load any one on demand via the `skill`
    /// tool. Only `name: description` lines go in — bodies are loaded lazily —
    /// and the whole block rides the prompt cache, so even hundreds of skills
    /// cost roughly one line each after the first turn. Idempotent-ish: call
    /// once during setup, after `cwd` is set. No-ops when nothing is installed.
    pub fn apply_skill_manifest(&mut self) {
        let entries = crate::skill::discover(&self.cwd);
        if entries.is_empty() {
            return;
        }
        let count = entries.len();
        let manifest = crate::skill::manifest(&entries);
        self.system_prompt.push_str(&format!(
            "\n\n# Available skills ({count})\n\n\
             These curated playbooks are installed and available. Each is the distilled \
             approach for one kind of task. When a request clearly matches a skill's \
             description, call the `skill` tool with its exact name to load the full \
             instructions, then follow them. Load at most what you need — do not pull in \
             skills speculatively.\n\n{manifest}"
        ));
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
            match std::fs::read(path) {
                Ok(bytes) => {
                    use base64::Engine;
                    let mime = guess_mime(path);
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    content.push(MessageContent::InputImage {
                        image_url: format!("data:{};base64,{}", mime, b64),
                        detail: None,
                    });
                }
                Err(e) => {
                    tracing::warn!(?path, error = %e, "failed to read image attachment");
                    content.push(MessageContent::text(format!(
                        "[image attachment {} could not be read: {}]",
                        path.display(),
                        e
                    )));
                }
            }
        }
        self.history.push(InputItem::Message {
            role: "user".to_string(),
            content,
        });
    }

    /// Drive one full turn: send the current history, process tool calls until
    /// the model produces final assistant text. Emits events through `tx`.
    ///
    /// Thin wrapper so the `Stop` hook fires on EVERY exit — success or error —
    /// per its documented contract. The inner loop has several early error
    /// returns (idle timeout, stream error, response.failed); firing here covers
    /// all of them instead of only the clean-completion path.
    pub async fn run_turn(&mut self, tx: mpsc::Sender<AgentEvent>) -> Result<()> {
        let result = self.run_turn_inner(tx).await;
        self.hooks.fire_stop().await;
        result
    }

    /// Replace the entire conversation history with one model-generated summary
    /// message, reclaiming context-window space. Provider-agnostic: it operates
    /// on `self.history` before any request is built, so every model benefits.
    ///
    /// On a trivially short history, an empty summary, or a stream error,
    /// `self.history` is left UNTOUCHED and an `Err` is returned. On success
    /// returns the number of history items that were compacted away.
    pub async fn compact_history(&mut self) -> Result<usize> {
        let original_len = self.history.len();
        if !should_compact(original_len) {
            return Err(anyhow::anyhow!(
                "nothing to compact — conversation is already short"
            ));
        }

        // One-off summary request from the CURRENT history plus a summarize
        // instruction. Deliberately built WITHOUT `.with_tools(...)`: the
        // summary turn must not start editing files or running commands.
        let mut input = self.history.clone();
        input.push(InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(COMPACT_PROMPT)],
        });
        let request = ResponsesRequest::new(self.config.model.clone(), input)
            .with_instructions(self.system_prompt.clone())
            .with_reasoning(self.config.reasoning_effort.clone())
            .with_verbosity(self.config.verbosity.clone());

        let summary = self.collect_text(request).await?;
        if summary.trim().is_empty() {
            return Err(anyhow::anyhow!("compaction produced an empty summary"));
        }

        self.history = compacted_history(&summary);
        Ok(original_len)
    }

    /// Drive a request through the streaming path and return the accumulated
    /// assistant text. A minimal recv loop for tool-free turns (used by
    /// `compact_history`): it handles only text and terminal events. It does
    /// NOT call `emit_usage`, so the summary turn's large input doesn't re-fire
    /// the 85% context warning while we are in the middle of compacting.
    async fn collect_text(&self, request: ResponsesRequest) -> Result<String> {
        let mut handle = self.client.stream(request).await?;
        let mut text = String::new();
        loop {
            let recv = tokio::time::timeout(STREAM_IDLE_TIMEOUT, handle.rx.recv()).await;
            let ev = match recv {
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "stream idle for {}s — connection may be stale, try again",
                        STREAM_IDLE_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(e),
                Ok(Some(Ok(v))) => v,
            };
            match ev {
                ResponseStreamEvent::OutputTextDelta { delta, .. } => {
                    text.push_str(&delta);
                }
                // Fall back to the block's full text only if no deltas arrived
                // (some providers emit Done without deltas); otherwise keep the
                // accumulated deltas.
                ResponseStreamEvent::OutputTextDone { text: t, .. } if text.is_empty() => {
                    text = t;
                }
                ResponseStreamEvent::Completed { .. } => break,
                ResponseStreamEvent::Failed { response } => {
                    let message = response
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("response.failed (no message)")
                        .to_string();
                    return Err(anyhow::anyhow!("response.failed: {message}"));
                }
                ResponseStreamEvent::Error { message } => {
                    return Err(anyhow::anyhow!(message));
                }
                _ => {}
            }
        }
        Ok(text)
    }

    async fn run_turn_inner(&mut self, tx: mpsc::Sender<AgentEvent>) -> Result<()> {
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
                            .send(AgentEvent::Error {
                                message: msg.clone(),
                            })
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
                    ResponseStreamEvent::OutputItemAdded { item, .. }
                        if item.get("type").and_then(|v| v.as_str()) == Some("function_call") =>
                    {
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
                        // If both ids are missing, downstream args deltas can't
                        // be matched back to this call — pending_calls would
                        // hold a ghost entry that any later "empty id" delta
                        // would corrupt, eventually dispatching a tool with
                        // bogus or empty arguments. Drop the event with a
                        // warning instead of pretending it worked.
                        if call_id.is_empty() && item_id.is_empty() {
                            tracing::warn!(
                                name = %name,
                                "function_call event missing both call_id and id; skipping"
                            );
                            continue;
                        }
                        // Some models send the complete arguments inline on the
                        // OutputItemAdded item; capture them as an initial buffer.
                        let args_buf = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // A duplicate non-empty call_id would later collapse in
                        // the results HashMap, silently dropping one tool output
                        // and leaving an unanswered call in history. Skip the
                        // second item and log the server-side anomaly instead.
                        if !call_id.is_empty() && pending_calls.iter().any(|p| p.call_id == call_id)
                        {
                            tracing::warn!(
                                call_id = %call_id,
                                name = %name,
                                "duplicate call_id from server; ignoring second function_call item"
                            );
                            continue;
                        }
                        pending_calls.push(PendingCall {
                            call_id: call_id.clone(),
                            item_id,
                            name: name.clone(),
                            args_buf,
                        });
                        let _ = tx.send(AgentEvent::ToolCallStarted { name, call_id }).await;
                    }
                    ResponseStreamEvent::OutputItemDone { item, .. }
                        if item.get("type").and_then(|v| v.as_str()) == Some("function_call") =>
                    {
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
                    ResponseStreamEvent::OutputTextDelta { delta, .. } => {
                        final_text.push_str(&delta);
                        let _ = tx
                            .send(AgentEvent::AssistantTextDelta { text: delta })
                            .await;
                    }
                    ResponseStreamEvent::OutputTextDone { text, .. } => {
                        // `text` is only THIS block's text. Anthropic emits one
                        // OutputTextDone per text block, so overwriting would drop
                        // earlier blocks (e.g. text before AND after a tool_use in
                        // one response). `final_text` already holds the full
                        // accumulation from the deltas for both providers; keep it,
                        // falling back to `text` only if no deltas arrived. Emit the
                        // cumulative text so the UI's "done" replace stays complete.
                        if final_text.is_empty() {
                            final_text = text;
                        }
                        let _ = tx
                            .send(AgentEvent::AssistantTextDone {
                                text: final_text.clone(),
                            })
                            .await;
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
                                // Only overwrite when the done event actually
                                // carried args; an empty/absent `arguments` must
                                // not wipe the buffer accumulated from the deltas
                                // (matches the OutputItemDone handler above).
                                if !arguments.is_empty() {
                                    pc.args_buf = arguments.clone();
                                }
                                pc.call_id.clone()
                            })
                            .unwrap_or_else(|| item_id.clone());
                        let _ = tx
                            .send(AgentEvent::ToolCallArgsDone { call_id, arguments })
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
                            .send(AgentEvent::Error {
                                message: message.clone(),
                            })
                            .await;
                        return Err(anyhow::anyhow!("response.failed: {message}"));
                    }
                    ResponseStreamEvent::Error { message } => {
                        let _ = tx
                            .send(AgentEvent::Error {
                                message: message.clone(),
                            })
                            .await;
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
                            let truncated: String = pc.args_buf.chars().take(200).collect();
                            format!("{truncated}…")
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
                                        let auto_via_accept_edits = self.auto_approve_edits
                                            && EDIT_TOOLS.contains(&pc.name.as_str());
                                        let needs_gate = self.require_approval
                                            && matches!(
                                                self.approval,
                                                ApprovalMode::OnRequest | ApprovalMode::Manual
                                            )
                                            && !t.is_read_only()
                                            && !ALWAYS_AUTO_TOOLS.contains(&pc.name.as_str())
                                            && !auto_via_accept_edits;
                                        if needs_gate {
                                            let diff_preview = t.compute_preview(&args, &ctx).await;
                                            let (resp_tx, resp_rx) =
                                                tokio::sync::oneshot::channel::<bool>();
                                            self.pending_approvals
                                                .lock()
                                                .await
                                                .insert(pc.call_id.clone(), resp_tx);
                                            let _ = tx
                                                .send(AgentEvent::ApprovalRequest {
                                                    call_id: pc.call_id.clone(),
                                                    tool_name: pc.name.clone(),
                                                    args_json: pc.args_buf.clone(),
                                                    diff_preview,
                                                })
                                                .await;
                                            let granted = match tokio::time::timeout(
                                                APPROVAL_TIMEOUT,
                                                resp_rx,
                                            )
                                            .await
                                            {
                                                Ok(Ok(g)) => g,
                                                _ => {
                                                    self.pending_approvals
                                                        .lock()
                                                        .await
                                                        .remove(&pc.call_id);
                                                    false
                                                }
                                            };
                                            let _ = tx
                                                .send(if granted {
                                                    AgentEvent::ApprovalGranted {
                                                        call_id: pc.call_id.clone(),
                                                    }
                                                } else {
                                                    AgentEvent::ApprovalDenied {
                                                        call_id: pc.call_id.clone(),
                                                    }
                                                })
                                                .await;
                                            if granted {
                                                runnable.push((pc.call_id.clone(), args, t));
                                            } else {
                                                precomputed.push((
                                                    pc.call_id.clone(),
                                                    "Error: tool call denied by user".to_string(),
                                                    true,
                                                ));
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
                let hooks_for_post = self.hooks.clone();
                let post_args = args.clone();
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
                    // Best-effort PostToolUse hook. We do not propagate
                    // failures from here — the call already happened.
                    hooks_for_post.fire_post(&tool_name, &post_args, &output, is_err).await;
                    (call_id, output, is_err)
                }
            });
            let mut results: Vec<(String, String, bool)> = futures::future::join_all(futures).await;

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
            // Record the assistant's narration that preceded the tool calls
            // BEFORE the function-call items, so the transcript (and the next
            // turn's context, and any resumed session) keeps what the model said.
            // Without this, an "I'll read that file…" preamble vanished whenever
            // a response mixed text with tool calls (the only other push of
            // assistant text lives in the no-tool-calls branch).
            if !final_text.is_empty() {
                self.history.push(InputItem::Message {
                    role: "assistant".to_string(),
                    content: vec![MessageContent::OutputText {
                        text: std::mem::take(&mut final_text),
                    }],
                });
            }
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
        let get = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        // With prompt caching on, Anthropic reports the cached prompt tokens
        // separately from `input_tokens`. The true context occupancy (what the
        // window limit applies to) is the sum of all three; folding them in
        // keeps the /compact warning accurate. OpenAI omits the cache fields,
        // so this is a no-op there.
        let i = get("input_tokens")
            + get("cache_read_input_tokens")
            + get("cache_creation_input_tokens");
        let o = get("output_tokens");
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
        let limit = model_context_limit(model);
        // 85% threshold escalates to a stronger AutoCompactSuggested so the
        // TUI can show a sticky banner urging /compact before a hard 1xx
        // context-window failure on the next turn. Checked first (narrower
        // condition) so the stronger event replaces — not supplements — the
        // 80% ContextWarning.
        if i * 100 >= limit * 85 {
            let _ = tx
                .send(AgentEvent::AutoCompactSuggested { used: i, limit })
                .await;
        } else if i * 10 >= limit * 8 {
            let _ = tx.send(AgentEvent::ContextWarning { used: i, limit }).await;
        }
    }
}

fn guess_mime(p: &std::path::Path) -> &'static str {
    match p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
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
  - `run_shell` — builds, tests, formatters, git, one-shot commands (use `run_in_background: true` for dev servers/watchers)
  - `web_search` — find pages by query when you don't know the URL; pair with `web_fetch` to read the best hit
  - `web_fetch` — fetch a known URL's contents (upstream docs, a raw file, a public API)
  - `notebook_edit` — edit a Jupyter notebook (`.ipynb`) cell: replace, insert, or delete
  - `skill` — load a curated playbook by exact name when the task matches one listed under "Available skills"
  - `dispatch_agent` — hand a large, self-contained sub-task to a child agent (see Subagents)
  - `ask_user_question` — surface multiple-choice options when only the user can decide
- Read before you edit. `edit_file`/`multi_edit` require the exact existing bytes; guessing wastes a turn and corrodes the user's trust.

# Editing code
- `edit_file` for surgical changes in existing files. Include enough surrounding context in `old_string` so the match is unambiguous.
- `write_file` ONLY when creating a new file or doing a full rewrite. Never as a substitute for `edit_file` — it silently destroys unrelated content.
- `multi_edit` when you have several edits to the SAME file: they apply in order and roll back atomically if any one fails. Prefer it over a sequence of `edit_file` calls on one file.
- `undo_last_edit` reverts your most recent file write if you got it wrong. It refuses if the file changed underneath you, so don't rely on it to paper over a destructive mistake.
- `read_file` prefixes each line with `<lineno>\t` for display only. NEVER include that prefix in `old_string` — match the real file bytes.
- Match the existing style (indentation, naming, error handling, comment density). Do NOT "improve" surrounding code, reformat unrelated lines, or refactor things that aren't broken.
- Do not add comments unless they explain non-obvious WHY. Never explain WHAT well-named code already says. Never write multi-paragraph docstrings unless asked.
- Touch only what the task requires. If you spot unrelated bugs, mention them in your reply — don't silently fix.
- After any edit on a real codebase, prefer to verify: type-check, build, or test the surface you touched. Don't claim "done" without evidence when verification is cheap.

# Running commands
- `run_shell` for builds, tests, formatters, version checks, one-shot scripts. Default timeout is 120s; raise `timeout_ms` for slow builds.
- For long-lived processes (dev servers, watchers, log tails) pass `run_in_background: true` — it returns a `bash_id`. Poll new output with `bash_output {bash_id}` and stop it with `kill_shell {bash_id}`. A foreground command that never exits will block until timeout.
- The shell sandbox strips secret-like env vars (TOKEN, SECRET, KEY, …). Don't rely on those being present in the child process.
- Destructive commands (`rm -rf` on broad targets, force push, `git reset --hard`, fs format, dropping tables) are refused unless you pass `dangerous_override: true` — and you only do that AFTER the user explicitly confirmed. When in doubt, ask first.

# Asking the user
- Use `ask_user_question` ONLY when a decision is genuinely the user's to make and you can't resolve it from the code, the request, or a sensible default — which approach, which trade-off, consent before a hard-to-reverse action. 1–4 questions, each with 2–4 mutually-exclusive options. After calling it, STOP and wait for the reply; don't assume an answer in the same turn.
- If the answer is derivable by reading code or running a command, do that instead. For a free-text answer, just ask in plain text — don't force it into options.

# Subagents (dispatch_agent)
- `dispatch_agent` spawns a child agent for a large, self-contained sub-task — heavy exploration, multi-file research, a focused review — that would otherwise crowd out this conversation. Issue several in one turn to run them in parallel. Definitions are discovered from `~/.config/opencli/agents/<name>.md`, `~/.claude/agents/` (Claude Code's agents work directly), and the project's `.claude/agents/` or `.opencli/agents/`; `/agents` lists them.
- The child sees only the `prompt` you pass, never this conversation, and returns only its final text. Give it all the context it needs. Don't use it for quick lookups (one or two direct tool calls are cheaper) or for edits the user expects to review step by step.

# Skills
- The `# Available skills` manifest below lists every installed playbook by name + one-line description. They are discovered from opencli (`~/.config/opencli/skills/`), Claude Code (`~/.claude/skills/`, including plugin libraries), Codex, and the project (`.claude/skills/`, `.opencli/skills/`).
- The manifest is name+description only. When a task clearly matches a skill, call the `skill` tool with its EXACT name to load the full body, then follow it. This progressive disclosure keeps context lean — load only what the task needs, never speculatively, and never twice. `/skills` lists what's installed.

# Plan mode
- In plan mode every mutating tool (`write_file`, `edit_file`, `multi_edit`, `run_shell`, `todo_write`, …) is rejected; only read-only tools run. Investigate and propose a plan. If you need to make changes, tell the user to leave plan mode (`/normal`) rather than retrying the blocked call.

# Context window & compaction
- The context window is finite. The UI warns near 80% and urges `/compact` near 85%. In long sessions, keep tool output lean (narrow `grep`, targeted `read_file` slices) and don't re-read files already in context. After `/compact` the history is summarized — keep working from the summary.

# Other capabilities
- MCP: tools named `mcp__<server>__<tool>` come from user-configured MCP servers (`/mcp` lists them). Call them like any other tool.
- Images: the user can attach images (`/img`); when an image is present in the conversation, read it as part of the request.

# Frontend & UI design
When you build any interface — a component, page, or app — aim for distinctive, production-grade design. Never ship generic "AI slop": the default centered-hero + card-grid template, purple-gradient-on-white, Inter/Roboto/Arial/system fonts, uniform spacing and emphasis everywhere.
- Commit first to ONE bold, intentional aesthetic direction (editorial, brutalist, refined-minimal, retro-futuristic, luxury, playful, industrial, …) and execute it with precision. Intentionality beats intensity — disciplined minimalism and full maximalism both work when the point of view is clear and cohesive.
- Typography carries it: choose distinctive, characterful fonts; pair a display face with a clean body face. Don't converge on the same "safe" choice across projects.
- Color: a cohesive palette via CSS variables; a dominant color with a sharp accent beats a timid, evenly-spread one. Decide light vs dark deliberately — don't default to dark.
- Hierarchy & layout: drive emphasis through scale contrast and intentional rhythm, not uniform padding. Use asymmetry, overlap, grid-breaking or bento composition, and either generous negative space or controlled density.
- Motion: animate compositor-friendly properties (`transform`, `opacity`); CSS-only for plain HTML, the Motion library for React when available. Spend the effort on a few high-impact moments — one well-orchestrated staggered page-load reveal beats scattered micro-interactions. Design real hover/focus/active states. Honor `prefers-reduced-motion`.
- Depth & atmosphere: gradient meshes, noise/grain, layered transparency, considered shadows, decorative borders — not flat solid fills.
- Always: semantic HTML, keyboard access, sufficient contrast, explicit image dimensions, and Core Web Vitals discipline (lazy-load below the fold, defer non-critical JS/CSS).
- For deep frontend work, load the `frontend-design` skill (and `design-system`, `motion-ui`, `frontend-a11y`, `liquid-glass-design` when relevant) via the `skill` tool and follow it — that playbook is what this summary distills.

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
- File tools (`read_file`, `write_file`, `edit_file`, `multi_edit`, `list_dir`) are SANDBOXED to the working directory: pass paths RELATIVE to `cwd` (e.g. `src/main.rs`, not `/home/you/proj/src/main.rs`). Absolute paths and `..` traversal are rejected — using them just wastes a turn.
- `run_shell` runs with `cwd` as its working directory, so relative paths work there too.
- Cite locations to the user as `path:line` so they can jump straight there in their editor.

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

#[cfg(test)]
mod context_limit_tests {
    use super::model_context_limit;

    #[test]
    fn anthropic_context_windows_match_docs() {
        // Opus 4.6+ and Sonnet 4.6 → 1M.
        assert_eq!(model_context_limit("claude-opus-4-8"), 1_000_000);
        assert_eq!(model_context_limit("claude-opus-4-7"), 1_000_000);
        assert_eq!(model_context_limit("claude-opus-4-6"), 1_000_000);
        assert_eq!(model_context_limit("claude-sonnet-4-6"), 1_000_000);
        // Opus 4.5, Sonnet 4.5 and Haiku → 200K.
        assert_eq!(model_context_limit("claude-opus-4-5"), 200_000);
        assert_eq!(model_context_limit("claude-sonnet-4-5"), 200_000);
        assert_eq!(model_context_limit("claude-haiku-4-5"), 200_000);
    }

    #[test]
    fn openai_context_windows() {
        assert_eq!(model_context_limit("gpt-5.5"), 1_050_000);
        assert_eq!(model_context_limit("gpt-5.4"), 1_000_000);
        assert_eq!(model_context_limit("gpt-5.3"), 400_000);
        assert_eq!(model_context_limit("gpt-5-mini"), 400_000);
        assert_eq!(model_context_limit("gpt-5-nano"), 200_000);
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::{compacted_history, should_compact, COMPACT_MIN_ITEMS};
    use crate::openai::{InputItem, MessageContent};

    #[test]
    fn compacted_history_is_single_orphan_free_user_message() {
        let h = compacted_history("a summary of the work");
        assert_eq!(h.len(), 1, "compaction must collapse to exactly one item");
        match &h[0] {
            InputItem::Message { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    MessageContent::InputText { text } => {
                        assert!(text.contains("a summary of the work"));
                    }
                    other => panic!("expected input_text, got {other:?}"),
                }
            }
            other => panic!("expected a Message, got {other:?}"),
        }
        // The point of full replacement: no tool-call pairing survives, so the
        // compacted history can never present an orphaned call/output (which
        // both Anthropic and OpenAI reject with a 4xx).
        assert!(!h.iter().any(|i| matches!(
            i,
            InputItem::FunctionCall { .. } | InputItem::FunctionCallOutput { .. }
        )));
    }

    #[test]
    fn should_compact_respects_min_items() {
        assert!(!should_compact(0));
        assert!(!should_compact(COMPACT_MIN_ITEMS));
        assert!(should_compact(COMPACT_MIN_ITEMS + 1));
    }
}

//! Split out of `agent`; logic unchanged.

use super::*;

/// Abort the recv loop if the upstream SSE stream falls silent for this long.
/// Catches network hangs where the server stops emitting events without
/// closing the channel — previously this left the UI stuck on "Reasoning…"
/// forever with no way to recover.
pub(super) const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Backstop on tool-call round-trips within a single user turn. Each iteration
/// is one model response; the loop only ends naturally when the model replies
/// without a tool call. A model wedged in a call→result→call cycle (e.g. a
/// tool that keeps failing) would otherwise loop forever, burning tokens. This
/// is intentionally generous — far above any legitimate task — and surfaces as
/// a clear error so the user can re-prompt to continue.
pub(super) const MAX_AGENT_STEPS: usize = 250;

/// How many times a single turn may auto-recover from a hard context-window
/// overflow (shed tool-output bulk and retry) before giving up and surfacing the
/// error. Bounded so a history that can't be shrunk further can't spin forever.
pub(super) const MAX_OVERFLOW_RECOVERIES: usize = 2;

/// How many times a single turn may retry after the SSE stream ends before its
/// terminal event *without having produced any answer content yet*. This case is
/// a transport truncation (the connection dropped before the model said
/// anything), so re-sending the identical request is safe — nothing was
/// committed to history or shown to the user. Bounded so a persistently broken
/// connection surfaces the error instead of spinning.
pub(super) const MAX_STREAM_RECOVERIES: usize = 2;

/// How many times a single turn may fail over to a different model after the
/// active one is rate-limited / its provider is overloaded. Bounded so a chain
/// of overloaded providers surfaces the error instead of spinning, and a const
/// (not a config knob) to keep the surface small.
pub(super) const MAX_FALLBACK_ATTEMPTS: usize = 2;

/// Heuristic match for a provider's "input too large for the context window"
/// rejection across OpenAI / Anthropic / OpenAI-compatible phrasings. The single
/// source of truth the agent's auto-recovery and the TUI's error hint both use.
pub fn is_context_overflow_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("context window")
        || m.contains("context length")
        || m.contains("context_length_exceeded")
        || m.contains("maximum context")
        || m.contains("prompt is too long")
        || m.contains("too many tokens")
        || m.contains("exceeds the context")
        || m.contains("reduce the length")
}

/// A streamed response whose SSE feed ended before its terminal event
/// (`response.completed` / `message_stop` / `[DONE]`). Emitted by every
/// provider stream pump (`openai`, `anthropic`, `chat`), so match on the shared
/// "ended before" phrasing rather than one provider's exact wording.
pub(super) fn is_stream_truncation_error(message: &str) -> bool {
    message.contains("ended before")
}

/// A transport-level failure surfaced by a stream pump (TCP reset, decode error)
/// rather than a model/usage error. Anchored to the pumps' shared `SSE transport:`
/// prefix (openai/stream.rs, anthropic/stream.rs, openai/chat.rs) rather than a
/// bare `transport` substring, so an unrelated error whose text merely contains
/// the word "transport" isn't misclassified as retryable. Safe to retry only
/// when nothing was produced yet — see `MAX_STREAM_RECOVERIES`.
pub(super) fn is_stream_transport_error(message: &str) -> bool {
    message.contains("SSE transport:")
}

#[cfg(test)]
mod stream_error_tests {
    use super::{is_stream_transport_error, is_stream_truncation_error};

    #[test]
    fn classifies_each_provider_truncation_message() {
        // The exact strings emitted by the three stream pumps. Keep this in sync
        // with openai/stream.rs, anthropic/stream.rs, and openai/chat.rs — the
        // shared "ended before" phrasing is the contract.
        for msg in [
            "SSE stream ended before a terminal event",
            "Anthropic SSE stream ended before message_stop",
            "Chat Completions stream ended before completion",
        ] {
            assert!(
                is_stream_truncation_error(msg),
                "should be truncation: {msg}"
            );
        }
    }

    #[test]
    fn classifies_transport_errors() {
        for msg in [
            "SSE transport: connection reset",
            "Anthropic SSE transport: broken pipe",
        ] {
            assert!(is_stream_transport_error(msg), "should be transport: {msg}");
        }
    }

    #[test]
    fn does_not_misclassify_idle_or_model_errors() {
        // The idle-timeout and genuine model/usage errors must NOT be treated as a
        // retryable truncation/transport hiccup.
        for msg in [
            "stream idle for 120s — connection may be stale, try again",
            "response.failed: content filtered",
            "Anthropic 400 invalid request",
        ] {
            assert!(!is_stream_truncation_error(msg), "not truncation: {msg}");
            assert!(!is_stream_transport_error(msg), "not transport: {msg}");
        }
    }
}

/// Bound streamed function-call arguments before parsing. Normal tool calls are
/// small JSON objects; a runaway provider or incompatible model can otherwise
/// stream unbounded bytes and wedge the process before the tool layer can reject
/// it. Kept high enough for large write/edit payloads.
pub(super) const MAX_TOOL_ARGUMENT_BYTES: usize = 2 * 1024 * 1024;

/// Final backstop on any tool result before it is emitted to the UI or appended
/// to model history. Individual tools should still return lean, structured
/// output, but MCP/custom tools and explicit high limits can otherwise push a
/// multi-megabyte blob into the next provider request.
pub(super) const TOOL_RESULT_MAX_BYTES: usize = 1_048_576;

/// Cap concurrent read-only tool execution. Models can emit a large batch of
/// file/search calls in one response; bounding the batch keeps the CLI
/// responsive and avoids IO/socket stampedes while still preserving parallelism.
pub(super) const MAX_PARALLEL_TOOL_CALLS: usize = 8;

/// Cap on distinct orphan tool-argument buffers held during one stream —
/// argument fragments that arrive before their tool call's `OutputItemAdded`.
/// A normal stream has a handful; the cap stops a malformed stream of unique
/// item ids from growing the buffer map without bound.
pub(super) const MAX_ORPHAN_ARG_BUFFERS: usize = 256;

/// Aggregate byte cap across all orphan argument buffers in one stream. The
/// per-buffer cap (`MAX_TOOL_ARGUMENT_BYTES`, 2 MiB) times the count cap (256)
/// still allows ~512 MiB of pinned memory on a malformed stream, so bound the
/// total too — well above any legitimate batch of pre-`OutputItemAdded`
/// fragments. Once reached, further orphan accumulation is dropped.
pub(super) const MAX_ORPHAN_ARG_TOTAL_BYTES: usize = 16 * 1024 * 1024;

/// Unknown/malformed tool calls are replayed as a plain user message instead of
/// a provider function_call item. Keep model-controlled text inside that
/// reminder bounded and inert.
pub(super) const SAFE_TOOL_HISTORY_NAME_CHARS: usize = 128;
pub(super) const SAFE_TOOL_HISTORY_ERROR_CHARS: usize = 4_096;
pub(super) const TODO_REMINDER_MAX_ITEMS: usize = 20;
pub(super) const TODO_REMINDER_ITEM_CHARS: usize = 180;

/// Above this many MCP tools, switch to progressive tool disclosure: withhold
/// their schemas from each request and let the model load them on demand via
/// `tool_search`. Below it, the per-request schema cost is small enough that
/// the extra round-trip isn't worth it, so every MCP tool stays directly
/// callable.
pub(super) const MCP_DEFER_THRESHOLD: usize = 12;

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
    /// The session working directory changed, typically after entering/exiting a worktree.
    CwdChanged {
        cwd: String,
    },
    /// Snapshot of the session todo list. Emitted after every tool batch so
    /// the UI can render `/todos` and the status line without locking the
    /// agent mutex itself. Replaces the entire client-side cache each time.
    TodosSnapshot {
        todos: Vec<TodoItem>,
    },
    /// Status update from the `goal_update` tool. The TUI owns the active
    /// `/goal` loop and uses this as the explicit stop/progress signal.
    GoalStatusUpdated {
        status: String,
        summary: String,
    },
    /// Plan-mode control tool requested approval to leave plan mode and start
    /// implementation.
    PlanExitRequested {
        plan: String,
    },
    /// Plan-mode control tool requested entering read-only planning mode.
    PlanModeRequested,
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        total_tokens: u64,
    },
    /// Cumulative per-model billed token tally after a response was accounted.
    /// The TUI mirrors this for `/cost`; persistence lives in the session record.
    CostUpdate {
        usage: Vec<crate::session::ModelUsage>,
    },
    /// The active provider's real quota/rate-limit snapshot, captured from the
    /// turn's response (headers or the Codex `codex.rate_limits` event). The TUI
    /// stores the latest for `/usage`. Fire-and-forget, never blocks the turn.
    Quota {
        snapshot: crate::usage::QuotaSnapshot,
    },
    TurnComplete,
    Error {
        message: String,
    },
    /// The active model was rate-limited / its provider was overloaded, so the
    /// turn transparently failed over to a configured fallback model (which may
    /// be a different provider or a local endpoint). The UI surfaces this and
    /// adopts `to` as the current model for the rest of the session.
    FallbackSwitched {
        from: String,
        to: String,
        reason: String,
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
    /// A sub-agent (`dispatch_agent`) started. `id` is unique within the run and
    /// keys the sub-agent's row in the TUI fleet view; `subagent_type` is the
    /// definition name and `prompt` the (possibly long) task it was given.
    SubagentStarted {
        id: String,
        subagent_type: String,
        prompt: String,
    },
    /// A sub-agent made progress — `summary` is a short label (e.g. the tool it
    /// just started) for the live fleet view.
    SubagentActivity {
        id: String,
        summary: String,
    },
    /// A sub-agent finished. `ok` is false when it errored or produced no answer.
    SubagentDone {
        id: String,
        ok: bool,
    },
}

pub(super) const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Tools that touch only session-local state (no FS, no shell). Safe to run
/// without per-call approval even when `require_approval` is on.
pub(super) const ALWAYS_AUTO_TOOLS: &[&str] = &["todo_write", "goal_update", "memory"];

/// File-mutation tools auto-approved in "accept edits" mode.
pub(super) const EDIT_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "multi_edit",
    "undo_last_edit",
    "notebook_edit",
];

/// Below this many history items, compaction is a no-op: too short to be worth
/// a summarization round-trip (and the result wouldn't free meaningful space).
pub(super) const COMPACT_MIN_ITEMS: usize = 4;

/// Instruction appended to the history to elicit a compact, self-contained
/// summary that becomes the new conversation baseline.
pub(super) const COMPACT_PROMPT: &str =
    "Summarize the conversation so far into a single self-contained block: \
                              what we worked on, what files / decisions matter going forward, and \
                              where we left off. Keep it under 30 lines. After this, treat the \
                              summary as the canonical context — earlier messages are gone.";

/// Whether an Anthropic model is eligible for the `context-1m-2025-08-07` beta
/// header. Thin delegator to the model catalogue (the single source of truth);
/// see [`crate::catalog`].
pub fn model_supports_1m(model: &str) -> bool {
    crate::catalog::supports_1m(model)
}

/// Context-window size (tokens) per model, used to warn before a turn
/// overflows. Thin delegator to the model catalogue; see [`crate::catalog`].
pub fn model_context_limit(model: &str) -> u64 {
    crate::catalog::context_limit(model)
}

/// Human-readable context-window label for a model (e.g. "1M", "1.05M",
/// "200K"), derived from `model_context_limit`. Used in the model catalogue so
/// users can see at a glance which models have the 1M window and which don't.
pub fn context_window_label(model: &str) -> String {
    let n = model_context_limit(model);
    if n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000_000 {
        let s = format!("{:.2}", n as f64 / 1_000_000.0);
        format!("{}M", s.trim_end_matches('0').trim_end_matches('.'))
    } else {
        format!("{}K", n / 1_000)
    }
}

/// Build the post-compaction history: a single user message carrying the
/// summary as the new canonical context. A plain text message with no
/// `call_id` can never leave an orphaned function_call/output pair, which is
/// why full replacement (rather than mid-history truncation) is the safe,
/// provider-agnostic strategy. Role `user` because `translate.rs` coalesces
/// non-assistant roles to Anthropic's `user`, and a conversation that opens
/// with a user turn is valid for every provider.
pub(super) fn compacted_history(summary: &str) -> Vec<InputItem> {
    vec![InputItem::Message {
        role: "user".to_string(),
        content: vec![MessageContent::text(format!(
            "[Conversation summary — earlier history was compacted to save context]\n\n{summary}"
        ))],
    }]
}

/// Whether a history of this many items is worth compacting. Pulled out so the
/// threshold is unit-testable without constructing an Agent or a network call.
pub(super) fn should_compact(history_len: usize) -> bool {
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
    /// True when no human can answer an approval prompt (headless `chat`/`run`).
    /// The gate then fails closed immediately for tools that would otherwise
    /// prompt, instead of blocking for `APPROVAL_TIMEOUT` on a request nobody
    /// will see; it also tells tools to ignore model-supplied confirmations.
    pub non_interactive: bool,
    /// Input (context) tokens the provider reported for the most recent request,
    /// folded with cache-read/creation tokens. Drives proactive microcompaction:
    /// it is the only *accurate* measure of context occupancy we have (the
    /// system prompt + tool schemas dwarf `history`, so a local byte estimate
    /// would badly undercount). 0 until the first response lands.
    pub last_input_tokens: u64,
    /// Length of `history` as of the most recently built request — i.e. the
    /// prefix the model has already been shown. Microcompaction only sheds
    /// outputs within this prefix, never the just-produced batch a multi-tool
    /// response appended but hasn't been sent back yet (which would hand the
    /// model "[cleared]" for results it never saw). 0 until the first request.
    pub history_seen_len: usize,
    /// Cumulative billed token counts per model for this session, accumulated
    /// from each response's `usage` block. Persisted in the session record so
    /// `/cost` survives a `/resume`. Keyed by model so a mid-session `/model`
    /// switch bills each model at its own rate.
    pub cost_usage: Vec<crate::session::ModelUsage>,
}

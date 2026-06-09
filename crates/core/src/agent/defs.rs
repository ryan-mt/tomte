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
///
/// Also matches HTTP 413, which OpenAI-compatible endpoints (Groq, etc.) return
/// when the request body exceeds the server's size cap — shedding stale tool
/// output is the right recovery there too. We anchor on the canonical reason
/// text ("payload too large" / "request entity too large"), which
/// `reqwest::StatusCode::Display` always appends, rather than the bare number
/// "413": a token count like "Requested 41300" contains "413", and OpenAI's
/// *429* tokens-per-minute rate-limit reads "Request too large …", which must
/// fail over to another model, not shed context. Keeping the needles textual
/// avoids both false positives.
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
        || m.contains("payload too large")
        || m.contains("request entity too large")
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
    use super::{
        is_context_overflow_message, is_stream_transport_error, is_stream_truncation_error,
    };

    #[test]
    fn classifies_413_payload_too_large_as_overflow() {
        // OpenAI-compatible endpoints (e.g. Groq) reject an oversized request
        // body with HTTP 413. `StatusCode::Display` always appends the canonical
        // reason, so the surfaced error carries "Payload Too Large" — treat it as
        // an overflow so the turn sheds stale tool output and retries.
        for msg in [
            "groq 413 Payload Too Large: request body too large",
            "Anthropic 413 Request Entity Too Large",
        ] {
            assert!(
                is_context_overflow_message(msg),
                "should be overflow: {msg}"
            );
        }
    }

    #[test]
    fn does_not_treat_tpm_rate_limit_as_overflow() {
        // OpenAI's 429 tokens-per-minute rate-limit message reads "Request too
        // large …" and embeds a token count ("Requested 41300" contains "413").
        // It must NOT be read as a context overflow — that would shed context and
        // retry the same model instead of failing over. Guards the deliberate
        // choice to match the canonical 413 reason text, never a numeric needle.
        for msg in [
            "OpenAI 429 Request too large for gpt-5.5 on tokens per min (TPM): Limit 30000, Requested 41300",
            "rate_limit_exceeded: too many requests",
        ] {
            assert!(
                !is_context_overflow_message(msg),
                "should NOT be overflow: {msg}"
            );
        }
    }

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
    /// SOUL Pillar 1 — the glass-box pre-flight. Emitted for a consequential
    /// tool call (a write or a shell command) just before it runs, including an
    /// auto-approved one, so every such action is legible *before* it happens
    /// rather than only narrated as it streams. Informational only — the
    /// approval gate is unchanged. `scope` states the blast radius; `leash` is a
    /// one-line safety note when the call is flagged destructive.
    PreFlight {
        call_id: String,
        scope: String,
        leash: Option<String>,
        /// Pillar 5 (A2 Tier 1) — the file's recorded decisions surfaced as
        /// "house rules" when an edit targets it; empty otherwise. Pure recall
        /// at the moment of risk, never a gate.
        house_rules: Vec<String>,
    },
    /// Pillar 5 (A2 Tier 2) — the conscience self-check judged that a pending
    /// edit contradicts a recorded decision. The TUI raises a three-way card
    /// (abort / supersede / edit-anyway) and returns the choice on the matching
    /// `pending_conscience` channel. Interactive only — a headless run proceeds
    /// and logs the override instead (see [`AgentEvent::DecisionOverturned`]).
    ConscienceConflict {
        call_id: String,
        tool_name: String,
        file: String,
        /// The overturned decision's trail id (`ts`), its text, and the model
        /// that recorded it.
        ts: u64,
        prev_decision: String,
        prev_model: String,
        /// The editing model's one-line reason the edit conflicts.
        reason: String,
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
    /// Pillar 5 (A3 — On the Record) — a recorded decision was overturned by an
    /// approved edit. The audit line surfaced to the user; `recorded` is true
    /// when a superseding decision was written to the trail (supersede), false
    /// when the edit proceeded without one (edit-anyway, or a headless run).
    DecisionOverturned {
        file: String,
        prev_decision: String,
        prev_model: String,
        reason: String,
        recorded: bool,
    },
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
    /// A sub-agent reported updated token usage. `output_tokens` is the running
    /// total of tokens the sub-agent's model has generated so far (summed across
    /// the turn's responses); the fleet view shows it in place of a raw step
    /// count, which better reflects how much work a child has actually done.
    SubagentTokens {
        id: String,
        output_tokens: u64,
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
pub(super) const ALWAYS_AUTO_TOOLS: &[&str] =
    &["todo_write", "goal_update", "memory", "record_decision"];

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

/// Build the compaction instruction, optionally steered by a user-supplied
/// focus (`/compact <focus>`). Without a focus this is exactly [`COMPACT_PROMPT`];
/// with one, the focus is appended as an emphasis directive while the
/// self-contained-summary contract is preserved (a blank/whitespace focus is
/// treated as no focus, so `/compact   ` behaves like a bare `/compact`).
pub(super) fn compact_prompt(focus: Option<&str>) -> String {
    match focus.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => format!(
            "{COMPACT_PROMPT}\n\nThe user asked you to pay particular attention to: {f}. \
             Give that extra weight and detail in the summary, but still keep it \
             self-contained so nothing else essential is lost."
        ),
        None => COMPACT_PROMPT.to_string(),
    }
}

/// Appended after a file-changing turn to elicit an auto-captured decision. The
/// model answers as itself (provider-agnostic); the harness parses the reply
/// with [`crate::decisions::parse_captured`]. It asks for ONE JSON object or the
/// literal `NONE`, nothing else, so a routine edit records nothing. Pillar 2 —
/// auto-capture.
pub(super) const CAPTURE_PROMPT: &str =
    "You just finished a turn that changed files in this project. If — and only if — you made a \
     NON-OBVIOUS decision worth preserving for whoever (or whichever model) works here next — a \
     real trade-off, a constraint you honored, or a design choice with genuine rejected \
     alternatives — record it. Output exactly ONE JSON object on a single line and nothing else: \
     {\"loc\":\"path/file.ext:line\",\"decision\":\"the choice in one line\",\"why\":\"the reasoning\",\
     \"rejected\":[\"alternative -> consequence\"]}. Use a real file:line you just touched. If the \
     change was routine, mechanical, or self-evident, output exactly NONE. No prose, no markdown — \
     only the JSON object or NONE.";

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

/// The auto-capture signal a landed tool call carries (Pillar 2). A file edit
/// marks a turn as worth a capture self-check; `record_decision` marks that the
/// model already captured the why itself, so the self-check is skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureKind {
    Mutated,
    Recorded,
}

/// Classify a landed tool call for auto-capture (Pillar 2). Returns `None` for
/// tools that neither edit files nor record a decision, so they never trigger
/// the end-of-turn self-check. Mirrors the file-mutation set the
/// pre-flight/house-rules path keys on, deliberately excluding `undo_last_edit`
/// and `notebook_edit` (an undo or a notebook tweak rarely embodies a decision).
pub(super) fn capture_kind(tool_name: &str) -> Option<CaptureKind> {
    match tool_name {
        "edit_file" | "write_file" | "multi_edit" => Some(CaptureKind::Mutated),
        "record_decision" => Some(CaptureKind::Recorded),
        _ => None,
    }
}

/// Whether the end-of-turn auto-capture self-check should run (Pillar 2). True
/// only when auto-capture is on, a real edit landed, and the model didn't
/// already record a decision itself — and never in an unattended headless run,
/// where the replayed trail is a prompt-injection vector (the same gate the
/// `record_decision` tool enforces). Pure, so the guard is unit-testable apart
/// from the network self-check it gates.
pub(super) fn should_auto_capture(
    auto_capture: bool,
    non_interactive: bool,
    require_approval: bool,
    turn_mutated: bool,
    turn_recorded: bool,
) -> bool {
    if !auto_capture || !turn_mutated || turn_recorded {
        return false;
    }
    !(non_interactive && require_approval)
}

/// The human's resolution of an [`AgentEvent::ConscienceConflict`] card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConscienceChoice {
    /// Don't run the edit.
    Abort,
    /// Run the edit and record a superseding decision linked to the overturned one.
    Supersede,
    /// Run the edit without recording a superseding decision (the override is
    /// still surfaced/logged).
    EditAnyway,
}

/// A conscience conflict found for one pending edit (Pillar 5 A2 Tier 2),
/// carried from the pre-check to the approval gate.
pub(super) struct ConscienceConflictInfo {
    pub file: String,
    pub ts: u64,
    pub prev_decision: String,
    pub prev_model: String,
    pub reason: String,
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
    /// Lifecycle hooks loaded from `~/.config/tomte/settings.json`. Pre-tool
    /// hooks can block a tool call by exiting with code 2; the model receives
    /// the hook's stdout as the block reason.
    pub hooks: Arc<crate::hooks::HookSet>,
    pub pending_approvals:
        Arc<Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    /// Pending conscience-conflict cards, keyed by call_id — the three-valued
    /// sibling of `pending_approvals` (which is bool-only), so the abort /
    /// supersede / edit-anyway choice can be returned distinctly. Pillar 5 (A2).
    pub pending_conscience: Arc<
        Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<ConscienceChoice>>>,
    >,
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
    /// Rewind points, one per user turn (chronological). Each records where the
    /// conversation and the undo stack stood as the turn began, so `/rewind` can
    /// restore both. In-memory only (it indexes the non-persisted undo stack);
    /// reset on `/resume` and `/clear`, and dropped past the rewound-to point.
    pub checkpoints: Vec<crate::tools::Checkpoint>,
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn capture_kind_maps_edits_and_record_decision() {
        assert_eq!(capture_kind("edit_file"), Some(CaptureKind::Mutated));
        assert_eq!(capture_kind("write_file"), Some(CaptureKind::Mutated));
        assert_eq!(capture_kind("multi_edit"), Some(CaptureKind::Mutated));
        assert_eq!(capture_kind("record_decision"), Some(CaptureKind::Recorded));
        // Reads, shell, undo, and session-only tools don't signal a captured edit.
        for t in [
            "read_file",
            "grep",
            "run_shell",
            "undo_last_edit",
            "notebook_edit",
            "memory",
            "todo_write",
        ] {
            assert_eq!(capture_kind(t), None, "{t} should not signal capture");
        }
    }

    #[test]
    fn should_auto_capture_requires_an_edit_and_no_self_record() {
        // Happy path: edited, model didn't record, enabled, interactive.
        assert!(should_auto_capture(true, false, false, true, false));
        // Disabled by config.
        assert!(!should_auto_capture(false, false, false, true, false));
        // No edit landed this turn.
        assert!(!should_auto_capture(true, false, false, false, false));
        // The model already recorded a decision itself — don't double up.
        assert!(!should_auto_capture(true, false, false, true, true));
    }

    #[test]
    fn should_auto_capture_respects_the_unattended_headless_gate() {
        // non_interactive + require_approval = unattended headless: the replayed
        // trail is an injection vector, so stay off even on a real edit.
        assert!(!should_auto_capture(true, true, true, true, false));
        // A non-interactive run that cleared require_approval (e.g. skip-perms)
        // is allowed, mirroring the `record_decision` tool's own gate.
        assert!(should_auto_capture(true, true, false, true, false));
    }
}

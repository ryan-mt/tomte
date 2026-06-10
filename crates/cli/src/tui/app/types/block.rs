/// SOUL Pillar 1 — the glass-box pre-flight attached to a consequential tool
/// call: a one-line scope statement (the blast radius) shown before the call
/// runs, plus an optional leash (a safety note for a flagged-destructive call).
#[derive(Debug, Clone)]
pub struct PreFlight {
    pub scope: String,
    pub leash: Option<String>,
    /// Pillar 5 (A2 Tier 1) — the file's recorded decisions, surfaced as
    /// "house rules" when an edit targets it; empty otherwise.
    pub house_rules: Vec<String>,
    /// Pillar 3 — the Context Manifest shown on a session's first edit to a
    /// file: pulling X because <edge> · read/not read, leaving out Y because
    /// <reason>. Empty otherwise.
    pub context_manifest: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Welcome,
    User(String),
    Assistant {
        text: String,
        reasoning: String,
        /// Whether the assistant's text/reasoning is done (i.e. moved to history).
        done: bool,
        /// Set when reasoning has been collapsed: the value is the elapsed seconds.
        thought_for_secs: Option<u64>,
        /// Marks the moment we received the first reasoning event for this block.
        reasoning_started_at: Option<std::time::Instant>,
        /// When the block has collapsed to a "Thought for Xs" line, whether the
        /// user has clicked it open to re-show the retained reasoning text below
        /// it. Toggled via [`App::thought_rows`] click hit-testing.
        thinking_expanded: bool,
    },
    Tool {
        call_id: String,
        name: String,
        args: String,
        output: Option<String>,
        error: bool,
        /// SOUL Pillar 1 — the pre-flight card, attached when the `PreFlight`
        /// event arrives (just before a write/shell call runs). `None` for a
        /// read-only call and until that event lands.
        preflight: Option<PreFlight>,
    },
    System(String),
    /// Pre-styled, fixed-layout content (e.g. the `/context` report). The lines
    /// are built once with their own colors/indentation and rendered verbatim,
    /// so they don't go through the text-wrap path.
    Rich(Vec<ratatui::text::Line<'static>>),
}

/// An in-progress `/buddy` hatch animation: a wobbling egg that cracks and
/// reveals the account's companion, after which the small corner buddy takes
/// over. Time-driven, like the spinner, so the frame follows wall-clock.
#[derive(Debug, Clone)]
pub struct HatchAnim {
    pub pet: usize,
    pub started: std::time::Instant,
}

/// One live sub-agent row in the fleet view, populated from `Subagent*` events
/// forwarded by `dispatch_agent`.
#[derive(Debug, Clone)]
pub struct SubagentView {
    pub id: String,
    pub kind: String,
    pub prompt: String,
    pub activity: String,
    /// Running total of tokens the sub-agent's model has generated, mirrored
    /// from `AgentEvent::SubagentTokens`. Shown in the fleet view instead of a
    /// raw step count.
    pub tokens: u64,
    pub started_at: std::time::Instant,
    /// None while running; `Some(ok)` once the sub-agent finishes.
    pub done: Option<bool>,
    /// Toggled by clicking the row — shows the full prompt + status detail.
    pub expanded: bool,
}

/// Wrapped lines of the transcript's STABLE prefix — every block before the
/// live turn (all of them when idle). The live tail (the streaming turn, the
/// only part that mutates) is re-wrapped every frame from scratch, so no event
/// handler ever needs to invalidate this cache by hand: `render_chat` validates
/// the prefix each frame with a cheap fingerprint fold and rebuilds or extends
/// it as the boundary moves. Keyed on width and the display toggles; a
/// `/thinking` or expanded-tools flip produces a new key and forces a re-wrap.
#[derive(Clone)]
pub struct ChatRenderCache {
    pub inner_width: usize,
    pub expanded_tools: bool,
    pub show_thinking: bool,
    /// Fold of the live App state the Welcome card renders from (model, effort,
    /// auth, cwd, pet). The Welcome block itself never mutates, so without this
    /// a `/model` switch or login would keep showing the stale card.
    pub welcome_fp: u64,
    /// `lines` covers `blocks[..stable_blocks]`.
    pub stable_blocks: usize,
    /// Order-sensitive fold of `block_fingerprint` over the covered prefix.
    /// A mismatch on the next frame (e.g. `/resume`/`/rewind` replaced the
    /// transcript wholesale) drops the cache.
    pub stable_fp: u64,
    pub lines: Vec<ratatui::text::Line<'static>>,
    /// `(flat line index into `lines`, absolute block index)` for each collapsed
    /// "Thought for Xs" line in the cached prefix — the click targets, computed
    /// once when the slice is appended (same append-only discipline as `lines`).
    /// `render_chat` maps these (plus the live tail's) to screen rects each frame.
    pub thought_marks: Vec<(usize, usize)>,
}

# Pillars 1–3 — build plan (after 0.0.2)

> Build-ready designs grounded in the real code. Companion to `pillar-4-calm-terminal.md`.
> **Hard rule:** build after 0.0.2 (2026-06-08). Keep vibe-coding + a foreground fallback.
> Each pillar has a runnable preview under `docs/previews/` showing the target.

## Pillar 1 — Glass-box: legible & bounded

- **Hook:** `crates/core/src/agent/toolphase.rs:6` `run_tool_phase`. Tools are split into a
  runnable set (`toolphase.rs:45`) then executed; `ToolContext.events` (`toolphase.rs:36`) is
  the live UI channel.
- **Change:** before executing the runnable set, emit a new `AgentEvent::PreFlight { intent,
  target, writes, scope, est }` per call; the TUI renders a calm pre-flight card (Pillar-4
  palette). Derive the fields from tool metadata already present — `is_read_only()`,
  `danger_reason()`, and the args (file path for edits, command for shell). Auto-approved
  actions **still** show the card (legible, not silent). The approval gate
  (`approval_outcome`, `toolphase.rs:128-211`) is **unchanged** — we add visibility, not friction.
- **Verify:** a real run where every action shows intent/scope/cost *before* it happens; a
  read-only batch shows "0 writes"; an edit shows the one file + line delta. TUI render test via
  `TestBackend` asserting the card; behaviour identical when the card is off. Preview: `glass_box.rs`.
- **Safety:** purely additive event + render; gate logic untouched.

## Pillar 2 — Memory of why (the decision trail)

- **Hook:** `SessionSnapshot` (`crates/core/src/session.rs:43`) persists per session; the memory
  store (`crates/core/src/tools/memory.rs`, `~/.config/tomte/projects/*/memory/`) persists
  cross-session.
- **Change:** add a **provider-independent** decision store —
  `~/.config/tomte/projects/<key>/decisions.jsonl`, append-only, one
  `DecisionRecord { loc, decision, why, rejected[], model, turn, ts }` per entry (exactly the
  round-trip the `why_trail.rs` preview already proves). A `record_decision` tool (or
  auto-capture on edit) writes; a `/why` command and `tomte why <loc>` read. On a mid-session
  model switch the trail is re-injected so the new model inherits the *reasoning*, not a lossy
  summary — **this is the multi-model moat**.
- **Verify:** write a decision under one model, switch models mid-session, confirm
  `tomte why <loc>` returns it verbatim and the new model can cite it. `why_trail.rs` already
  demonstrates the persistence round-trip.
- **Safety:** additive new file + new tool; existing session/memory formats untouched.

## Pillar 3 — A voice with a spine

- **Hook:** `default_system_prompt` (`crates/core/src/agent/usage.rs:142`). The
  harness-not-persona rule (`usage.rs:143`) **stays**.
- **Change:** add a `# Voice` section — push back on bad ideas, state confidence ("~70% sure"),
  anchor claims to receipts (numbers, versions, `file:line`), no sycophancy or emoji. Calibrate
  per provider by **deriving from the selected model's provider** (never hardcode a model), so
  the stance survives a model switch. The voice belongs to the product; the model still answers
  truthfully as itself.
- **Verify:** A/B the same "bad idea" input against the current prompt (expect push-back +
  confidence); check the stance is consistent across providers via headless
  `chat --output-format json` on both. Preview: `voice.rs`.
- **Safety:** prompt-only, fully reversible.

## Cross-cutting — cost receipts

Normalized per-turn / per-session cost across providers, built on `ModelUsage`
(`session.rs:60`). Supports Pillars 1 and 4. Additive display only.

## Order & risk (lowest-risk first)

1. **Pillar 4** — calm terminal (`pillar-4-calm-terminal.md`).
2. **Pillar 1** — glass-box: additive event + render. Low risk.
3. **Pillar 3** — voice: prompt-only, reversible.
4. **Pillar 2** — decision trail: new store + tool, the most surface. **Design-and-approve before coding.**

Each lands on a branch, verifies (build + test + a real run), and merges only when green.
Foreground vibe-coding stays the fallback at every step.

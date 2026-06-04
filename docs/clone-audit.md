# Tomte — Clone-Tell Audit

> Where tomte still reads as a Claude Code / Codex clone, and the call on each.
> Checklist from the 2026 landscape research (see `SOUL.md` §2, §10). Date: 2026-06-04.
> **Verdicts:** CHANGE (after 0.0.2) · KEEP (healthy convergence / interop) · DECIDE (needs a human call) · DONE.

## Branding / identity
- **Name "tomte"** — does NOT piggyback a frontier brand (unlike OpenClaude / Claw-…). A real
  point of difference already. → **KEEP.**
- **`/buddy` pixel companion** (`/pet`) — original, not a borrowed mascot. → **KEEP** as light
  character; the real soul is the voice (SOUL §5, Pillar 3), not the pet.
- **README "drop-in for Claude Code" + "lives in your terminal"** (`README.md:5,7`) — verbatim
  Claude Code framing/tagline. → **DONE** (removed 2026-06-04).
- **System-prompt identity:** *"interactive CLI coding agent… an engineer, not a chatbot"*
  (`crates/core/src/agent/usage.rs:143`) — the generic Codex/Claude Code stance, no opinion of
  its own. → **CHANGE** (after 0.0.2): give it the Pillar-3 voice. The harness-not-persona rule
  (`usage.rs:143`) stays.

## Copied UX surface (the biggest cluster)
- **Slash commands** — ~40% share Claude Code's exact names: clear, compact, config, cost,
  doctor, init, login, logout, mcp, memory, model, resume, review, status, agents
  (`crates/cli/src/tui/app/slash*.rs`). → **DECIDE.** Tension: the project previously wanted
  Claude Code *parity* (familiar muscle memory) vs distinctiveness now. Research says
  *convergence is fine; carbon-copying specifics is the tell.* Recommendation: keep the standard
  verbs (clear/compact/model/resume are genuine industry conventions) but make the *experience
  behind them* tomte's (a calm, tidy `/compact`; a glass-box `/commit`). Don't rename for its
  own sake — that's churn, and it breaks muscle memory.
- **Tool names** — read_file / edit_file / write_file / multi_edit / grep / glob / run_shell:
  same semantics as Claude Code's Read/Edit/Write/Bash. → **KEEP** (now industry-standard;
  renaming breaks `AGENTS.md`/skill interop for no gain).
- **Concepts lifted whole** — plan mode, accept-edits, todo_write, dispatch_agent, hooks, MCP.
  → **KEEP** as healthy convergence; these are the 2026 baseline, not a tell by themselves.

## Ecosystem interop (clone, or feature?)
- tomte reads `~/.claude/`, `~/.codex/`, `CLAUDE.md`, `AGENTS.md`, and discovers Claude/Codex
  agents + skills (`crates/core/src/memory.rs`, `usage.rs:194`). → **KEEP, but reframe.** This is
  *interop*, not cloning — a migration on-ramp (the healthy version of what Hermes'
  `claw migrate` does cynically). Position it as "works with your existing setup," never "a
  replacement for X."

## TUI craft (the felt clone-tell)
- **Alt-screen takeover** (`crates/cli/src/tui/app/entry.rs`) destroys terminal scrollback — the
  exact thing Claude Code / Gemini CLI are mocked for. → **CHANGE** (after 0.0.2): Pillar 4
  inline viewport. Highest-value distinctiveness move.
- **Streaming chat transcript as the only surface** → the generic AI-TUI look. → **CHANGE**
  (after 0.0.2): Pillar 1 glass-box pre-flight + Pillar 4 "left in order" summary.

## Bottom line
- **Already fixed (DONE):** README framing.
- **CHANGE after 0.0.2:** system-prompt voice (Pillar 3) · alt-screen → inline viewport
  (Pillar 4) · streaming-only surface (Pillars 1 & 4).
- **KEEP (healthy):** the name · `/buddy` · standard tool/command verbs · ecosystem interop.
- **DECIDE with the user:** how far to diverge slash-command naming (parity vs distinctiveness).

The key finding, matching `SOUL.md` §2: **tomte's clone problem is NOT its feature set** — those
are industry-standard and worth keeping. It's the borrowed *soul* (generic voice), the *terminal
craft* (alt-screen), and the *framing* (now fixed). De-cloning = fix voice + terminal craft +
framing. Renaming standard verbs would just be churn.

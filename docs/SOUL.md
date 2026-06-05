# Tomte — The Soul Document

> What tomte is *for*, and why it is not another coding-agent clone.
> This is the product's source of truth for identity and direction.
> Code implements this; when they disagree, this document wins until amended.
> **v2 (2026-06-04)** — rewritten after a 4-way landscape research sweep. Evidence in §10.

## 1. The problem we are solving

Tomte today is, architecturally, **Claude Code rewritten in Rust** — same shape, same stance.
A faithful clone is still a clone. Worse: the first "fix" we reached for — *lead with
multi-model* — turns out to be just as generic. The research below reshaped this document, so
the direction here is grounded in what the market actually looks like, not a hunch.

## 2. What the research found (so we don't fool ourselves)

- **The 2026 generic baseline.** Nearly every CLI agent now ships: terminal chat TUI + slash
  commands + read/edit/bash loop + plan mode + MCP + approval gates + subagents +
  `AGENTS.md`/`CLAUDE.md` rules + git auto-commit. **Tomte has all of it** → it sits in the
  saturated center.
- **Multi-model is table stakes, not a differentiator.** OpenCode (75+ providers), Crush
  (30+), Cline (30+), Aider (70+), Goose (15+) already own "BYOK multi-model terminal agent" —
  the *most* crowded quadrant. Leading with multi-model means joining the pile. So multi-model
  must become an **enabler**, never the pitch.
- **The loudest open complaints** (HN/Reddit, 2026): context/**decision amnesia** ("why did it
  do that — and where did the reasoning go?"); **anti-spectacle** fatigue ("one bounded agent,
  one clear job" beats orchestration vanity); opaque **cost**; sycophantic, generic **voice**.
- **The open lane is the opposite of the autonomy race.** What's underserved is *legible,
  bounded, persistent, opinionated* tools. Background/async "hand-off" agents are **crowded**
  (Cursor Cloud, Devin, Warp Oz) and the wrong direction for us.
- **Clone tells to avoid:** a name piggybacking a frontier brand; "the open-source <BigTool>"
  framing; **"drop-in replacement" framing** (tomte's own `README.md:7` says *"a drop-in for
  Claude Code"*); copying another tool's UX verbatim (same slash names, same `CLAUDE.md`);
  having no opinion of your own.
- **TUI craft.** Claude Code and Gemini CLI are widely mocked for an alt-screen takeover that
  destroys terminal scrollback and a redraw-the-world loop that flickers. **Tomte currently
  uses the alternate screen** (`crates/cli/src/tui/app/entry.rs`) → it's in the same trap. The
  fix (an inline viewport that leaves output in scrollback) maps perfectly to our soul.

## 3. The soul: the Quiet Custodian (refined by evidence)

The name chose the philosophy; the research sharpened it. A *tomte* (Nordic house spirit)
keeps the home tidy, asks only to be respected, and is never showy.

The refinement: tomte is **not** "a worker that runs in the background while you sleep" — that
lane is crowded. It is **a calm, legible custodian that keeps your codebase: you can always
follow what it's doing and why, and it remembers.** That maps 1:1 onto *Calm Technology*
(Weiser/Case) **and** onto every open complaint above.

- The others are **"a colleague you chat with,"** racing toward autonomy you can't follow.
- Tomte is **"a custodian you can follow"** — present, legible, tidy, opinionated, with memory.

We **keep vibe-coding**: we lean *into* the chat loop, not escape it. The custodian is *how*
tomte vibe-codes — calmly and legibly — not a replacement for it.

## 4. Hard constraints (do not break these)

1. **Soul belongs to the harness, not the model.** The model answers truthfully as itself and
   never claims to be "tomte" (`crates/core/src/agent/usage.rs:143`). No per-model
   special-casing — provider-agnostic throughout.
2. **Multi-model is plumbing, never the pitch.** Tomte stays genuinely multi-model (OpenAI +
   Anthropic, `/model` mid-session) — but we *use* it to enable things single-vendor tools
   can't (Pillar 2, cross-provider cost), and never advertise it as the differentiator.
3. **Keep vibe-coding foreground.** Chat + watch-it-work stays the default and the heart.
4. **No core changes before 0.0.2 (ships 2026-06-08).** Stability first; soul-bets staged after.

## 5. The pillars

Each pillar is an *open* lane (evidence in §10) that also *is* the custodian. Together they
form one stance, not a feature pile. Pillars 1–4 are the foundation; Pillar 5 is a newer lane that
builds *on top of* them (and is staged last).

**Pillar 1 — Glass-box: legible & bounded.**
Narrate intent, scope, and cost *before* acting; show the blast radius; no silent multi-minute
churn. The agent you can actually follow. (Answers anti-spectacle + review-burden.) This is the
custodian being *followable*.

**Pillar 2 — Memory of "why": the decision trail.**
Persist not just *what* changed but *why* — rejected alternatives, constraints, trade-offs —
queryable, surviving across sessions **and across model switches**. When tomte moves from one
model to another mid-task, the new model inherits the reasoning, not a lossy summary. **This is
where multi-model becomes a real moat.** (Answers the #1 complaint: decision amnesia.) Builds on
tomte's existing memory store (`crates/core/src/tools/memory.rs`,
`~/.config/tomte/projects/*/memory/`). The custodian *remembers the house*.

**Pillar 3 — A voice with a spine.**
Opinionated, pushes back on bad ideas, admits uncertainty plainly ("I'm ~70% sure"), anchors
claims to receipts (numbers, versions) — **not** emoji or a mascot. (Answers sycophantic/generic
voice; avoids the clone tell of a bolted-on cute companion.) Calibrated per provider so the
voice survives a model switch. The custodian is *respected and honest*.

**Pillar 4 — Calm, tidy terminal.**
Inline viewport that leaves finished turns in native scrollback (not an alt-screen takeover); a
one-line "left in order" summary at end of turn; a disciplined, calm palette (achromatic base +
one muted accent); notifications only at decision points; the diff shown before it's applied.
(Fixes the real, widely-cited Claude Code/Gemini scrollback+flicker complaint.) The custodian
*leaves the room tidy*. Built on the existing ratatui/crossterm stack.

**Pillar 5 — The custodian's conscience: the active decision trail.** *(Proposed; builds on 1, 2 & 4.)*
The Pillar-2 trail today is inert — replayed verbatim, never reconciled against the code it describes.
Pillar 5 makes it *active*: it reconciles itself against the working tree (a decision never cites code
that moved or vanished), surfaces a file's recorded decisions in the Pillar-1 pre-flight *before* an
edit could break one, and lets **only a human** overturn a decision — on the record, in the Pillar-4
end-of-turn summary. A promise one model made weeks ago confronts the edit another model is about to
make today; that needs durable rejected-alternatives + a model stamp + cross-model replay + an active
gate all at once, so no single-vendor or write-only-memory tool can copy it. (The endpoint of
"multi-model as plumbing.") Design: `docs/pillar-5-conscience.md`. The custodian *keeps its own promises*.

*Cross-cutting enabler:* normalized **cost/token receipts** across providers (a multi-model-only
advantage) support Pillars 1 and 4.

## 6. What we deliberately avoid (saturated lanes)

Multi-model **as a headline**; background/async autonomy; spec-driven development; multi-agent
orchestration; visual TUI "glamour" (Charm/Crush own it — we compete on *voice and legibility*,
not split-pane styling); "drop-in for <BigTool>" framing; and copying another tool's UX naming
verbatim. Where we adopt now-standard mechanics (slash commands, plan mode, MCP), we implement
them in tomte's own idiom.

## 7. What already resonates (build on, not against)

- The **memory store** — the seed of Pillar 2.
- **Surgical-edit discipline** in the prompt — already Pillar 1's ethos.
- The hardened **sandbox** stays for safety, but it is *no longer framed as a background-autonomy
  enabler* — its job now is bounding the glass-box (Pillar 1).
- The `/buddy` pixel companion is a light, existing bit of character — keep it playful, but the
  real "soul" is Pillar 3 (judgment), not the mascot.

## 8. Roadmap — staged by risk and timing

**Hard rule: nothing touches the core before 0.0.2 (ships 2026-06-08). Stability first.**

### Stage 0 — Now → 0.0.2: direction only, zero core risk
- [x] This document (v2) — the spec everything else implements.
- [x] Reposition README + website away from clone framing toward the custodian / multi-model
      stance — clone-tells removed, both build green.
- [x] Audit tomte against the clone-tells in §2 → `docs/clone-audit.md`.
- [x] Runnable, std-only previews of all 4 pillars → `docs/previews/` (verified compile + run).
- Verify: repo builds green; no behavioral diff. ✓ cargo test + release + next build all exit 0.

### Stage 1 — After 0.0.2: first visible pillar, small surface
- Recommended: **Pillar 4** (inline viewport + "left in order" summary) — design ready in
  `docs/pillar-4-calm-terminal.md`. Lowest risk, highest felt difference, fixes a real, named
  complaint. Alternative: **Pillar 1** pre-flight intent/scope/cost line.
- Build-ready designs for Pillars 1–3 are in `docs/pillars-1-3-build-plan.md`.
- Build on the existing turn path; keep current behavior as fallback.
- Verify: a real run showing the new behavior + tests; old behavior intact.

### Stage 2 — Pillar 2 (decision trail): the moat
- Extend the memory store to capture *why*, queryable, cross-model. **Design-and-approve before
  coding.**
- Verify: switch models mid-task, confirm the reasoning carries over; `tomte why` answers from
  the trail, not a re-derivation.

### Stage 3 — Pillar 3 (voice) + cost receipts
- Per-provider voice calibration; normalized cross-provider cost display.
- Verify: consistent stance across a model switch; cost figures reconcile with provider usage.

### Stage 4 — Pillar 5 (the conscience): the active decision trail
- Make the Pillar-2 trail self-auditing: reconcile against the tree, confront edits in the Pillar-1
  pre-flight, log human overrides in the Pillar-4 summary. **Depends on Pillars 1 & 4;
  design-and-approve before coding.** Full design in `docs/pillar-5-conscience.md`.
- Verify: a decision self-heals after its code moves; an edit that contradicts a recorded decision is
  surfaced before it lands; only a human clears it, and the override appears in the end-of-turn summary.

Every stage keeps the vibe-coding loop and a foreground fallback.

## 9. The one-line identity

> **"The coding agent you can actually follow — it tells you what it's about to do, what it'll
> cost, why it chose it, and remembers that reasoning across every model and session."**

## 10. Evidence (research sweep, 2026-06-04)

- Landscape / generic baseline: OpenCode `opencode.ai`, Charm Crush `github.com/charmbracelet/crush`,
  Aider `aider.chat`, Cline `cline.bot`, Goose `github.com/block/goose`, Amp `ampcode.com`.
- Clone tells / openclaw+hermes: `en.wikipedia.org/wiki/OpenClaw`, `github.com/nousresearch/hermes-agent`,
  `dev.to/soulentheo/every-ai-coding-cli-in-2026-the-complete-map-30-tools-compared-4gob`.
- Open complaints / niches: HN `news.ycombinator.com/item?id=46844822` (decision memory, cost,
  branching), `news.ycombinator.com/item?id=46286057` (TUI scrollback/flicker),
  `mindstudio.ai/blog/context-rot-ai-coding-agents-how-to-prevent`, `me2resh.com/blog/agent-decision-records`.
- TUI craft / calm tech: `ratatui.rs/examples/apps/inline/`, `charm.land/blog/v2/`,
  `bwplotka.dev/2025/lazygit/`, `ixdf.org/literature/topics/calm-computing`.

# Pillar 5 — The Custodian's Conscience: the Active Decision Trail (design)

> A 5th soul-lane proposed after a 2026-06-04 ideation sweep (8 creative lenses, 31 concepts,
> adversarial critique). Design-only; **build after 0.0.2** (ships 2026-06-08) and **after**
> Pillars 1 and 4 — it depends on Pillar 1's pre-flight event and Pillar 4's end-of-turn summary
> existing first. See `SOUL.md` §5 (Pillars 1–4) and `pillars-1-3-build-plan.md`.
> **Hard rule unchanged:** no core changes before 0.0.2. This document is the spec, not a green light.

## The one move

The four pillars make tomte **legible, remembering, opinionated, and calm**. They leave one lane
open that no tool in the mold (Claude Code / Codex / OpenCode) can copy: make the decision trail
**active and self-auditing** instead of a write-only graveyard.

Today (Pillar 2, shipped) the trail records *why* and re-injects it across sessions and model
switches. But it is inert: it is replayed verbatim, never reconciled against the code it describes,
and the agent is never confronted with a past decision *before* it breaks one. The conscience lane
closes that loop:

> A promise GPT-5.5 recorded weeks ago physically interrupts an edit Opus is about to make today —
> and only a human may overturn it, on the record.

That single property requires all four moat pieces at once — durable rejected-alternatives + a model
stamp + cross-model replay + an *active* pre-flight gate — so it is structurally unforgeable by any
single-vendor or write-only-memory tool. This is the natural endpoint of "multi-model as plumbing,
never the pitch" (`SOUL.md` §4).

## The gap, grounded in our code

- `DecisionRecord` (`crates/core/src/decisions.rs:21`) is `loc / decision / why / rejected / model /
  ts` — **no content anchor, no status, no link**. `loc` is a frozen `file:line` string.
- `for_loc` (`decisions.rs:85`) matches by **exact string equality** (`d.loc == needle`). The moment
  code at `src/auth.rs:88` shifts to line 90, `tomte why src/auth.rs:88` returns nothing and the loc
  is silently stale.
- `apply_trail_to_prompt` (`decisions.rs:165`, called every session via
  `lifecycle.rs:158` → `refresh_system_context:188`) injects the raw trail with **zero validation
  that each `loc` still exists**. So tomte already feeds the model stale citations *as authority* —
  a real, shipped defect, not a hypothetical.
- The agent is the only writer (`record_decision`, `crates/core/src/tools/decision.rs`) and the only
  reconciler — which today is nobody. Nothing ever checks the trail back against the tree.

The conscience lane is built on three additive moves: **A1 reconcile the trail**, **A2 confront the
edit**, **A3 keep the override honest**.

## A1 · Drift Watch — the trail audits itself against the code

The trail must never cite reasoning for code that no longer exists.

- **Change:** add `anchor: Option<String>` to `DecisionRecord` (serde `default`, so every existing
  `decisions.jsonl` line still loads). `record_decision` snapshots the trimmed text of the line(s)
  at `loc` when it writes. A reconcile pass walks each record:
  - **present** — the line at `loc` still matches the anchor → silent.
  - **moved** — the anchored text is found elsewhere in the same file, *uniquely* → re-anchor the
    `loc` to the new line and rewrite the record. Silent (a tidy house needs no announcement).
  - **gone / altered / ambiguous** — not found, or found in 2+ places → surface **one** calm opener
    line ("3 decisions no longer match their code — `tomte why --reconcile`"), never a gate.
- **Hook:** content-search, not line-pin — this is what kills line-drift (the critic flagged
  line-pinning as the original slop). New `decisions::reconcile(cwd) -> ReconcileReport` reusing
  `load_at` (`decisions.rs:74`); rewrite is a full-file rewrite of `decisions.jsonl` (small, append
  format is already line-oriented). Ship as `tomte why --reconcile` (extend `commands/why.rs:8`) plus
  an **opt-in** startup check that prints at most the one opener line.
- **Verify:** record a decision at `src/x.rs:10`; insert lines above it; run `--reconcile` → the loc
  self-heals to the new line and `tomte why src/x.rs:<new>` returns it. Delete the line → exactly one
  opener line, never a stack trace, never a block. A record with no `anchor` (old format) is left
  untouched, not dropped.
- **Safety:** additive field; reconcile is read-mostly and explicitly invoked or opt-in; the inert
  Pillar-2 behavior remains the fallback when reconcile is off.

## A2 · The Reckoning — an old promise confronts the edit about to break it

Two tiers, escalating in cost and in how rarely they fire.

- **Tier 1 (free, always-on, cannot false-positive):** when a mutating tool (`edit_file`,
  `write_file`, `multi_edit`) targets file `F`, the **Pillar-1 pre-flight card** surfaces every
  decision recorded for `F` as a quiet *"house rules for this file"* note. This is pure surfacing —
  zero detection logic, so it can never be wrong — and it forces the agent to re-read its own
  recorded constraints at the exact instant it would violate one (the #1 mold complaint: memory is
  recorded and then ignored). Needs a new `decisions::for_file(cwd, "src/auth.rs")` that matches on
  the **file** component of `loc`, not the frozen `file:line` (drop exact-equality here).
- **Tier 2 (opt-in conscience, fires rarely):** when Tier 1 surfaced decisions for `F`, the harness
  issues *one* cheap self-check to the **same model making the edit**: "here is the edit; here are
  the recorded decisions for this file; does the edit contradict any? Answer `CONFLICT <ts> — <one
  sentence>` or `CLEAR`." Because the editing model uses its own semantic judgment, it catches "this
  adds a `panic!`" even when the decision text never said the word "panic" — something a substring
  lint (the rejected original) cannot. On `CONFLICT`: a supersede/abort card (reuses the existing
  approval gate, not a new one).
- **Hook:** depends on Pillar 1's `AgentEvent::PreFlight` landing first (planned in
  `pillars-1-3-build-plan.md`; the insertion point is `run_tool_phase`, `toolphase.rs:6`, right
  before the `runnable` set is executed). The trail block for `F` is rendered into that card. Tier 2
  is one extra model call gated behind "this file has decisions," so the median edit pays nothing.
- **Why it's tomte:** the soul belongs to the harness — the harness *asks*, the model *answers* as
  itself, provider-agnostic, calibrated per-provider for free (`SOUL.md` §4). It is conscience, not
  recall.
- **Verify:** record "reject bcrypt, use argon2" for `src/auth.rs`; ask tomte to switch to bcrypt →
  Tier 1 shows the house rule in the pre-flight; Tier 2 returns `CONFLICT` with a one-line reason;
  the supersede/abort card appears. An unrelated edit to the same file returns `CLEAR` and shows no
  card. With Tier 2 off, Tier 1 still surfaces the rule.
- **Safety:** Tier 1 is render-only over an event that already exists once Pillar 1 ships — the
  approval gate logic is untouched (visibility, not friction). Tier 2 is opt-in and skippable.

## A3 · On the Record — only a human overturns a decision, and it is logged as one

The payoff that turns A1/A2 from a nag into a moat: a spine the agent can talk itself past is
decoration.

- **Change:** when an edit supersedes a recorded decision, the agent **may not silently clear its own
  gate**. The contradiction is logged and surfaced to the *user* in the **Pillar-4 end-of-turn
  summary** ("overturned `src/auth.rs` — GPT-5.5's *reject bcrypt for argon2* — reason: argon2 dep
  dropped"). A new `supersedes: Option<u64>` field (serde `default`) on the superseding
  `DecisionRecord` links it to the `ts` of the one it overturned, so the trail becomes
  **git-blame-for-conscience**: an audit log of promises kept and deliberately broken.
- **Hook:** `supersedes: Option<u64>` on `DecisionRecord` (`decisions.rs:21`); rendered into the
  Pillar-4 "left in order" line emitted on `AgentEvent::TurnComplete` (`defs.rs:283`,
  `toolphase.rs:21`). `tomte why <loc>` shows the supersede chain; the older record is kept (history),
  not deleted.
- **Headless:** unattended runs narrate-and-proceed and log "edited over decision #<ts> without
  override" — turning the limitation into a feature: *decisions someone stepped on while you weren't
  watching*, readable after the fact.
- **Verify:** approve a superseding edit interactively → the end-of-turn summary names the overturned
  decision and the new record carries `supersedes`. Run the same headless → it proceeds and the
  override is in the log. The agent cannot clear the gate without the human (interactive) or an
  explicit skip flag (headless).
- **Safety:** additive field + one summary line; no change to approval mechanics beyond what A2 adds.

## Data-model change (one struct, all additive)

```rust
// crates/core/src/decisions.rs — DecisionRecord, all new fields serde(default)
pub struct DecisionRecord {
    pub loc: String,
    pub decision: String,
    pub why: String,
    #[serde(default)] pub rejected: Vec<String>,
    pub model: String,
    pub ts: u64,
    #[serde(default)] pub anchor: Option<String>,     // A1: snapshot of the line(s) at record time
    #[serde(default)] pub supersedes: Option<u64>,    // A3: ts of the decision this overturns
}
```

Every existing `decisions.jsonl` line still deserializes unchanged (the `malformed_lines_are_skipped`
and `append_then_load_roundtrips` tests at `decisions.rs:229`–`254` define the contract — the new
fields are optional, so they pass as-is). No format break; this is the same additive discipline as
the existing `#[serde(default)] rejected`.

## Build order, risk, and dependencies

Lowest-risk first; nothing here precedes 0.0.2, and A2/A3 explicitly wait on Pillars 1 and 4.

1. **A1 Drift Watch** — `anchor` field + `decisions::reconcile` + `tomte why --reconcile`. Self-
   contained; no dependency on other pillars. Fixes the shipped stale-citation defect. *(Medium.)*
2. **A2 Tier 1** — `decisions::for_file` + render the trail block in Pillar 1's pre-flight card.
   **Blocked on Pillar 1's `PreFlight` event.** *(Small once P1 exists.)*
3. **A2 Tier 2** — the one self-check call + supersede/abort card. Opt-in. *(Medium-large.)*
4. **A3 On the Record** — `supersedes` field + the override line in Pillar 4's end-of-turn summary.
   **Blocked on Pillar 4's summary line.** *(Small.)*

Each lands on a branch, verifies (build + test + a real run showing the new behavior), and merges
only when green. Foreground vibe-coding stays the fallback at every step; reconcile and Tier 2 are
both opt-in so the default loop is unchanged until a user turns them on.

## Deliberately rejected (the guardrails held)

These were generated and cut in the same sweep; recording them so the rejection is on the record too:

- **Line-pinned matching + prose-substring conflict detection.** The original A2 mechanism. Cut: it
  false-positives on wording and misses real conflicts that don't share vocabulary. Replaced by
  content anchors (A1) + the editing model's own semantic self-check (A2 Tier 2).
- **A "grudge / assertiveness dial"** — a place-bound sentiment score silently tuning how hard tomte
  pushes back. Cut: the resolution signal doesn't exist (a revert conflates "wrong edit" / "changed
  my mind" / "unrelated breakage"), and a "you've been overruled, stop asserting" prompt produces the
  anxious people-pleaser Pillar 3 exists to kill. Only the narrow, observable piece — an explicit
  overrule of a *recorded decision* — survives, folded into A3.
- **A mandatory whole-turn approval turnstile.** Cut: it taxes vibe-coding (the heart, `SOUL.md` §4)
  and recreates the approval-fatigue that pushes everyone to `--yolo`. (The transactional staging
  *underneath* it may be worth shipping invisibly someday, but not as a gate, and not here.)

## Appendix — A2 (The Reckoning) in depth

Build-ready detail for the feature that defines the lane. Everything here is additive over Pillar
1's pre-flight; nothing changes the approval gate's decision logic.

### When it fires
- Trigger: a mutating tool call (`edit_file` / `write_file` / `multi_edit`) enters `run_tool_phase`
  (`toolphase.rs:6`) for file `F`, and `decisions::for_file(cwd, F)` is non-empty.
- Tier 1 fires on *every* such call (render-only, free). Tier 2 fires only when Tier 1 surfaced
  decisions **and** the user enabled it (`conscience = "off" | "surface" | "check"`, default
  `"surface"`). So the median edit — to a file with no recorded decisions — pays nothing and shows
  nothing new.

### Tier 1 — surface (cannot be wrong)
- Render the decisions for `F` inside the existing pre-flight card as a short block: a
  `house rules · src/auth.rs` header, then up to N (default 3, most-recent-first)
  `decision — why (by model)` lines, with a `+k more · tomte why src/auth.rs` overflow line.
- No detection, no model call, no gate change. Pure recall at the moment of risk.

### Tier 2 — check (one cheap call; the editing model judges)
- Prompt shape (harness-authored, model-answered — provider-agnostic): the proposed diff for `F`
  plus the recorded decisions for `F`, asking for exactly `CLEAR` or `CONFLICT <ts> — <one sentence>`
  (one line, no prose). Parsed by the harness.
- Model: the *same* model making the edit (`ctx.config.model`), so it survives a mid-session switch
  for free and never special-cases a provider (`SOUL.md` §4).
- Cost: one short completion, gated behind "file has decisions," capped to the same
  `TRAIL_MAX_RECORDS` budget as injection (`decisions.rs:158`). Skipped entirely under `--no-conscience`.

### The CONFLICT card (reuses the approval gate, adds no new turnstile)
- On `CONFLICT`, the pre-flight card escalates to one decision point with three choices: **abort**
  (don't edit), **supersede** (edit + record a superseding decision — A3), or **edit anyway**
  (proceed, logged). Interactive only — see A3 for the human-override rule.
- `CLEAR` → the edit proceeds exactly as today; the card stays Tier-1 quiet.

### Edge cases (decided)
- **Parse failure / garbage answer:** treat as `CLEAR` and log it. The conscience must never block an
  edit because a self-check was malformed — fail-open, never fail-shut on a model quirk.
- **Many decisions for one file:** Tier 2 sees up to `TRAIL_MAX_RECORDS`; a `CONFLICT` may name only
  the first it finds — one clear conflict is enough to stop and ask.
- **Self-edit of the trail store / tooling:** never run the conscience on `decisions.jsonl` itself
  (no recursion).
- **Headless / non-interactive:** Tier 2 may run but there is no card — it narrates-and-proceeds and
  logs the conflict (A3), since there is no human to hold the override.
- **Drifted `loc`:** A1's reconcile runs first; a `gone` decision is surfaced for review, never used
  to block an unrelated edit.

### Defaults and escape hatches
- `conscience = "surface"` by default (Tier 1 on, Tier 2 off) — zero added model cost out of the box.
- `--no-conscience` / `conscience = "off"` disables both; `conscience = "check"` enables Tier 2.
- The whole feature is dark until Pillar 1's pre-flight exists; until then it is unreachable code
  behind the flag, never on the default path.

## Where this leads (not in this doc's scope)

The same active-trail foundation makes two follow-on lanes cheap later — **a refusal ledger** (record
what tomte deliberately *didn't* build, the antibody to over-engineering) and **a pipeable trail**
(`tomte blame <file> | grep`, the moat as Unix text). Both reuse the `anchor` from A1. They are noted
here only as direction; design them separately if and when Theme A proves out.

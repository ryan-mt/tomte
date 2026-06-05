# Pillar previews — the tomte direction, running

Standalone, dependency-free sketches of all five pillars from [`../SOUL.md`](../SOUL.md) (the four
foundation pillars, the proposed Pillar-5 conscience, and the cross-cutting cost-receipts enabler).
They are **not part of tomte's build**: std-only, compiled by hand with `rustc`, invisible to
cargo/CI → **zero risk to the 0.0.2 release**. They exist so the direction is visible and
verifiable *before* real integration (which happens after 0.0.2, 2026-06-08).

Start with the guided tour (all five pillars in one coherent flow), or run them individually:

```sh
for p in tour calm_preview glass_box why_trail voice conscience cost_receipts; do
  rustc docs/previews/$p.rs -o /tmp/$p && /tmp/$p
done
```

| File | Pillar | Shows |
|---|---|---|
| `tour.rs` | all five | the whole custodian experience in one coherent flow |
| `calm_preview.rs` | 4 — calm terminal | **animated**: a turn redraws in a slim live viewport, then its receipt settles into real scrollback · calm palette |
| `glass_box.rs` | 1 — glass-box | pre-flight intent / scope / cost · visible blast radius |
| `why_trail.rs` | 2 — memory of why | a tiny `tomte why` CLI: writes + reads real JSONL · query a loc · survives the model switch |
| `voice.rs` | 3 — voice with a spine | generic vs. opinionated — receipts, not sycophancy |
| `conscience.rs` | 5 — the conscience | the active trail: reconciles itself to the code (A1), confronts an edit that would reverse a past decision (A2), logs the human override (A3) · real JSONL + real files |
| `cost_receipts.rs` | cross-cutting | one session across two providers · real per-class arithmetic reconciled into ONE normalized bill — the multi-model cost advantage a single-vendor tool can't show |

`why_trail.rs` doubles as a tiny CLI: after running it, try `/tmp/why_trail why src/cache.rs:42`
or `/tmp/why_trail why --all` to read the same on-disk trail a fresh session would.

`conscience.rs` is a CLI too: `/tmp/conscience reconcile` audits the on-disk trail against the code,
and `/tmp/conscience why src/auth.rs:7` follows the supersede chain to the decision it overturned.

`calm_preview.rs` animates over ~9s and is the **relaunch demo** — record it with `asciinema rec`
or a vhs tape to get the GIF that answers "why not opencode?" by *showing* the tidy terminal, not
arguing it. A still screenshot reads like a doc; the motion is the point.

Each verified by `rustc -D warnings` (clean compile) + run. **Next:** build these into tomte
itself after 0.0.2, starting with Pillar 4 (see [`pillar-4-calm-terminal.md`](../pillar-4-calm-terminal.md)).

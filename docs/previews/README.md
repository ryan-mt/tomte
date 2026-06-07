# Feature previews — the tomte direction, running

Standalone, dependency-free sketches of tomte's core ideas: a legible glass-box
pre-flight, a cross-model decision trail, an opinionated voice, a calm terminal, an
active conscience, and normalized cross-provider cost receipts. They are **not part of
tomte's build**: std-only, compiled by hand with `rustc`, invisible to cargo/CI →
**zero risk to the 0.0.2 release**. They exist so the direction is visible and
verifiable *before* real integration (which happens after 0.0.2).

Start with the guided tour (every idea in one coherent flow), or run them individually:

```sh
for p in tour calm_preview glass_box why_trail voice conscience cost_receipts; do
  rustc docs/previews/$p.rs -o /tmp/$p && /tmp/$p
done
```

| File | Idea | Shows |
|---|---|---|
| `tour.rs` | the whole direction | the whole custodian experience in one coherent flow |
| `calm_preview.rs` | calm terminal | **animated**: a turn redraws in a slim live viewport, then its receipt settles into real scrollback · calm palette |
| `glass_box.rs` | glass-box | pre-flight intent / scope / cost · visible blast radius |
| `why_trail.rs` | memory of why | a tiny `tomte why` CLI: writes + reads real JSONL · query a loc · survives the model switch |
| `voice.rs` | voice with a spine | generic vs. opinionated — receipts, not sycophancy |
| `conscience.rs` | the conscience | the active trail: reconciles itself to the code, confronts an edit that would reverse a past decision, logs the human override · real JSONL + real files |
| `cost_receipts.rs` | cross-provider cost | one session across two providers · real per-class arithmetic reconciled into ONE normalized bill — the multi-model cost advantage a single-vendor tool can't show |

`why_trail.rs` doubles as a tiny CLI: after running it, try `/tmp/why_trail why src/cache.rs:42`
or `/tmp/why_trail why --all` to read the same on-disk trail a fresh session would.

`conscience.rs` is a CLI too: `/tmp/conscience reconcile` audits the on-disk trail against the code,
and `/tmp/conscience why src/auth.rs:7` follows the supersede chain to the decision it overturned.

`calm_preview.rs` animates over ~9s and is the **relaunch demo** — record it with `asciinema rec`
or a vhs tape to get the GIF that answers "why not opencode?" by *showing* the tidy terminal, not
arguing it. A still screenshot reads like a doc; the motion is the point.

Each verified by `rustc -D warnings` (clean compile) + run. **Next:** build these into tomte
itself after 0.0.2, starting with the calm terminal.

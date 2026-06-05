# Pillar previews — the tomte direction, running

Standalone, dependency-free sketches of the four pillars from [`../SOUL.md`](../SOUL.md).
They are **not part of tomte's build**: std-only, compiled by hand with `rustc`, invisible to
cargo/CI → **zero risk to the 0.0.2 release**. They exist so the direction is visible and
verifiable *before* real integration (which happens after 0.0.2, 2026-06-08).

Start with the guided tour (all four pillars in one coherent flow), or run them individually:

```sh
for p in tour calm_preview glass_box why_trail voice; do
  rustc docs/previews/$p.rs -o /tmp/$p && /tmp/$p
done
```

| File | Pillar | Shows |
|---|---|---|
| `tour.rs` | all four | the whole custodian experience in one coherent flow |
| `calm_preview.rs` | 4 — calm terminal | inline viewport keeps scrollback · "left in order" receipt · calm palette |
| `glass_box.rs` | 1 — glass-box | pre-flight intent / scope / cost · visible blast radius |
| `why_trail.rs` | 2 — memory of why | a tiny `tomte why` CLI: writes + reads real JSONL · query a loc · survives the model switch |
| `voice.rs` | 3 — voice with a spine | generic vs. opinionated — receipts, not sycophancy |

`why_trail.rs` doubles as a tiny CLI: after running it, try `/tmp/why_trail why src/cache.rs:42`
or `/tmp/why_trail why --all` to read the same on-disk trail a fresh session would.

Each verified by `rustc -D warnings` (clean compile) + run. **Next:** build these into tomte
itself after 0.0.2, starting with Pillar 4 (see [`pillar-4-calm-terminal.md`](../pillar-4-calm-terminal.md)).

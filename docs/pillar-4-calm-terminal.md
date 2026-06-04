# Pillar 4 — The Calm, Tidy Terminal (design)

> The first soul-bet, to build *immediately after 0.0.2*. See `SOUL.md` §5 Pillar 4, §8 Stage 1.
> Goal: make tomte leave the terminal tidy and intact — the felt difference a "quiet custodian"
> owes you — and fix the exact complaint Claude Code / Gemini CLI are mocked for.

## The problem, grounded in our code

- `crates/cli/src/tui/app/entry.rs:58` enters the **alternate screen** (`EnterAlternateScreen`).
  On exit the whole UI is erased and the user's native scrollback is gone — the #1 reason these
  TUIs are called "terrible" (HN 46286057): you can't scroll up past a page, can't copy history,
  and end up in tmux to get it back.
- `crates/cli/src/tui/ui.rs:33-79` renders **one full-screen 7-zone layout**; the whole chat
  transcript scrolls *inside* the chat zone (`Constraint::Min(5)`), redrawn each frame. That's
  the generic AI-TUI surface, and the in-zone scroll is what jumps and flickers.

## The design: inline viewport + push-to-scrollback

Ratatui supports this first-class (`Viewport::Inline`, `Terminal::insert_before`;
ratatui.rs/examples/apps/inline). The custodian model:

1. **No alternate screen.** `setup_terminal` (`entry.rs:48`) uses `Viewport::Inline(height)`
   instead of `EnterAlternateScreen`. The terminal's own scrollback, scroll, and copy keep
   working — nothing is hijacked.
2. **Finished turns go to real scrollback.** When a turn completes, its block is committed to the
   terminal via `insert_before` and leaves the live viewport. History lives in the user's
   terminal, not in an app-owned scroll buffer.
3. **The live viewport stays slim** — only the *active* turn + input + a one-line status. Not the
   whole transcript. (Trade-off: committed lines aren't re-editable; only the active turn is —
   acceptable, and exactly the "done is done, tidy it away" custodian behavior.)
4. **A "left in order" summary** closes each turn: one or two lines — files touched, tests run,
   and the *why* (a Pillar-2 seed) — then it too settles into scrollback. The custodian reports
   tidily and steps back.
5. **A calm palette** (a `palette` module): achromatic base + **one** muted accent (sage/teal),
   semantic muted red/amber/green for diff and status. Replaces scattered color literals. One
   accent, used for one or two things — the single biggest "craft, not slop" signal.

## Before → after (the felt difference)

**Today** (alt-screen, full-screen, narration, scrollback eaten):

```
┌────────────────────────────────────────────────┐  ← alt-screen eats scrollback
│ > add a test for the parser                     │
│ I'll start by reading the parser…               │  ← narration
│ ⠙ Reading src/parser.rs                          │
│ [tool] edit_file src/parser.rs                  │  ← whole transcript scrolls in
│ …(scroll-up jumps to top, flickers on redraw)…  │     here; can't copy history
├────────────────────────────────────────────────┤
│ > ▌                                             │
│ gpt-5.5 · 42% ctx · main                        │
└────────────────────────────────────────────────┘
```

**Pillar 4** (inline viewport, history intact, tidy hand-off):

```
   …earlier conversation stays in the terminal's OWN scrollback —          ← native scroll
    scroll with the mouse, copy like any command output, never erased…        & copy work

   ✓ added parser test · 1 file · cargo test 12 passed                     ← "left in order"
       src/parser.rs:88   why: cover the empty-input case                       (then settles)

   running cargo test … 3.2s · esc to interrupt                            ← only the ACTIVE
   > ▌                                                                         turn is live
   gpt-5.5 · 42% · main                                                    ← quiet, peripheral
```

Same engine, same vibe-coding loop — but it no longer hijacks the terminal, no longer narrates,
and leaves a tidy receipt. That is the custodian, made visible.

## Implementation plan (after 0.0.2) + verification

- **B1** `entry.rs` — `Viewport::Inline` instead of `EnterAlternateScreen`; update
  `restore_terminal`. *Verify:* launch, run a turn, exit → prior shell scrollback is intact;
  mouse-scroll and copy work.
- **B2** `mainloop.rs` + `ui.rs` — commit finished-turn blocks via `insert_before`; shrink the
  live layout to active-turn + input + status. *Verify:* long turns leave no flicker; scroll-up
  shows real history, never jumps to top.
- **B3** end-of-turn "left in order" summary line(s) on `TurnComplete`. *Verify:* a real run shows
  the one-line receipt with files/tests/why.
- **B4** `palette` module; migrate color literals. *Verify:* `cargo build`; visual check of the
  calm palette; no behavioral change.

**Safety:** stage behind the current renderer; keep alt-screen as a fallback until B1–B2 are
verified on a real terminal across turns. The proven core stays the safety net (`SOUL.md` §8).
```

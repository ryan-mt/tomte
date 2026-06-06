# Pillar 4 — The Calm, Tidy Terminal (design)

> The first soul-bet, to build *immediately after 0.0.2*. See `SOUL.md` §5 Pillar 4, §8 Stage 1.
> Goal: make tomte leave the terminal tidy and intact — the felt difference a "quiet custodian"
> owes you — and fix the exact complaint Claude Code / Gemini CLI are mocked for.

## Status (shipped in 0.0.2)

The build landed: the **inline viewport** (B1–B2), the **"left in order"** end-of-turn summary
(B3), and the **calm `palette`** (B4) are all in (`render_inline` / `commit_finished_blocks` via
`insert_before`; `crates/cli/src/tui/palette.rs`). The mouse-capture fork below resolved as **B
(keep capture, keep the features)**: tomte keeps in-app scroll, click-drag selection → clipboard,
and clickable jump/fleet targets. Because those depend on mouse capture — which pairs with the
alternate screen — the **alternate screen stays the default**, and this inline design ships
**opt-in via `TOMTE_INLINE=1`** (`RenderMode::from_env_value`). So the "Today" picture below is
still the default; the "Pillar 4" picture is what `TOMTE_INLINE=1` delivers. The design is kept
here as the rationale and the trending-to-A goal.

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
  `restore_terminal`. *Verify:* launch, run a turn, exit → prior shell scrollback is intact.
  (Native mouse-scroll/copy are **not** free here — see the mouse-capture decision below.)
- **B2** `mainloop.rs` + `ui.rs` — commit finished-turn blocks via `insert_before`; shrink the
  live layout to active-turn + input + status. *Verify:* long turns leave no flicker; scroll-up
  shows real history, never jumps to top.
- **B3** end-of-turn "left in order" summary line(s) on `TurnComplete`. *Verify:* a real run shows
  the one-line receipt with files/tests/why.
- **B4** `palette` module; migrate color literals. *Verify:* `cargo build`; visual check of the
  calm palette; no behavioral change.

**Safety:** stage behind the current renderer; keep alt-screen as a fallback until B1–B2 are
verified on a real terminal across turns. The proven core stays the safety net (`SOUL.md` §8).

## Build-readiness: the mouse-capture decision (DECIDE before B1)

The "scroll with the mouse, copy like any command" promise above is **not free** — it collides
with a feature tomte already ships. `entry.rs:59` enables `EnableMouseCapture`, and
`mainloop.rs:313-351` uses it for real: in-app scroll (`ScrollUp/Down`), a click-drag text
**selection** (`Down/Drag/Up(Left)` → `app.selection` → `finish_selection`), and **clickable
targets** (a plain click → `handle_left_click`: jump / fleet toggle). While capture is on the
terminal never sees the mouse, so native scroll and native click-drag copy *cannot* work — no
matter how inline the viewport is. So this is a real fork, not a one-line cleanup:

- **A — Cede the mouse to the terminal** *(purest custodian).* Drop `EnableMouseCapture`; native
  scroll + selection/copy return for free. **Cost:** lose the in-app selection and the clickable
  jump/fleet targets (or re-bind them to the keyboard). Matches "don't hijack the terminal" 1:1 —
  and in inline mode the in-app `ScrollUp/Down` handler has almost nothing left to scroll anyway
  (the transcript now lives in native scrollback), so capture mostly just *blocks* the wheel.
- **B — Keep capture, keep the features.** Then the headline promise is false: strike "copy like
  any command" from the pitch and lean on tomte's own selection→clipboard instead.
- **C — Stage it.** Ship **B1 = inline viewport only, mouse untouched** first: that alone delivers
  the big win (finished turns in native scrollback, no alt-screen, no flicker) at near-zero risk.
  Decide the mouse (A vs B) as a separate **B1b** once the viewport is proven.

**Recommendation:** C now, trending to A — the scrollback win is the meme-killer and is independent
of the mouse, so don't let the harder selection/clickable-target call delay it. Mark **DECIDE**
(human call) per `clone-audit.md`.

//! Compaction progress, jump-to-bottom, and the chat transcript. Split out of `ui`; logic unchanged.

use super::*;

/// One-row progress bar for a running compaction. With no real percentage from
/// the model, the fill eases asymptotically toward 95% by elapsed time (so it
/// always looks alive, never stalls at 0), then snaps to 100% once the task
/// reports done. All widths use saturating/clamped math so a narrow terminal
/// can't underflow.
pub(super) fn render_compact_progress(f: &mut Frame, area: Rect, app: &App) {
    let pct: u16 = if app.compact_done_at.is_some() {
        100
    } else {
        let t = app
            .compact_started_at
            .map(|s| s.elapsed().as_millis() as f64)
            .unwrap_or(0.0);
        (95.0 * t / (t + 4000.0)).round().clamp(0.0, 95.0) as u16
    };
    let purple = Style::default().fg(palette::INFO);
    let dim = Style::default().fg(palette::TEXT_MUTED);
    let track = Style::default().fg(palette::TEXT_FAINT);

    let label = " compacting ";
    let suffix = format!(" {pct:>3}%");
    // Reserve room for the label, the "[" "]" brackets and the suffix so the
    // bar itself can never be wider than the row.
    let reserved = label.chars().count() + suffix.chars().count() + 2;
    let bar_width = (area.width as usize).saturating_sub(reserved).min(40);
    let filled = bar_width * pct as usize / 100;
    let empty = bar_width.saturating_sub(filled);

    let line = Line::from(vec![
        Span::styled(label, purple.add_modifier(Modifier::BOLD)),
        Span::styled("[", dim),
        Span::styled("█".repeat(filled), purple),
        Span::styled("░".repeat(empty), track),
        Span::styled("]", dim),
        Span::styled(suffix, dim),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Draw the "Jump to bottom" bar on the last row of the chat area and return
/// its screen rect so the mouse handler can hit-test a click. The label is
/// centered and flanked by a horizontal rule.
pub(super) fn render_jump_to_bottom(f: &mut Frame, chat_area: Rect) -> Rect {
    let row = Rect {
        x: chat_area.x,
        y: chat_area.y + chat_area.height - 1,
        width: chat_area.width,
        height: 1,
    };
    let label = " Jump to bottom (ctrl+End) ↓ ";
    let total = row.width as usize;
    let label_w = unicode_width::UnicodeWidthStr::width(label);
    let rule = Style::default().fg(palette::TEXT_FAINT);
    let label_style = Style::default()
        .fg(palette::TEXT_BRIGHT)
        .add_modifier(Modifier::BOLD);
    let spans = if total > label_w {
        let left = (total - label_w) / 2;
        let right = total - label_w - left;
        vec![
            Span::styled("─".repeat(left), rule),
            Span::styled(label, label_style),
            Span::styled("─".repeat(right), rule),
        ]
    } else {
        vec![Span::styled(label, label_style)]
    };
    f.render_widget(Paragraph::new(Line::from(spans)), row);
    row
}

pub(super) fn render_chat(f: &mut Frame, area: Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let expanded = app.expanded_tools;
    let show_thinking = app.config.show_thinking;
    let stable = stable_boundary(&app.blocks, app.busy);
    let welcome_fp = welcome_fp(app);

    // Re-wrapping every block on every frame is O(blocks * avg_text_len) of
    // textwrap + markdown + syntect work; for a long session that shows up as
    // visible stutter on every agent event. Instead the STABLE prefix — every
    // block before the live turn — is wrapped once and cached, and only the
    // live tail (the turn that actually mutates) is re-wrapped per frame. The
    // cache is validated here with a fingerprint fold, never invalidated by
    // event handlers.
    let taken = app.chat_render_cache.take();
    let cached = taken.filter(|c| {
        c.inner_width == inner_width
            && c.expanded_tools == expanded
            && c.show_thinking == show_thinking
            && c.welcome_fp == welcome_fp
            && c.stable_blocks <= stable
            && fold_fp(0, &app.blocks[..c.stable_blocks]) == c.stable_fp
            // A fresh build merges consecutive read_file blocks into one
            // stanza; appending can't merge across the old boundary, so the
            // rare group that spans it forces a rebuild instead.
            && !read_group_crosses(&app.blocks, c.stable_blocks)
    });
    let mut cache = cached.unwrap_or(crate::tui::app::ChatRenderCache {
        inner_width,
        expanded_tools: expanded,
        show_thinking,
        welcome_fp,
        stable_blocks: 0,
        stable_fp: 0,
        lines: Vec::new(),
    });
    if cache.stable_blocks < stable {
        // The boundary moved forward (a turn settled): wrap the newly stable
        // blocks once and append them; everything already cached is untouched.
        let mut more = super::inline_blocks_to_lines(
            &app.blocks[cache.stable_blocks..stable],
            inner_width,
            expanded,
            app,
        );
        cache.stable_fp = fold_fp(cache.stable_fp, &app.blocks[cache.stable_blocks..stable]);
        cache.stable_blocks = stable;
        cache.lines.append(&mut more);
    }
    // The live tail — the streaming turn — re-wraps every frame; it's bounded
    // by one turn, not the whole transcript.
    let tail = super::inline_blocks_to_lines(&app.blocks[stable..], inner_width, expanded, app);
    let (scroll, auto) = render_window(f, area, &cache.lines, &tail, app.auto_scroll, app.scroll);
    app.scroll = scroll;
    app.auto_scroll = auto;
    app.chat_render_cache = Some(cache);
}

/// Index of the first LIVE block: while a turn streams, everything from the
/// turn's User prompt onward keeps mutating (text deltas, tool args/results,
/// assistant rotation) and must re-wrap per frame; everything before it
/// settled when the previous turn ended. When idle the whole transcript is
/// settled. The prompt itself never mutates, so it joins the stable prefix
/// (+1) — re-wrapping a large pasted prompt every frame would defeat the
/// cache. A missing User block (defensive) keeps the whole transcript live,
/// which is merely slower, never wrong.
pub(super) fn stable_boundary(blocks: &[Block], busy: bool) -> usize {
    if !busy {
        return blocks.len();
    }
    blocks
        .iter()
        .rposition(|b| matches!(b, Block::User(_)))
        .map_or(0, |i| i + 1)
}

/// Order-sensitive fold of [`block_fingerprint`] over `blocks`, continuing
/// from `seed` so an appended slice can extend a stored fold incrementally.
pub(super) fn fold_fp(seed: u64, blocks: &[Block]) -> u64 {
    blocks.iter().fold(seed, |acc, b| {
        acc.rotate_left(7) ^ (block_fingerprint(b) as u64)
    })
}

/// True when a run of consecutive `read_file` tool blocks spans `boundary` —
/// the one shape an append-only cache extension would render differently from
/// a fresh build (the group renders as a single merged stanza).
pub(super) fn read_group_crosses(blocks: &[Block], boundary: usize) -> bool {
    let is_read = |b: &Block| matches!(b, Block::Tool { name, .. } if name == "read_file");
    boundary > 0
        && boundary < blocks.len()
        && is_read(&blocks[boundary - 1])
        && is_read(&blocks[boundary])
}

/// Fold of the live App state the Welcome card renders from, so a `/model`
/// switch, login, cwd change, or hatch refreshes the cached card. The
/// filesystem probe (`has_rules`) is deliberately excluded — hashing it would
/// cost the very per-frame syscalls the cache exists to avoid; that ○→✓ flip
/// rides the next natural rebuild instead.
fn welcome_fp(app: &App) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    app.config.model.hash(&mut h);
    app.config.reasoning_effort.hash(&mut h);
    (app.auth_mode as u8).hash(&mut h);
    app.cwd.hash(&mut h);
    app.buddy_pet.unwrap_or(app.welcome_pet).hash(&mut h);
    h.finish()
}

/// Render a single `Assistant` block's wrapped lines into `lines`. Pulled out
/// of `render_chat`'s main loop so the streaming fast path can re-wrap just the
/// final block. A no-op for non-Assistant blocks.
pub(super) fn push_assistant_lines(
    lines: &mut Vec<Line<'static>>,
    block: &Block,
    inner_width: usize,
    show_thinking: bool,
) {
    let Block::Assistant {
        text,
        reasoning,
        thought_for_secs,
        ..
    } = block
    else {
        return;
    };
    // Live reasoning: while the model is thinking (reasoning is streaming and
    // hasn't collapsed yet), show it muted + italic so the user can follow the
    // thought, like Claude Code. It's cleared into the "Thought for Xs" line
    // below the moment the answer starts, so the two are never shown together.
    if show_thinking && !reasoning.is_empty() {
        let think_style = Style::default()
            .fg(palette::TEXT_MUTED)
            .add_modifier(Modifier::ITALIC);
        let marker_style = Style::default()
            .fg(palette::INFO)
            .add_modifier(Modifier::ITALIC);
        let content_width = inner_width.saturating_sub(2);
        let mut first = true;
        for raw in reasoning.split('\n') {
            for w in wrap(raw, content_width) {
                let row = if first {
                    vec![
                        Span::styled("✦ ", marker_style),
                        Span::styled(w, think_style),
                    ]
                } else {
                    vec![Span::raw("  "), Span::styled(w, think_style)]
                };
                first = false;
                lines.push(Line::from(row));
            }
        }
        lines.push(Line::raw(""));
    }
    // Compact "Thought for Xs" line once reasoning has completed for this
    // assistant block — it replaces the live reasoning text above.
    if let Some(secs) = thought_for_secs {
        lines.push(Line::from(vec![
            Span::styled("· ", Style::default().fg(palette::INFO)),
            Span::styled(
                format!("Thought for {secs}s"),
                Style::default()
                    .fg(palette::TEXT_MUTED)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        lines.push(Line::raw(""));
    }
    // Raw reasoning text is intentionally suppressed in chat history.
    if !text.is_empty() {
        // Mark the assistant's turn with a bullet on its first line (like the
        // tool bullet, so prose and tool calls read as one consistent column),
        // then indent continuation lines to align under it.
        let marker_style = Style::default()
            .fg(palette::INFO)
            .add_modifier(Modifier::BOLD);
        // Block-level markdown: fenced code blocks get syntax highlighting and
        // tables get box-drawing borders; everything else is wrapped + inline
        // styled. Each returned row is the content (no gutter); the first row
        // carries the assistant bullet, the rest a 2-col indent.
        let content_width = inner_width.saturating_sub(2);
        let mut first = true;
        for spans in render_assistant_md(text, content_width) {
            let mut row = if first {
                vec![Span::styled("● ", marker_style)]
            } else {
                vec![Span::raw("  ")]
            };
            first = false;
            row.extend(spans);
            lines.push(Line::from(row));
        }
        lines.push(Line::raw(""));
    }
}

/// Render a `User` block as the full-width gray stanza: each
/// wrapped line is padded with background-carrying spaces so the fill reaches
/// the right edge. Shared by the alt-screen transcript (`render_chat`) and the
/// inline viewport so both render the user's turn identically.
pub(super) fn push_user_lines(lines: &mut Vec<Line<'static>>, text: &str, inner_width: usize) {
    let user_bg = palette::SURFACE;
    let chevron_style = Style::default()
        .fg(palette::INFO)
        .bg(user_bg)
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(palette::TEXT).bg(user_bg);
    let mut first = true;
    for raw in text.split('\n') {
        for w in wrap(raw, inner_width.saturating_sub(2)) {
            let prefix = if first { "> " } else { "  " };
            first = false;
            let used = 2 + unicode_width::UnicodeWidthStr::width(w.as_str());
            let pad = inner_width.saturating_sub(used);
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), chevron_style),
                Span::styled(w, body_style),
                Span::styled(" ".repeat(pad), body_style),
            ]));
        }
    }
    lines.push(Line::raw(""));
}

/// Compute a cheap fingerprint of a block's mutable content. Streaming
/// deltas grow `text`/`output`; a length change invalidates the cache. The
/// fingerprint deliberately ignores identifiers and timing fields because
/// those don't affect the wrapped output.
pub(super) fn block_fingerprint(block: &Block) -> usize {
    match block {
        Block::Welcome => 0,
        Block::User(s) | Block::System(s) => s.len(),
        // Rich blocks are built once and never mutated, so their line count is a
        // stable fingerprint.
        Block::Rich(lines) => lines.len(),
        Block::Assistant {
            text,
            reasoning,
            thought_for_secs,
            done,
            ..
        } => {
            // Multiply each field by a distinct prime so e.g. a block that
            // moves bytes from `reasoning` into `text` still produces a
            // different fingerprint instead of an accidental cache hit.
            text.len()
                .wrapping_mul(31)
                .wrapping_add(reasoning.len().wrapping_mul(17))
                .wrapping_add(thought_for_secs.unwrap_or(0) as usize)
                .wrapping_add(if *done { 1 } else { 0 })
        }
        Block::Tool {
            args,
            output,
            error,
            preflight,
            ..
        } => args
            .len()
            .wrapping_mul(31)
            .wrapping_add(
                output
                    .as_deref()
                    .map(|s| s.len())
                    .unwrap_or(0)
                    .wrapping_mul(17),
            )
            .wrapping_add(if *error { 1 } else { 0 })
            .wrapping_add(if preflight.is_some() { 7 } else { 0 }),
    }
}

/// Shared tail of `render_chat`: scroll math + Paragraph dispatch. Same
/// code runs whether we hit the cache (early return) or just rebuilt the
/// lines; pulled into a helper to keep the two paths in lockstep.
/// Render only the visible window of the transcript — whose wrapped lines are
/// the concatenation of `head` then `tail` — into `area`. Clones at most
/// `area.height` lines (the viewport), never the whole transcript, so a frame
/// stays O(viewport) instead of O(blocks). `head` + `tail` lets the streaming
/// path pass the cached prefix (borrowed) plus the freshly wrapped final block
/// without concatenating them into one big Vec first. Returns the resolved
/// `(scroll, auto_scroll)` for the caller to store.
pub(super) fn render_window(
    f: &mut Frame,
    area: Rect,
    head: &[Line<'static>],
    tail: &[Line<'static>],
    auto_scroll: bool,
    cur_scroll: u16,
) -> (u16, bool) {
    let total = head.len() + tail.len();
    let viewport = area.height as usize;
    let (scroll, auto) = resolve_scroll(total, viewport, auto_scroll, cur_scroll);
    // Materialize only `[scroll, scroll+viewport)` — the same rows the old
    // full-Vec `Paragraph.scroll(scroll)` would have shown, minus the cost of
    // building and offsetting every line above the viewport.
    let start = (scroll as usize).min(total);
    let visible: Vec<Line<'static>> = head
        .iter()
        .chain(tail.iter())
        .skip(start)
        .take(viewport)
        .cloned()
        .collect();
    f.render_widget(Paragraph::new(visible), area);
    (scroll, auto)
}

/// Resolve the scroll offset + auto-follow state for a transcript of `total`
/// wrapped lines in a `viewport`-row area. Pure (no `Frame`) so the scroll math
/// — the part that had to stay byte-identical when the renderer switched from
/// "build the whole transcript then `Paragraph::scroll`" to "materialize only
/// the visible window" — is unit-tested directly. `max_scroll` uses
/// `viewport - 2` (a 2-row breathing gap at the bottom), the long-standing
/// alt-screen behavior.
fn resolve_scroll(
    total: usize,
    viewport: usize,
    auto_scroll: bool,
    cur_scroll: u16,
) -> (u16, bool) {
    let inner_height = viewport.saturating_sub(2);
    let max_scroll = total.saturating_sub(inner_height) as u16;
    // Scrolling back to (or past) the bottom resumes auto-follow — how
    // mouse-wheel / PageDown re-enables sticky-bottom without a dedicated key.
    let auto = auto_scroll || cur_scroll >= max_scroll;
    let scroll = if auto {
        max_scroll
    } else {
        cur_scroll.min(max_scroll)
    };
    (scroll, auto)
}

#[cfg(test)]
mod tests {
    use super::{read_group_crosses, resolve_scroll, stable_boundary};
    use crate::tui::app::Block;

    fn read_tool(id: &str) -> Block {
        Block::Tool {
            call_id: id.to_string(),
            name: "read_file".to_string(),
            args: String::new(),
            output: None,
            error: false,
            preflight: None,
        }
    }

    #[test]
    fn stable_boundary_covers_everything_when_idle() {
        let blocks = vec![Block::Welcome, Block::User("hi".into()), read_tool("c1")];
        assert_eq!(stable_boundary(&blocks, false), 3);
    }

    #[test]
    fn stable_boundary_starts_the_live_turn_after_its_prompt() {
        // Busy: the live turn begins right AFTER the last User block (the
        // prompt itself never mutates, so it stays cached).
        let blocks = vec![
            Block::Welcome,
            Block::User("first".into()),
            Block::System("done".into()),
            Block::User("second".into()),
            read_tool("c1"),
        ];
        assert_eq!(stable_boundary(&blocks, true), 4);
    }

    #[test]
    fn stable_boundary_without_a_prompt_keeps_everything_live() {
        let blocks = vec![Block::Welcome, read_tool("c1")];
        assert_eq!(stable_boundary(&blocks, true), 0);
    }

    #[test]
    fn read_group_crossing_is_detected_only_inside_a_run() {
        let blocks = vec![
            Block::User("hi".into()),
            read_tool("c1"),
            read_tool("c2"),
            Block::System("done".into()),
        ];
        // Boundary inside the read_file run → crossing.
        assert!(read_group_crosses(&blocks, 2));
        // Boundaries at the run's edges, the ends, or out of range → no crossing.
        assert!(!read_group_crosses(&blocks, 0));
        assert!(!read_group_crosses(&blocks, 1));
        assert!(!read_group_crosses(&blocks, 3));
        assert!(!read_group_crosses(&blocks, 4));
    }

    #[test]
    fn auto_follow_pins_to_the_bottom_gap() {
        // 100 lines, 22-row viewport → inner_height 20 → max_scroll 80.
        // Auto-following shows the last 20 lines with the 2-row bottom gap.
        assert_eq!(resolve_scroll(100, 22, true, 0), (80, true));
    }

    #[test]
    fn parked_above_the_tail_keeps_the_users_scroll() {
        // Not auto-following and parked below max → scroll is held, auto stays off.
        assert_eq!(resolve_scroll(100, 22, false, 30), (30, false));
    }

    #[test]
    fn scrolling_to_the_bottom_resumes_auto_follow() {
        // cur_scroll at/over max_scroll re-arms auto-follow (sticky bottom).
        assert_eq!(resolve_scroll(100, 22, false, 80), (80, true));
        assert_eq!(resolve_scroll(100, 22, false, 999), (80, true));
    }

    #[test]
    fn short_transcript_never_scrolls() {
        // Fewer lines than the viewport → max_scroll 0, always top-anchored.
        assert_eq!(resolve_scroll(5, 22, true, 0), (0, true));
        assert_eq!(resolve_scroll(5, 22, false, 3), (0, true));
    }

    #[test]
    fn a_parked_scroll_is_clamped_to_max() {
        // A stale cur_scroll past the new max clamps down (no overscroll).
        assert_eq!(resolve_scroll(50, 22, false, 40), (30, true));
    }
}

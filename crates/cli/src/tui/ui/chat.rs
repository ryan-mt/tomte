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

    // Re-wrapping every block on every frame is O(blocks * avg_text_len) of
    // textwrap calls plus matching allocations. For a 500-block chat at
    // 30Hz that's tens of thousands of textwrap invocations per second and
    // shows up as visible CPU + lag. Skip the whole pass when nothing
    // observable has changed since the previous frame.
    let last_block_size = app.blocks.last().map(block_fingerprint).unwrap_or(0);
    let cache_meta_matches = app.chat_render_cache.as_ref().is_some_and(|c| {
        c.blocks_len == app.blocks.len()
            && c.inner_width == inner_width
            && c.expanded_tools == expanded
    });
    if cache_meta_matches {
        // Borrow the cache only long enough to produce owned lines, so the
        // mutable cache write below doesn't overlap the immutable read. The
        // bool says whether the cache's `lines`/`last_block_size` need updating
        // (true only on the streaming fast path).
        let hit: Option<(Vec<Line<'static>>, bool)> = {
            let c = app.chat_render_cache.as_ref().unwrap();
            if c.last_block_size == last_block_size {
                // Exact hit: nothing observable changed since last frame.
                Some((c.lines.clone(), false))
            } else if let (Some(prefix), Some(Block::Assistant { .. })) =
                (c.prefix_lines.as_ref(), app.blocks.last())
            {
                // Streaming fast path: only the final Assistant block grew.
                // Reuse the cached prefix; re-wrap just that one block.
                let mut lines = prefix.clone();
                push_assistant_lines(&mut lines, app.blocks.last().unwrap(), inner_width);
                Some((lines, true))
            } else {
                None
            }
        };
        if let Some((lines, update_cache)) = hit {
            if update_cache {
                if let Some(c) = app.chat_render_cache.as_mut() {
                    c.lines = lines.clone();
                    c.last_block_size = last_block_size;
                }
            }
            finalize_chat_render(f, area, app, lines);
            return;
        }
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Records where the final block's lines begin, so a streaming frame can
    // reuse everything before it. Set only when the last block renders as its
    // own standalone stanza; left `None` when it merges into a read_file group.
    let mut prefix_split: Option<usize> = None;
    let mut i = 0;
    while i < app.blocks.len() {
        if i + 1 == app.blocks.len() {
            prefix_split = Some(lines.len());
        }
        // Group consecutive read_file tool calls into a single block so a
        // batch of reads doesn't dominate the chat with one stanza per file.
        if matches!(&app.blocks[i], Block::Tool { name, .. } if name == "read_file") {
            let mut j = i;
            while j < app.blocks.len()
                && matches!(&app.blocks[j], Block::Tool { name, .. } if name == "read_file")
            {
                j += 1;
            }
            // A group that reaches the end swallows the final block, so there
            // is no standalone last-block stanza to split on.
            if j == app.blocks.len() {
                prefix_split = None;
            }
            render_read_group(&mut lines, &app.blocks[i..j], expanded);
            i = j;
            continue;
        }
        match &app.blocks[i] {
            Block::Welcome => {
                render_welcome(&mut lines, app);
            }
            Block::User(text) => push_user_lines(&mut lines, text, inner_width),
            Block::Assistant { .. } => {
                push_assistant_lines(&mut lines, &app.blocks[i], inner_width);
            }
            Block::Tool {
                name,
                args,
                output,
                error,
                preflight,
                ..
            } => {
                render_tool(
                    &mut lines,
                    name,
                    args,
                    output.as_deref(),
                    *error,
                    preflight.as_ref(),
                    inner_width,
                    expanded,
                );
            }
            Block::System(text) => {
                for l in wrap(text, inner_width.saturating_sub(2)) {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(palette::TEXT_MUTED),
                    )));
                }
                lines.push(Line::raw(""));
            }
            // Pre-styled fixed-layout content (e.g. `/context`): pushed verbatim,
            // no wrapping (the lines carry their own indent and colors).
            Block::Rich(rich_lines) => {
                for l in rich_lines {
                    lines.push(l.clone());
                }
                lines.push(Line::raw(""));
            }
        }
        i += 1;
    }

    // Save into the cache so the next frame can skip the rebuild loop. The
    // lines clone here is cheap relative to the textwrap pass we just did.
    let prefix_lines = prefix_split.map(|s| lines[..s].to_vec());
    app.chat_render_cache = Some(crate::tui::app::ChatRenderCache {
        blocks_len: app.blocks.len(),
        inner_width,
        expanded_tools: expanded,
        last_block_size,
        lines: lines.clone(),
        prefix_lines,
    });

    finalize_chat_render(f, area, app, lines);
}

/// Render a single `Assistant` block's wrapped lines into `lines`. Pulled out
/// of `render_chat`'s main loop so the streaming fast path can re-wrap just the
/// final block. A no-op for non-Assistant blocks.
pub(super) fn push_assistant_lines(
    lines: &mut Vec<Line<'static>>,
    block: &Block,
    inner_width: usize,
) {
    let Block::Assistant {
        text,
        thought_for_secs,
        ..
    } = block
    else {
        return;
    };
    // Compact "Thought for Xs" line once reasoning has completed for this
    // assistant block. While reasoning is still streaming, we suppress it —
    // the spinner row already communicates that the model is thinking.
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

/// Render a `User` block as the full-width gray stanza (like Claude Code): each
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
pub(super) fn finalize_chat_render(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    lines: Vec<Line<'static>>,
) {
    let total_lines = lines.len();
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = total_lines.saturating_sub(inner_height) as u16;
    // If the user manually scrolled back to (or past) the bottom, resume the
    // auto-follow behaviour. This is how scroll-down with the mouse wheel or
    // PageDown re-enables sticky-bottom without a dedicated key.
    if !app.auto_scroll && app.scroll >= max_scroll {
        app.auto_scroll = true;
    }
    let scroll = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };
    // Sync app.scroll with what we actually rendered. Without this, the field
    // is stale (initially 0); when the user mouse-scrolls up from a fully
    // auto-scrolled bottom, `scroll - 3` underflows to 0 and the view jumps
    // to the very top of the chat — the main "scroll feels broken" symptom.
    app.scroll = scroll;

    let p = Paragraph::new(lines).scroll((scroll, 0));
    f.render_widget(p, area);
}

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use std::path::Path;

use super::app::{App, Block};
use opencli_core::auth::AuthMode;

pub fn render(f: &mut Frame, app: &mut App) {
    // The same one-row slot shows the turn spinner OR the compaction progress
    // bar — they never run at once (compaction only starts once a turn ends).
    let spinner_h: u16 = if app.busy || app.compacting { 1 } else { 0 };
    let queue_h: u16 = queued_height(app);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                    // chat
            Constraint::Length(spinner_h),         // spinner (only while busy)
            Constraint::Length(queue_h),           // queued messages
            Constraint::Length(input_height(app)), // input
            Constraint::Length(1),                 // status line
        ])
        .split(f.area());

    render_chat(f, layout[0], app);
    if app.busy {
        render_spinner(f, layout[1], app);
    } else if app.compacting {
        render_compact_progress(f, layout[1], app);
    }
    if queue_h > 0 {
        render_queue(f, layout[2], app);
    }
    render_input(f, layout[3], app);
    render_status(f, layout[4], app);

    // Overlay popup — drawn above the input.
    if let Some((_, picker)) = &app.overlay {
        super::picker::render(f, layout[3], picker);
    }
    if app.pending_approval.is_some() {
        render_approval(f, layout[3], app);
    }
}

fn queued_height(app: &App) -> u16 {
    if app.message_queue.is_empty() {
        0
    } else {
        // Must match render_queue exactly: up to 4 message rows, plus a
        // " …+N queued" overflow row when more than 4 are queued, plus the
        // summary row. Omitting the overflow row clipped the summary off-screen.
        let total = app.message_queue.len();
        let shown = total.min(4) as u16;
        let overflow = u16::from(total > 4);
        shown + overflow + 1
    }
}

fn render_queue(f: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().fg(Color::Rgb(150, 150, 150));
    let chev = Style::default().fg(Color::Rgb(110, 110, 110));
    let mut lines: Vec<Line> = Vec::new();
    let width = area.width.saturating_sub(4) as usize;
    let show = app.message_queue.iter().take(4);
    let total = app.message_queue.len();
    for msg in show {
        let one_line = msg.replace('\n', " ");
        let truncated: String = if width > 0 && one_line.chars().count() > width {
            format!(
                "{}…",
                one_line
                    .chars()
                    .take(width.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            one_line
        };
        lines.push(Line::from(vec![
            Span::styled(" ⤴ ", chev),
            Span::styled(truncated, dim),
        ]));
    }
    if total > 4 {
        lines.push(Line::from(Span::styled(
            format!(" …+{} queued", total - 4),
            chev,
        )));
    }
    lines.push(Line::from(Span::styled(
        format!(
            " {} message{} queued · will send all together after this turn",
            total,
            if total == 1 { "" } else { "s" }
        ),
        chev,
    )));
    f.render_widget(Paragraph::new(lines), area);
}

fn render_spinner(f: &mut Frame, area: Rect, app: &App) {
    use super::app::SPINNER_FRAMES;
    let frame = SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()];
    let elapsed = app.turn_started_at.map(|t| t.elapsed()).unwrap_or_default();
    let mut extras = String::new();
    if app.tokens_used > 0 {
        extras.push_str(&format!(" · ↑ {} tokens", format_tokens(app.tokens_used)));
    }
    if app.is_thinking {
        extras.push_str(" · thinking");
    }
    let line = Line::from(vec![
        Span::styled(
            format!(" {frame} "),
            Style::default().fg(Color::Rgb(220, 130, 220)),
        ),
        Span::styled(
            format!("{}…", app.spinner_word),
            Style::default()
                .fg(Color::Rgb(220, 130, 220))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}{extras})", format_elapsed(elapsed)),
            Style::default().fg(Color::Rgb(160, 160, 160)),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// One-row progress bar for a running compaction. With no real percentage from
/// the model, the fill eases asymptotically toward 95% by elapsed time (so it
/// always looks alive, never stalls at 0), then snaps to 100% once the task
/// reports done. All widths use saturating/clamped math so a narrow terminal
/// can't underflow.
fn render_compact_progress(f: &mut Frame, area: Rect, app: &App) {
    let pct: u16 = if app.compact_done_at.is_some() {
        100
    } else {
        let t = app
            .compact_started_at
            .map(|s| s.elapsed().as_millis() as f64)
            .unwrap_or(0.0);
        (95.0 * t / (t + 4000.0)).round().clamp(0.0, 95.0) as u16
    };
    let purple = Style::default().fg(Color::Rgb(220, 130, 220));
    let dim = Style::default().fg(Color::Rgb(160, 160, 160));
    let track = Style::default().fg(Color::Rgb(90, 90, 90));

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

fn render_chat(f: &mut Frame, area: Rect, app: &mut App) {
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
            Block::User(text) => {
                let chevron_style = Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD);
                let body_style = Style::default().fg(Color::White);
                let mut first = true;
                for raw in text.split('\n') {
                    for w in wrap(raw, inner_width.saturating_sub(2)) {
                        let prefix = if first { "> " } else { "  " };
                        first = false;
                        lines.push(Line::from(vec![
                            Span::styled(prefix.to_string(), chevron_style),
                            Span::styled(w, body_style),
                        ]));
                    }
                }
                lines.push(Line::raw(""));
            }
            Block::Assistant { .. } => {
                push_assistant_lines(&mut lines, &app.blocks[i], inner_width);
            }
            Block::Tool {
                name,
                args,
                output,
                error,
                ..
            } => {
                render_tool(
                    &mut lines,
                    name,
                    args,
                    output.as_deref(),
                    *error,
                    inner_width,
                    expanded,
                );
            }
            Block::System(text) => {
                for l in wrap(text, inner_width.saturating_sub(2)) {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(Color::Rgb(160, 160, 160)),
                    )));
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
fn push_assistant_lines(lines: &mut Vec<Line<'static>>, block: &Block, inner_width: usize) {
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
            Span::styled("· ", Style::default().fg(Color::Rgb(200, 120, 220))),
            Span::styled(
                format!("Thought for {secs}s"),
                Style::default()
                    .fg(Color::Rgb(190, 190, 190))
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        lines.push(Line::raw(""));
    }
    // Raw reasoning text is intentionally suppressed in chat history.
    if !text.is_empty() {
        for raw in text.split('\n') {
            for l in wrap(raw, inner_width.saturating_sub(2)) {
                let spans = render_markdown_inline(&l);
                let mut row = vec![Span::raw("  ")];
                row.extend(spans);
                lines.push(Line::from(row));
            }
        }
        lines.push(Line::raw(""));
    }
}

/// Compute a cheap fingerprint of a block's mutable content. Streaming
/// deltas grow `text`/`output`; a length change invalidates the cache. The
/// fingerprint deliberately ignores identifiers and timing fields because
/// those don't affect the wrapped output.
fn block_fingerprint(block: &Block) -> usize {
    match block {
        Block::Welcome => 0,
        Block::User(s) | Block::System(s) => s.len(),
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
            .wrapping_add(if *error { 1 } else { 0 }),
    }
}

/// Shared tail of `render_chat`: scroll math + Paragraph dispatch. Same
/// code runs whether we hit the cache (early return) or just rebuilt the
/// lines; pulled into a helper to keep the two paths in lockstep.
fn finalize_chat_render(f: &mut Frame, area: Rect, app: &mut App, lines: Vec<Line<'static>>) {
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

fn render_input(f: &mut Frame, area: Rect, app: &App) {
    use ratatui::widgets::{Block as RBlock, BorderType, Borders};

    let prompt_color = if app.busy {
        Color::Rgb(160, 160, 160)
    } else {
        Color::Magenta
    };
    let prompt_style = Style::default()
        .fg(prompt_color)
        .add_modifier(Modifier::BOLD);

    // Rounded border around the prompt, matching Claude Code's input box. The
    // border dims while a turn is running so the box reads as "not your turn".
    let border_color = if app.busy {
        Color::Rgb(80, 80, 80)
    } else {
        Color::Rgb(120, 120, 120)
    };
    let block = RBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));
    let bordered = block.inner(area);
    f.render_widget(block, area);
    // One column of breathing room inside the border on each side, so the "> "
    // prompt isn't flush against the left edge (matches Claude Code's box).
    let inner = Rect {
        x: bordered.x.saturating_add(1),
        y: bordered.y,
        width: bordered.width.saturating_sub(2),
        height: bordered.height,
    };

    if app.input.is_empty() {
        let lines = vec![Line::from(vec![
            Span::styled("> ", prompt_style),
            Span::styled(
                "Try \"build me a todo list app\"",
                Style::default().fg(Color::Rgb(160, 160, 160)),
            ),
        ])];
        f.render_widget(Paragraph::new(lines), inner);
        if inner.width > 2 && inner.height > 0 {
            f.set_cursor_position((inner.x + 2, inner.y));
        }
        return;
    }

    // Char-wrap each logical line at the content width and prefix every visual
    // row with a 2-col gutter, so the rendered rows and the cursor share ONE
    // wrap model. ratatui's word Wrap diverged from cursor_pos()'s logical
    // coordinates, which let the cursor drift off the row (and vanish) on long
    // or soft-wrapped input.
    let content_w = (inner.width as usize).saturating_sub(2).max(1);
    let (cur_line, cur_col) = app.input.cursor_pos();
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_rc: Option<(usize, usize)> = None;
    for (li, logical) in app.input.lines().into_iter().enumerate() {
        let want = if li == cur_line { Some(cur_col) } else { None };
        let (rows, cur_in_line) = wrap_visual_rows(logical, content_w, want);
        if let Some((r, c)) = cur_in_line {
            cursor_rc = Some((lines.len() + r, c));
        }
        for (vi, row) in rows.into_iter().enumerate() {
            let gutter = if li == 0 && vi == 0 {
                Span::styled("> ", prompt_style)
            } else {
                Span::raw("  ")
            };
            lines.push(Line::from(vec![gutter, Span::raw(row)]));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);

    if let Some((row, col)) = cursor_rc {
        let cx = inner
            .x
            .saturating_add(2)
            .saturating_add(u16::try_from(col).unwrap_or(u16::MAX));
        let cy = inner
            .y
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        if cx < inner.x + inner.width && cy < inner.y + inner.height {
            f.set_cursor_position((cx, cy));
        }
    }
}

/// Character-wrap one logical input line into visual rows of at most `width`
/// display columns. When `cursor_col` is the cursor's display column within
/// this logical line, also return the cursor's (visual_row, visual_col) under
/// the SAME wrapping, so the rendered cursor never drifts off the drawn text.
fn wrap_visual_rows(
    line: &str,
    width: usize,
    cursor_col: Option<usize>,
) -> (Vec<String>, Option<(usize, usize)>) {
    use unicode_width::UnicodeWidthChar;
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize; // display width of `cur`
    let mut col = 0usize; // display cols consumed from line start
    let mut cursor_rc: Option<(usize, usize)> = None;
    for ch in line.chars() {
        let w = ch.width().unwrap_or(0);
        // Break before a char that would overflow the current row.
        if cur_w + w > width && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        // The cursor sits immediately before the char at display col `col`.
        if cursor_rc.is_none() && cursor_col == Some(col) {
            cursor_rc = Some((rows.len(), cur_w));
        }
        cur.push(ch);
        cur_w += w;
        col += w;
    }
    // Cursor at (or past) the end of the line.
    if cursor_rc.is_none() {
        if let Some(c) = cursor_col {
            if c >= col {
                cursor_rc = Some((rows.len(), cur_w));
            }
        }
    }
    rows.push(cur);
    // A cursor exactly at the right edge (end of a full row) belongs at the
    // start of the next visual row, not off the edge.
    if let Some((r, c)) = cursor_rc {
        if c >= width {
            cursor_rc = Some((r + 1, 0));
        }
    }
    (rows, cursor_rc)
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    // Left side: just a small hint or the explicit status line.
    let mode_label = app.permission_mode().label();
    let left_text = if !app.status_line.is_empty() {
        format!("{mode_label}  ·  {}", app.status_line)
    } else if app.expanded_tools {
        format!("{mode_label}  ·  ⇲ tool view: expanded · Ctrl+O to collapse")
    } else {
        format!("{mode_label}  ·  shift+tab cycles mode · ? for shortcuts")
    };
    let left_para = Paragraph::new(Line::from(Span::styled(
        left_text,
        Style::default().fg(Color::Rgb(160, 160, 160)),
    )));

    // Right side: model · effort · cwd
    let cwd = shorten_home_path(&app.cwd);
    let auth_dot = match app.auth_mode {
        AuthMode::None => Span::styled("● ", Style::default().fg(Color::Red)),
        AuthMode::OpenaiApiKey => Span::styled("● ", Style::default().fg(Color::Cyan)),
        AuthMode::OpenaiOauth => Span::styled("● ", Style::default().fg(Color::Green)),
        AuthMode::AnthropicApiKey => Span::styled("● ", Style::default().fg(Color::Magenta)),
        AuthMode::AnthropicOauth => Span::styled("● ", Style::default().fg(Color::Yellow)),
    };
    let right_spans = vec![
        auth_dot,
        Span::styled(app.config.model.clone(), Style::default().fg(Color::Gray)),
        Span::styled(
            format!(" · {}", app.config.reasoning_effort),
            Style::default().fg(Color::Rgb(160, 160, 160)),
        ),
        Span::styled(
            format!("  {cwd} "),
            Style::default().fg(Color::Rgb(160, 160, 160)),
        ),
    ];
    let right_text: String = right_spans.iter().map(|s| s.content.as_ref()).collect();
    let right_width = unicode_width::UnicodeWidthStr::width(right_text.as_str()) as u16;
    let total = area.width;
    let left_width = total.saturating_sub(right_width).saturating_sub(1);

    let left_rect = Rect {
        x: area.x,
        y: area.y,
        width: left_width,
        height: 1,
    };
    let right_rect = Rect {
        x: area.x + left_width,
        y: area.y,
        width: total.saturating_sub(left_width),
        height: 1,
    };
    f.render_widget(left_para, left_rect);
    f.render_widget(Paragraph::new(Line::from(right_spans)), right_rect);
}

fn render_approval(f: &mut Frame, anchor_area: ratatui::layout::Rect, app: &App) {
    use ratatui::widgets::{Block as RBlock, BorderType, Borders, Clear};
    let Some(p) = app.pending_approval.as_ref() else {
        return;
    };

    let dim = Style::default().fg(Color::Rgb(170, 170, 170));
    let bg = Style::default().bg(Color::Rgb(20, 20, 22));
    let accent = Style::default()
        .fg(Color::Rgb(25, 195, 154))
        .add_modifier(Modifier::BOLD);
    let warn = Style::default()
        .fg(Color::Rgb(255, 182, 73))
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  Tool: ", dim),
        Span::styled(p.tool_name.clone(), accent),
    ]));
    let args_preview = condense_args(&p.args_json);
    if !args_preview.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  args: {args_preview}"),
            dim,
        )));
    }
    if let Some(d) = p.diff_preview.as_ref() {
        lines.push(Line::from(Span::styled("  ─ preview ─", dim)));
        for raw in d.lines().take(8) {
            lines.push(Line::from(Span::styled(
                format!("  {raw}"),
                Style::default().fg(Color::Rgb(220, 220, 220)),
            )));
        }
        if d.lines().count() > 8 {
            lines.push(Line::from(Span::styled("  …", dim)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  [Y] ", warn),
        Span::styled("approve   ", Style::default().fg(Color::Rgb(235, 235, 235))),
        Span::styled("[N] ", warn),
        Span::styled("deny   ", Style::default().fg(Color::Rgb(235, 235, 235))),
        Span::styled("[Esc] ", warn),
        Span::styled("cancel", Style::default().fg(Color::Rgb(235, 235, 235))),
    ]));

    let height = (lines.len() as u16).min(14) + 2;
    let width = 72u16.min(anchor_area.width.saturating_sub(4));
    let x = anchor_area.x + 1;
    let bottom = anchor_area.y;
    let y = bottom.saturating_sub(height);
    let popup = ratatui::layout::Rect {
        x,
        y,
        width,
        height,
    };
    f.render_widget(Clear, popup);
    let block = RBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(255, 182, 73)))
        .title(Span::styled(" Approve tool call? ", warn))
        .style(bg);
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(
        Paragraph::new(lines).style(bg).wrap(Wrap { trim: false }),
        inner,
    );
}

fn condense_args(json: &str) -> String {
    let trimmed = json.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return String::new();
    }
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    let one_line = match parsed {
        Ok(serde_json::Value::Object(m)) => m
            .into_iter()
            .map(|(k, v)| {
                let vs = serde_json::to_string(&v).unwrap_or_default();
                // Truncate by char count — byte index would panic mid-codepoint.
                let vs = if vs.chars().count() > 60 {
                    let cut: String = vs.chars().take(60).collect();
                    format!("{cut}…")
                } else {
                    vs
                };
                format!("{k}={vs}")
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => trimmed.replace('\n', " "),
    };
    if one_line.chars().count() > 220 {
        let cut: String = one_line.chars().take(219).collect();
        format!("{cut}…")
    } else {
        one_line
    }
}

fn format_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Very small inline markdown renderer: handles `code`, **bold**, *italic*.
fn render_markdown_inline(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();
    let code_style = Style::default()
        .fg(Color::Rgb(255, 184, 108))
        .bg(Color::Rgb(40, 30, 18));
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let italic_style = Style::default().add_modifier(Modifier::ITALIC);
    let plain = Style::default().fg(Color::Gray);

    while let Some(c) = chars.next() {
        match c {
            '`' => {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), plain));
                }
                let mut code = String::new();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc == '`' {
                        break;
                    }
                    code.push(nc);
                }
                spans.push(Span::styled(code, code_style));
            }
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), plain));
                }
                let mut bold = String::new();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc == '*' && chars.peek() == Some(&'*') {
                        chars.next();
                        break;
                    }
                    bold.push(nc);
                }
                spans.push(Span::styled(bold, bold_style));
            }
            '*' => {
                if !buf.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut buf), plain));
                }
                let mut italic = String::new();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc == '*' {
                        break;
                    }
                    italic.push(nc);
                }
                spans.push(Span::styled(italic, italic_style));
            }
            _ => buf.push(c),
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, plain));
    }
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    spans
}

fn render_welcome(lines: &mut Vec<Line<'static>>, app: &App) {
    let dim = Style::default().fg(Color::Rgb(160, 160, 160));
    let muted = Style::default().fg(Color::Rgb(125, 125, 125));
    let strong = Style::default()
        .fg(Color::Rgb(230, 230, 230))
        .add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(Color::Rgb(25, 195, 154));
    let border = Style::default().fg(Color::Rgb(80, 80, 80));

    let cwd = shorten_home_path(&app.cwd);

    let auth_label = match app.auth_mode {
        opencli_core::auth::AuthMode::OpenaiOauth => "ChatGPT account",
        opencli_core::auth::AuthMode::OpenaiApiKey => "OpenAI API key",
        opencli_core::auth::AuthMode::AnthropicOauth => "Claude OAuth",
        opencli_core::auth::AuthMode::AnthropicApiKey => "Anthropic API key",
        opencli_core::auth::AuthMode::None => "offline",
    };

    // Each row is `(spans, visible_width)`. Width is precomputed so the
    // right edge of the rounded border stays pinned even when the terminal
    // resizes or the cwd changes — mirrors Claude Code's welcome card.
    let version = env!("CARGO_PKG_VERSION");
    let sparkle = "✻ ";
    let title = "Welcome to opencli! ";
    let version_label = format!("v{version}");
    let header_w = sparkle.chars().count() + title.chars().count() + version_label.chars().count();
    let header_row = (
        vec![
            Span::styled(sparkle, accent),
            Span::styled(title, strong),
            Span::styled(version_label, dim),
        ],
        header_w,
    );

    let help_text = "/help for commands · /clear to reset · Ctrl+C to exit";
    let help_row = (
        vec![Span::styled(help_text, dim)],
        help_text.chars().count(),
    );

    let paste_text = "Ctrl+V to paste text or an image from your clipboard";
    let paste_row = (
        vec![Span::styled(paste_text, dim)],
        paste_text.chars().count(),
    );

    let model_summary = format!(
        "{} · effort {} · verbosity {} · {}",
        app.config.model, app.config.reasoning_effort, app.config.verbosity, auth_label
    );
    let model_row = (
        vec![Span::styled(model_summary.clone(), muted)],
        model_summary.chars().count(),
    );

    let cwd_label = "cwd: ";
    let cwd_row = (
        vec![
            Span::styled(cwd_label, muted),
            Span::styled(cwd.clone(), strong),
        ],
        cwd_label.chars().count() + cwd.chars().count(),
    );

    let rows: Vec<(Vec<Span<'static>>, usize)> = vec![
        header_row,
        (vec![], 0),
        help_row,
        paste_row,
        (vec![], 0),
        model_row,
        cwd_row,
    ];

    const MIN_INNER: usize = 56;
    let inner_width = rows
        .iter()
        .map(|(_, w)| *w)
        .max()
        .unwrap_or(0)
        .max(MIN_INNER);

    let horiz: String = "─".repeat(inner_width + 2);

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  ", muted),
        Span::styled(format!("╭{horiz}╮"), border),
    ]));
    for (spans, w) in rows {
        let pad = inner_width.saturating_sub(w);
        let mut row: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 3);
        row.push(Span::styled("  ", muted));
        row.push(Span::styled("│ ", border));
        row.extend(spans);
        row.push(Span::styled(format!("{} ", " ".repeat(pad)), border));
        row.push(Span::styled("│", border));
        lines.push(Line::from(row));
    }
    lines.push(Line::from(vec![
        Span::styled("  ", muted),
        Span::styled(format!("╰{horiz}╯"), border),
    ]));
    lines.push(Line::raw(""));
}

/// Render a run of consecutive `read_file` tool calls as a single block.
/// Avoids the "N back-to-back Read stanzas" wall of identical-looking
/// 3-line groups that dominates the chat when the model reads many files.
fn render_read_group(lines: &mut Vec<Line<'static>>, blocks: &[Block], expanded: bool) {
    use serde_json::Value;

    let mut entries: Vec<(String, usize, bool, bool)> = Vec::new();
    for b in blocks {
        if let Block::Tool {
            args,
            output,
            error,
            ..
        } = b
        {
            let parsed: Value = if args.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(args).unwrap_or(Value::Null)
            };
            let path = parsed
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let line_count = output.as_deref().map(|o| o.lines().count()).unwrap_or(0);
            entries.push((path, line_count, *error, output.is_some()));
        }
    }
    if entries.is_empty() {
        return;
    }

    let any_error = entries.iter().any(|(_, _, err, _)| *err);
    let any_pending = entries.iter().any(|(_, _, _, done)| !done);
    let bullet_color = if any_error {
        Color::Red
    } else if any_pending {
        Color::Yellow
    } else {
        Color::Green
    };

    let total_lines: usize = entries.iter().map(|(_, l, _, _)| *l).sum();
    let count = entries.len();
    let dim = Style::default().fg(Color::Rgb(160, 160, 160));
    let gray = Style::default().fg(Color::Gray);

    if count == 1 {
        // Single read: keep the familiar "Read(path)" header but with a tiny
        // summary on the next line, no contents.
        let (path, lc, _err, _done) = &entries[0];
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(bullet_color)),
            Span::styled(
                "Read".to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("({})", pretty_path(path)), gray),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  ⎿ ".to_string(), dim),
            Span::styled(format!("{} line{}", lc, plural(*lc)), gray),
        ]));
    } else {
        // Multi-read: one header summarising the batch, optional file list
        // only when in expanded mode.
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(bullet_color)),
            Span::styled(
                format!("Read {} files", count),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {} lines total", total_lines), gray),
        ]));
        if expanded {
            for (idx, (path, lc, err, done)) in entries.iter().enumerate() {
                let branch = if idx == 0 { "  ⎿ " } else { "    " };
                let path_style = if *err {
                    Style::default().fg(Color::Red)
                } else if !*done {
                    Style::default().fg(Color::Yellow)
                } else {
                    gray
                };
                lines.push(Line::from(vec![
                    Span::styled(branch.to_string(), dim),
                    Span::styled(pretty_path(path), path_style),
                    Span::styled(format!(" · {} line{}", lc, plural(*lc)), dim),
                ]));
            }
        }
    }
    lines.push(Line::raw(""));
}

fn render_tool(
    lines: &mut Vec<Line<'static>>,
    name: &str,
    args: &str,
    output: Option<&str>,
    error: bool,
    inner_width: usize,
    expanded: bool,
) {
    // Args may still be partial JSON while streaming — treat parse failure as
    // Null so the header degrades to "Tool()" rather than disappearing.
    let parsed: serde_json::Value = if args.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(args).unwrap_or(serde_json::Value::Null)
    };

    let (display_name, summary) = friendly_header(name, &parsed);
    let bullet_color = if error {
        Color::Red
    } else if output.is_none() {
        Color::Yellow
    } else {
        Color::Green
    };

    // Header: ● Write(file)
    let mut header_spans = vec![
        Span::styled("● ", Style::default().fg(bullet_color)),
        Span::styled(
            display_name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!("({summary})"),
            Style::default().fg(Color::Gray),
        ));
    }
    lines.push(Line::from(header_spans));

    // Body
    let body_lines = friendly_body(name, &parsed, output, error, inner_width, expanded);
    let total = body_lines.len();
    // When there is no body content (e.g. a tool that produced an empty
    // summary in compact mode), don't emit a trailing blank line — that just
    // adds noise between successive tool calls.
    if total == 0 {
        lines.push(Line::raw(""));
        return;
    }
    for (i, body) in body_lines.into_iter().enumerate() {
        // Claude Code branches the first result line with `⎿`, then aligns any
        // continuation lines under it — no per-line gutter glyph.
        let branch = if i == 0 { "  ⎿ " } else { "    " };
        lines.push(Line::from(
            std::iter::once(Span::styled(
                branch.to_string(),
                Style::default().fg(Color::Rgb(160, 160, 160)),
            ))
            .chain(body.spans)
            .collect::<Vec<_>>(),
        ));
    }
    lines.push(Line::raw(""));
}

fn friendly_header(name: &str, args: &serde_json::Value) -> (String, String) {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    match name {
        "read_file" => ("Read".into(), pretty_path(&s("path"))),
        "write_file" => ("Write".into(), pretty_path(&s("path"))),
        "edit_file" => ("Update".into(), pretty_path(&s("path"))),
        "multi_edit" => ("Update".into(), pretty_path(&s("path"))),
        "list_dir" => ("List".into(), pretty_path(&s("path"))),
        "grep" => {
            let pat = s("pattern");
            let path = s("path");
            if path.is_empty() {
                ("Grep".into(), format!("\"{pat}\""))
            } else {
                (
                    "Grep".into(),
                    format!("\"{pat}\" in {}", pretty_path(&path)),
                )
            }
        }
        "glob" => ("Glob".into(), s("pattern")),
        "todo_write" => ("Update Todos".into(), String::new()),
        "ask_user_question" => {
            // The full question + options render in the System block pushed just
            // below this tool result; the header only needs the first chip label.
            let first_header = args
                .get("questions")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|q| q.get("header"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            ("Question".into(), first_header.to_string())
        }
        "run_shell" => {
            let cmd = s("command");
            let short = if cmd.chars().count() > 80 {
                format!("{}…", cmd.chars().take(80).collect::<String>())
            } else {
                cmd
            };
            ("Bash".into(), short)
        }
        other => (other.to_string(), compact_args(&args.to_string())),
    }
}

fn friendly_body<'a>(
    name: &str,
    args: &serde_json::Value,
    output: Option<&str>,
    error: bool,
    width: usize,
    expanded: bool,
) -> Vec<Line<'a>> {
    let mut out: Vec<Line> = Vec::new();
    let avail = width.saturating_sub(4); // minus branch "  │ "
    let Some(text) = output else {
        out.push(Line::from(Span::styled(
            "…",
            Style::default().fg(Color::Yellow),
        )));
        return out;
    };

    if error {
        // Always show full error text (regardless of compact mode) so the model
        // and the user can diagnose. Cap to avoid runaway output.
        let max_err = if expanded { usize::MAX } else { 30 };
        let total = text.lines().count();
        for raw in text.lines().take(max_err) {
            for w in wrap(raw, avail) {
                out.push(Line::from(Span::styled(w, Style::default().fg(Color::Red))));
            }
        }
        if total > max_err {
            out.push(Line::from(Span::styled(
                format!("… +{} lines", total - max_err),
                Style::default().fg(Color::Rgb(160, 160, 160)),
            )));
        }
        return out;
    }

    let style_summary = Style::default().fg(Color::Gray);
    let style_meta = Style::default().fg(Color::Rgb(160, 160, 160));
    let style_code = Style::default().fg(Color::White);
    let style_lineno = Style::default().fg(Color::Rgb(160, 160, 160));

    // Per-tool limits: (compact, expanded).
    let limits = BodyLimits::for_mode(expanded);

    match name {
        "write_file" => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let line_count = if content.is_empty() {
                0
            } else {
                content.lines().count().max(1)
            };
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("(file)");
            out.push(Line::from(Span::styled(
                format!("Wrote {} lines to {}", line_count, pretty_path(path)),
                style_summary,
            )));
            append_numbered(
                &mut out,
                content,
                limits.write_preview,
                style_lineno,
                style_code,
                avail,
            );
        }
        "edit_file" => {
            let old = args
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_ = args
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let added = if new_.is_empty() {
                0
            } else {
                new_.lines().count()
            };
            let removed = if old.is_empty() {
                0
            } else {
                old.lines().count()
            };
            let summary_text = match (added, removed) {
                (a, 0) => format!("Added {a} line{}", plural(a)),
                (0, r) => format!("Removed {r} line{}", plural(r)),
                (a, r) => format!("Added {a} line{}, removed {r} line{}", plural(a), plural(r)),
            };
            out.push(Line::from(Span::styled(summary_text, style_summary)));

            // Determine starting line number by trying to locate old_string in the file.
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let start_line = locate_line_number(path, old).unwrap_or(1);

            let removed_bg = Style::default()
                .bg(Color::Rgb(60, 0, 0))
                .fg(Color::Rgb(255, 120, 120));
            let added_bg = Style::default()
                .bg(Color::Rgb(0, 50, 0))
                .fg(Color::Rgb(160, 255, 160));
            let lineno_removed = Style::default()
                .bg(Color::Rgb(60, 0, 0))
                .fg(Color::Rgb(200, 80, 80));
            let lineno_added = Style::default()
                .bg(Color::Rgb(0, 50, 0))
                .fg(Color::Rgb(120, 200, 120));

            let mut shown = 0usize;
            let max_diff = limits.edit_diff;

            for (i, line) in old.lines().enumerate() {
                if shown >= max_diff {
                    break;
                }
                let n = start_line + i;
                out.push(diff_line(n, "-", line, lineno_removed, removed_bg, avail));
                shown += 1;
            }
            for (i, line) in new_.lines().enumerate() {
                if shown >= max_diff {
                    break;
                }
                let n = start_line + i;
                out.push(diff_line(n, "+", line, lineno_added, added_bg, avail));
                shown += 1;
            }
            let total = old.lines().count() + new_.lines().count();
            if total > max_diff {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines", total - max_diff),
                    style_meta,
                )));
            }
        }
        "read_file" => {
            // Just the line count — never dump file contents into the chat.
            // The model already has the file in its context; the user only
            // needs to know that the read happened.
            let total = text.lines().count();
            out.push(Line::from(Span::styled(
                format!("Read {total} line{}", plural(total)),
                style_summary,
            )));
        }
        "run_shell" => {
            // Output format: "exit_code: N\n--- stdout ---\n…\n--- stderr ---\n…"
            let (code, stdout, stderr) = parse_shell_output(text);
            let success = code == 0;
            if !stdout.is_empty() {
                let total = stdout.lines().count();
                for raw in stdout.lines().take(limits.shell_stdout) {
                    for w in wrap(raw, avail) {
                        out.push(Line::from(Span::styled(w, style_code)));
                    }
                }
                let extra = total.saturating_sub(limits.shell_stdout);
                if extra > 0 {
                    out.push(Line::from(Span::styled(
                        format!("… +{extra} more line{} (Ctrl+O for more)", plural(extra)),
                        style_meta,
                    )));
                }
            }
            let stderr_trim = stderr.trim();
            if !stderr_trim.is_empty() {
                if !success || expanded {
                    // Show stderr in full when the command failed, or when in
                    // expanded mode for diagnostic context.
                    out.push(Line::from(Span::styled(
                        "─ stderr ─",
                        Style::default().fg(Color::Yellow),
                    )));
                    let total_err = stderr.lines().count();
                    let max_err = limits.shell_stderr;
                    for raw in stderr.lines().take(max_err) {
                        for w in wrap(raw, avail) {
                            out.push(Line::from(Span::styled(
                                w,
                                Style::default().fg(Color::Yellow),
                            )));
                        }
                    }
                    if total_err > max_err {
                        out.push(Line::from(Span::styled(
                            format!(
                                "… +{} stderr line{}",
                                total_err - max_err,
                                plural(total_err - max_err)
                            ),
                            style_meta,
                        )));
                    }
                } else {
                    // Success but with stderr noise (warnings, etc.). One-line
                    // hint keeps things clean without losing the signal.
                    let n = stderr.lines().filter(|l| !l.trim().is_empty()).count();
                    out.push(Line::from(Span::styled(
                        format!(
                            "(+ {n} stderr line{} suppressed — Ctrl+O to view)",
                            plural(n)
                        ),
                        style_meta,
                    )));
                }
            }
            if !success || expanded {
                let code_style = if success {
                    style_meta
                } else {
                    Style::default().fg(Color::Red)
                };
                out.push(Line::from(Span::styled(format!("exit {code}"), code_style)));
            }
        }
        "grep" => {
            let total = text.lines().filter(|l| !l.is_empty()).count();
            out.push(Line::from(Span::styled(
                format!("{total} match{}", if total == 1 { "" } else { "es" }),
                style_summary,
            )));
            for raw in text.lines().take(limits.grep_preview) {
                for w in wrap(raw, avail) {
                    out.push(Line::from(Span::styled(w, style_code)));
                }
            }
            if total > limits.grep_preview {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines (Ctrl+O for more)", total - limits.grep_preview),
                    style_meta,
                )));
            }
        }
        "todo_write" => {
            // Render the canonical todo list as a checklist, mirroring the
            // Claude Code CLI presentation. Falls back to the summary text
            // when the args JSON hasn't fully arrived yet.
            let Some(todos) = args.get("todos").and_then(|v| v.as_array()) else {
                out.push(Line::from(Span::styled(
                    text.lines().next().unwrap_or("").to_string(),
                    style_summary,
                )));
                return out;
            };

            let done_text = Style::default()
                .fg(Color::Rgb(130, 130, 130))
                .add_modifier(Modifier::CROSSED_OUT);
            let pending_text = Style::default().fg(Color::Gray);
            let active_text = Style::default()
                .fg(Color::Rgb(255, 184, 108))
                .add_modifier(Modifier::BOLD);
            let check_done = Style::default().fg(Color::Green);
            let check_pending = Style::default().fg(Color::Rgb(160, 160, 160));
            let check_active = Style::default().fg(Color::Rgb(255, 184, 108));

            for todo in todos {
                let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let active = todo
                    .get("active_form")
                    .and_then(|v| v.as_str())
                    .unwrap_or(content);
                let status = todo
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending");
                let (symbol, sym_style, body_style, label) = match status {
                    "completed" => ("☒", check_done, done_text, content),
                    "in_progress" => ("☐", check_active, active_text, active),
                    _ => ("☐", check_pending, pending_text, content),
                };
                let label_wrapped = wrap(label, avail.saturating_sub(2));
                let mut first = true;
                for piece in label_wrapped {
                    if first {
                        out.push(Line::from(vec![
                            Span::styled(format!("{symbol} "), sym_style),
                            Span::styled(piece, body_style),
                        ]));
                        first = false;
                    } else {
                        out.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(piece, body_style),
                        ]));
                    }
                }
            }
        }
        "ask_user_question" => {
            // No inline body: the System block rendered right below this tool
            // result already shows the questions and options. Dumping the raw
            // envelope JSON here just duplicated it.
        }
        "glob" | "list_dir" => {
            let total = text.lines().filter(|l| !l.is_empty()).count();
            out.push(Line::from(Span::styled(
                format!("{total} entr{}", if total == 1 { "y" } else { "ies" }),
                style_summary,
            )));
            for raw in text.lines().take(limits.list_preview) {
                if raw.is_empty() {
                    continue;
                }
                for w in wrap(raw, avail) {
                    out.push(Line::from(Span::styled(w, style_code)));
                }
            }
            if total > limits.list_preview {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines (Ctrl+O for more)", total - limits.list_preview),
                    style_meta,
                )));
            }
        }
        _ => {
            let total = text.lines().count();
            for raw in text.lines().take(limits.default_preview) {
                for w in wrap(raw, avail) {
                    out.push(Line::from(Span::styled(w, style_code)));
                }
            }
            if total > limits.default_preview {
                out.push(Line::from(Span::styled(
                    format!(
                        "… +{} lines (Ctrl+O for more)",
                        total - limits.default_preview
                    ),
                    style_meta,
                )));
            }
        }
    }
    out
}

struct BodyLimits {
    write_preview: usize,
    edit_diff: usize,
    shell_stdout: usize,
    shell_stderr: usize,
    grep_preview: usize,
    list_preview: usize,
    default_preview: usize,
}

impl BodyLimits {
    fn for_mode(expanded: bool) -> Self {
        if expanded {
            Self {
                write_preview: 60,
                edit_diff: 80,
                shell_stdout: 80,
                shell_stderr: 30,
                grep_preview: 60,
                list_preview: 60,
                default_preview: 60,
            }
        } else {
            Self {
                write_preview: 6,
                edit_diff: 8,
                shell_stdout: 3,
                shell_stderr: 6,
                grep_preview: 5,
                list_preview: 5,
                default_preview: 5,
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn diff_line<'a>(
    n: usize,
    sigil: &'a str,
    text: &str,
    no_style: Style,
    body_style: Style,
    width: usize,
) -> Line<'a> {
    let body_width = width.saturating_sub(7);
    let truncated: String = if body_width > 0 && text.chars().count() > body_width {
        format!(
            "{}…",
            text.chars()
                .take(body_width.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        text.to_string()
    };
    Line::from(vec![
        Span::styled(format!("{:>4} ", n), no_style),
        Span::styled(format!("{sigil} "), body_style.add_modifier(Modifier::BOLD)),
        Span::styled(truncated, body_style),
    ])
}

fn locate_line_number(path: &str, needle: &str) -> Option<usize> {
    if path.is_empty() || needle.is_empty() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let idx = content.find(needle)?;
    Some(content[..idx].matches('\n').count() + 1)
}

fn append_numbered(
    out: &mut Vec<Line<'static>>,
    content: &str,
    max_lines: usize,
    no_style: Style,
    code_style: Style,
    width: usize,
) {
    let total = content.lines().count();
    for (i, raw) in content.lines().enumerate().take(max_lines) {
        let n = i + 1;
        let mut first = true;
        for w in wrap(raw, width.saturating_sub(5)) {
            if first {
                out.push(Line::from(vec![
                    Span::styled(format!("{:>4} ", n), no_style),
                    Span::styled(w, code_style),
                ]));
                first = false;
            } else {
                out.push(Line::from(vec![
                    Span::styled("     ".to_string(), no_style),
                    Span::styled(w, code_style),
                ]));
            }
        }
    }
    if total > max_lines {
        out.push(Line::from(Span::styled(
            format!("… +{} lines", total - max_lines),
            Style::default().fg(Color::Rgb(160, 160, 160)),
        )));
    }
}

fn parse_shell_output(text: &str) -> (i32, String, String) {
    let mut code = 0i32;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut section = 0; // 0=preamble, 1=stdout, 2=stderr
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("exit_code: ") {
            code = rest.trim().parse().unwrap_or(0);
            continue;
        }
        if line.starts_with("--- stdout") {
            section = 1;
            continue;
        }
        if line.starts_with("--- stderr") {
            section = 2;
            continue;
        }
        match section {
            1 => {
                if !stdout.is_empty() {
                    stdout.push('\n');
                }
                stdout.push_str(line);
            }
            2 => {
                if !stderr.is_empty() {
                    stderr.push('\n');
                }
                stderr.push_str(line);
            }
            _ => {}
        }
    }
    (code, stdout, stderr)
}

fn pretty_path(p: &str) -> String {
    shorten_home_path(Path::new(p))
}

fn shorten_home_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        return shorten_path_with_home(path, &home);
    }
    path.display().to_string()
}

fn shorten_path_with_home(path: &Path, home: &Path) -> String {
    let Ok(rest) = path.strip_prefix(home) else {
        return path.display().to_string();
    };
    if rest.as_os_str().is_empty() {
        "~".to_string()
    } else {
        format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return text.lines().map(|s| s.to_string()).collect();
    }
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        for w in textwrap::wrap(line, width) {
            out.push(w.into_owned());
        }
    }
    out
}

fn compact_args(s: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        if let Some(obj) = v.as_object() {
            return obj
                .iter()
                .map(|(k, val)| {
                    let pretty = match val {
                        serde_json::Value::String(s) => {
                            let trimmed: String = s.chars().take(50).collect();
                            format!(
                                "\"{}{}\"",
                                trimmed,
                                if s.chars().count() > 50 { "…" } else { "" }
                            )
                        }
                        _ => val.to_string(),
                    };
                    format!("{k}={pretty}")
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    let mut s = s.replace('\n', " ");
    if s.len() > 100 {
        // String::truncate panics if the byte index isn't on a char boundary
        // (Vietnamese/emoji/CJK in tool args). Walk back to the previous
        // valid boundary before slicing.
        let mut cut = 100;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

fn input_height(app: &App) -> u16 {
    let max_visible = (app.last_height / 3).max(3) as usize;
    // -6: 1 col of rounded border + 1 col of inner padding on each side (4),
    // plus the 2-col "> " gutter. Must match the content width used in
    // `render_input` so the wrapped row count here matches what's actually
    // drawn (otherwise a long line overflows the box's bottom border).
    let content_w = (app.last_width as usize).saturating_sub(6).max(1);
    let rows = input_visual_row_count(app.input.lines(), content_w);
    let inner = rows.min(max_visible);
    // +2 for the top and bottom border rows of the rounded input box.
    (inner as u16).saturating_add(2)
}

fn input_visual_row_count<'a, I>(lines: I, content_w: usize) -> usize
where
    I: IntoIterator<Item = &'a str>,
{
    lines
        .into_iter()
        .map(|line| wrap_visual_rows(line, content_w, None).0.len())
        .sum::<usize>()
        .max(1)
}

#[cfg(test)]
mod path_display_tests {
    use super::shorten_path_with_home;
    use std::path::Path;

    #[test]
    fn shortens_home_and_children_only_on_path_boundaries() {
        let home = Path::new("/home/ryan");

        assert_eq!(shorten_path_with_home(Path::new("/home/ryan"), home), "~");
        assert_eq!(
            shorten_path_with_home(Path::new("/home/ryan/project"), home),
            "~/project"
        );
        assert_eq!(
            shorten_path_with_home(Path::new("/home/ryan2/project"), home),
            "/home/ryan2/project"
        );
    }
}

#[cfg(test)]
mod input_wrap_tests {
    use super::{input_visual_row_count, wrap_visual_rows};

    #[test]
    fn no_wrap_short_line() {
        assert_eq!(
            wrap_visual_rows("hello", 10, Some(5)),
            (vec!["hello".to_string()], Some((0, 5)))
        );
    }

    #[test]
    fn cursor_tracked_into_second_row() {
        assert_eq!(
            wrap_visual_rows("abcdef", 3, Some(4)),
            (vec!["abc".to_string(), "def".to_string()], Some((1, 1)))
        );
    }

    #[test]
    fn cursor_at_wrap_boundary_starts_next_row() {
        assert_eq!(
            wrap_visual_rows("abcdef", 3, Some(3)),
            (vec!["abc".to_string(), "def".to_string()], Some((1, 0)))
        );
    }

    #[test]
    fn cursor_at_end_of_full_row_wraps() {
        assert_eq!(
            wrap_visual_rows("abc", 3, Some(3)),
            (vec!["abc".to_string()], Some((1, 0)))
        );
    }

    #[test]
    fn empty_line_keeps_one_row() {
        assert_eq!(
            wrap_visual_rows("", 5, Some(0)),
            (vec![String::new()], Some((0, 0)))
        );
    }

    #[test]
    fn no_cursor_off_this_line() {
        assert_eq!(
            wrap_visual_rows("abcdef", 3, None),
            (vec!["abc".to_string(), "def".to_string()], None)
        );
    }

    #[test]
    fn wide_chars_use_two_columns() {
        assert_eq!(
            wrap_visual_rows("世界A", 4, None).0,
            vec!["世界".to_string(), "A".to_string()]
        );
    }

    #[test]
    fn input_height_counts_soft_wrapped_rows() {
        assert_eq!(input_visual_row_count(["abcdefgh"].into_iter(), 4), 2);
    }
}

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use super::app::{todo_completion_key, App, Block, SPINNER_FRAMES, TODO_RECENT_COMPLETED_TTL};
use opencli_core::auth::AuthMode;
use opencli_core::tools::{TodoItem, TodoStatus};

pub fn render(f: &mut Frame, app: &mut App) {
    // The same one-row slot shows the turn spinner OR the compaction progress
    // bar — they never run at once (compaction only starts once a turn ends).
    let spinner_h: u16 = if app.busy || app.compacting { 1 } else { 0 };
    let queue_h: u16 = queued_height(app);
    let fleet_h: u16 = fleet_height(app);
    let todos_h: u16 = todos_height(app);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                    // chat            [0]
            Constraint::Length(spinner_h),         // spinner         [1]
            Constraint::Length(queue_h),           // queued messages [2]
            Constraint::Length(fleet_h),           // sub-agent fleet [3]
            Constraint::Length(todos_h),           // todos           [4]
            Constraint::Length(input_height(app)), // input           [5]
            Constraint::Length(1),                 // status line     [6]
        ])
        .split(f.area());

    render_chat(f, layout[0], app);
    // render_chat reconciles auto_scroll (it flips back on once the user scrolls
    // to the tail). Only when the user is parked above the tail do we offer the
    // clickable jump-to-bottom bar; otherwise clear its hit-test rect.
    if !app.auto_scroll && layout[0].height > 0 {
        app.jump_to_bottom_hint = Some(render_jump_to_bottom(f, layout[0]));
    } else {
        app.jump_to_bottom_hint = None;
    }
    if app.busy {
        render_spinner(f, layout[1], app);
    } else if app.compacting {
        render_compact_progress(f, layout[1], app);
    }
    if queue_h > 0 {
        render_queue(f, layout[2], app);
    }
    if fleet_h > 0 {
        render_fleet(f, layout[3], app);
    } else {
        app.subagent_rows.clear();
    }
    if todos_h > 0 {
        render_todos(f, layout[4], app);
    }
    render_input(f, layout[5], app);
    render_status(f, layout[6], app);

    // Buddy companion: the hatch animation takes over the chat area; otherwise
    // the adopted pet tucks into the bottom-right corner.
    if app.hatch.is_some() {
        render_hatch(f, layout[0], app);
    } else if let (Some(pet), false) = (app.buddy_pet, app.buddy_hidden) {
        render_corner_buddy(f, layout[0], pet);
    }

    // Overlay popup — drawn above the input.
    if let Some((_, picker)) = &app.overlay {
        super::picker::render(f, layout[5], picker);
    }
    if app.pending_approval.is_some() {
        render_approval(f, layout[5], app);
    }
}

/// The hatch animation as a centered overlay over the chat area.
fn render_hatch(f: &mut Frame, area: Rect, app: &App) {
    use ratatui::widgets::{Block as RBlock, BorderType, Borders, Clear};
    let Some(h) = &app.hatch else {
        return;
    };
    let elapsed = h.started.elapsed().as_millis() as u64;
    let lines = crate::tui::buddy::hatch_lines(h.pet, elapsed);
    let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (inner_w + 4).min(area.width.max(1));
    let height = (lines.len() as u16 + 2).min(area.height.max(1));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    f.render_widget(Clear, popup);
    let block = RBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(255, 200, 100)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines), inner);
}

/// The adopted companion, small, tucked into the bottom-right of the chat.
fn render_corner_buddy(f: &mut Frame, area: Rect, pet: usize) {
    use ratatui::widgets::Clear;
    let lines = crate::tui::buddy::mini_lines(pet);
    let w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let h = lines.len() as u16;
    if w == 0 || h == 0 || area.width < w + 2 || area.height < h + 1 {
        return;
    }
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w + 1),
        y: area.y + area.height.saturating_sub(h),
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines), rect);
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

/// Rows the sub-agent fleet view needs this frame: a header, one row per
/// sub-agent, and one detail row for each expanded one. Capped so it can't
/// crowd out the chat. Zero when no sub-agents are running.
fn fleet_height(app: &App) -> u16 {
    if app.subagents.is_empty() {
        return 0;
    }
    let mut h = 1usize; // header
    for s in &app.subagents {
        h += 1;
        if s.expanded {
            h += 1;
        }
    }
    h.min(12) as u16
}

const TODO_VISIBLE_ROWS: usize = 6;

fn todos_height(app: &App) -> u16 {
    if !app.show_todos {
        return 0;
    }
    todos_height_for_count(app.session_todos.len())
}

fn todos_height_for_count(count: usize) -> u16 {
    if count == 0 {
        return 0;
    }
    let visible = count.min(TODO_VISIBLE_ROWS);
    let overflow = usize::from(count > visible);
    (1 + visible + overflow) as u16
}

/// Truncate `s` to `max` display-ish chars with an ellipsis (char-based, so it
/// never splits a UTF-8 codepoint).
fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() > max {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    } else {
        s.to_string()
    }
}

/// Live fleet view: a list of the sub-agents `dispatch_agent` spawned this turn,
/// each with a status dot, type, prompt summary, current activity and elapsed
/// time — Claude Code's sub-agent list. Records each row's screen rect into
/// `app.subagent_rows` so a left-click can toggle the row's detail.
fn render_fleet(f: &mut Frame, area: Rect, app: &mut App) {
    let header_dim = Style::default().fg(Color::Rgb(150, 155, 165));
    let kind_style = Style::default()
        .fg(Color::Rgb(230, 230, 235))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::Rgb(140, 140, 150));
    let accent = Style::default().fg(Color::Rgb(140, 170, 255));

    let total = app.subagents.len();
    let running = app.subagents.iter().filter(|s| s.done.is_none()).count();
    let prompt_max = ((area.width as usize) / 3).clamp(12, 50);

    let mut rows: Vec<(Rect, String)> = Vec::new();
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" ⛓ ", accent),
        Span::styled(
            format!("Sub-agents · {running} running / {total}"),
            header_dim.add_modifier(Modifier::BOLD),
        ),
        Span::styled("   click a row to expand", header_dim),
    ]));

    for s in &app.subagents {
        // The row's absolute y is the panel origin plus lines already emitted.
        let y = area.y.saturating_add(lines.len() as u16);
        rows.push((
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            s.id.clone(),
        ));

        let dot = match s.done {
            Some(true) => Span::styled("✓ ", Style::default().fg(Color::Rgb(120, 200, 120))),
            Some(false) => Span::styled("✗ ", Style::default().fg(Color::Rgb(225, 110, 110))),
            None => {
                let frame = SPINNER_FRAMES
                    [(s.started_at.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len()];
                Span::styled(
                    format!("{frame} "),
                    Style::default().fg(Color::Rgb(120, 200, 255)),
                )
            }
        };
        lines.push(Line::from(vec![
            Span::raw(" "),
            dot,
            Span::styled(format!("{}  ", s.kind), kind_style),
            Span::styled(truncate_chars(&s.prompt, prompt_max), dim),
            Span::styled(
                format!(
                    "  · {} · {} steps · {}",
                    s.activity,
                    s.steps,
                    format_elapsed(s.started_at.elapsed())
                ),
                dim,
            ),
        ]));
        if s.expanded {
            lines.push(Line::from(vec![
                Span::raw("     ↳ "),
                Span::styled(
                    s.prompt.clone(),
                    Style::default().fg(Color::Rgb(175, 175, 185)),
                ),
            ]));
        }
    }

    app.subagent_rows = rows;
    f.render_widget(Paragraph::new(lines), area);
}

fn render_todos(f: &mut Frame, area: Rect, app: &App) {
    let done = app
        .session_todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .count();
    let total = app.session_todos.len();
    let in_progress = app
        .session_todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::InProgress))
        .count();
    let pending = app
        .session_todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .count();

    let header = Style::default()
        .fg(Color::Rgb(165, 170, 180))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::Rgb(135, 140, 150));

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(total.to_string(), header),
        Span::styled(" tasks (", dim),
        Span::styled(done.to_string(), header),
        Span::styled(" done, ", dim),
        if in_progress > 0 {
            Span::styled(in_progress.to_string(), header)
        } else {
            Span::styled(in_progress.to_string(), dim)
        },
        Span::styled(" in progress, ", dim),
        Span::styled(pending.to_string(), header),
        Span::styled(" open)", dim),
    ]));

    let label_width = (area.width as usize).saturating_sub(4);
    let recent_completed = recent_completed_todo_indices(app);
    let visible = visible_todo_indices(&app.session_todos, &recent_completed);
    let completed_ids: HashSet<&str> = app
        .session_todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .filter_map(|t| t.id.as_deref())
        .collect();
    for idx in &visible {
        let todo = &app.session_todos[*idx];
        let blocked = matches!(todo.status, TodoStatus::Pending)
            && !todo.blocked_by.is_empty()
            && !todo
                .blocked_by
                .iter()
                .all(|b| completed_ids.contains(b.as_str()));
        lines.push(render_todo_line(todo, label_width, blocked));
    }
    if let Some(summary) = hidden_todos_summary(&app.session_todos, &visible) {
        lines.push(Line::from(Span::styled(format!("  {summary}"), dim)));
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn render_todo_line(todo: &TodoItem, label_width: usize, blocked: bool) -> Line<'static> {
    let active_style = Style::default()
        .fg(Color::Rgb(255, 184, 108))
        .add_modifier(Modifier::BOLD);
    let pending_style = Style::default().fg(Color::Rgb(205, 205, 210));
    let done_style = Style::default()
        .fg(Color::Rgb(125, 130, 140))
        .add_modifier(Modifier::CROSSED_OUT);
    let done_mark = Style::default().fg(Color::Rgb(120, 200, 120));
    let pending_mark = Style::default().fg(Color::Rgb(145, 150, 160));
    // A pending item still waiting on an unfinished dependency: dimmer body and
    // a distinct marker so it reads as "not yet startable".
    let blocked_style = Style::default().fg(Color::Rgb(120, 124, 134));

    let (icon, mark_style, body_style) = match todo.status {
        TodoStatus::Completed => ("✓", done_mark, done_style),
        TodoStatus::InProgress => ("▪", active_style, active_style),
        TodoStatus::Pending if blocked => ("⊘", blocked_style, blocked_style),
        TodoStatus::Pending => ("□", pending_mark, pending_style),
    };
    let label = todo_label(todo);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{icon} "), mark_style),
        Span::styled(truncate_chars(label, label_width), body_style),
    ])
}

fn render_spinner(f: &mut Frame, area: Rect, app: &App) {
    let elapsed = app.turn_started_at.map(|t| t.elapsed()).unwrap_or_default();
    // Drive the spinner from wall-clock elapsed, not a counter ticked by the
    // 80ms select arm: under a heavy event stream (e.g. a large Update whose
    // arguments arrive as many deltas) the `biased` select starves that arm and
    // the counter — and thus the glyph — freezes. Elapsed-based advancement
    // animates smoothly as long as draws happen (they do, every ~16ms).
    let frame = SPINNER_FRAMES[(elapsed.as_millis() / 80) as usize % SPINNER_FRAMES.len()];
    let mut extras = String::new();
    if app.tokens_used > 0 {
        // Current context window usage, not cumulative throughput. Mirrors
        // Claude Code's "X% context" readout.
        let limit = app.config.effective_context_limit();
        let pct = app.tokens_used.saturating_mul(100) / limit.max(1);
        extras.push_str(&format!(
            " · {} ctx ({pct}%)",
            format_tokens(app.tokens_used)
        ));
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

fn todo_label(todo: &TodoItem) -> &str {
    match todo.status {
        TodoStatus::InProgress => &todo.active_form,
        TodoStatus::Pending | TodoStatus::Completed => &todo.content,
    }
}

fn recent_completed_todo_indices(app: &App) -> HashSet<usize> {
    let now = std::time::Instant::now();
    app.session_todos
        .iter()
        .enumerate()
        .filter_map(|(idx, todo)| {
            if !matches!(todo.status, TodoStatus::Completed) {
                return None;
            }
            let completed_at = app.todo_completed_at.get(&todo_completion_key(todo))?;
            if now.duration_since(*completed_at) <= TODO_RECENT_COMPLETED_TTL {
                Some(idx)
            } else {
                None
            }
        })
        .collect()
}

fn visible_todo_indices(todos: &[TodoItem], recent_completed: &HashSet<usize>) -> Vec<usize> {
    if todos.len() <= TODO_VISIBLE_ROWS {
        return (0..todos.len()).collect();
    }

    let mut indices = Vec::with_capacity(TODO_VISIBLE_ROWS);
    let mut recent_completed = recent_completed.iter().copied().collect::<Vec<_>>();
    recent_completed.sort_unstable();
    for idx in recent_completed {
        if matches!(
            todos.get(idx).map(|todo| todo.status),
            Some(TodoStatus::Completed)
        ) {
            indices.push(idx);
            if indices.len() == TODO_VISIBLE_ROWS {
                return indices;
            }
        }
    }

    for status in [
        TodoStatus::InProgress,
        TodoStatus::Pending,
        TodoStatus::Completed,
    ] {
        for (idx, todo) in todos.iter().enumerate() {
            if todo.status == status && !indices.contains(&idx) {
                indices.push(idx);
                if indices.len() == TODO_VISIBLE_ROWS {
                    return indices;
                }
            }
        }
    }
    indices
}

fn hidden_todos_summary(todos: &[TodoItem], visible: &[usize]) -> Option<String> {
    if todos.len() <= visible.len() {
        return None;
    }
    let visible: std::collections::HashSet<usize> = visible.iter().copied().collect();
    let mut hidden_in_progress = 0usize;
    let mut hidden_pending = 0usize;
    let mut hidden_completed = 0usize;
    for (idx, todo) in todos.iter().enumerate() {
        if visible.contains(&idx) {
            continue;
        }
        match todo.status {
            TodoStatus::InProgress => hidden_in_progress += 1,
            TodoStatus::Pending => hidden_pending += 1,
            TodoStatus::Completed => hidden_completed += 1,
        }
    }

    let mut parts = Vec::new();
    if hidden_in_progress > 0 {
        parts.push(format!("{hidden_in_progress} in progress"));
    }
    if hidden_pending > 0 {
        parts.push(format!("{hidden_pending} pending"));
    }
    if hidden_completed > 0 {
        parts.push(format!("{hidden_completed} completed"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("… +{}", parts.join(", ")))
    }
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

/// Draw the "Jump to bottom" bar on the last row of the chat area and return
/// its screen rect so the mouse handler can hit-test a click. The label is
/// centered and flanked by a horizontal rule, matching Claude Code's affordance.
fn render_jump_to_bottom(f: &mut Frame, chat_area: Rect) -> Rect {
    let row = Rect {
        x: chat_area.x,
        y: chat_area.y + chat_area.height - 1,
        width: chat_area.width,
        height: 1,
    };
    let label = " Jump to bottom (ctrl+End) ↓ ";
    let total = row.width as usize;
    let label_w = unicode_width::UnicodeWidthStr::width(label);
    let rule = Style::default().fg(Color::Rgb(80, 85, 95));
    let label_style = Style::default()
        .fg(Color::Rgb(210, 210, 220))
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
                // User turns render as a full-width gray block (like Claude
                // Code): every wrapped line is padded with spaces carrying the
                // background so the fill reaches the right edge.
                let user_bg = Color::Rgb(38, 40, 44);
                let chevron_style = Style::default()
                    .fg(Color::Rgb(130, 170, 255))
                    .bg(user_bg)
                    .add_modifier(Modifier::BOLD);
                let body_style = Style::default().fg(Color::Rgb(225, 225, 230)).bg(user_bg);
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
        // Mark the assistant's turn with a bullet on its first line (like the
        // tool bullet, so prose and tool calls read as one consistent column),
        // then indent continuation lines to align under it.
        let marker_style = Style::default()
            .fg(Color::Rgb(140, 170, 255))
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
    let left_text = status_left_text(app);
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

fn status_left_text(app: &App) -> String {
    let mode_label = app.permission_mode().label();
    let goal_elapsed = app.active_goal.as_ref().map(|goal| goal.elapsed_label());
    let mut text = status_left_text_for_parts(
        mode_label,
        &app.status_line,
        app.expanded_tools,
        goal_elapsed.as_deref(),
    );
    if !app.session_todos.is_empty() {
        if app.show_todos {
            text.push_str(" · Ctrl+T hide tasks");
        } else {
            text.push_str(" · Ctrl+T show tasks");
        }
    }
    text
}

fn status_left_text_for_parts(
    mode_label: &str,
    status_line: &str,
    expanded_tools: bool,
    goal_elapsed: Option<&str>,
) -> String {
    let activity = if !status_line.is_empty() {
        status_line.to_string()
    } else if expanded_tools {
        "⇲ tool view: expanded · Ctrl+O to collapse".to_string()
    } else {
        "shift+tab cycles mode · ? for shortcuts".to_string()
    };
    if let Some(elapsed) = goal_elapsed {
        format!("{mode_label}  ·  goal {elapsed}  ·  {activity}")
    } else {
        format!("{mode_label}  ·  {activity}")
    }
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
    // Option 1 persists a per-project allow-rule (same logic as Claude Code's
    // "don't ask again", but opencli's own wording): the label names exactly
    // what gets allowed in this project.
    let allow_label = {
        let args_val: serde_json::Value =
            serde_json::from_str(&p.args_json).unwrap_or(serde_json::Value::Null);
        format!(
            "Allow {} in this project",
            opencli_core::permissions::rule_label(&p.tool_name, &args_val)
        )
    };
    let opts = ["Allow once".to_string(), allow_label, "Deny".to_string()];
    let sel_style = Style::default()
        .fg(Color::Rgb(255, 255, 255))
        .bg(Color::Rgb(60, 50, 20))
        .add_modifier(Modifier::BOLD);
    let opt_style = Style::default().fg(Color::Rgb(220, 220, 220));
    for (i, label) in opts.iter().enumerate() {
        let is_sel = i == p.selected;
        let marker = if is_sel { "  ❯ " } else { "    " };
        let style = if is_sel { sel_style } else { opt_style };
        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(label.clone(), style),
        ]));
    }
    lines.push(Line::from(Span::styled(
        "  ↑/↓ select · enter confirm · y/n/esc",
        dim,
    )));

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
    // Clear the FULL row span the popup occupies, not just the narrow box.
    // The box is only `width` cols wide, but it floats over chat rows whose
    // long lines extend past it — without clearing to the right edge, the tail
    // of those lines bleeds out beside the modal border.
    let clear_area = ratatui::layout::Rect {
        x: anchor_area.x,
        y,
        width: anchor_area.width,
        height,
    };
    f.render_widget(Clear, clear_area);
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

/// Lazily-loaded syntect assets (syntax definitions + a dark theme). Loading
/// the default sets parses an embedded binary dump, so do it once per process.
fn syntax_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme) {
    use std::sync::OnceLock;
    static ASSETS: OnceLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        OnceLock::new();
    ASSETS.get_or_init(|| {
        let ss = syntect::parsing::SyntaxSet::load_defaults_newlines();
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let theme = ts.themes["base16-ocean.dark"].clone();
        (ss, theme)
    })
}

/// Background fill behind a fenced code block, so it reads as one solid panel.
const CODE_BG: Color = Color::Rgb(30, 31, 38);

/// Render the assistant's markdown text into content rows (each a `Vec<Span>`,
/// without the leading bullet/indent gutter). Handles fenced code blocks
/// (syntax-highlighted) and GFM tables (box-drawn) as whole blocks; all other
/// lines are word-wrapped and passed through the inline markdown styler.
fn render_assistant_md(text: &str, content_width: usize) -> Vec<Vec<Span<'static>>> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        // Fenced code block: ``` or ~~~ with an optional language token.
        let fence = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(marker) = fence {
            let info = trimmed[marker.len()..].trim();
            let lang = info.split_whitespace().next().filter(|s| !s.is_empty());
            let mut code_lines: Vec<&str> = Vec::new();
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                if lines[j].trim_start().starts_with(marker) {
                    closed = true;
                    break;
                }
                code_lines.push(lines[j]);
                j += 1;
            }
            let code = code_lines.join("\n");
            out.extend(highlight_code_lines(&code, lang, content_width));
            i = if closed { j + 1 } else { j };
            continue;
        }
        // GFM table: a row containing `|` immediately followed by a separator
        // row (`|---|:--:|` …).
        if line.contains('|') && i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
            let mut tbl: Vec<&str> = vec![lines[i], lines[i + 1]];
            let mut j = i + 2;
            while j < lines.len() && lines[j].contains('|') && !lines[j].trim().is_empty() {
                tbl.push(lines[j]);
                j += 1;
            }
            out.extend(render_md_table(&tbl, content_width));
            i = j;
            continue;
        }
        // Plain prose line.
        for w in wrap(line, content_width) {
            out.push(render_markdown_inline(&w));
        }
        i += 1;
    }
    if out.is_empty() {
        out.push(vec![Span::raw("")]);
    }
    out
}

/// Resolve a fenced-code language token to a syntect syntax. syntect matches by
/// file extension or exact name, so common fence labels (`rust`, `python`,
/// `bash`, …) miss unless mapped to the extension first; we then fall back to
/// the raw/lower-cased token and finally plain text.
fn resolve_syntax<'a>(
    ss: &'a syntect::parsing::SyntaxSet,
    lang: &str,
) -> &'a syntect::parsing::SyntaxReference {
    let lower = lang.to_ascii_lowercase();
    let token = match lower.as_str() {
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" | "node" | "mjs" => "js",
        "typescript" | "ts" => "ts",
        "jsx" | "tsx" => "tsx",
        "bash" | "sh" | "shell" | "zsh" | "console" => "sh",
        "yaml" | "yml" => "yaml",
        "markdown" | "md" => "md",
        "go" | "golang" => "go",
        "c++" | "cpp" | "cxx" | "cc" => "cpp",
        "c#" | "csharp" | "cs" => "cs",
        "ruby" | "rb" => "rb",
        "kotlin" | "kt" => "kt",
        "rust-script" => "rs",
        other => other,
    };
    ss.find_syntax_by_token(token)
        .or_else(|| ss.find_syntax_by_token(&lower))
        .or_else(|| ss.find_syntax_by_extension(&lower))
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// Syntax-highlight `code` and return content rows padded to `content_width`
/// with the code background. `lang` is the fence's language token, if any.
fn highlight_code_lines(
    code: &str,
    lang: Option<&str>,
    content_width: usize,
) -> Vec<Vec<Span<'static>>> {
    use syntect::easy::HighlightLines;
    use syntect::util::LinesWithEndings;
    let (ss, theme) = syntax_assets();
    let syntax = lang
        .map(|l| resolve_syntax(ss, l))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, theme);

    // Sanitize once up front (preserving newlines) so embedded tabs/escapes in a
    // model-echoed code block can't desync the terminal; syntect then highlights
    // the cleaned text.
    let code = sanitize_display(code);
    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    for line in LinesWithEndings::from(code.as_ref()) {
        let ranges = hl.highlight_line(line, ss).unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (style, piece) in ranges {
            let piece = piece.trim_end_matches(['\n', '\r']);
            if piece.is_empty() {
                continue;
            }
            let c = style.foreground;
            spans.push(Span::styled(
                piece.to_string(),
                Style::default().fg(Color::Rgb(c.r, c.g, c.b)).bg(CODE_BG),
            ));
        }
        out.extend(wrap_spans(spans, content_width, CODE_BG));
    }
    if out.is_empty() {
        out.push(wrap_spans(Vec::new(), content_width, CODE_BG).remove(0));
    }
    out
}

/// Hard-wrap a styled span run to `width` display columns, padding every row to
/// the full width with `bg` so a code block renders as a flush rectangle.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize, bg: Color) -> Vec<Vec<Span<'static>>> {
    use unicode_width::UnicodeWidthChar;
    let pad = |row: &mut Vec<Span<'static>>, used: usize| {
        if width > used {
            row.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
    };
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for span in spans {
        let style = span.style;
        let mut buf = String::new();
        for ch in span.content.chars() {
            let w = ch.width().unwrap_or(0);
            if width > 0 && cur_w + w > width {
                if !buf.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut buf), style));
                }
                pad(&mut cur, cur_w);
                rows.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            buf.push(ch);
            cur_w += w;
        }
        if !buf.is_empty() {
            cur.push(Span::styled(buf, style));
        }
    }
    pad(&mut cur, cur_w);
    rows.push(cur);
    rows
}

/// True when `line` is a GFM table separator row, e.g. `|---|:--:|---|`.
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.contains('-') {
        return false;
    }
    t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Split one table row into trimmed cell strings, dropping the empty cells that
/// flank rows written with outer pipes (`| a | b |`).
fn split_table_row(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = line.split('|').map(|c| c.trim().to_string()).collect();
    if cells.first().is_some_and(|c| c.is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|c| c.is_empty()) {
        cells.pop();
    }
    cells
}

/// Display width of a cell ignoring the inline markdown markers that won't be
/// drawn (backticks, `*`), so column sizing matches the rendered text.
fn md_cell_width(s: &str) -> usize {
    let stripped: String = s.chars().filter(|c| !matches!(c, '`' | '*')).collect();
    unicode_width::UnicodeWidthStr::width(stripped.as_str())
}

/// Render a GFM table (header row, separator, body rows) into box-drawn content
/// rows. Columns are sized to content and shrunk to fit `content_width`; cells
/// that still overflow are word-wrapped.
fn render_md_table(tbl: &[&str], content_width: usize) -> Vec<Vec<Span<'static>>> {
    let border = Style::default().fg(Color::Rgb(90, 95, 105));
    let header_style = Style::default()
        .fg(Color::Rgb(235, 235, 240))
        .add_modifier(Modifier::BOLD);

    // tbl[0] = header, tbl[1] = separator, tbl[2..] = body.
    let header = split_table_row(tbl[0]);
    let body: Vec<Vec<String>> = tbl[2..].iter().map(|r| split_table_row(r)).collect();
    let ncols = header
        .len()
        .max(body.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);

    // Natural column widths from content.
    let mut widths = vec![0usize; ncols];
    let measure = |row: &[String], widths: &mut [usize]| {
        for (c, cell) in row.iter().enumerate() {
            if c < ncols {
                widths[c] = widths[c].max(md_cell_width(cell));
            }
        }
    };
    measure(&header, &mut widths);
    for r in &body {
        measure(r, &mut widths);
    }
    for w in widths.iter_mut() {
        *w = (*w).max(1);
    }

    // Shrink to fit: each column costs `width + 2` (1 space padding per side)
    // plus one border, with a final closing border.
    let budget = content_width.saturating_sub(ncols + 1);
    while widths.iter().map(|w| w + 2).sum::<usize>() > budget {
        // Trim the widest column until the table fits (or all are minimal).
        let Some((idx, max_w)) = widths.iter().copied().enumerate().max_by_key(|(_, w)| *w) else {
            break;
        };
        if max_w <= 3 {
            break;
        }
        widths[idx] = max_w - 1;
    }

    let v = Span::styled("│", border);
    let pad_cell = |cell: &str, w: usize, style: Option<Style>| -> Vec<Vec<Span<'static>>> {
        // Wrap the cell's plain text to the column width, then inline-style each
        // wrapped sub-line and pad it out to the column width.
        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let wrapped = wrap(cell, w.max(1));
        for sub in wrapped {
            let mut spans = render_markdown_inline(&sub);
            if let Some(st) = style {
                for s in spans.iter_mut() {
                    s.style = s.style.patch(st);
                }
            }
            let used: usize = spans
                .iter()
                .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            if w > used {
                spans.push(Span::raw(" ".repeat(w - used)));
            }
            rows.push(spans);
        }
        if rows.is_empty() {
            rows.push(vec![Span::raw(" ".repeat(w))]);
        }
        rows
    };

    let rule = |left: &str, mid: &str, right: &str| -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(left.to_string(), border)];
        for (c, w) in widths.iter().enumerate() {
            spans.push(Span::styled("─".repeat(w + 2), border));
            spans.push(Span::styled(
                if c + 1 == ncols { right } else { mid }.to_string(),
                border,
            ));
        }
        spans
    };

    let render_row = |cells: &[String], style: Option<Style>| -> Vec<Vec<Span<'static>>> {
        // Each column produces 1+ wrapped lines; the row's height is the max.
        let col_rows: Vec<Vec<Vec<Span<'static>>>> = (0..ncols)
            .map(|c| {
                let cell = cells.get(c).map(|s| s.as_str()).unwrap_or("");
                pad_cell(cell, widths[c], style)
            })
            .collect();
        let height = col_rows.iter().map(|r| r.len()).max().unwrap_or(1);
        let mut out_rows: Vec<Vec<Span<'static>>> = Vec::new();
        for line_idx in 0..height {
            let mut spans: Vec<Span<'static>> = vec![v.clone()];
            for c in 0..ncols {
                spans.push(Span::raw(" "));
                if let Some(cell_line) = col_rows[c].get(line_idx) {
                    spans.extend(cell_line.iter().cloned());
                } else {
                    spans.push(Span::raw(" ".repeat(widths[c])));
                }
                spans.push(Span::raw(" "));
                spans.push(v.clone());
            }
            out_rows.push(spans);
        }
        out_rows
    };

    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    out.push(rule("┌", "┬", "┐"));
    out.extend(render_row(&header, Some(header_style)));
    out.push(rule("├", "┼", "┤"));
    for r in &body {
        out.extend(render_row(r, None));
    }
    out.push(rule("└", "┴", "┘"));
    out
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
        "goal_update" => ("Goal Update".into(), s("status")),
        "enter_plan_mode" => ("Enter Plan".into(), String::new()),
        "exit_plan_mode" => ("Plan Ready".into(), String::new()),
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
        "multi_edit" => {
            // Render a per-edit diff like edit_file so an Update from multi_edit
            // shows what changed (previously it fell through to the raw "Applied
            // N edits" text — the "sometimes the diff shows, sometimes not"
            // inconsistency between edit_file and multi_edit).
            let edits = args.get("edits").and_then(|v| v.as_array());
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
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

            let (mut total_added, mut total_removed) = (0usize, 0usize);
            if let Some(edits) = edits {
                for e in edits {
                    let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new_ = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    total_added += if new_.is_empty() {
                        0
                    } else {
                        new_.lines().count()
                    };
                    total_removed += if old.is_empty() {
                        0
                    } else {
                        old.lines().count()
                    };
                }
            }
            let n_edits = edits.map(|e| e.len()).unwrap_or(0);
            out.push(Line::from(Span::styled(
                format!(
                    "{n_edits} edit{}: added {total_added} line{}, removed {total_removed} line{}",
                    plural(n_edits),
                    plural(total_added),
                    plural(total_removed)
                ),
                style_summary,
            )));

            let max_diff = limits.edit_diff;
            let mut shown = 0usize;
            if let Some(edits) = edits {
                'edits: for e in edits {
                    let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new_ = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let start_line = locate_line_number(path, old).unwrap_or(1);
                    for (i, line) in old.lines().enumerate() {
                        if shown >= max_diff {
                            break 'edits;
                        }
                        out.push(diff_line(
                            start_line + i,
                            "-",
                            line,
                            lineno_removed,
                            removed_bg,
                            avail,
                        ));
                        shown += 1;
                    }
                    for (i, line) in new_.lines().enumerate() {
                        if shown >= max_diff {
                            break 'edits;
                        }
                        out.push(diff_line(
                            start_line + i,
                            "+",
                            line,
                            lineno_added,
                            added_bg,
                            avail,
                        ));
                        shown += 1;
                    }
                }
            }
            let total_lines = total_added + total_removed;
            if total_lines > max_diff {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines", total_lines - max_diff),
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
            // A failed command's output is the whole point, so when collapsed we
            // show a bigger slice than the 3-line success preview — enough to
            // read the error inline, still bounded with a "Ctrl+O for more" hint.
            const FAILED_SHELL_PREVIEW: usize = 15;
            let (stdout_budget, stderr_budget) = if !success && !expanded {
                (FAILED_SHELL_PREVIEW, FAILED_SHELL_PREVIEW)
            } else {
                (limits.shell_stdout, limits.shell_stderr)
            };
            if !stdout.is_empty() {
                let total = stdout.lines().count();
                for raw in stdout.lines().take(stdout_budget) {
                    for w in wrap(raw, avail) {
                        out.push(Line::from(Span::styled(w, style_code)));
                    }
                }
                let extra = total.saturating_sub(stdout_budget);
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
                    // Claude Code style: stderr rendered in red, with no
                    // separator box — the colour alone sets it apart from stdout.
                    let err_style = Style::default().fg(Color::Red);
                    let total_err = stderr.lines().count();
                    for raw in stderr.lines().take(stderr_budget) {
                        for w in wrap(raw, avail) {
                            out.push(Line::from(Span::styled(w, err_style)));
                        }
                    }
                    if total_err > stderr_budget {
                        out.push(Line::from(Span::styled(
                            format!(
                                "… +{} stderr line{} (Ctrl+O for more)",
                                total_err - stderr_budget,
                                plural(total_err - stderr_budget)
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
            // Claude Code shows no "exit 0" on success; only a compact red
            // footer when the command failed.
            if !success {
                out.push(Line::from(Span::styled(
                    format!("Error (exit {code})"),
                    Style::default().fg(Color::Red),
                )));
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
                    .or_else(|| todo.get("activeForm").and_then(|v| v.as_str()))
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
        "ask_user_question" | "enter_plan_mode" | "exit_plan_mode" => {
            // No inline body: the System block rendered right below this tool
            // result already shows the question or plan approval prompt.
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
    let text = sanitize_display(text);
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
    display_path(path)
}

fn shorten_path_with_home(path: &Path, home: &Path) -> String {
    let Ok(rest) = path.strip_prefix(home) else {
        return display_path(path);
    };
    if rest.as_os_str().is_empty() {
        "~".to_string()
    } else {
        format!("~/{}", display_path(rest))
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let text = sanitize_display(text);
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

/// Strip terminal control sequences and other non-printable bytes that would
/// corrupt the display. Tool output (notably colorized `cargo`/`rustc`) embeds
/// ANSI escape sequences; rendered verbatim, those bytes reach the terminal,
/// move the real cursor, and desync ratatui's incremental buffer diff — leaving
/// persistent on-screen garbage that piles up over a long session (the
/// `\x1b(B\x1b[m` resets show up as stray `(B` / `m` fragments). Tabs and
/// carriage returns break layout the same way, so expand tabs to spaces and drop
/// CR / other C0 / DEL controls. Newlines are preserved so multi-line callers
/// can still split on them.
fn sanitize_display(s: &str) -> Cow<'_, str> {
    // Fast path: ESC, tab, CR, and other C0/DEL bytes are exactly the ones
    // `< 0x20` (minus newline) or `0x7f`. Clean text borrows with no allocation.
    if !s.bytes().any(|b| (b < 0x20 && b != b'\n') || b == 0x7f) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize; // visible column since line start, for tab-stop expansion
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    // CSI: consume params/intermediates up to the final byte.
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&p) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: consume up to BEL or the ST terminator (ESC \).
                    chars.next();
                    while let Some(p) = chars.next() {
                        if p == '\u{07}' {
                            break;
                        }
                        if p == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    // Shorter forms like `ESC ( B`: optional intermediate bytes
                    // (0x20..=0x2f) then a single final byte.
                    while let Some(&p) = chars.peek() {
                        if ('\u{20}'..='\u{2f}').contains(&p) {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    chars.next();
                }
                None => {}
            },
            '\t' => {
                let n = 4 - (col % 4);
                for _ in 0..n {
                    out.push(' ');
                }
                col += n;
            }
            '\n' => {
                out.push('\n');
                col = 0;
            }
            c if (c as u32) < 0x20 || c == '\u{7f}' => {}
            c => {
                out.push(c);
                col += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            }
        }
    }
    Cow::Owned(out)
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
mod todo_panel_tests {
    use super::{
        hidden_todos_summary, todo_label, todos_height_for_count, truncate_chars,
        visible_todo_indices, TODO_VISIBLE_ROWS,
    };
    use opencli_core::tools::{TodoItem, TodoStatus};
    use std::collections::HashSet;

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
            active_form: format!("Doing {content}"),
            id: None,
            blocked_by: Vec::new(),
        }
    }

    #[test]
    fn todo_panel_height_caps_and_reserves_overflow_row() {
        assert_eq!(todos_height_for_count(0), 0);
        assert_eq!(todos_height_for_count(1), 2);
        assert_eq!(todos_height_for_count(TODO_VISIBLE_ROWS), 7);
        assert_eq!(todos_height_for_count(TODO_VISIBLE_ROWS + 2), 8);
    }

    #[test]
    fn truncated_todos_prioritize_active_and_pending_items() {
        let todos = vec![
            item("completed one", TodoStatus::Completed),
            item("pending one", TodoStatus::Pending),
            item("completed two", TodoStatus::Completed),
            item("active one", TodoStatus::InProgress),
            item("pending two", TodoStatus::Pending),
            item("completed three", TodoStatus::Completed),
            item("pending three", TodoStatus::Pending),
            item("completed four", TodoStatus::Completed),
        ];

        let visible = visible_todo_indices(&todos, &HashSet::new());

        assert_eq!(visible, vec![3, 1, 4, 6, 0, 2]);
        assert_eq!(
            hidden_todos_summary(&todos, &visible),
            Some("… +2 completed".to_string())
        );
    }

    #[test]
    fn truncated_todos_prioritize_recently_completed_items() {
        let todos = vec![
            item("pending one", TodoStatus::Pending),
            item("pending two", TodoStatus::Pending),
            item("active one", TodoStatus::InProgress),
            item("pending three", TodoStatus::Pending),
            item("pending four", TodoStatus::Pending),
            item("completed old", TodoStatus::Completed),
            item("completed recent", TodoStatus::Completed),
            item("pending five", TodoStatus::Pending),
        ];
        let recent_completed = HashSet::from([6usize]);

        let visible = visible_todo_indices(&todos, &recent_completed);

        assert_eq!(visible, vec![6, 2, 0, 1, 3, 4]);
        assert_eq!(
            hidden_todos_summary(&todos, &visible),
            Some("… +1 pending, 1 completed".to_string())
        );
    }

    #[test]
    fn truncated_recent_completed_todos_are_deterministic() {
        let todos = (0..TODO_VISIBLE_ROWS + 2)
            .map(|i| item(&format!("completed {i}"), TodoStatus::Completed))
            .collect::<Vec<_>>();
        let recent_completed = HashSet::from([5usize, 2usize, 4usize, 1usize, 3usize, 0usize]);

        let visible = visible_todo_indices(&todos, &recent_completed);

        assert_eq!(visible, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn todo_label_uses_active_form_only_for_active_item() {
        let active = item("write tests", TodoStatus::InProgress);
        let done = item("read code", TodoStatus::Completed);

        assert_eq!(todo_label(&active), "Doing write tests");
        assert_eq!(todo_label(&done), "read code");
    }

    #[test]
    fn truncation_handles_narrow_width_without_splitting_utf8() {
        assert_eq!(truncate_chars("abcdef", 0), "");
        assert_eq!(truncate_chars("éclair", 2), "é…");
    }
}

#[cfg(test)]
mod todo_tool_render_tests {
    use super::friendly_body;
    use serde_json::json;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn todo_write_body_accepts_claude_code_active_form_spelling() {
        let lines = friendly_body(
            "todo_write",
            &json!({
                "todos": [
                    {
                        "content": "Run tests",
                        "activeForm": "Running tests",
                        "status": "in_progress"
                    }
                ]
            }),
            Some("stored"),
            false,
            80,
            false,
        );

        assert!(text(&lines).contains("Running tests"));
    }
}

#[cfg(test)]
mod shell_tool_render_tests {
    use super::friendly_body;
    use serde_json::json;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn shell_output(code: i32, stdout: &str, stderr: &str) -> String {
        format!("exit_code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")
    }

    #[test]
    fn failed_command_shows_red_stderr_and_error_footer_no_box() {
        let out = shell_output(101, "", "error: no such command: audit");
        // A non-zero exit is NOT a tool error — run_shell returns Ok with the
        // exit code embedded, so `error` is false and the run_shell formatter runs.
        let lines = friendly_body(
            "run_shell",
            &json!({"command": "cargo audit"}),
            Some(&out),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("error: no such command: audit"),
            "got: {rendered}"
        );
        assert!(rendered.contains("Error (exit 101)"), "got: {rendered}");
        // Claude Code style: no yellow "─ stderr ─" separator box.
        assert!(!rendered.contains("─ stderr ─"), "got: {rendered}");
    }

    #[test]
    fn successful_command_has_no_exit_footer() {
        let out = shell_output(0, "all good", "");
        let lines = friendly_body(
            "run_shell",
            &json!({"command": "echo hi"}),
            Some(&out),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(rendered.contains("all good"), "got: {rendered}");
        assert!(
            !rendered.contains("exit"),
            "success must not show an exit line: {rendered}"
        );
        assert!(!rendered.contains("Error"), "got: {rendered}");
    }

    #[test]
    fn failed_command_shows_more_than_the_success_preview() {
        // 20 stdout lines: the collapsed failure budget (15) shows far more than
        // the 3-line success preview, still bounded with a "more" hint.
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let out = shell_output(1, body.trim_end(), "");
        let lines = friendly_body(
            "run_shell",
            &json!({"command": "cargo fmt --check"}),
            Some(&out),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("line 15"),
            "should show ~15 lines on failure: {rendered}"
        );
        assert!(
            !rendered.contains("line 16"),
            "should cap at the failure budget: {rendered}"
        );
        assert!(rendered.contains("+5 more line"), "got: {rendered}");
    }
}

#[cfg(test)]
mod status_footer_tests {
    use super::status_left_text_for_parts;

    #[test]
    fn includes_goal_elapsed_when_goal_is_active() {
        assert_eq!(
            status_left_text_for_parts("default", "", false, Some("1m32")),
            "default  ·  goal 1m32  ·  shift+tab cycles mode · ? for shortcuts"
        );
    }

    #[test]
    fn keeps_status_activity_after_goal_elapsed() {
        assert_eq!(
            status_left_text_for_parts("plan", "(continuing active goal...)", false, Some("12s")),
            "plan  ·  goal 12s  ·  (continuing active goal...)"
        );
    }
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
mod sanitize_tests {
    use super::sanitize_display;

    #[test]
    fn strips_ansi_color_and_reset_sequences() {
        // Colorized cargo/rustc output: SGR color + the `\x1b(B\x1b[m` reset that
        // leaked as stray `(B` / `m` fragments and desynced the terminal.
        let input = "\x1b[1m\x1b[31merror\x1b[0m\x1b(B\x1b[m: boom";
        assert_eq!(sanitize_display(input), "error: boom");
    }

    #[test]
    fn strips_osc_and_drops_cr() {
        // OSC title sequence (ESC ] ... BEL) plus a CRLF carriage return.
        let input = "\x1b]0;title\x07line\r";
        assert_eq!(sanitize_display(input), "line");
    }

    #[test]
    fn expands_tabs_to_tab_stops() {
        assert_eq!(sanitize_display("a\tb"), "a   b"); // col 1 -> next stop at 4
        assert_eq!(sanitize_display("\tx"), "    x"); // col 0 -> stop at 4
    }

    #[test]
    fn preserves_newlines_and_resets_tab_column() {
        assert_eq!(sanitize_display("a\tb\n\tc"), "a   b\n    c");
    }

    #[test]
    fn clean_text_borrows_without_allocating() {
        assert!(matches!(
            sanitize_display("plain ascii"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}

#[cfg(test)]
mod input_wrap_tests {
    use super::{
        input_visual_row_count, is_table_separator, render_assistant_md, wrap_visual_rows, CODE_BG,
    };

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

    #[test]
    fn code_fence_is_highlighted_and_padded() {
        let md = "intro\n```rust\nfn main() {}\n```\nafter";
        let rows = render_assistant_md(md, 40);
        // Each code row is padded to the full content width with the bg fill.
        let code_row = &rows[1];
        let total: usize = code_row
            .iter()
            .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        assert_eq!(total, 40);
        assert!(code_row.iter().any(|s| s.style.bg == Some(CODE_BG)));
        // Real Rust highlighting (not the plain-text fallback) yields more than
        // one distinct foreground colour — guards the language-alias mapping.
        let colors: std::collections::HashSet<_> =
            code_row.iter().filter_map(|s| s.style.fg).collect();
        assert!(
            colors.len() >= 2,
            "expected syntax highlighting, got {colors:?}"
        );
    }

    #[test]
    fn table_renders_box_borders() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let rows = render_assistant_md(md, 40);
        let first: String = rows[0].iter().map(|s| s.content.as_ref()).collect();
        assert!(first.starts_with('┌') && first.ends_with('┐'));
        // top rule, header, divider, one body row, bottom rule.
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn is_table_separator_detects_rows() {
        assert!(is_table_separator("|---|:--:|"));
        assert!(is_table_separator(" --- | --- "));
        assert!(!is_table_separator("| a | b |"));
        assert!(!is_table_separator("plain text"));
    }

    #[test]
    fn markdown_blocks_never_panic_on_narrow_widths() {
        let md = "| col one | col two | col three |\n|---|---|---|\n| `x` | very long value here | z |\n\n```python\ndef f(x):\n    return x*x\n```";
        for w in [0usize, 1, 3, 5, 12, 80] {
            let _ = render_assistant_md(md, w);
        }
    }
}

//! Top panels: hatch, corner buddy, queue, fleet, todos, spinner.
//! Split out of `ui`; logic unchanged.

use super::*;

/// The hatch animation as a centered overlay over the chat area.
pub(super) fn render_hatch(f: &mut Frame, area: Rect, app: &App) {
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
pub(super) fn render_corner_buddy(f: &mut Frame, area: Rect, pet: usize) {
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

pub(super) fn queued_height(app: &App) -> u16 {
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

pub(super) fn render_queue(f: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().fg(palette::TEXT_MUTED);
    let chev = Style::default().fg(palette::TEXT_FAINT);
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
pub(super) fn fleet_height(app: &App) -> u16 {
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

pub(super) const TODO_VISIBLE_ROWS: usize = 6;

pub(super) fn todos_height(app: &App) -> u16 {
    if !app.show_todos {
        return 0;
    }
    todos_height_for_count(app.session_todos.len())
}

pub(super) fn todos_height_for_count(count: usize) -> u16 {
    if count == 0 {
        return 0;
    }
    let visible = count.min(TODO_VISIBLE_ROWS);
    let overflow = usize::from(count > visible);
    (1 + visible + overflow) as u16
}

/// Truncate `s` to `max` display-ish chars with an ellipsis (char-based, so it
/// never splits a UTF-8 codepoint).
pub(super) fn truncate_chars(s: &str, max: usize) -> String {
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
pub(super) fn render_fleet(f: &mut Frame, area: Rect, app: &mut App) {
    let header_dim = Style::default().fg(palette::TEXT_MUTED);
    let kind_style = Style::default()
        .fg(palette::TEXT_BRIGHT)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette::TEXT_MUTED);
    let accent = Style::default().fg(palette::INFO);

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
        // The panel height is capped (see `fleet_height`). Stop once the next
        // row would fall outside it: otherwise off-screen rows still get a
        // click hit-rect, and those land on the input box / status line so a
        // click there silently toggles a sub-agent you can't even see.
        if lines.len() as u16 >= area.height {
            break;
        }
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
            Some(true) => Span::styled("✓ ", Style::default().fg(palette::SUCCESS)),
            Some(false) => Span::styled("✗ ", Style::default().fg(palette::DANGER)),
            None => {
                let frame = SPINNER_FRAMES
                    [(s.started_at.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len()];
                Span::styled(format!("{frame} "), Style::default().fg(palette::INFO))
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
                Span::styled(s.prompt.clone(), Style::default().fg(palette::TEXT_MUTED)),
            ]));
        }
    }

    app.subagent_rows = rows;
    f.render_widget(Paragraph::new(lines), area);
}

pub(super) fn render_todos(f: &mut Frame, area: Rect, app: &App) {
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
        .fg(palette::TEXT_MUTED)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(palette::TEXT_MUTED);

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

pub(super) fn render_todo_line(
    todo: &TodoItem,
    label_width: usize,
    blocked: bool,
) -> Line<'static> {
    let active_style = Style::default()
        .fg(palette::WARNING)
        .add_modifier(Modifier::BOLD);
    let pending_style = Style::default().fg(palette::TEXT);
    let done_style = Style::default()
        .fg(palette::TEXT_MUTED)
        .add_modifier(Modifier::CROSSED_OUT);
    let done_mark = Style::default().fg(palette::SUCCESS);
    let pending_mark = Style::default().fg(palette::TEXT_MUTED);
    // A pending item still waiting on an unfinished dependency: dimmer body and
    // a distinct marker so it reads as "not yet startable".
    let blocked_style = Style::default().fg(palette::TEXT_FAINT);

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

pub(super) fn render_spinner(f: &mut Frame, area: Rect, app: &App) {
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
    // Claude-parity (its `a = d?.activeForm ?? r`): when a concrete task is in
    // progress, show *its* active form as the live word so the line says what
    // tomte is actually doing; otherwise the drifting companion word from the
    // (possibly user-customized) pool. The drift index is pure, so it never
    // flickers between draws.
    let word: String = app
        .session_todos
        .iter()
        .find(|t| matches!(t.status, TodoStatus::InProgress))
        .map(|t| t.active_form.trim())
        .filter(|f| !f.is_empty())
        .map(|f| f.chars().take(48).collect::<String>())
        .unwrap_or_else(|| {
            let words = &app.spinner_words;
            let idx = spinner_word_index(app.spinner_seed, elapsed, words.len());
            words
                .get(idx)
                .cloned()
                .unwrap_or_else(|| "Working".to_string())
        });
    let line = Line::from(vec![
        Span::styled(format!(" {frame} "), Style::default().fg(palette::INFO)),
        Span::styled(
            format!("{word}…"),
            Style::default()
                .fg(palette::INFO)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({}{extras})", format_elapsed(elapsed)),
            Style::default().fg(palette::TEXT_MUTED),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

pub(super) fn todo_label(todo: &TodoItem) -> &str {
    match todo.status {
        TodoStatus::InProgress => &todo.active_form,
        TodoStatus::Pending | TodoStatus::Completed => &todo.content,
    }
}

pub(super) fn recent_completed_todo_indices(app: &App) -> HashSet<usize> {
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

pub(super) fn visible_todo_indices(
    todos: &[TodoItem],
    recent_completed: &HashSet<usize>,
) -> Vec<usize> {
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

pub(super) fn hidden_todos_summary(todos: &[TodoItem], visible: &[usize]) -> Option<String> {
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

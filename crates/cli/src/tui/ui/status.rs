//! Input box, status footer, and the approval modal. Split out of `ui`; logic unchanged.

use super::*;

pub(super) fn render_input(f: &mut Frame, area: Rect, app: &App) {
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
pub(super) fn wrap_visual_rows(
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

pub(super) fn render_status(f: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn status_left_text(app: &App) -> String {
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
    if let Some(notice) = &app.copy_notice {
        text.push_str(" · ");
        text.push_str(notice);
    }
    text
}

pub(super) fn status_left_text_for_parts(
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

pub(super) fn render_approval(f: &mut Frame, anchor_area: ratatui::layout::Rect, app: &App) {
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
    // "don't ask again", but tomte's own wording): the label names exactly
    // what gets allowed in this project.
    let allow_label = {
        let args_val: serde_json::Value =
            serde_json::from_str(&p.args_json).unwrap_or(serde_json::Value::Null);
        format!(
            "Allow {} in this project",
            tomte_core::permissions::rule_label(&p.tool_name, &args_val)
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

pub(super) fn condense_args(json: &str) -> String {
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

pub(super) fn format_elapsed(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

pub(super) fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, Block};
use opencli_core::auth::AuthMode;

pub fn render(f: &mut Frame, app: &mut App) {
    let spinner_h: u16 = if app.busy { 1 } else { 0 };
    let queue_h: u16 = queued_height(app);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                        // chat
            Constraint::Length(spinner_h),             // spinner (only while busy)
            Constraint::Length(queue_h),               // queued messages
            Constraint::Length(input_height(app)),     // input
            Constraint::Length(1),                     // status line
        ])
        .split(f.area());

    render_chat(f, layout[0], app);
    if app.busy {
        render_spinner(f, layout[1], app);
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
        // 1 row per queued message (truncated) + 1 hint row
        let n = app.message_queue.len().min(4) as u16;
        n + 1
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
            format!("{}…", one_line.chars().take(width.saturating_sub(1)).collect::<String>())
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

fn render_chat(f: &mut Frame, area: Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let expanded = app.expanded_tools;
    let mut lines: Vec<Line> = Vec::new();
    let mut i = 0;
    while i < app.blocks.len() {
        // Group consecutive read_file tool calls into a single block so a
        // batch of reads doesn't dominate the chat with one stanza per file.
        if matches!(&app.blocks[i], Block::Tool { name, .. } if name == "read_file") {
            let mut j = i;
            while j < app.blocks.len()
                && matches!(&app.blocks[j], Block::Tool { name, .. } if name == "read_file")
            {
                j += 1;
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
                let chevron_style = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);
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
            Block::Assistant { text, reasoning, thought_for_secs, .. } => {
                // Compact "Thought for Xs" line once reasoning has completed for this
                // assistant block. While reasoning is still streaming, we suppress it —
                // the spinner row already communicates that the model is thinking.
                if let Some(secs) = thought_for_secs {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "● ",
                            Style::default().fg(Color::Rgb(200, 120, 220)),
                        ),
                        Span::styled(
                            format!("Thought for {secs}s"),
                            Style::default()
                                .fg(Color::Rgb(190, 190, 190))
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                    lines.push(Line::raw(""));
                }
                // Suppress raw reasoning text in chat history.
                let _ = reasoning;
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
            Block::Tool {
                name,
                args,
                output,
                error,
                ..
            } => {
                render_tool(&mut lines, name, args, output.as_deref(), *error, inner_width, expanded);
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
    let prompt_color = if app.busy { Color::Rgb(160, 160, 160) } else { Color::Magenta };
    let prompt = Span::styled(
        "> ",
        Style::default().fg(prompt_color).add_modifier(Modifier::BOLD),
    );

    let lines: Vec<Line> = if app.input.is_empty() {
        vec![Line::from(vec![
            prompt.clone(),
            Span::styled(
                "Try \"build me a todo list app\"",
                Style::default().fg(Color::Rgb(160, 160, 160)),
            ),
        ])]
    } else {
        app.input
            .lines()
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let prefix = if i == 0 {
                    prompt.clone()
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix, Span::raw(l.to_string())])
            })
            .collect()
    };

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);

    // cursor
    let (line_idx, col) = app.input.cursor_pos();
    let cx = area.x + 2 + col as u16; // +2 for "> "
    let cy = area.y + line_idx as u16;
    if cx < area.x + area.width && cy < area.y + area.height {
        f.set_cursor_position((cx, cy));
    }
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
    let mut cwd = app.cwd.display().to_string();
    if let Some(home) = dirs::home_dir() {
        let h = home.display().to_string();
        if cwd.starts_with(&h) {
            cwd = format!("~{}", &cwd[h.len()..]);
        }
    }
    let auth_dot = match app.auth_mode {
        AuthMode::None => Span::styled("● ", Style::default().fg(Color::Red)),
        AuthMode::ApiKey => Span::styled("● ", Style::default().fg(Color::Cyan)),
        AuthMode::ChatGPT => Span::styled("● ", Style::default().fg(Color::Green)),
    };
    let right_spans = vec![
        auth_dot,
        Span::styled(app.config.model.clone(), Style::default().fg(Color::Gray)),
        Span::styled(
            format!(" · {}", app.config.reasoning_effort),
            Style::default().fg(Color::Rgb(160, 160, 160)),
        ),
        Span::styled(format!("  {cwd} "), Style::default().fg(Color::Rgb(160, 160, 160))),
    ];
    let right_text: String = right_spans.iter().map(|s| s.content.as_ref()).collect();
    let right_width = unicode_width::UnicodeWidthStr::width(right_text.as_str()) as u16;
    let total = area.width;
    let left_width = total.saturating_sub(right_width).saturating_sub(1);

    let left_rect = Rect { x: area.x, y: area.y, width: left_width, height: 1 };
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
    use ratatui::widgets::{Block as RBlock, Borders, BorderType, Clear};
    let Some(p) = app.pending_approval.as_ref() else { return };

    let dim = Style::default().fg(Color::Rgb(170, 170, 170));
    let bg = Style::default().bg(Color::Rgb(20, 20, 22));
    let accent = Style::default().fg(Color::Rgb(25, 195, 154)).add_modifier(Modifier::BOLD);
    let warn = Style::default().fg(Color::Rgb(255, 182, 73)).add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("  Tool: ", dim),
        Span::styled(p.tool_name.clone(), accent),
    ]));
    let args_preview = condense_args(&p.args_json);
    if !args_preview.is_empty() {
        lines.push(Line::from(Span::styled(format!("  args: {args_preview}"), dim)));
    }
    if let Some(d) = p.diff_preview.as_ref() {
        lines.push(Line::from(Span::styled("  ─ preview ─", dim)));
        for raw in d.lines().take(8) {
            lines.push(Line::from(Span::styled(format!("  {raw}"), Style::default().fg(Color::Rgb(220, 220, 220)))));
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
    let popup = ratatui::layout::Rect { x, y, width, height };
    f.render_widget(Clear, popup);
    let block = RBlock::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(255, 182, 73)))
        .title(Span::styled(" Approve tool call? ", warn))
        .style(bg);
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    f.render_widget(Paragraph::new(lines).style(bg).wrap(Wrap { trim: false }), inner);
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
                let vs = if vs.len() > 60 { format!("{}…", &vs[..60]) } else { vs };
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

    let mut cwd = app.cwd.display().to_string();
    if let Some(home) = dirs::home_dir() {
        let h = home.display().to_string();
        if cwd.starts_with(&h) {
            cwd = format!("~{}", &cwd[h.len()..]);
        }
    }

    let auth_label = match app.auth_mode {
        opencli_core::auth::AuthMode::ChatGPT => "ChatGPT account",
        opencli_core::auth::AuthMode::ApiKey => "API key",
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
    let help_row = (vec![Span::styled(help_text, dim)], help_text.chars().count());

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
        if let Block::Tool { args, output, error, .. } = b {
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
            let line_count = output
                .as_deref()
                .map(|o| o.lines().count())
                .unwrap_or(0);
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
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("({})", pretty_path(path)), gray),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  └ ".to_string(), dim),
            Span::styled(format!("{} line{}", lc, plural(*lc)), gray),
        ]));
    } else {
        // Multi-read: one header summarising the batch, optional file list
        // only when in expanded mode.
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(bullet_color)),
            Span::styled(
                format!("Read {} files", count),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {} lines total", total_lines), gray),
        ]));
        if expanded {
            for (idx, (path, lc, err, done)) in entries.iter().enumerate() {
                let last = idx + 1 == count;
                let branch = if last { "  └ " } else { "  ├ " };
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
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
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
        let branch = if i + 1 == total { "  └ " } else { "  │ " };
        lines.push(Line::from(
            std::iter::once(Span::styled(
                branch.to_string(),
                Style::default().fg(Color::Rgb(160, 160, 160)),
            ))
            .chain(body.spans.into_iter())
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
        "edit_file" => ("Edit".into(), pretty_path(&s("path"))),
        "list_dir" => ("List".into(), pretty_path(&s("path"))),
        "grep" => {
            let pat = s("pattern");
            let path = s("path");
            if path.is_empty() {
                ("Grep".into(), format!("\"{pat}\""))
            } else {
                ("Grep".into(), format!("\"{pat}\" in {}", pretty_path(&path)))
            }
        }
        "glob" => ("Glob".into(), s("pattern")),
        "todo_write" => ("Update Todos".into(), String::new()),
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
            let added = if new_.is_empty() { 0 } else { new_.lines().count() };
            let removed = if old.is_empty() { 0 } else { old.lines().count() };
            let summary_text = match (added, removed) {
                (a, 0) => format!("Added {a} line{}", plural(a)),
                (0, r) => format!("Removed {r} line{}", plural(r)),
                (a, r) => format!("Added {a} line{}, removed {r} line{}", plural(a), plural(r)),
            };
            out.push(Line::from(Span::styled(summary_text, style_summary)));

            // Determine starting line number by trying to locate old_string in the file.
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let start_line = locate_line_number(path, old).unwrap_or(1);

            let removed_bg = Style::default().bg(Color::Rgb(60, 0, 0)).fg(Color::Rgb(255, 120, 120));
            let added_bg = Style::default().bg(Color::Rgb(0, 50, 0)).fg(Color::Rgb(160, 255, 160));
            let lineno_removed = Style::default().bg(Color::Rgb(60, 0, 0)).fg(Color::Rgb(200, 80, 80));
            let lineno_added = Style::default().bg(Color::Rgb(0, 50, 0)).fg(Color::Rgb(120, 200, 120));

            let mut shown = 0usize;
            let max_diff = limits.edit_diff;

            for (i, line) in old.lines().enumerate() {
                if shown >= max_diff { break; }
                let n = start_line + i;
                out.push(diff_line(n, "-", line, lineno_removed, removed_bg, avail));
                shown += 1;
            }
            for (i, line) in new_.lines().enumerate() {
                if shown >= max_diff { break; }
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
                            format!("… +{} stderr line{}", total_err - max_err, plural(total_err - max_err)),
                            style_meta,
                        )));
                    }
                } else {
                    // Success but with stderr noise (warnings, etc.). One-line
                    // hint keeps things clean without losing the signal.
                    let n = stderr.lines().filter(|l| !l.trim().is_empty()).count();
                    out.push(Line::from(Span::styled(
                        format!("(+ {n} stderr line{} suppressed — Ctrl+O to view)", plural(n)),
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
                out.push(Line::from(Span::styled(
                    format!("exit {code}"),
                    code_style,
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
        "glob" | "list_dir" => {
            let total = text.lines().filter(|l| !l.is_empty()).count();
            out.push(Line::from(Span::styled(
                format!("{total} entr{}", if total == 1 { "y" } else { "ies" }),
                style_summary,
            )));
            for raw in text.lines().take(limits.list_preview) {
                if raw.is_empty() { continue; }
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
                    format!("… +{} lines (Ctrl+O for more)", total - limits.default_preview),
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
    if n == 1 { "" } else { "s" }
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
        format!("{}…", text.chars().take(body_width.saturating_sub(1)).collect::<String>())
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
    if let Some(home) = dirs::home_dir() {
        let h = home.display().to_string();
        if let Some(stripped) = p.strip_prefix(&h) {
            return format!("~{stripped}");
        }
    }
    p.to_string()
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
                            format!("\"{}{}\"", trimmed, if s.chars().count() > 50 { "…" } else { "" })
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
    let lines = app.input.lines().len().max(1);
    let inner = lines.min(max_visible);
    (inner as u16).saturating_add(2)
}

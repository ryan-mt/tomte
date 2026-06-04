//! Welcome banner, read-group, and tool-call headers. Split out of `ui`; logic unchanged.

use super::*;

pub(super) fn render_welcome(lines: &mut Vec<Line<'static>>, app: &App) {
    let dim = Style::default().fg(Color::Rgb(160, 160, 160));
    let muted = Style::default().fg(Color::Rgb(125, 125, 125));
    let strong = Style::default()
        .fg(Color::Rgb(230, 230, 230))
        .add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(Color::Rgb(25, 195, 154));
    let border = Style::default().fg(Color::Rgb(80, 80, 80));

    let cwd = shorten_home_path(&app.cwd);

    let auth_label = match app.auth_mode {
        tomte_core::auth::AuthMode::OpenaiOauth => "ChatGPT account",
        tomte_core::auth::AuthMode::OpenaiApiKey => "OpenAI API key",
        tomte_core::auth::AuthMode::AnthropicOauth => "Claude OAuth",
        tomte_core::auth::AuthMode::AnthropicApiKey => "Anthropic API key",
        tomte_core::auth::AuthMode::None => "offline",
    };

    // Each row is `(spans, visible_width)`. Width is precomputed so the
    // right edge of the rounded border stays pinned even when the terminal
    // resizes or the cwd changes — mirrors Claude Code's welcome card.
    let version = env!("CARGO_PKG_VERSION");
    let sparkle = "✻ ";
    let title = "Welcome to tomte! ";
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
pub(super) fn render_read_group(lines: &mut Vec<Line<'static>>, blocks: &[Block], expanded: bool) {
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

pub(super) fn render_tool(
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

pub(super) fn friendly_header(name: &str, args: &serde_json::Value) -> (String, String) {
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

//! Welcome banner, read-group, and tool-call headers. Split out of `ui`; logic unchanged.

use super::*;

pub(super) fn render_welcome(lines: &mut Vec<Line<'static>>, app: &App) {
    let muted = Style::default().fg(palette::TEXT_MUTED);
    let faint = Style::default().fg(palette::TEXT_FAINT);
    let strong = Style::default()
        .fg(palette::TEXT_BRIGHT)
        .add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(palette::ACCENT);
    let border = Style::default().fg(palette::BORDER);
    let ok = Style::default().fg(palette::SUCCESS);

    let cwd = shorten_home_path(&app.cwd);
    let version = env!("CARGO_PKG_VERSION");

    let auth_label = match app.auth_mode {
        tomte_core::auth::AuthMode::OpenaiOauth => "ChatGPT account",
        tomte_core::auth::AuthMode::OpenaiApiKey => "OpenAI API key",
        tomte_core::auth::AuthMode::AnthropicOauth => "Claude OAuth",
        tomte_core::auth::AuthMode::AnthropicApiKey => "Anthropic API key",
        tomte_core::auth::AuthMode::None => "offline",
    };

    // The welcome companion: a glimpse of the account's pet (see `welcome_pet`),
    // or the adopted one once hatched. The sprite keeps its own bright colors —
    // a deliberate exception to the calm palette (SOUL.md), like the corner
    // buddy. Six half-block rows, each a fixed 14 cols wide.
    let pet_idx = app.buddy_pet.unwrap_or(app.welcome_pet);
    let pet_lines = crate::tui::buddy::mini_lines(pet_idx);
    let pet_w = pet_lines.iter().map(Line::width).max().unwrap_or(0);

    // Live onboarding signal: is there a project house-rules file in cwd? (See
    // `core::memory` for the discovery order — we test the same candidates here.)
    // Its presence flips the first getting-started step to a done ✓ instead of a
    // to-do ○.
    let has_rules = ["AGENTS.override.md", "AGENTS.md", "CLAUDE.md"]
        .iter()
        .any(|f| app.cwd.join(f).exists());

    let greeting = "hi, i'm tomte!";
    let tagline = "i keep your codebase tidy — and remember why";
    let model_summary = format!(
        "{} · effort {} · {}",
        app.config.model, app.config.reasoning_effort, auth_label
    );

    // Setup facts share a label column so their values line up; getting-started
    // steps carry a ✓/○ bullet. Each row is a short, never-trimmed `prefix`
    // (heading / label / bullet) plus a `body` trimmed to fit a narrow terminal,
    // and an optional value pinned to the right border.
    const LABEL_COL: usize = 9; // "workspace"
    let setup = |label: &'static str, value: String| Row {
        prefix: vec![Span::styled(format!("{label:<LABEL_COL$}  "), faint)],
        prefix_w: LABEL_COL + 2,
        body: value,
        body_style: muted,
        right: String::new(),
        right_style: muted,
    };
    let step = |done: bool, text: &'static str| {
        let (bullet, bstyle) = if done { ("✓ ", ok) } else { ("○ ", faint) };
        Row {
            prefix: vec![Span::styled(bullet, bstyle)],
            prefix_w: 2,
            body: text.to_string(),
            body_style: muted,
            right: String::new(),
            right_style: muted,
        }
    };
    let plain = |text: &'static str, style: Style| Row {
        prefix: vec![],
        prefix_w: 0,
        body: text.to_string(),
        body_style: style,
        right: String::new(),
        right_style: style,
    };

    // A complete-but-calm panel sized to the pet's height (so no row of the card
    // is petless): brand + version, the active setup, where we are, a live
    // getting-started signal, then a quiet shortcuts footer. The version is
    // anchored to the right border so the header spans the full width.
    let rows: Vec<Row> = vec![
        Row {
            prefix: vec![Span::styled("✿ ", accent)],
            prefix_w: 2,
            body: greeting.to_string(),
            body_style: strong,
            right: format!("v{version}"),
            right_style: muted,
        },
        plain(tagline, muted),
        setup("model", model_summary),
        setup("workspace", cwd),
        if has_rules {
            step(true, "house rules in play — i'll honor your AGENTS.md")
        } else {
            step(false, "run /init so i learn this project's house rules")
        },
        plain(
            "/help · shift+tab cycles · Ctrl/Alt+V paste · ^C exit",
            faint,
        ),
    ];

    const GAP: usize = 3; // columns between the sprite and the text column
    const MIN_GAP: usize = 2; // min columns between the body and a right value

    // Span the full terminal width so a wide terminal doesn't leave a large
    // empty gutter beside the card: the text column simply takes all the width
    // left after the sprite. A full line is `inner_width + 6` (2 margin + 2
    // borders + 2 inner spaces), and inner_width = pet_w + GAP + text_w.
    // last_width is 0 before the first real draw — assume 80 then; over-long
    // rows are trimmed per row below.
    let term_width = if app.last_width == 0 {
        80
    } else {
        app.last_width as usize
    };
    let text_w = term_width.saturating_sub(6 + pet_w + GAP).max(8);
    let inner_width = pet_w + GAP + text_w;
    let horiz: String = "─".repeat(inner_width + 2);

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("  ", muted),
        Span::styled(format!("╭{horiz}╮"), border),
    ]));

    let row_count = pet_lines.len().max(rows.len());
    for i in 0..row_count {
        let mut row: Vec<Span<'static>> = Vec::new();
        row.push(Span::styled("  ", muted));
        row.push(Span::styled("│ ", border));
        // Sprite column, padded to `pet_w` so the text column always aligns.
        if let Some(pl) = pet_lines.get(i) {
            let plw = pl.width();
            row.extend(pl.spans.clone());
            if plw < pet_w {
                row.push(Span::raw(" ".repeat(pet_w - plw)));
            }
        } else {
            row.push(Span::raw(" ".repeat(pet_w)));
        }
        row.push(Span::raw(" ".repeat(GAP)));

        // Text column, exactly `text_w` wide: prefix, body (trimmed to fit),
        // padding, then the right value flush against the inner border.
        if let Some(r) = rows.get(i) {
            row.extend(r.prefix.clone());
            // Measure by display width, not code points: the text column is
            // `text_w` terminal columns wide (the sprite column already uses
            // `.width()`), so a wide CJK/emoji glyph in the body (e.g. a `cwd`
            // with a CJK directory) or the right value must cost two columns —
            // counting chars let a wide row overrun and push the right `│` border
            // out of alignment.
            let right_w = unicode_width::UnicodeWidthStr::width(r.right.as_str());
            let body_cap =
                text_w.saturating_sub(r.prefix_w + if right_w > 0 { MIN_GAP + right_w } else { 0 });
            let body = truncate_to_width(&r.body, body_cap);
            let body_w = unicode_width::UnicodeWidthStr::width(body.as_str());
            if !body.is_empty() {
                row.push(Span::styled(body, r.body_style));
            }
            let pad = text_w.saturating_sub(r.prefix_w + body_w + right_w);
            row.push(Span::raw(" ".repeat(pad)));
            if right_w > 0 {
                row.push(Span::styled(r.right.clone(), r.right_style));
            }
        } else {
            row.push(Span::raw(" ".repeat(text_w)));
        }

        row.push(Span::styled(" │", border));
        lines.push(Line::from(row));
    }

    lines.push(Line::from(vec![
        Span::styled("  ", muted),
        Span::styled(format!("╰{horiz}╯"), border),
    ]));
}

/// One text row of the welcome panel: a short, never-trimmed `prefix` (heading,
/// label, or ✓/○ bullet), a `body` trimmed to fit the panel width, and an
/// optional value pinned to the right border.
struct Row {
    prefix: Vec<Span<'static>>,
    prefix_w: usize,
    body: String,
    body_style: Style,
    right: String,
    right_style: Style,
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
        palette::DANGER
    } else if any_pending {
        palette::WARNING
    } else {
        palette::SUCCESS
    };

    let total_lines: usize = entries.iter().map(|(_, l, _, _)| *l).sum();
    let count = entries.len();
    let dim = Style::default().fg(palette::TEXT_MUTED);
    let gray = Style::default().fg(palette::TEXT_MUTED);

    if count == 1 {
        // Single read: keep the familiar "Read(path)" header but with a tiny
        // summary on the next line, no contents.
        let (path, lc, _err, _done) = &entries[0];
        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(bullet_color)),
            Span::styled(
                "Read".to_string(),
                Style::default()
                    .fg(palette::TEXT_BRIGHT)
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
                    .fg(palette::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" · {} lines total", total_lines), gray),
        ]));
        if expanded {
            for (idx, (path, lc, err, done)) in entries.iter().enumerate() {
                let branch = if idx == 0 { "  ⎿ " } else { "    " };
                let path_style = if *err {
                    Style::default().fg(palette::DANGER)
                } else if !*done {
                    Style::default().fg(palette::WARNING)
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

// Flat render of a tool block's fields (the Pillar-1 `preflight` pushed this to
// 8); bundling them into a struct would just add boilerplate at both call sites.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_tool(
    lines: &mut Vec<Line<'static>>,
    name: &str,
    args: &str,
    output: Option<&str>,
    error: bool,
    preflight: Option<&PreFlight>,
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
        palette::DANGER
    } else if output.is_none() {
        palette::WARNING
    } else {
        palette::SUCCESS
    };

    // Header: ● Write(file)
    let mut header_spans = vec![
        Span::styled("● ", Style::default().fg(bullet_color)),
        Span::styled(
            display_name,
            Style::default()
                .fg(palette::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!("({summary})"),
            Style::default().fg(palette::TEXT_MUTED),
        ));
    }
    lines.push(Line::from(header_spans));

    // SOUL Pillar 1 — the glass-box pre-flight: WHAT this call will do and HOW
    // FAR it can reach, shown between the header and the result. A leashed
    // (flagged-destructive) call adds a second, warning-toned line.
    if let Some(pf) = preflight {
        lines.push(Line::from(vec![
            Span::styled("  ▸ ", Style::default().fg(palette::ACCENT)),
            Span::styled(pf.scope.clone(), Style::default().fg(palette::TEXT_FAINT)),
        ]));
        if let Some(leash) = &pf.leash {
            lines.push(Line::from(vec![
                Span::styled("    ⚠ ", Style::default().fg(palette::WARNING)),
                Span::styled(leash.clone(), Style::default().fg(palette::WARNING)),
            ]));
        }
        // Pillar 5 (A2 Tier 1): the file's recorded decisions, surfaced as house
        // rules at the instant of an edit — the custodian re-reads its own promises
        // before it could break one. Pure recall; never a gate.
        if !pf.house_rules.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ⌂ ", Style::default().fg(palette::ACCENT)),
                Span::styled(
                    "house rules for this file",
                    Style::default().fg(palette::TEXT_MUTED),
                ),
            ]));
            for rule in &pf.house_rules {
                lines.push(Line::from(Span::styled(
                    format!("    {rule}"),
                    Style::default().fg(palette::TEXT_FAINT),
                )));
            }
        }
    }

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
        // Branch the first result line with `⎿`, then align any continuation
        // lines under it — no per-line gutter glyph.
        let branch = if i == 0 { "  ⎿ " } else { "    " };
        lines.push(Line::from(
            std::iter::once(Span::styled(
                branch.to_string(),
                Style::default().fg(palette::TEXT_MUTED),
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
        // The custodian remembering the house: surface the decision in the
        // header and the *why* in the body, so the moat is visible the instant
        // it's recorded — not a silent tool call (Pillar 2).
        "record_decision" => ("Remember".into(), s("decision")),
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
            // Sanitize and flatten to one line: a raw control char or newline in
            // `command` (heredocs, pasted multi-line commands) must not break or
            // desync the single-line header the way the unsanitized value did.
            let cmd = sanitize_display(&s("command")).replace('\n', " ");
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

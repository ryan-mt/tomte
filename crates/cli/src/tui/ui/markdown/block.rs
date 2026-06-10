use super::*;

/// Render the assistant's markdown text into content rows (each a `Vec<Span>`,
/// without the leading bullet/indent gutter). Handles fenced code blocks
/// (syntax-highlighted) and GFM tables (box-drawn) as whole blocks; all other
/// lines are word-wrapped and passed through the inline markdown styler.
pub(crate) fn render_assistant_md(text: &str, content_width: usize) -> Vec<Vec<Span<'static>>> {
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
        // Thematic break: a line of only `-`/`*`/`_` (3+ of one kind, spaces
        // allowed) renders as a faint horizontal rule instead of literal
        // dashes. Checked AFTER the fence (``` opens a block) and BEFORE the
        // table look-ahead, but never when the NEXT line makes this a table
        // separator's header (that can't happen: a `---` line under a `|` row
        // is consumed by the table branch below via lines[i+1]).
        if is_thematic_break(trimmed) {
            out.push(vec![Span::styled(
                "─".repeat(content_width.clamp(1, 80)),
                Style::default().fg(palette::TEXT_FAINT),
            )]);
            i += 1;
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
        // Heading, list item, blockquote, or plain paragraph.
        push_prose_line(&mut out, line, content_width);
        i += 1;
    }
    if out.is_empty() {
        out.push(vec![Span::raw("")]);
    }
    out
}

/// A leading ATX heading (`#`..`######` then a space): returns the heading text
/// with the markers stripped. Requires the space so `#define`, `#!/bin/sh`, and
/// a `#1` issue reference are not mistaken for headings.
fn parse_heading(s: &str) -> Option<&str> {
    let hashes = s.len() - s.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) {
        if let Some(text) = s[hashes..].strip_prefix(' ') {
            return Some(text.trim_end_matches('#').trim());
        }
    }
    None
}

/// A thematic break (`---`, `***`, `___`, also spaced `- - -`): one marker kind
/// only, at least three of it, nothing else but spaces. Rendered as a faint
/// horizontal rule. The single-kind requirement keeps mixed runs (`-*-`) and
/// the empty string literal.
fn is_thematic_break(trimmed: &str) -> bool {
    for marker in ['-', '*', '_'] {
        let n = trimmed.chars().filter(|&c| c == marker).count();
        if n >= 3 && trimmed.chars().all(|c| c == marker || c == ' ') {
            return true;
        }
    }
    false
}

/// A leading list marker: a `-`/`*`/`+` bullet (normalized to `•`, or to a
/// `☐`/`✓` checkbox for a GFM task item `- [ ]` / `- [x]`) or an ordered
/// `1.`/`1)` item (number kept). Returns the marker to render plus the item
/// body. The trailing space is required, so `*emphasis*` and `3.14` are not
/// treated as list items.
fn parse_list_marker(s: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(bullet) {
            // GFM task-list item: render the checkbox as a glyph instead of
            // literal brackets, mirroring the todo panel's done/pending marks.
            if let Some(body) = rest.strip_prefix("[ ] ") {
                return Some(("☐ ".to_string(), body));
            }
            if let Some(body) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                return Some(("✓ ".to_string(), body));
            }
            return Some(("• ".to_string(), rest));
        }
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&digits) {
        let after = &s[digits..];
        for delim in [". ", ") "] {
            if let Some(rest) = after.strip_prefix(delim) {
                return Some((format!("{}{} ", &s[..digits], &delim[..1]), rest));
            }
        }
    }
    None
}

/// Render one non-fence, non-table line. A heading renders bright/bold with its
/// `#`s stripped; a list item and a blockquote get a hanging indent so a wrapped
/// continuation line aligns under the text instead of back at the marker — the
/// single biggest "reads like raw markdown" gap in everyday answers. Plain
/// paragraphs keep the original wrap-then-inline-style path.
fn push_prose_line(out: &mut Vec<Vec<Span<'static>>>, line: &str, content_width: usize) {
    use unicode_width::UnicodeWidthStr;
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let indent_cols = indent.width();
    let ind = || -> Vec<Span<'static>> {
        if indent.is_empty() {
            Vec::new()
        } else {
            vec![Span::raw(indent.to_string())]
        }
    };
    let faint = Style::default().fg(palette::TEXT_FAINT);

    if let Some(text) = parse_heading(rest) {
        let style = Style::default()
            .fg(palette::TEXT_BRIGHT)
            .add_modifier(Modifier::BOLD);
        push_wrapped_with_prefix(
            out,
            ind(),
            ind(),
            indent_cols,
            text,
            content_width,
            Some(style),
        );
    } else if let Some(body) = rest
        .strip_prefix("> ")
        .or_else(|| (rest == ">").then_some(""))
    {
        let mut first = ind();
        first.push(Span::styled("│ ", faint));
        let mut cont = ind();
        cont.push(Span::styled("│ ", faint));
        push_wrapped_with_prefix(
            out,
            first,
            cont,
            indent_cols + 2,
            body,
            content_width,
            Some(faint),
        );
    } else if let Some((marker, body)) = parse_list_marker(rest) {
        let mcols = marker.width();
        let mut first = ind();
        first.push(Span::styled(marker, faint));
        let mut cont = ind();
        cont.push(Span::raw(" ".repeat(mcols)));
        push_wrapped_with_prefix(
            out,
            first,
            cont,
            indent_cols + mcols,
            body,
            content_width,
            None,
        );
    } else {
        for w in wrap(line, content_width) {
            out.push(render_markdown_inline(&w));
        }
    }
}

/// Wrap `body` to the room left after a `prefix_cols`-wide gutter and emit one
/// content row per wrapped line: the first row carries `first`, every
/// continuation row `cont` (same width), giving list items and blockquotes their
/// hanging indent. `body_style`, when set, is patched over the inline styling.
fn push_wrapped_with_prefix(
    out: &mut Vec<Vec<Span<'static>>>,
    first: Vec<Span<'static>>,
    cont: Vec<Span<'static>>,
    prefix_cols: usize,
    body: &str,
    content_width: usize,
    body_style: Option<Style>,
) {
    let body_width = content_width.saturating_sub(prefix_cols).max(1);
    let mut wrapped = wrap(body, body_width);
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    for (idx, w) in wrapped.into_iter().enumerate() {
        let mut row = if idx == 0 {
            first.clone()
        } else {
            cont.clone()
        };
        let mut spans = render_markdown_inline(&w);
        if let Some(st) = body_style {
            for s in spans.iter_mut() {
                s.style = s.style.patch(st);
            }
        }
        row.extend(spans);
        out.push(row);
    }
}

/// Hard-wrap a styled span run to `width` display columns, padding every row to
/// the full width with `bg` so a code block renders as a flush rectangle.
pub(crate) fn wrap_spans(
    spans: Vec<Span<'static>>,
    width: usize,
    bg: Color,
) -> Vec<Vec<Span<'static>>> {
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

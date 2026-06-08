//! Inline markdown, syntax highlighting, and table rendering. Split out of `ui`; logic unchanged.

use super::*;

/// Very small inline markdown renderer: handles `` `code` ``, **bold**, and
/// *italic*. Only a *matched* pair styles its span — an unmatched marker (a glob
/// like `*.rs`, a multiplication `2 * 3`, or an unterminated `` `code ``) is
/// emitted literally instead of swallowing the rest of the line. Emphasis
/// follows CommonMark's flanking rule loosely: a marker only opens when the
/// character after it is non-whitespace, and only closes when the character
/// before it is non-whitespace, so a spaced asterisk stays a plain asterisk.
pub(super) fn render_markdown_inline(line: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let code_style = Style::default()
        .fg(Color::Rgb(255, 184, 108))
        .bg(Color::Rgb(40, 30, 18));
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let italic_style = Style::default().add_modifier(Modifier::ITALIC);
    let plain = Style::default().fg(palette::TEXT_MUTED);
    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), plain));
        }
    };

    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // Code span: style only when a closing backtick exists on the line.
            '`' => {
                if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                    flush(&mut buf, &mut spans);
                    let code: String = chars[i + 1..close].iter().collect();
                    spans.push(Span::styled(code, code_style));
                    i = close + 1;
                } else {
                    buf.push('`');
                    i += 1;
                }
            }
            // Bold: `**x**` where `x` is non-empty and not space-flanked.
            '*' if chars.get(i + 1) == Some(&'*') => {
                let start = i + 2;
                let close = chars
                    .get(start)
                    .filter(|c| !c.is_whitespace())
                    .and_then(|_| {
                        (start + 1..chars.len()).find(|&j| {
                            chars[j] == '*'
                                && chars.get(j + 1) == Some(&'*')
                                && !chars[j - 1].is_whitespace()
                        })
                    });
                if let Some(k) = close {
                    flush(&mut buf, &mut spans);
                    let bold: String = chars[start..k].iter().collect();
                    spans.push(Span::styled(bold, bold_style));
                    i = k + 2;
                } else {
                    // Emit BOTH markers literally and skip past them. Leaving the
                    // second `*` to be re-evaluated would let it pair with a later
                    // lone `*` (e.g. the `*` in a `**/*.ts` glob), re-introducing
                    // the runaway this guards against.
                    buf.push_str("**");
                    i += 2;
                }
            }
            // Italic: `*x*` where `x` is non-empty and not space-flanked.
            '*' => {
                let start = i + 1;
                let close = chars
                    .get(start)
                    .filter(|c| !c.is_whitespace() && **c != '*')
                    .and_then(|_| {
                        (start + 1..chars.len())
                            .find(|&j| chars[j] == '*' && !chars[j - 1].is_whitespace())
                    });
                if let Some(k) = close {
                    flush(&mut buf, &mut spans);
                    let italic: String = chars[start..k].iter().collect();
                    spans.push(Span::styled(italic, italic_style));
                    i = k + 1;
                } else {
                    buf.push('*');
                    i += 1;
                }
            }
            c => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush(&mut buf, &mut spans);
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    spans
}

/// Lazily-loaded syntect assets (syntax definitions + a dark theme). Loading
/// the default sets parses an embedded binary dump, so do it once per process.
pub(super) fn syntax_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme)
{
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
pub(super) const CODE_BG: Color = palette::SURFACE_CODE;

/// Render the assistant's markdown text into content rows (each a `Vec<Span>`,
/// without the leading bullet/indent gutter). Handles fenced code blocks
/// (syntax-highlighted) and GFM tables (box-drawn) as whole blocks; all other
/// lines are word-wrapped and passed through the inline markdown styler.
pub(super) fn render_assistant_md(text: &str, content_width: usize) -> Vec<Vec<Span<'static>>> {
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

/// A leading list marker: a `-`/`*`/`+` bullet (normalized to `•`) or an
/// ordered `1.`/`1)` item (number kept). Returns the marker to render plus the
/// item body. The trailing space is required, so `*emphasis*` and `3.14` are
/// not treated as list items.
fn parse_list_marker(s: &str) -> Option<(String, &str)> {
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(bullet) {
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
    } else if let Some(body) = rest.strip_prefix("> ").or((rest == ">").then_some("")) {
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

/// Resolve a fenced-code language token to a syntect syntax. syntect matches by
/// file extension or exact name, so common fence labels (`rust`, `python`,
/// `bash`, …) miss unless mapped to the extension first; we then fall back to
/// the raw/lower-cased token and finally plain text.
pub(super) fn resolve_syntax<'a>(
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
pub(super) fn highlight_code_lines(
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
pub(super) fn wrap_spans(
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

/// True when `line` is a GFM table separator row, e.g. `|---|:--:|---|`.
pub(super) fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    // Require a `|`: a separator delimits columns. Otherwise a bare `---`
    // thematic break (valid CommonMark, common in assistant output) following
    // any line that merely contains a `|` would be misread as a table.
    if !t.contains('-') || !t.contains('|') {
        return false;
    }
    t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Split one table row into trimmed cell strings, dropping the empty cells that
/// flank rows written with outer pipes (`| a | b |`).
pub(super) fn split_table_row(line: &str) -> Vec<String> {
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
pub(super) fn md_cell_width(s: &str) -> usize {
    let stripped: String = s.chars().filter(|c| !matches!(c, '`' | '*')).collect();
    unicode_width::UnicodeWidthStr::width(stripped.as_str())
}

/// Render a GFM table (header row, separator, body rows) into box-drawn content
/// rows. Columns are sized to content and shrunk to fit `content_width`; cells
/// that still overflow are word-wrapped.
pub(super) fn render_md_table(tbl: &[&str], content_width: usize) -> Vec<Vec<Span<'static>>> {
    let border = Style::default().fg(palette::BORDER);
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

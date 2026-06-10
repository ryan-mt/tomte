use super::*;

/// True when `line` is a GFM table separator row, e.g. `|---|:--:|---|`.
pub(crate) fn is_table_separator(line: &str) -> bool {
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
pub(crate) fn split_table_row(line: &str) -> Vec<String> {
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
pub(crate) fn md_cell_width(s: &str) -> usize {
    let stripped: String = s.chars().filter(|c| !matches!(c, '`' | '*')).collect();
    unicode_width::UnicodeWidthStr::width(stripped.as_str())
}

/// Render a GFM table (header row, separator, body rows) into box-drawn content
/// rows. Columns are sized to content and shrunk to fit `content_width`; cells
/// that still overflow are word-wrapped.
pub(crate) fn render_md_table(tbl: &[&str], content_width: usize) -> Vec<Vec<Span<'static>>> {
    let border = Style::default().fg(palette::BORDER);
    let header_style = Style::default()
        .fg(palette::TEXT_BRIGHT)
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

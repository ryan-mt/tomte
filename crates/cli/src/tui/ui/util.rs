//! Path/text utilities and input-height math. Split out of `ui`; logic unchanged.

use super::*;

pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

pub(super) fn diff_line<'a>(
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

/// One row of a rendered diff: an unchanged context line, a removed line, or an
/// added line. Carrying the borrowed text keeps [`diff_hunk`] allocation-free.
pub(super) enum DiffRow<'a> {
    Context(&'a str),
    Del(&'a str),
    Add(&'a str),
}

/// tomte's own minimal line-diff. An edit's `old_string`/`new_string` usually
/// share a few unchanged anchor lines at the top and bottom; trimming those into
/// context leaves only the divergent middle as removed/added — so an edit that
/// touches one line in a five-line block reads as a single `-`/`+` pair framed by
/// context, not five removed lines stacked above five added ones. Deliberately
/// *not* a full LCS/Myers diff: an `edit_file` call is one contiguous change, so a
/// shared prefix/suffix trim covers it and stays trivial to reason about.
pub(super) fn diff_hunk<'a>(old: &'a str, new: &'a str) -> Vec<DiffRow<'a>> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Shared leading lines (identical in both sides).
    let max_lead = old_lines.len().min(new_lines.len());
    let mut lead = 0;
    while lead < max_lead && old_lines[lead] == new_lines[lead] {
        lead += 1;
    }
    // Shared trailing lines, never crossing back into the leading region.
    let mut tail = 0;
    while tail < old_lines.len() - lead
        && tail < new_lines.len() - lead
        && old_lines[old_lines.len() - 1 - tail] == new_lines[new_lines.len() - 1 - tail]
    {
        tail += 1;
    }

    let mut rows = Vec::with_capacity(old_lines.len() + new_lines.len());
    rows.extend(new_lines[..lead].iter().copied().map(DiffRow::Context));
    rows.extend(
        old_lines[lead..old_lines.len() - tail]
            .iter()
            .copied()
            .map(DiffRow::Del),
    );
    rows.extend(
        new_lines[lead..new_lines.len() - tail]
            .iter()
            .copied()
            .map(DiffRow::Add),
    );
    rows.extend(
        new_lines[new_lines.len() - tail..]
            .iter()
            .copied()
            .map(DiffRow::Context),
    );
    rows
}

/// The diff palette: matching coloured beds for removed/added lines plus a quiet,
/// bed-less style for unchanged context. Bundled so the two edit renderers share
/// one source of truth (and to keep [`push_diff_rows`] under the arg-count lint).
pub(super) struct DiffStyles {
    removed_bg: Style,
    added_bg: Style,
    lineno_removed: Style,
    lineno_added: Style,
    ctx_lineno: Style,
    ctx_body: Style,
}

impl DiffStyles {
    pub(super) fn new() -> Self {
        Self {
            removed_bg: Style::default()
                .bg(palette::DIFF_DEL_BG)
                .fg(palette::DIFF_DEL_FG),
            added_bg: Style::default()
                .bg(palette::DIFF_ADD_BG)
                .fg(palette::DIFF_ADD_FG),
            lineno_removed: Style::default()
                .bg(palette::DIFF_DEL_BG)
                .fg(palette::DANGER),
            lineno_added: Style::default()
                .bg(palette::DIFF_ADD_BG)
                .fg(palette::SUCCESS),
            ctx_lineno: Style::default().fg(palette::TEXT_FAINT),
            ctx_body: Style::default().fg(palette::TEXT_MUTED),
        }
    }
}

/// Render one hunk's rows, advancing a caller-owned `shown` counter against
/// `max_diff` so a multi-edit's hunks can share a single budget. Line numbers
/// follow the unified-diff convention: context and added lines carry the
/// new-file number, removed lines the old-file number, so the gutter stays
/// honest as the two sides diverge.
pub(super) fn push_diff_rows<'a>(
    out: &mut Vec<Line<'a>>,
    rows: &[DiffRow<'_>],
    start_line: usize,
    max_diff: usize,
    shown: &mut usize,
    styles: &DiffStyles,
    avail: usize,
) {
    let mut old_n = start_line;
    let mut new_n = start_line;
    for row in rows {
        if *shown >= max_diff {
            return;
        }
        match *row {
            DiffRow::Context(l) => {
                out.push(diff_line(
                    new_n,
                    " ",
                    l,
                    styles.ctx_lineno,
                    styles.ctx_body,
                    avail,
                ));
                old_n += 1;
                new_n += 1;
            }
            DiffRow::Del(l) => {
                out.push(diff_line(
                    old_n,
                    "-",
                    l,
                    styles.lineno_removed,
                    styles.removed_bg,
                    avail,
                ));
                old_n += 1;
            }
            DiffRow::Add(l) => {
                out.push(diff_line(
                    new_n,
                    "+",
                    l,
                    styles.lineno_added,
                    styles.added_bg,
                    avail,
                ));
                new_n += 1;
            }
        }
        *shown += 1;
    }
}

pub(super) fn locate_line_number(path: &str, needle: &str) -> Option<usize> {
    if path.is_empty() || needle.is_empty() {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let idx = content.find(needle)?;
    Some(content[..idx].matches('\n').count() + 1)
}

pub(super) fn append_numbered(
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
            Style::default().fg(palette::TEXT_MUTED),
        )));
    }
}

/// Parse the fixed `run_shell` framing (`exit_code: …\n--- stdout ---\n…\n---
/// stderr ---\n…`) positionally, so command output that happens to contain a
/// line like `exit_code: 0` or `--- stderr ---` can't spoof the code or the
/// section split. Only the first line is authoritative for the exit code, and
/// stderr is split on the first `--- stderr ---` that follows the stdout marker.
pub(super) fn parse_shell_output(text: &str) -> (i32, String, String) {
    let lines: Vec<&str> = text.lines().collect();

    // The exit code is authoritative only on the very first line.
    let code = lines
        .first()
        .and_then(|l| l.strip_prefix("exit_code: "))
        .and_then(|rest| rest.trim().parse().ok())
        .unwrap_or(0);

    // Everything up to the first `--- stdout ---` is preamble (ignored).
    let Some(stdout_start) = lines.iter().position(|l| l.starts_with("--- stdout")) else {
        return (code, String::new(), String::new());
    };
    // stderr opens at the first `--- stderr ---` AFTER the stdout marker; any
    // earlier marker-looking line is stdout content, not framing.
    let stderr_start = lines[stdout_start + 1..]
        .iter()
        .position(|l| l.starts_with("--- stderr"))
        .map(|rel| stdout_start + 1 + rel);

    let stdout_end = stderr_start.unwrap_or(lines.len());
    let stdout = lines[stdout_start + 1..stdout_end].join("\n");
    let stderr = match stderr_start {
        Some(s) => lines[s + 1..].join("\n"),
        None => String::new(),
    };
    (code, stdout, stderr)
}

pub(super) fn pretty_path(p: &str) -> String {
    shorten_home_path(Path::new(p))
}

pub(super) fn shorten_home_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        return shorten_path_with_home(path, &home);
    }
    display_path(path)
}

pub(super) fn shorten_path_with_home(path: &Path, home: &Path) -> String {
    let Ok(rest) = path.strip_prefix(home) else {
        return display_path(path);
    };
    if rest.as_os_str().is_empty() {
        "~".to_string()
    } else {
        format!("~/{}", display_path(rest))
    }
}

pub(super) fn display_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
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
pub(super) fn sanitize_display(s: &str) -> Cow<'_, str> {
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

pub(super) fn compact_args(s: &str) -> String {
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

pub(super) fn input_height(app: &App) -> u16 {
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

pub(super) fn input_visual_row_count<'a, I>(lines: I, content_w: usize) -> usize
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
mod tests {
    use super::parse_shell_output;

    #[test]
    fn parse_shell_output_parses_well_formed_framing() {
        let raw = "exit_code: 0\n--- stdout ---\nhello\n--- stderr ---\n";
        let (code, stdout, stderr) = parse_shell_output(raw);
        assert_eq!((code, stdout.as_str(), stderr.as_str()), (0, "hello", ""));
    }

    #[test]
    fn parse_shell_output_ignores_spoofed_framing_in_content() {
        // Output lines that look like framing — a second `exit_code:`, an earlier
        // `--- stderr ---` — must be treated as plain content. Only the first line
        // is authoritative for the code, and stderr splits on the first marker
        // AFTER the stdout marker.
        let raw = "exit_code: 1\n--- stdout ---\nexit_code: 0\n--- stderr ---\nreal error";
        let (code, stdout, stderr) = parse_shell_output(raw);
        assert_eq!(code, 1, "only the first line sets the exit code");
        assert_eq!(stdout, "exit_code: 0");
        assert_eq!(stderr, "real error");
    }
}

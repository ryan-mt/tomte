use super::*;

/// Very small inline markdown renderer: handles `` `code` ``, **bold**,
/// *italic*, `~~strikethrough~~`, and `[text](http…)` links. Only a *matched*
/// pair styles its span — an unmatched marker (a glob like `*.rs`, a
/// multiplication `2 * 3`, or an unterminated `` `code ``) is emitted literally
/// instead of swallowing the rest of the line. Emphasis follows CommonMark's
/// flanking rule loosely: a marker only opens when the character after it is
/// non-whitespace, and only closes when the character before it is
/// non-whitespace, so a spaced asterisk stays a plain asterisk.
pub(crate) fn render_markdown_inline(line: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let code_style = Style::default()
        .fg(palette::INLINE_CODE)
        .bg(palette::INLINE_CODE_BG);
    let bold_style = Style::default().add_modifier(Modifier::BOLD);
    let italic_style = Style::default().add_modifier(Modifier::ITALIC);
    let strike_style = Style::default()
        .fg(palette::TEXT_MUTED)
        .add_modifier(Modifier::CROSSED_OUT);
    let link_style = Style::default()
        .fg(palette::ACCENT)
        .add_modifier(Modifier::UNDERLINED);
    let plain = Style::default().fg(palette::TEXT_MUTED);
    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), plain));
        }
    };

    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // Link: `[text](url)` where the url carries a real scheme. The
            // label renders accent + underlined, the target stays visible in
            // dim parens (a terminal can't click, so hiding the url would lose
            // the one thing the reader needs). Anything else — `arr[i](x)`,
            // footnote `[1]`, a bare bracket — stays literal.
            '[' => {
                if let Some((label, url, after)) = parse_md_link(&chars, i) {
                    flush(&mut buf, &mut spans);
                    spans.push(Span::styled(label, link_style));
                    spans.push(Span::styled(format!(" ({url})"), plain));
                    i = after;
                } else {
                    buf.push('[');
                    i += 1;
                }
            }
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
            // Strikethrough: `~~x~~` where `x` is non-empty and not
            // space-flanked — same matched-pair rule as bold, so a lone `~` (a
            // home path `~/src`) or an unterminated `~~` stays literal.
            '~' if chars.get(i + 1) == Some(&'~') => {
                let start = i + 2;
                let close = chars
                    .get(start)
                    .filter(|c| !c.is_whitespace())
                    .and_then(|_| {
                        (start + 1..chars.len()).find(|&j| {
                            chars[j] == '~'
                                && chars.get(j + 1) == Some(&'~')
                                && !chars[j - 1].is_whitespace()
                        })
                    });
                if let Some(k) = close {
                    flush(&mut buf, &mut spans);
                    let strike: String = chars[start..k].iter().collect();
                    spans.push(Span::styled(strike, strike_style));
                    i = k + 2;
                } else {
                    buf.push_str("~~");
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

/// Parse a markdown link `[text](url)` starting at `chars[open]` (the `[`).
/// Returns `(label, url, index_after)` only for the safe shape: a closing `]`
/// immediately followed by `(`, a closing `)`, a non-empty label, and a url
/// with a real link scheme (`http://`, `https://`, `mailto:`). The scheme
/// requirement keeps prose like `arr[i](x)` or footnote `[1]` literal.
fn parse_md_link(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    let label_end = (open + 1..chars.len()).find(|&j| chars[j] == ']')?;
    if label_end == open + 1 || chars.get(label_end + 1) != Some(&'(') {
        return None;
    }
    let url_start = label_end + 2;
    let url_end = (url_start..chars.len()).find(|&j| chars[j] == ')')?;
    let label: String = chars[open + 1..label_end].iter().collect();
    let url: String = chars[url_start..url_end].iter().collect();
    let linky =
        url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:");
    if !linky {
        return None;
    }
    Some((label, url, url_end + 1))
}

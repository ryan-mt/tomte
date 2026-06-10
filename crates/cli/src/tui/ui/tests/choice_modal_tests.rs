use super::super::truncate_line_to_width;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

fn flat(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn truncate_line_keeps_fitting_spans_and_cuts_the_overflow() {
    let line = Line::from(vec![
        Span::styled("  Tool: ", Style::default().fg(Color::Gray)),
        Span::raw("a-very-long-tool-name"),
    ]);
    let cut = truncate_line_to_width(line, 14);
    assert_eq!(flat(&cut), "  Tool: a-ver…");
    // The first span survives with its own style.
    assert_eq!(cut.spans[0].style.fg, Some(Color::Gray));
}

#[test]
fn truncate_line_passes_short_lines_through() {
    let line = Line::from(vec![Span::raw("ok")]);
    assert_eq!(flat(&truncate_line_to_width(line, 10)), "ok");
}

#[test]
fn truncate_line_is_display_width_aware() {
    // Two CJK glyphs cost four columns; a budget of 3 fits one glyph + `…`.
    let line = Line::from(vec![Span::raw("日本")]);
    assert_eq!(flat(&truncate_line_to_width(line, 3)), "日…");
}

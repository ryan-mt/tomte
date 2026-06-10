use super::super::{
    input_visual_row_count, is_table_separator, render_assistant_md, wrap_visual_rows, CODE_BG,
};

#[test]
fn no_wrap_short_line() {
    assert_eq!(
        wrap_visual_rows("hello", 10, Some(5)),
        (vec!["hello".to_string()], Some((0, 5)))
    );
}

#[test]
fn cursor_tracked_into_second_row() {
    assert_eq!(
        wrap_visual_rows("abcdef", 3, Some(4)),
        (vec!["abc".to_string(), "def".to_string()], Some((1, 1)))
    );
}

#[test]
fn cursor_at_wrap_boundary_starts_next_row() {
    assert_eq!(
        wrap_visual_rows("abcdef", 3, Some(3)),
        (vec!["abc".to_string(), "def".to_string()], Some((1, 0)))
    );
}

#[test]
fn cursor_at_end_of_full_row_wraps() {
    assert_eq!(
        wrap_visual_rows("abc", 3, Some(3)),
        (vec!["abc".to_string()], Some((1, 0)))
    );
}

#[test]
fn empty_line_keeps_one_row() {
    assert_eq!(
        wrap_visual_rows("", 5, Some(0)),
        (vec![String::new()], Some((0, 0)))
    );
}

#[test]
fn no_cursor_off_this_line() {
    assert_eq!(
        wrap_visual_rows("abcdef", 3, None),
        (vec!["abc".to_string(), "def".to_string()], None)
    );
}

#[test]
fn wide_chars_use_two_columns() {
    assert_eq!(
        wrap_visual_rows("世界A", 4, None).0,
        vec!["世界".to_string(), "A".to_string()]
    );
}

#[test]
fn input_height_counts_soft_wrapped_rows() {
    assert_eq!(input_visual_row_count(["abcdefgh"].into_iter(), 4), 2);
}

#[test]
fn code_fence_is_highlighted_and_padded() {
    let md = "intro\n```rust\nfn main() {}\n```\nafter";
    let rows = render_assistant_md(md, 40);
    // Each code row is padded to the full content width with the bg fill.
    let code_row = &rows[1];
    let total: usize = code_row
        .iter()
        .map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    assert_eq!(total, 40);
    assert!(code_row.iter().any(|s| s.style.bg == Some(CODE_BG)));
    // Real Rust highlighting (not the plain-text fallback) yields more than
    // one distinct foreground colour — guards the language-alias mapping.
    let colors: std::collections::HashSet<_> = code_row.iter().filter_map(|s| s.style.fg).collect();
    assert!(
        colors.len() >= 2,
        "expected syntax highlighting, got {colors:?}"
    );
}

#[test]
fn table_renders_box_borders() {
    let md = "| A | B |\n|---|---|\n| 1 | 2 |";
    let rows = render_assistant_md(md, 40);
    let first: String = rows[0].iter().map(|s| s.content.as_ref()).collect();
    assert!(first.starts_with('┌') && first.ends_with('┐'));
    // top rule, header, divider, one body row, bottom rule.
    assert_eq!(rows.len(), 5);
}

#[test]
fn is_table_separator_detects_rows() {
    assert!(is_table_separator("|---|:--:|"));
    assert!(is_table_separator(" --- | --- "));
    assert!(!is_table_separator("| a | b |"));
    assert!(!is_table_separator("plain text"));
}

#[test]
fn markdown_blocks_never_panic_on_narrow_widths() {
    let md = "| col one | col two | col three |\n|---|---|---|\n| `x` | very long value here | z |\n\n```python\ndef f(x):\n    return x*x\n```";
    for w in [0usize, 1, 3, 5, 12, 80] {
        let _ = render_assistant_md(md, w);
    }
}

#[test]
fn table_with_ragged_columns_does_not_panic() {
    // Body rows with more and fewer cells than the header: `ncols` widens to
    // the max, short rows fall back to empty cells. Guards the `ncols.max(1)`
    // sizing and the `cells.get(c)` access in render_md_table against a
    // malformed table from model output.
    let md = "| A | B |\n|---|---|\n| 1 | 2 | 3 | 4 |\n| only-one |";
    let rows = render_assistant_md(md, 60);
    // top rule + header + divider + 2 body rows + bottom rule.
    assert_eq!(rows.len(), 6);
    let first: String = rows[0].iter().map(|s| s.content.as_ref()).collect();
    assert!(first.starts_with('┌') && first.ends_with('┐'));
}

#[test]
fn table_rows_share_one_display_width_so_borders_align() {
    // Every rendered table row (rules + content) must have the same display
    // width or the box borders visibly misalign. Stress the column-shrink +
    // cell-wrap path with wide CJK glyphs and a long unbreakable word at
    // several narrow widths — a regression guard for the cell-overflow risk.
    let cases = [
        "| A | B |\n|---|---|\n| 中文字符非常长的内容示例 | y |",
        "| A | B |\n|---|---|\n| supercalifragilisticexpialidocious | 中 |",
        "| 名前 | 値 |\n|---|---|\n| とても長い日本語のセル内容 | x |",
    ];
    for md in cases {
        for w in [6usize, 8, 10, 14, 20] {
            let rows = render_assistant_md(md, w);
            let widths: Vec<usize> = rows
                .iter()
                .map(|row| {
                    let s: String = row.iter().map(|sp| sp.content.as_ref()).collect();
                    unicode_width::UnicodeWidthStr::width(s.as_str())
                })
                .collect();
            assert!(
                widths.windows(2).all(|pair| pair[0] == pair[1]),
                "table rows must share one display width (md={md:?}, w={w}); got {widths:?}"
            );
        }
    }
}

#[test]
fn table_with_header_only_renders_without_body() {
    // Header + separator but no body rows: `tbl` is exactly two lines, so the
    // `tbl[2..]` body slice is empty. Guards the `tbl[0]` / `tbl[2..]`
    // indexing against a header-only table.
    let md = "| H1 | H2 |\n|----|----|";
    let rows = render_assistant_md(md, 40);
    // top rule + header + divider + bottom rule (no body).
    assert_eq!(rows.len(), 4);
    let first: String = rows[0].iter().map(|s| s.content.as_ref()).collect();
    assert!(first.starts_with('┌') && first.ends_with('┐'));
}

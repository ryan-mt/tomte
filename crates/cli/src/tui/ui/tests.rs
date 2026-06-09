//! Rendering tests, split out of `ui`.

#[cfg(test)]
mod todo_panel_tests {
    use super::super::{
        hidden_todos_summary, todo_label, todos_height_for_count, truncate_chars,
        visible_todo_indices, TODO_VISIBLE_ROWS,
    };
    use std::collections::HashSet;
    use tomte_core::tools::{TodoItem, TodoStatus};

    fn item(content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            content: content.to_string(),
            status,
            active_form: format!("Doing {content}"),
            id: None,
            blocked_by: Vec::new(),
        }
    }

    #[test]
    fn todo_panel_height_caps_and_reserves_overflow_row() {
        assert_eq!(todos_height_for_count(0), 0);
        assert_eq!(todos_height_for_count(1), 2);
        assert_eq!(todos_height_for_count(TODO_VISIBLE_ROWS), 7);
        assert_eq!(todos_height_for_count(TODO_VISIBLE_ROWS + 2), 8);
    }

    #[test]
    fn truncated_todos_prioritize_active_and_pending_items() {
        let todos = vec![
            item("completed one", TodoStatus::Completed),
            item("pending one", TodoStatus::Pending),
            item("completed two", TodoStatus::Completed),
            item("active one", TodoStatus::InProgress),
            item("pending two", TodoStatus::Pending),
            item("completed three", TodoStatus::Completed),
            item("pending three", TodoStatus::Pending),
            item("completed four", TodoStatus::Completed),
        ];

        let visible = visible_todo_indices(&todos, &HashSet::new());

        assert_eq!(visible, vec![3, 1, 4, 6, 0, 2]);
        assert_eq!(
            hidden_todos_summary(&todos, &visible),
            Some("… +2 completed".to_string())
        );
    }

    #[test]
    fn truncated_todos_prioritize_recently_completed_items() {
        let todos = vec![
            item("pending one", TodoStatus::Pending),
            item("pending two", TodoStatus::Pending),
            item("active one", TodoStatus::InProgress),
            item("pending three", TodoStatus::Pending),
            item("pending four", TodoStatus::Pending),
            item("completed old", TodoStatus::Completed),
            item("completed recent", TodoStatus::Completed),
            item("pending five", TodoStatus::Pending),
        ];
        let recent_completed = HashSet::from([6usize]);

        let visible = visible_todo_indices(&todos, &recent_completed);

        assert_eq!(visible, vec![6, 2, 0, 1, 3, 4]);
        assert_eq!(
            hidden_todos_summary(&todos, &visible),
            Some("… +1 pending, 1 completed".to_string())
        );
    }

    #[test]
    fn truncated_recent_completed_todos_are_deterministic() {
        let todos = (0..TODO_VISIBLE_ROWS + 2)
            .map(|i| item(&format!("completed {i}"), TodoStatus::Completed))
            .collect::<Vec<_>>();
        let recent_completed = HashSet::from([5usize, 2usize, 4usize, 1usize, 3usize, 0usize]);

        let visible = visible_todo_indices(&todos, &recent_completed);

        assert_eq!(visible, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn todo_label_uses_active_form_only_for_active_item() {
        let active = item("write tests", TodoStatus::InProgress);
        let done = item("read code", TodoStatus::Completed);

        assert_eq!(todo_label(&active), "Doing write tests");
        assert_eq!(todo_label(&done), "read code");
    }

    #[test]
    fn truncation_handles_narrow_width_without_splitting_utf8() {
        assert_eq!(truncate_chars("abcdef", 0), "");
        assert_eq!(truncate_chars("éclair", 2), "é…");
    }
}

#[cfg(test)]
mod todo_tool_render_tests {
    use super::super::friendly_body;
    use serde_json::json;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn todo_write_body_accepts_claude_code_active_form_spelling() {
        let lines = friendly_body(
            "todo_write",
            &json!({
                "todos": [
                    {
                        "content": "Run tests",
                        "activeForm": "Running tests",
                        "status": "in_progress"
                    }
                ]
            }),
            Some("stored"),
            false,
            80,
            false,
        );

        assert!(text(&lines).contains("Running tests"));
    }

    #[test]
    fn todo_write_body_uses_panel_glyphs_with_distinct_in_progress() {
        // The inline checklist must use the same glyph set as the pinned todo
        // panel (✓ done, ▪ in-progress, □ pending) and give the in-progress item
        // its own filled ▪ — not the hollow box it used to share with pending,
        // where the two were told apart by colour alone. Guards against a
        // regression to the old ☒/☐ set.
        let lines = friendly_body(
            "todo_write",
            &json!({
                "todos": [
                    {"content": "Done one", "activeForm": "Doing one", "status": "completed"},
                    {"content": "Active one", "activeForm": "Doing two", "status": "in_progress"},
                    {"content": "Pending one", "activeForm": "Doing three", "status": "pending"},
                ]
            }),
            Some("stored"),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(rendered.contains('✓'), "completed glyph: {rendered}");
        assert!(rendered.contains('▪'), "in-progress glyph: {rendered}");
        assert!(rendered.contains('□'), "pending glyph: {rendered}");
        assert!(
            !rendered.contains('☐') && !rendered.contains('☒'),
            "must not use the old checkbox glyphs: {rendered}"
        );
    }
}

#[cfg(test)]
mod shell_tool_render_tests {
    use super::super::friendly_body;
    use serde_json::json;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn shell_output(code: i32, stdout: &str, stderr: &str) -> String {
        format!("exit_code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")
    }

    #[test]
    fn failed_command_shows_red_stderr_and_error_footer_no_box() {
        let out = shell_output(101, "", "error: no such command: audit");
        // A non-zero exit is NOT a tool error — run_shell returns Ok with the
        // exit code embedded, so `error` is false and the run_shell formatter runs.
        let lines = friendly_body(
            "run_shell",
            &json!({"command": "cargo audit"}),
            Some(&out),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("error: no such command: audit"),
            "got: {rendered}"
        );
        assert!(rendered.contains("Error (exit 101)"), "got: {rendered}");
        // No yellow "─ stderr ─" separator box.
        assert!(!rendered.contains("─ stderr ─"), "got: {rendered}");
    }

    #[test]
    fn successful_command_has_no_exit_footer() {
        let out = shell_output(0, "all good", "");
        let lines = friendly_body(
            "run_shell",
            &json!({"command": "echo hi"}),
            Some(&out),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(rendered.contains("all good"), "got: {rendered}");
        assert!(
            !rendered.contains("exit"),
            "success must not show an exit line: {rendered}"
        );
        assert!(!rendered.contains("Error"), "got: {rendered}");
    }

    #[test]
    fn failed_command_shows_more_than_the_success_preview() {
        // 20 stdout lines: the collapsed failure budget (15) shows far more than
        // the 3-line success preview, still bounded with a "more" hint.
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let out = shell_output(1, body.trim_end(), "");
        let lines = friendly_body(
            "run_shell",
            &json!({"command": "cargo fmt --check"}),
            Some(&out),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("line 15"),
            "should show ~15 lines on failure: {rendered}"
        );
        assert!(
            !rendered.contains("line 16"),
            "should cap at the failure budget: {rendered}"
        );
        assert!(rendered.contains("+5 more line"), "got: {rendered}");
    }
}

#[cfg(test)]
mod status_footer_tests {
    use super::super::{context_gauge, status_left_text_for_parts};
    use crate::tui::palette;

    #[test]
    fn context_gauge_hidden_before_any_usage() {
        assert!(context_gauge(0, 1_000_000).is_none());
    }

    #[test]
    fn context_gauge_ramps_calm_warning_danger() {
        assert_eq!(
            context_gauge(500_000, 1_000_000).unwrap(),
            ("50% ctx".to_string(), palette::TEXT_MUTED)
        );
        assert_eq!(
            context_gauge(700_000, 1_000_000).unwrap(),
            ("70% ctx".to_string(), palette::WARNING)
        );
        assert_eq!(
            context_gauge(900_000, 1_000_000).unwrap(),
            ("90% ctx".to_string(), palette::DANGER)
        );
    }

    #[test]
    fn context_gauge_caps_at_100_and_survives_zero_limit() {
        assert_eq!(context_gauge(2_000_000, 1_000_000).unwrap().0, "100% ctx");
        assert_eq!(context_gauge(5, 0).unwrap().0, "100% ctx");
    }

    #[test]
    fn includes_goal_elapsed_when_goal_is_active() {
        assert_eq!(
            status_left_text_for_parts("default", "", false, Some("1m32")),
            "default  ·  goal 1m32  ·  shift+tab cycles mode · ? for shortcuts"
        );
    }

    #[test]
    fn keeps_status_activity_after_goal_elapsed() {
        assert_eq!(
            status_left_text_for_parts("plan", "(continuing active goal...)", false, Some("12s")),
            "plan  ·  goal 12s  ·  (continuing active goal...)"
        );
    }
}

#[cfg(test)]
mod path_display_tests {
    use super::super::shorten_path_with_home;
    use std::path::Path;

    #[test]
    fn shortens_home_and_children_only_on_path_boundaries() {
        let home = Path::new("/home/ryan");

        assert_eq!(shorten_path_with_home(Path::new("/home/ryan"), home), "~");
        assert_eq!(
            shorten_path_with_home(Path::new("/home/ryan/project"), home),
            "~/project"
        );
        assert_eq!(
            shorten_path_with_home(Path::new("/home/ryan2/project"), home),
            "/home/ryan2/project"
        );
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::super::sanitize_display;

    #[test]
    fn strips_ansi_color_and_reset_sequences() {
        // Colorized cargo/rustc output: SGR color + the `\x1b(B\x1b[m` reset that
        // leaked as stray `(B` / `m` fragments and desynced the terminal.
        let input = "\x1b[1m\x1b[31merror\x1b[0m\x1b(B\x1b[m: boom";
        assert_eq!(sanitize_display(input), "error: boom");
    }

    #[test]
    fn strips_osc_and_drops_cr() {
        // OSC title sequence (ESC ] ... BEL) plus a CRLF carriage return.
        let input = "\x1b]0;title\x07line\r";
        assert_eq!(sanitize_display(input), "line");
    }

    #[test]
    fn expands_tabs_to_tab_stops() {
        assert_eq!(sanitize_display("a\tb"), "a   b"); // col 1 -> next stop at 4
        assert_eq!(sanitize_display("\tx"), "    x"); // col 0 -> stop at 4
    }

    #[test]
    fn preserves_newlines_and_resets_tab_column() {
        assert_eq!(sanitize_display("a\tb\n\tc"), "a   b\n    c");
    }

    #[test]
    fn clean_text_borrows_without_allocating() {
        assert!(matches!(
            sanitize_display("plain ascii"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strips_8bit_c1_control_introducers() {
        // Pure-C1 controls (U+0080..=U+009F) carry no 7-bit ESC, so a byte-level
        // fast path let them through; a terminal honoring 8-bit controls reads
        // U+009B/U+009D as CSI/OSC. They must be dropped, like the headless path.
        let input = "\u{9b}2Jwiped \u{9d}52;c;clip\u{9c} ok";
        let out = sanitize_display(input);
        for c in ['\u{9b}', '\u{9d}', '\u{9c}', '\u{80}'] {
            assert!(!out.contains(c), "C1 control {c:?} survived: {out:?}");
        }
        // Payload demoted to plain text, surrounding text intact.
        assert!(out.contains("wiped") && out.contains("ok"), "{out:?}");
    }
}

#[cfg(test)]
mod input_wrap_tests {
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
        let colors: std::collections::HashSet<_> =
            code_row.iter().filter_map(|s| s.style.fg).collect();
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
}

#[cfg(test)]
mod markdown_inline_tests {
    use super::super::render_markdown_inline;
    use ratatui::style::Modifier;

    fn joined(line: &str) -> String {
        render_markdown_inline(line)
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    fn has_modifier(line: &str, m: Modifier) -> bool {
        render_markdown_inline(line)
            .iter()
            .any(|s| s.style.add_modifier.contains(m))
    }

    #[test]
    fn matched_markers_style_and_strip() {
        // A real pair styles its content and drops the markers.
        assert_eq!(joined("a *word* b"), "a word b");
        assert!(has_modifier("a *word* b", Modifier::ITALIC));
        assert_eq!(joined("a **strong** b"), "a strong b");
        assert!(has_modifier("a **strong** b", Modifier::BOLD));
        assert_eq!(joined("see `path/to/x` ok"), "see path/to/x ok");
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        // The shipped bug: an unterminated marker swallowed the rest of the line.
        // Now the marker is emitted verbatim and nothing is styled.
        for s in [
            "search *.rs files",
            "match **/*.ts here",
            "use 2 * 3 in code",
            "an unterminated `code span",
            "**bold never closed",
            "*italic never closed",
        ] {
            assert_eq!(joined(s), s, "literal text must be preserved for {s:?}");
            assert!(
                !has_modifier(s, Modifier::ITALIC) && !has_modifier(s, Modifier::BOLD),
                "no emphasis should apply to {s:?}"
            );
        }
    }

    #[test]
    fn space_flanked_asterisks_are_not_emphasis() {
        // `2 * 3 * 4` has matched asterisks but both are space-flanked, so the
        // flanking rule keeps them literal rather than italicizing " 3 ".
        assert_eq!(joined("2 * 3 * 4"), "2 * 3 * 4");
        assert!(!has_modifier("2 * 3 * 4", Modifier::ITALIC));
    }

    #[test]
    fn emphasis_survives_inner_lone_asterisk() {
        // Bold content may contain a stray `*`; the outer pair still matches.
        assert_eq!(joined("**a*b** tail"), "a*b tail");
        assert!(has_modifier("**a*b** tail", Modifier::BOLD));
    }

    #[test]
    fn http_link_styles_label_and_keeps_target_visible() {
        // `[text](http…)` renders as an underlined label with the url in dim
        // parens — a terminal can't click, so the target must stay readable.
        assert_eq!(
            joined("see [the docs](https://example.com/a) now"),
            "see the docs (https://example.com/a) now"
        );
        assert!(has_modifier(
            "see [the docs](https://example.com/a) now",
            Modifier::UNDERLINED
        ));
    }

    #[test]
    fn strikethrough_styles_matched_pairs_and_keeps_lone_tildes() {
        assert_eq!(joined("a ~~gone~~ b"), "a gone b");
        assert!(has_modifier("a ~~gone~~ b", Modifier::CROSSED_OUT));
        // A home path and an unterminated marker stay literal.
        for s in ["see ~/src/main.rs", "~~never closed", "a ~ b ~ c"] {
            assert_eq!(joined(s), s);
            assert!(!has_modifier(s, Modifier::CROSSED_OUT));
        }
    }

    #[test]
    fn bracket_pairs_without_a_link_scheme_stay_literal() {
        // Indexing, footnotes, and scheme-less targets are prose, not links.
        for s in [
            "use arr[i](x) here",
            "see note [1] below",
            "open [rel](./local/path) maybe",
            "broken [label](https://no-close",
            "empty [](https://example.com)",
        ] {
            assert_eq!(joined(s), s, "literal text must be preserved for {s:?}");
            assert!(!has_modifier(s, Modifier::UNDERLINED));
        }
    }
}

#[cfg(test)]
mod choice_modal_tests {
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
}

#[cfg(test)]
mod markdown_block_tests {
    use super::super::render_assistant_md;

    fn rows(md: &str, w: usize) -> Vec<String> {
        render_assistant_md(md, w)
            .iter()
            .map(|r| r.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn heading_strips_hashes() {
        let r = rows("## Setup steps", 40);
        assert_eq!(r, vec!["Setup steps".to_string()]);
    }

    #[test]
    fn hash_without_space_is_not_a_heading() {
        // `#define`, `#!shebang`, `#1` issue refs must render verbatim.
        assert_eq!(rows("#define FOO 1", 40), vec!["#define FOO 1".to_string()]);
    }

    #[test]
    fn bullet_normalizes_to_dot_glyph() {
        assert_eq!(rows("- first item", 40), vec!["• first item".to_string()]);
        assert_eq!(
            rows("* starred item", 40),
            vec!["• starred item".to_string()]
        );
    }

    #[test]
    fn ordered_item_keeps_its_number() {
        let r = rows("1. first\n2. second", 40);
        assert_eq!(r, vec!["1. first".to_string(), "2. second".to_string()]);
    }

    #[test]
    fn thematic_break_renders_as_a_rule_not_dashes() {
        // `---`, `***`, and a spaced `- - -` each become one faint rule row.
        for hr in ["---", "***", "- - -", "_____"] {
            let r = rows(hr, 20);
            assert_eq!(r.len(), 1, "{hr:?} must be one row");
            assert!(r[0].chars().all(|c| c == '─'), "{hr:?} → {:?}", r[0]);
        }
        // Two dashes and mixed markers are prose, not rules.
        assert_eq!(rows("--", 20), vec!["--".to_string()]);
        assert_eq!(rows("-*-", 20), vec!["-*-".to_string()]);
    }

    #[test]
    fn task_list_items_render_checkbox_glyphs() {
        assert_eq!(rows("- [ ] write tests", 40), vec!["☐ write tests"]);
        assert_eq!(rows("- [x] ship it", 40), vec!["✓ ship it"]);
        assert_eq!(rows("- [X] ship it", 40), vec!["✓ ship it"]);
        // A bracket body that is not a checkbox keeps the plain bullet.
        assert_eq!(rows("- [WIP] thing", 40), vec!["• [WIP] thing"]);
    }

    #[test]
    fn blockquote_gets_a_bar_prefix() {
        assert_eq!(
            rows("> a quoted note", 40),
            vec!["│ a quoted note".to_string()]
        );
    }

    #[test]
    fn wrapped_list_item_hangs_under_its_text() {
        // A narrow width forces a wrap; the continuation row must indent to align
        // under the text, not restart at the bullet.
        let r = rows("- alpha beta gamma delta", 12);
        assert!(r.len() >= 2, "should wrap: {r:?}");
        assert!(r[0].starts_with("• "), "{r:?}");
        assert!(
            r[1].starts_with("  ") && !r[1].trim_start().starts_with('•'),
            "continuation must hang under the text: {r:?}"
        );
    }

    #[test]
    fn plain_paragraph_is_unchanged() {
        assert_eq!(
            rows("just a sentence", 40),
            vec!["just a sentence".to_string()]
        );
    }
}

#[cfg(test)]
mod preflight_render_tests {
    use super::super::render_tool;
    use crate::tui::app::PreFlight;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn pre_flight_card_renders_the_scope_marker() {
        let mut lines = Vec::new();
        let pf = PreFlight {
            scope: "writes 1 file · nothing else moves".to_string(),
            leash: None,
            house_rules: Vec::new(),
        };
        render_tool(
            &mut lines,
            "edit_file",
            "{\"path\":\"src/parser.rs\"}",
            None,
            false,
            Some(&pf),
            80,
            false,
        );
        let rendered = text(&lines);
        // The glass-box marker + scope appear, attached to the action.
        assert!(rendered.contains('▸'), "got: {rendered}");
        assert!(
            rendered.contains("writes 1 file · nothing else moves"),
            "got: {rendered}"
        );
    }

    #[test]
    fn a_flagged_call_also_renders_its_leash() {
        let mut lines = Vec::new();
        let pf = PreFlight {
            scope: "runs a shell command · may change your tree".to_string(),
            leash: Some("rm -rf on a critical path".to_string()),
            house_rules: Vec::new(),
        };
        render_tool(
            &mut lines,
            "run_shell",
            "{\"command\":\"rm -rf /etc\"}",
            None,
            false,
            Some(&pf),
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(rendered.contains('⚠'), "got: {rendered}");
        assert!(
            rendered.contains("rm -rf on a critical path"),
            "got: {rendered}"
        );
    }

    #[test]
    fn no_card_when_preflight_is_absent() {
        let mut lines = Vec::new();
        render_tool(
            &mut lines,
            "read_file",
            "{\"path\":\"src/a.rs\"}",
            Some("1 line"),
            false,
            None,
            80,
            false,
        );
        assert!(!text(&lines).contains('▸'), "a read has no pre-flight card");
    }

    #[test]
    fn house_rules_for_the_file_render_under_the_card() {
        let mut lines = Vec::new();
        let pf = PreFlight {
            scope: "writes 1 file · nothing else moves".to_string(),
            leash: None,
            house_rules: vec![
                "reject bcrypt, use argon2 — memory-hard (gpt-5.5)".to_string(),
                "+2 more · tomte why src/auth.rs".to_string(),
            ],
        };
        render_tool(
            &mut lines,
            "edit_file",
            "{\"path\":\"src/auth.rs\"}",
            None,
            false,
            Some(&pf),
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("house rules for this file"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains("reject bcrypt, use argon2"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains("+2 more · tomte why src/auth.rs"),
            "got: {rendered}"
        );
    }
}

#[cfg(test)]
mod diff_hunk_tests {
    use super::super::{diff_hunk, DiffRow};

    fn tag(r: &DiffRow<'_>) -> (char, String) {
        match *r {
            DiffRow::Context(l) => (' ', l.to_string()),
            DiffRow::Del(l) => ('-', l.to_string()),
            DiffRow::Add(l) => ('+', l.to_string()),
        }
    }

    #[test]
    fn shared_anchor_lines_collapse_to_context() {
        // One changed line inside a 3-line block: the unchanged first and last
        // lines become context, not a removed+added echo of the whole block.
        let old = "fn f() {\n    let x = 1;\n}";
        let new = "fn f() {\n    let x = 2;\n}";
        let rows: Vec<_> = diff_hunk(old, new).iter().map(tag).collect();
        assert_eq!(
            rows,
            vec![
                (' ', "fn f() {".to_string()),
                ('-', "    let x = 1;".to_string()),
                ('+', "    let x = 2;".to_string()),
                (' ', "}".to_string()),
            ]
        );
    }

    #[test]
    fn pure_insertion_and_deletion_have_no_phantom_context() {
        let add: Vec<_> = diff_hunk("", "new line").iter().map(tag).collect();
        assert_eq!(add, vec![('+', "new line".to_string())]);
        let del: Vec<_> = diff_hunk("gone", "").iter().map(tag).collect();
        assert_eq!(del, vec![('-', "gone".to_string())]);
    }

    #[test]
    fn fully_distinct_blocks_keep_every_line() {
        // No shared anchors: all old lines removed, all new lines added, in order.
        let rows: Vec<_> = diff_hunk("alpha\nbeta", "gamma\ndelta")
            .iter()
            .map(tag)
            .collect();
        assert_eq!(
            rows,
            vec![
                ('-', "alpha".to_string()),
                ('-', "beta".to_string()),
                ('+', "gamma".to_string()),
                ('+', "delta".to_string()),
            ]
        );
    }
}

#[cfg(test)]
mod edit_diff_render_tests {
    use super::super::friendly_body;
    use serde_json::json;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn edit_diff_keeps_unchanged_lines_as_context_and_counts_only_changes() {
        let lines = friendly_body(
            "edit_file",
            &json!({
                // A path that does not exist, so locate_line_number falls back to
                // line 1 deterministically and the test never touches the disk.
                "path": "this_file_does_not_exist_in_tests.rs",
                "old_string": "fn f() {\n    let x = 1;\n}",
                "new_string": "fn f() {\n    let x = 2;\n}",
            }),
            Some("ok"),
            false,
            80,
            true, // expanded: nothing is truncated
        );
        let rendered = text(&lines);

        // Summary counts only the single changed line, not the whole 3-line block.
        assert!(
            rendered.contains("Added 1 line, removed 1 line"),
            "got: {rendered}"
        );
        // The unchanged anchor lines appear exactly once — as context, not echoed
        // as both removed and added (the old block-diff showed them twice).
        assert_eq!(
            rendered.matches("fn f() {").count(),
            1,
            "context line must not be duplicated: {rendered}"
        );
        // The real change is the only -/+ pair.
        assert_eq!(rendered.matches("let x = 1;").count(), 1, "got: {rendered}");
        assert_eq!(rendered.matches("let x = 2;").count(), 1, "got: {rendered}");
    }

    #[test]
    fn edit_diff_truncation_offers_ctrl_o_hint() {
        // A diff longer than the compact 8-row budget must end with the shared
        // "(Ctrl+O for more)" hint, like the shell/grep/list previews — so every
        // truncated tool body offers the same way to see the rest (the diff and
        // error paths used to omit it).
        let old: String = (1..=10).map(|i| format!("old line {i}\n")).collect();
        let new: String = (1..=10).map(|i| format!("new line {i}\n")).collect();
        let lines = friendly_body(
            "edit_file",
            &json!({
                "path": "this_file_does_not_exist_in_tests.rs",
                "old_string": old,
                "new_string": new,
            }),
            Some("ok"),
            false,
            80,
            false, // compact: the 8-row budget truncates the 20-row diff
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("(Ctrl+O for more)"),
            "truncated diff must offer the expand hint: {rendered}"
        );
    }
}

#[cfg(test)]
mod record_decision_render_tests {
    use super::super::{friendly_body, friendly_header};
    use serde_json::json;

    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn header_shows_remember_and_the_decision() {
        let (head, summary) = friendly_header(
            "record_decision",
            &json!({"decision": "use argon2 for hashing"}),
        );
        assert_eq!(head, "Remember");
        assert!(summary.contains("use argon2"), "{summary}");
    }

    #[test]
    fn body_surfaces_the_why_and_rejected_not_the_raw_output() {
        // The moat is the *why*; it must be visible the instant the decision is
        // recorded, not buried in a silent tool call.
        let lines = friendly_body(
            "record_decision",
            &json!({
                "decision": "use argon2 for hashing",
                "why": "memory-hard, resists GPU cracking",
                "rejected": ["bcrypt -> weaker against GPUs"]
            }),
            Some("Recorded decision at src/auth.rs:10 (model: gpt-5.5)."),
            false,
            80,
            false,
        );
        let rendered = text(&lines);
        assert!(
            rendered.contains("memory-hard"),
            "why must show: {rendered}"
        );
        assert!(
            rendered.contains("rejected bcrypt"),
            "rejected alternative must show: {rendered}"
        );
    }
}

mod thinking_render_tests {
    use super::super::{push_assistant_lines, Block};
    use ratatui::text::Line;

    fn assistant(reasoning: &str, text: &str) -> Block {
        Block::Assistant {
            text: text.to_string(),
            reasoning: reasoning.to_string(),
            done: false,
            thought_for_secs: None,
            reasoning_started_at: None,
        }
    }

    fn flatten(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn live_reasoning_shows_when_thinking_on() {
        let block = assistant("weighing the two options", "the final answer");
        let mut lines = Vec::new();
        push_assistant_lines(&mut lines, &block, 60, true);
        let out = flatten(&lines);
        assert!(
            out.contains("weighing the two options"),
            "thinking shown: {out:?}"
        );
        assert!(out.contains("the final answer"), "answer shown: {out:?}");
    }

    #[test]
    fn live_reasoning_hidden_when_thinking_off() {
        let block = assistant("weighing the two options", "the final answer");
        let mut lines = Vec::new();
        push_assistant_lines(&mut lines, &block, 60, false);
        let out = flatten(&lines);
        assert!(
            !out.contains("weighing the two options"),
            "thinking hidden: {out:?}"
        );
        assert!(
            out.contains("the final answer"),
            "answer still shown: {out:?}"
        );
    }
}

#[cfg(test)]
mod chat_cache_tests {
    use super::super::render_chat;
    use crate::tui::app::{App, Block};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn assistant(text: &str, done: bool) -> Block {
        Block::Assistant {
            text: text.to_string(),
            reasoning: String::new(),
            done,
            thought_for_secs: None,
            reasoning_started_at: None,
        }
    }

    fn read_tool(id: &str, path: &str, output: Option<&str>) -> Block {
        Block::Tool {
            call_id: id.to_string(),
            name: "read_file".to_string(),
            args: format!("{{\"path\":\"{path}\"}}"),
            output: output.map(str::to_string),
            error: false,
            preflight: None,
        }
    }

    fn draw(app: &mut App, width: u16) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(width, 16)).unwrap();
        t.draw(|f| {
            let area = f.area();
            render_chat(f, area, app);
        })
        .unwrap();
        t.backend().buffer().clone()
    }

    /// Render once through the cache as it evolved across the scripted steps,
    /// once from a cold cache, and demand identical frames. This is the
    /// correctness contract that replaced per-event invalidation: whatever the
    /// stable-prefix cache reuses must be indistinguishable from a fresh build.
    fn assert_step(app: &mut App, width: u16, label: &str) {
        let evolved = draw(app, width);
        let evolved_cache = app.chat_render_cache.take();
        let fresh = draw(app, width);
        assert_eq!(
            evolved, fresh,
            "cache-evolved frame diverged from fresh build at step: {label}"
        );
        app.chat_render_cache = evolved_cache;
    }

    #[test]
    fn cache_evolved_rendering_matches_fresh_build_across_turns() {
        let mut app = App::new();
        assert_step(&mut app, 60, "welcome, idle");

        // A turn starts: prompt + open assistant block, busy.
        app.blocks.push(Block::User("please do the thing".into()));
        app.blocks.push(assistant("", false));
        app.busy = true;
        assert_step(&mut app, 60, "turn start");

        // Streaming deltas grow the open assistant block.
        for chunk in ["Working on", " it — reading", " the files now."] {
            if let Some(Block::Assistant { text, .. }) = app.blocks.last_mut() {
                text.push_str(chunk);
            }
            assert_step(&mut app, 60, "stream delta");
        }

        // Tool lifecycle inside the live tail: started → args → a sibling that
        // groups with it → result landing on a NON-last block.
        app.blocks.push(read_tool("c1", "src/main.rs", None));
        assert_step(&mut app, 60, "tool started");
        app.blocks.push(read_tool("c2", "src/lib.rs", None));
        assert_step(&mut app, 60, "grouped second read");
        if let Some(Block::Tool { output, .. }) = app
            .blocks
            .iter_mut()
            .find(|b| matches!(b, Block::Tool { call_id, .. } if call_id == "c1"))
        {
            *output = Some("fn main() {}".into());
        }
        assert_step(&mut app, 60, "result on non-last block");

        // The turn settles: boundary moves to the end (append path).
        if let Some(Block::Assistant { done, .. }) = app
            .blocks
            .iter_mut()
            .find(|b| matches!(b, Block::Assistant { .. }))
        {
            *done = true;
        }
        app.busy = false;
        assert_step(&mut app, 60, "turn complete");

        // A second turn exercises append-after-idle.
        app.blocks.push(Block::User("and again".into()));
        app.blocks.push(assistant("Sure — done.", false));
        app.busy = true;
        assert_step(&mut app, 60, "second turn streaming");
        app.busy = false;
        assert_step(&mut app, 60, "second turn settled");

        // A width change must rebuild for the new wrap.
        assert_step(&mut app, 44, "narrower terminal");

        // A wholesale replacement (the `/clear` shape) must drop the cache.
        app.blocks.clear();
        app.blocks.push(Block::Welcome);
        assert_step(&mut app, 44, "cleared transcript");
    }
}

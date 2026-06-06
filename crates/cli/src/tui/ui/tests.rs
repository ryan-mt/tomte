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
        // Claude Code style: no yellow "─ stderr ─" separator box.
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
}

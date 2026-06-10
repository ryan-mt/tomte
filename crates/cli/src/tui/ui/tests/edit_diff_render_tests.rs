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

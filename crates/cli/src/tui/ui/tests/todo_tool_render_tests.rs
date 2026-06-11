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
    // panel (✓ done, ◆ in-progress, ◇ pending) and give the in-progress item
    // its own filled ◆ — not the hollow diamond it would share with pending,
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
    assert!(rendered.contains('◆'), "in-progress glyph: {rendered}");
    assert!(rendered.contains('◇'), "pending glyph: {rendered}");
    assert!(
        !rendered.contains('☐') && !rendered.contains('☒'),
        "must not use the old checkbox glyphs: {rendered}"
    );
}

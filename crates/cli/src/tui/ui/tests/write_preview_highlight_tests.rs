
use super::super::friendly_body;
use ratatui::style::Color;
use serde_json::json;

// The Write preview must run through syntect like the assistant's fenced
// code blocks — rust content yields more than one distinct RGB foreground
// across the code spans. Guards the "wall of one-color text" regression.
#[test]
fn write_body_is_syntax_highlighted_from_path_extension() {
    let lines = friendly_body(
        "write_file",
        &json!({
            "path": "src/main.rs",
            "content": "fn main() {\n    let greeting = \"hi\";\n}\n"
        }),
        Some("ok"),
        false,
        80,
        false,
    );
    let mut rgb_fgs = std::collections::HashSet::new();
    for line in &lines {
        for span in &line.spans {
            if let Some(Color::Rgb(r, g, b)) = span.style.fg {
                rgb_fgs.insert((r, g, b));
            }
        }
    }
    assert!(
        rgb_fgs.len() >= 2,
        "expected ≥2 distinct syntect colors, got {rgb_fgs:?}"
    );
}

// A path without a recognizable extension degrades to the plain-text
// syntax: no panic, and the numbered gutter still renders.
#[test]
fn write_body_without_extension_degrades_to_plain() {
    let lines = friendly_body(
        "write_file",
        &json!({"path": "Makefile", "content": "all:\n\techo hi\n"}),
        Some("ok"),
        false,
        80,
        false,
    );
    let joined = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(joined.contains("   1 "), "numbered gutter: {joined}");
}

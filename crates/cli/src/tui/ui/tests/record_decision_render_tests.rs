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

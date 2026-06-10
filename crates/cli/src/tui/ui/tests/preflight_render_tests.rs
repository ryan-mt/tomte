
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
        context_manifest: Vec::new(),
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
        context_manifest: Vec::new(),
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
        context_manifest: Vec::new(),
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

#[test]
fn context_manifest_renders_under_the_card() {
    let mut lines = Vec::new();
    let pf = PreFlight {
        scope: "writes 1 file · nothing else moves".to_string(),
        leash: None,
        house_rules: Vec::new(),
        context_manifest: vec![
            "pulling src/session.rs — imported by src/auth.rs · ✓ read this session".to_string(),
            "leaving out src/ui.rs — no path from the seed".to_string(),
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
        rendered.contains("context manifest for this edit"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("pulling src/session.rs — imported by src/auth.rs"),
        "got: {rendered}"
    );
    assert!(
        rendered.contains("leaving out src/ui.rs — no path from the seed"),
        "got: {rendered}"
    );
}

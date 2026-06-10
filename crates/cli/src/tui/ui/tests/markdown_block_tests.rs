
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

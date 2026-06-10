
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

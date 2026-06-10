
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

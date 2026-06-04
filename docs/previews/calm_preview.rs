//! Standalone, dependency-free preview of Pillar 4 — "the calm, tidy terminal".
//!
//! This is NOT part of tomte's build: it lives under docs/, uses only std, and is
//! compiled by hand with `rustc`, so it cannot affect the 0.0.2 binary or CI.
//! Its only job is to render the calm-terminal direction as REAL colored output
//! you can run and see, before we build it into tomte after 0.0.2.
//!
//!   rustc docs/previews/calm_preview.rs -o /tmp/calm_preview && /tmp/calm_preview
//!
//! See docs/pillar-4-calm-terminal.md and docs/SOUL.md (Pillar 4).

// ---- tiny ANSI palette (true-color), the only "framework" we need ----
fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

// Calm palette: an achromatic base + ONE muted accent (sage), plus muted semantics.
// (Disciplined palette = the single biggest "craft, not slop" signal — SOUL Pillar 4.)
fn text() -> String {
    fg(214, 214, 214)
}
fn faint() -> String {
    fg(120, 120, 120)
}
fn sage() -> String {
    fg(138, 176, 150)
} // the one accent
fn ok() -> String {
    fg(143, 188, 143)
}
fn warn() -> String {
    fg(210, 180, 120)
}
fn bad() -> String {
    fg(216, 131, 131)
}

fn rule(label: &str) {
    let dashes = "─".repeat(52usize.saturating_sub(label.chars().count()));
    println!("\n{}{}── {label} {dashes}{}", BOLD, faint(), RESET);
}

fn main() {
    println!(
        "{}{}tomte · Pillar 4 preview — \"the calm, tidy terminal\"{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}a real, runnable sketch of the Stage-1 direction (not the final build){}",
        faint(),
        RESET
    );

    // ---------- BEFORE: today's generic, alt-screen surface ----------
    rule("TODAY  (alt-screen · narration · scrollback eaten)");
    let box_c = faint();
    println!("{box_c}┌────────────────────────────────────────────────┐{RESET}");
    println!(
        "{box_c}│{RESET} {}> add a test for the parser{RESET}                     {box_c}│{RESET}",
        text()
    );
    println!(
        "{box_c}│{RESET} {}I'll start by reading the parser…{RESET}               {box_c}│{RESET}",
        faint()
    );
    println!(
        "{box_c}│{RESET} {}⠙ Reading src/parser.rs{RESET}                          {box_c}│{RESET}",
        warn()
    );
    println!(
        "{box_c}│{RESET} {}[tool] edit_file src/parser.rs{RESET}                  {box_c}│{RESET}",
        faint()
    );
    println!(
        "{box_c}│{RESET} {}…transcript scrolls in here; scroll-up jumps,{RESET}   {box_c}│{RESET}",
        faint()
    );
    println!(
        "{box_c}│{RESET} {} flickers, can't copy history…{RESET}                 {box_c}│{RESET}",
        faint()
    );
    println!("{box_c}├────────────────────────────────────────────────┤{RESET}");
    println!(
        "{box_c}│{RESET} {}> ▌{RESET}                                             {box_c}│{RESET}",
        text()
    );
    println!(
        "{box_c}│{RESET} {}gpt-5.5 · 42% ctx · main{RESET}                        {box_c}│{RESET}",
        faint()
    );
    println!("{box_c}└────────────────────────────────────────────────┘{RESET}");

    // ---------- AFTER: Pillar 4, inline + tidy hand-off ----------
    rule("PILLAR 4  (inline · history intact · tidy receipt)");
    println!(
        "   {}…earlier conversation stays in the terminal's OWN scrollback —{RESET}",
        faint()
    );
    println!(
        "   {} scroll with the mouse, copy like any command, never erased…{RESET}",
        faint()
    );
    println!();
    // the "left in order" receipt — what a finished turn leaves behind
    println!(
        "   {}✓{RESET} {}added parser test{RESET} {}·{RESET} 1 file {}·{RESET} {}cargo test 12 passed{RESET}",
        ok(),
        text(),
        faint(),
        faint(),
        ok()
    );
    println!(
        "       {}src/parser.rs:88{RESET}   {}why:{RESET} {}cover the empty-input case{RESET}",
        sage(),
        faint(),
        text()
    );
    println!();
    // the only live region: the active turn + input + a peripheral status line
    println!(
        "   {}running cargo test … 3.2s{RESET} {}· esc to interrupt{RESET}",
        faint(),
        faint()
    );
    println!("   {}>{RESET} {}▌{RESET}", sage(), text());
    println!("   {}gpt-5.5 · 42% · main{RESET}", faint());

    // a tiny diff-before-apply taste, in the calm palette
    rule("and the diff is shown before it's applied");
    println!("   {}src/parser.rs{RESET}", sage());
    println!(
        "   {} 87{RESET} {}    assert_eq!(parse(\"1+1\"), Ok(2));{RESET}",
        faint(),
        text()
    );
    println!(
        "   {} 88{RESET} {}+   assert!(parse(\"\").is_err());{RESET}",
        faint(),
        ok()
    );
    println!(
        "   {} 89{RESET} {}-   // TODO: empty input{RESET}",
        faint(),
        bad()
    );

    println!(
        "\n{}— one accent, quiet status, nothing erased. that's the custodian.{RESET}",
        faint()
    );
}

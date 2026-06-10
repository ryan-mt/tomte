//! Standalone, dependency-free preview of the calm, tidy terminal.
//!
//! This is NOT part of tomte's build: it lives under docs/, uses only std, and is
//! compiled by hand with `rustc`, so it cannot affect the 0.0.2 binary or CI.
//!
//! Unlike a static mock, it *animates the actual mechanic*: a turn runs inside a slim
//! live viewport that redraws in place (the screen never scrolls), and when it finishes
//! its receipt is committed for real — so every finished turn settles into the terminal's
//! OWN scrollback while only the active turn stays live. That is the felt difference a
//! "quiet custodian" owes you, and it is what static screenshots cannot show.
//!
//! Shipped state: as of 0.0.4 this inline viewport is the **default** renderer; the
//! full-screen alternate screen (input pinned to the bottom, in-app scroll +
//! drag-selection) stays available via `render_mode: "alt"` in config.json or
//! `TOMTE_INLINE=0`. This preview demos what the default delivers.
//! Record this for the relaunch (asciinema / vhs):
//!
//!   rustc -D warnings docs/previews/calm_preview.rs -o /tmp/calm_preview && /tmp/calm_preview

use std::io::{self, Write};
use std::thread::sleep;
use std::time::Duration;

// ---- tiny ANSI palette (true-color) + cursor control, the only "framework" we need ----
fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

// Calm palette: an achromatic base + ONE muted accent (sage), plus muted semantics.
// (A disciplined palette is the single biggest "craft, not slop" signal — SOUL Pillar 4.)
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
fn bad() -> String {
    fg(216, 131, 131)
}

fn flush() {
    let _ = io::stdout().flush();
}
fn pause(ms: u64) {
    flush();
    sleep(Duration::from_millis(ms));
}

/// Redraw the live region in place: move the cursor up `n` lines and clear to end of
/// screen. This — not an alternate screen — is the whole trick: committed lines above are
/// never touched, so they stay in real scrollback; only the active turn is ever rewritten.
fn rewind(n: usize) {
    if n > 0 {
        print!("\x1b[{n}A");
    }
    print!("\x1b[J");
}

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn rule(label: &str) {
    let dashes = "─".repeat(52usize.saturating_sub(label.chars().count()));
    println!("\n{}{}── {label} {dashes}{}", BOLD, faint(), RESET);
}

/// One live "active turn": a 3-line viewport (status · input · model) that redraws in
/// place across `steps`, never scrolling the screen. On return the region is wiped, ready
/// for the caller to commit a permanent receipt where it stood.
fn run_turn(steps: &[(&str, u64)]) {
    for (i, (status, ms)) in steps.iter().enumerate() {
        if i > 0 {
            rewind(3);
        }
        let spin = SPIN[i % SPIN.len()];
        println!("   {}{spin}{RESET} {}{status}{RESET}", sage(), faint());
        println!("   {}>{RESET} {}▌{RESET}", sage(), text());
        println!("   {}gpt-5.5 · 42% · main{RESET}", faint());
        pause(*ms);
    }
    rewind(3); // commit: drop the live region, leaving nothing of it behind
}

fn main() {
    print!("\x1b[?25l"); // hide cursor for a clean animation
    println!(
        "{}{}tomte · Pillar 4 — \"the calm, tidy terminal\"{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}a runnable demo of the Stage-1 direction — record it for the relaunch{}",
        faint(),
        RESET
    );

    // ---- the terminal you were already in: real history that must survive ----
    rule("your terminal, a moment before");
    println!("   {}$ cargo build --release{RESET}", faint());
    println!("   {}    Finished `release` profile in 18.40s{RESET}", faint());
    println!("   {}$ git status{RESET}", faint());
    println!("   {}    nothing to commit, working tree clean{RESET}", faint());
    pause(900);

    // ---- a turn runs inline; only the active turn redraws, the screen never jumps ----
    rule("tomte runs a turn — inline, nothing hijacked");
    pause(450);
    run_turn(&[
        ("reading src/parser.rs …", 700),
        ("drafting the empty-input test …", 700),
        ("applying edit · src/parser.rs", 600),
    ]);
    // the diff, shown plainly before it settles — committed into scrollback
    println!("   {}src/parser.rs{RESET}", sage());
    println!(
        "   {} 88{RESET} {}+   assert!(parse(\"\").is_err());{RESET}",
        faint(),
        ok()
    );
    println!("   {} 89{RESET} {}-   // TODO: empty input{RESET}", faint(), bad());
    pause(650);
    run_turn(&[
        ("running cargo test … 1.1s", 700),
        ("running cargo test … 2.4s", 700),
        ("running cargo test … 3.2s", 600),
    ]);
    // the "left in order" receipt — what a finished turn leaves behind, permanently
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
    pause(950);

    // ---- a second turn, same calm rhythm; the receipts stack up as real scrollback ----
    rule("another turn — receipts stack up, none of it erased");
    pause(400);
    run_turn(&[
        ("reading CHANGELOG.md …", 650),
        ("appending the 0.0.2 entry …", 650),
    ]);
    println!(
        "   {}✓{RESET} {}noted in CHANGELOG{RESET} {}·{RESET} 1 file {}·{RESET} {}no tests needed{RESET}",
        ok(),
        text(),
        faint(),
        faint(),
        faint()
    );
    println!(
        "       {}CHANGELOG.md:3{RESET}   {}why:{RESET} {}user-visible: inline viewport{RESET}",
        sage(),
        faint(),
        text()
    );
    pause(750);

    print!("\x1b[?25h"); // restore the cursor
    println!(
        "\n   {}↑ scroll up — your build, your git status, every finished turn are{RESET}",
        faint()
    );
    println!(
        "   {}  all still here, in the terminal's OWN scrollback. nothing was erased.{RESET}",
        faint()
    );
    println!(
        "\n{}— one accent, quiet status, the room left tidy. that's the custodian.{RESET}",
        faint()
    );
}

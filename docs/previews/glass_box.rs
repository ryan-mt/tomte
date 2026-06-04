//! Standalone preview of Pillar 1 — "glass-box: legible & bounded".
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc docs/previews/glass_box.rs -o /tmp/glass_box && /tmp/glass_box
//! See docs/SOUL.md (Pillar 1).

fn fg(r: u8, g: u8, b: u8) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

fn text() -> String {
    fg(214, 214, 214)
}
fn faint() -> String {
    fg(120, 120, 120)
}
fn sage() -> String {
    fg(138, 176, 150)
}
fn amber() -> String {
    fg(210, 180, 120)
}

fn rule(label: &str) {
    let dashes = "─".repeat(52usize.saturating_sub(label.chars().count()));
    println!("\n{}{}── {label} {dashes}{}", BOLD, faint(), RESET);
}

fn main() {
    println!(
        "{}{}tomte · Pillar 1 preview — \"glass-box: legible & bounded\"{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}a real, runnable sketch of the Stage-1 direction (not the final build){}",
        faint(),
        RESET
    );

    rule("before it runs anything, it shows the plan");
    println!(
        "   {}▸ about to{RESET}   {}run the test suite{RESET}",
        sage(),
        text()
    );
    println!(
        "     {}command{RESET}    {}cargo test{RESET}  {}(read-only to your working tree){RESET}",
        faint(),
        text(),
        faint()
    );
    println!("     {}writes{RESET}     {}0 files{RESET}", faint(), text());
    println!(
        "     {}scope{RESET}      {}crates/parser/{RESET}   {}·  est ~12s  ·  ~3k tokens{RESET}",
        faint(),
        text(),
        faint()
    );
    println!(
        "     {}leash{RESET}      {}this repo · no network · no force-push{RESET}",
        faint(),
        amber()
    );
    println!();
    println!(
        "   {}[enter] proceed    [e] narrow scope    [esc] cancel{RESET}",
        faint()
    );

    rule("and a bounded edit shows its blast radius");
    println!(
        "   {}▸ about to{RESET}   {}edit 1 file{RESET}",
        sage(),
        text()
    );
    println!(
        "     {}write{RESET}      {}src/parser.rs{RESET}  {}(+2 −1){RESET}",
        faint(),
        text(),
        faint()
    );
    println!(
        "     {}untouched{RESET}  {}everything else — nothing outside this file moves{RESET}",
        faint(),
        faint()
    );

    println!(
        "\n{}— you always see WHAT it will do and HOW FAR it can reach, first.{RESET}",
        faint()
    );
}

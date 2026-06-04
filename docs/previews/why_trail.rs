//! Standalone preview of Pillar 2 — "memory of why (the decision trail)".
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc docs/previews/why_trail.rs -o /tmp/why_trail && /tmp/why_trail
//! It actually WRITES a decision to disk and READS it back, proving the
//! "why" persists across sessions. See docs/SOUL.md (Pillar 2).

use std::fs;

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
fn bad() -> String {
    fg(216, 131, 131)
}

fn main() {
    let path = "/tmp/tomte_why_trail.txt";
    // A decision record: what changed AND why, the rejected alternatives, and
    // the model in play — so a LATER model inherits the reasoning, not a summary.
    let record = "loc=src/parser.rs:88\n\
        decision=empty input returns Err, not panic\n\
        model=gpt-5.5\n\
        turn=3\n\
        because=parser contract validates inputs at the boundary\n\
        rejected=panic!() -> would crash callers\n\
        rejected=Ok(0) -> hides the error\n";
    fs::write(path, record).expect("write trail");
    let back = fs::read_to_string(path).expect("read trail");

    println!(
        "{}{}tomte · Pillar 2 preview — \"memory of why (the decision trail)\"{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}wrote a decision to {path}; now reading it back as a fresh session would{}",
        faint(),
        RESET
    );

    println!("\n{}$ tomte why src/parser.rs:88{RESET}", text());
    let mut model = "?";
    for line in back.lines() {
        let (k, v) = line.split_once('=').unwrap_or(("", line));
        match k {
            "decision" => println!("   {}decision{RESET}  {}{v}{RESET}", sage(), text()),
            "model" => model = v,
            "turn" => println!(
                "   {}when{RESET}      turn {v} {}· model at the time: {model}{RESET}",
                faint(),
                faint()
            ),
            "because" => println!("   {}because{RESET}   {}{v}{RESET}", faint(), text()),
            "rejected" => println!("   {}rejected{RESET}  {}{v}{RESET}", faint(), bad()),
            _ => {}
        }
    }
    println!(
        "   {}carried{RESET}   {}survives a switch to another model — the reasoning, not a summary{RESET}",
        faint(),
        text()
    );

    println!(
        "\n{}— the \"why\" outlives the session AND the model switch. that's the moat.{RESET}",
        faint()
    );
}

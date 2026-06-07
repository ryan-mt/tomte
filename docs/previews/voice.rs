//! Standalone preview of tomte's voice with a spine.
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc docs/previews/voice.rs -o /tmp/voice && /tmp/voice
//! The soul is the harness's, not the model's.

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

fn rule(label: &str) {
    let dashes = "─".repeat(52usize.saturating_sub(label.chars().count()));
    println!("\n{}{}── {label} {dashes}{}", BOLD, faint(), RESET);
}

fn main() {
    println!(
        "{}{}tomte · Pillar 3 preview — \"a voice with a spine\"{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}same question, two voices (not the final build){}",
        faint(),
        RESET
    );

    rule("GENERIC assistant (today)");
    println!(
        "   {}\"Sure! Great question — I'd be happy to help! 🙌{RESET}",
        faint()
    );
    println!(
        "   {} Let me go ahead and add that lock for you right away!\"{RESET}",
        faint()
    );

    rule("TOMTE (with a spine)");
    println!(
        "   {}\"That races under load: the guard drops before the .await,{RESET}",
        text()
    );
    println!(
        "   {} so two tasks interleave between check and write. Hold one{RESET}",
        text()
    );
    println!(
        "   {} MutexGuard across the await instead. {}~80% sure{} from the{RESET}",
        text(),
        sage(),
        text()
    );
    println!(
        "   {} trace — want the failing interleaving?\"{RESET}",
        text()
    );

    println!(
        "\n{}— pushes back · cites the mechanism · states confidence · no sycophancy.{RESET}",
        faint()
    );
    println!(
        "{}  judgment, not decoration (and the soul is the harness's, not the model's).{RESET}",
        faint()
    );
}

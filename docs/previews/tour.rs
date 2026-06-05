//! Unified guided tour of the tomte "quiet custodian" direction — all five
//! pillars in ONE coherent runnable flow, so the whole vision can be seen and
//! judged in a single command.
//!
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc docs/previews/tour.rs -o /tmp/tomte_tour && /tmp/tomte_tour
//! A preview of the direction (see docs/SOUL.md), not the final integrated build.

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
fn ok() -> String {
    fg(143, 188, 143)
}
fn bad() -> String {
    fg(216, 131, 131)
}
fn amber() -> String {
    fg(210, 180, 120)
}

fn step(n: u8, label: &str) {
    println!("\n{}{}▸ {n} · {label}{}", BOLD, sage(), RESET);
}

fn main() {
    println!(
        "{}{}tomte — a guided tour of the custodian{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}one std-only command, no deps; a preview of the direction, not the final build{}",
        faint(),
        RESET
    );

    println!(
        "\n{}   ...your shell history stays right here in scrollback — nothing erased...{}",
        faint(),
        RESET
    );
    println!(
        "\n   {}you:{RESET} {}add a test for the parser{RESET}",
        faint(),
        text()
    );

    step(1, "glass-box — it tells you what it will do, first");
    println!(
        "     {}about to{RESET}  {}run the suite and add one test{RESET}",
        faint(),
        text()
    );
    println!(
        "     {}writes{RESET}    {}1 file  ·  src/parser.rs{RESET}",
        faint(),
        text()
    );
    println!(
        "     {}scope{RESET}     {}crates/parser/  ·  ~12s  ·  ~3k tokens{RESET}",
        faint(),
        text()
    );
    println!(
        "     {}[enter] proceed   [e] narrow scope   [esc] cancel{RESET}",
        faint()
    );

    step(2, "calm terminal — runs inline, leaves a tidy receipt");
    println!("     {}running cargo test ... 3.2s{RESET}", faint());
    println!(
        "     {}✓{RESET} {}added parser test{RESET} {}·{RESET} 1 file {}·{RESET} {}cargo test 12 passed{RESET}",
        ok(),
        text(),
        faint(),
        faint(),
        ok()
    );
    println!("         {}src/parser.rs:88{RESET}", sage());

    step(3, "memory of why — and it remembers, across models");
    println!(
        "     {}recorded{RESET}  {}chose Err over panic (parser validates at the boundary){RESET}",
        faint(),
        text()
    );
    println!(
        "     {}rejected{RESET}  {}Ok(0) — would hide the error{RESET}",
        faint(),
        bad()
    );
    println!(
        "     {}$ tomte why src/parser.rs:88{RESET}  {}->  returns it even after gpt-5.5 -> claude{RESET}",
        text(),
        faint()
    );

    step(4, "a voice with a spine — opinionated, not sycophantic");
    println!(
        "     {}\"Returning Ok(0) there would hide the error, so I used Err.{RESET}",
        text()
    );
    println!(
        "     {} ~80% sure that's what you want — say if not.\"{RESET}",
        text()
    );

    step(5, "the conscience — a past decision confronts a risky edit");
    println!(
        "     {}later · /model claude · about to edit src/parser.rs{RESET}",
        faint()
    );
    println!(
        "     {}house rules{RESET}  {}chose Err over panic  (recorded by gpt-5.5){RESET}",
        faint(),
        text()
    );
    println!(
        "     {}self-check (claude){RESET}  {}CONFLICT — this edit reintroduces panic!(){RESET}",
        faint(),
        bad()
    );
    println!(
        "     {}⚠{RESET} {}only you can clear it — the override lands in the end-of-turn summary{RESET}",
        amber(),
        faint()
    );

    println!(
        "\n{}{}calm · legible · remembers · opinionated · keeps its word — multi-model underneath.{}",
        BOLD,
        sage(),
        RESET
    );
    println!(
        "{}that's tomte. the soul is the product's; the model still answers as itself.{}",
        faint(),
        RESET
    );
}

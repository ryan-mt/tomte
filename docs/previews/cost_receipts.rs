//! Standalone preview of the cross-cutting enabler — "normalized cost receipts
//! across providers" (docs/SOUL.md §5; Stage 3 "normalized cross-provider cost
//! display"). It supports Pillar 1 (cost is legible before/after acting) and
//! Pillar 4 (a calm, quantitative receipt).
//!
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc -D warnings docs/previews/cost_receipts.rs -o /tmp/cost && /tmp/cost
//!
//! Not a mockup: it does the REAL arithmetic — per-billing-class (input, cached
//! read, cache write, output) cost = tokens × rate, summed per model, then
//! reconciled across two providers into ONE honest total. The per-1M-token rates
//! are illustrative (these model names are near-future), but the mechanism — a
//! single normalized bill spanning vendors that price differently — is the point,
//! and is exactly what a single-vendor tool structurally cannot show.
//!
//! See docs/SOUL.md (§4 multi-model as plumbing; §5 cross-cutting enabler).

// ---- calm palette (matches docs/previews/why_trail.rs) ----------------------
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
} // the one accent
fn ok() -> String {
    fg(143, 188, 143)
}

// ---- a billing class within one model's usage -------------------------------
struct Class {
    label: &'static str,
    tokens: u64,
    rate: f64, // US dollars per 1,000,000 tokens
}

impl Class {
    fn cost(&self) -> f64 {
        self.tokens as f64 / 1_000_000.0 * self.rate
    }
}

struct Receipt {
    model: &'static str,
    provider: &'static str,
    classes: Vec<Class>,
}

impl Receipt {
    fn subtotal(&self) -> f64 {
        self.classes.iter().map(|c| c.cost()).sum()
    }
    fn tokens(&self) -> u64 {
        self.classes.iter().map(|c| c.tokens).sum()
    }
}

// ---- formatting (std-only) --------------------------------------------------
/// 1240000 -> "1,240,000"
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// 1.55 -> "$1.55", 0.1025 -> "$0.1025", 50.01 -> "$50.01" (4 dp, trailing zeros trimmed)
fn money(v: f64) -> String {
    let s = format!("{v:.4}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    format!("${trimmed}")
}

fn rule(w: usize) -> String {
    "─".repeat(w)
}

// ---- rendering --------------------------------------------------------------
const COL: usize = 56; // inner card width for the divider rules

fn render(r: &Receipt) {
    println!(
        "\n  {s}{model}{RESET} {f}· {provider}{RESET}",
        model = r.model,
        provider = r.provider,
        s = sage(),
        f = faint()
    );
    for c in &r.classes {
        // label  |  right-aligned tokens  |  rate  |  right-aligned cost
        let tok = format!("{} tok", thousands(c.tokens));
        let rate = format!("× {}/M", money(c.rate));
        println!(
            "    {f}{label:<13}{RESET}{t}{tok:>15}{RESET}   {f}{rate:>12}{RESET}   {t}= {cost:>9}{RESET}",
            label = c.label,
            tok = tok,
            rate = rate,
            cost = money(c.cost()),
            f = faint(),
            t = text(),
        );
    }
    println!("    {f}{}{RESET}", rule(COL), f = faint());
    println!(
        "    {f}{label:<13}{RESET}{f}{tok:>15}{RESET}   {pad:>12}   {s}= {cost:>9}{RESET}",
        label = "subtotal",
        tok = format!("{} tok", thousands(r.tokens())),
        pad = "",
        cost = money(r.subtotal()),
        f = faint(),
        s = sage(),
    );
}

fn session() -> Vec<Receipt> {
    // One real session: it started on an OpenAI model, then /model-switched to an
    // Anthropic one mid-task. Two vendors, two price sheets, one piece of work.
    vec![
        Receipt {
            model: "gpt-5.5",
            provider: "openai",
            classes: vec![
                Class { label: "input", tokens: 1_240_000, rate: 1.25 },
                Class { label: "cached read", tokens: 820_000, rate: 0.125 },
                Class { label: "cache write", tokens: 96_000, rate: 1.5625 },
                Class { label: "output", tokens: 305_000, rate: 10.00 },
            ],
        },
        Receipt {
            model: "claude-opus-4-8",
            provider: "anthropic",
            classes: vec![
                Class { label: "input", tokens: 540_000, rate: 15.00 },
                Class { label: "cached read", tokens: 1_480_000, rate: 1.50 },
                Class { label: "cache write", tokens: 210_000, rate: 18.75 },
                Class { label: "output", tokens: 412_000, rate: 75.00 },
            ],
        },
    ]
}

fn main() {
    println!(
        "{B}{s}tomte · cost receipts — one normalized bill across providers{RESET}",
        B = BOLD,
        s = sage()
    );
    println!(
        "{f}illustrative rates · real arithmetic · the cross-provider total is the point{RESET}",
        f = faint()
    );

    let receipts = session();
    for r in &receipts {
        render(r);
    }

    let total: f64 = receipts.iter().map(|r| r.subtotal()).sum();
    let tokens: u64 = receipts.iter().map(|r| r.tokens()).sum();
    println!("\n  {f}{}{RESET}", rule(COL + 4), f = faint());
    println!(
        "  {B}{s}session total{RESET}  {f}{n} providers · {tok} tok normalized{RESET}   {B}{ok}{cost}{RESET}",
        n = receipts.len(),
        tok = thousands(tokens),
        cost = money(total),
        B = BOLD,
        s = sage(),
        f = faint(),
        ok = ok(),
    );

    // The multi-model point, made quantitative: each single-vendor tool sees only
    // the slice it billed; only a cross-provider agent can reconcile the whole.
    println!(
        "\n{f}— a single-vendor tool sees only its slice:{RESET}",
        f = faint()
    );
    for r in &receipts {
        println!(
            "{f}    {provider:<10} would show {cost}, never the whole{RESET}",
            provider = r.provider,
            cost = money(r.subtotal()),
            f = faint(),
        );
    }
    println!(
        "{f}  tomte normalizes both price sheets into one honest receipt — that's{RESET}",
        f = faint()
    );
    println!(
        "{f}  multi-model as plumbing (SOUL §4), and Pillar 1's cost line made exact.{RESET}",
        f = faint()
    );
}

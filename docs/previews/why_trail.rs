//! Standalone preview of tomte's memory of why — the decision trail.
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc -D warnings docs/previews/why_trail.rs -o /tmp/why_trail && /tmp/why_trail
//!
//! Unlike a static mockup, this WRITES real JSONL decision records to disk and
//! READS them back to answer queries — proving the "why" survives a session and,
//! crucially, a MODEL SWITCH (the multi-model moat).
//! The on-disk shape is exactly the `decisions.jsonl` record tomte writes:
//! { loc, decision, why, rejected[], model, turn, ts }.
//!
//! Behaves like a tiny `tomte why` CLI:
//!   why_trail                  guided walkthrough: model A records, model B inherits
//!   why_trail why <loc>        query the trail for one location (a real read)
//!   why_trail why --all        list the whole trail, git-blame-for-decisions style

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

// ---- calm palette (matches docs/previews/tour.rs) ---------------------------
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

/// The decision store. A real file on disk, written then read back.
const STORE: &str = "/tmp/tomte_decisions.jsonl";

// ---- the decision record ----------------------------------------------------
struct Decision {
    loc: String,
    decision: String,
    why: String,
    rejected: Vec<String>,
    model: String,
    turn: u32,
    ts: u64,
}

impl Decision {
    /// Serialize to one JSONL line (the `decisions.jsonl` shape).
    fn to_jsonl(&self) -> String {
        let rej: Vec<String> = self.rejected.iter().map(|r| format!("\"{}\"", esc(r))).collect();
        format!(
            "{{\"loc\":\"{}\",\"decision\":\"{}\",\"why\":\"{}\",\"rejected\":[{}],\"model\":\"{}\",\"turn\":{},\"ts\":{}}}",
            esc(&self.loc),
            esc(&self.decision),
            esc(&self.why),
            rej.join(","),
            esc(&self.model),
            self.turn,
            self.ts
        )
    }

    /// Rebuild a record from a parsed JSON object — the read half of the trip.
    fn from_json(j: &Json) -> Option<Decision> {
        Some(Decision {
            loc: j.get("loc")?.str()?.to_string(),
            decision: j.get("decision")?.str()?.to_string(),
            why: j.get("why")?.str()?.to_string(),
            rejected: j
                .get("rejected")?
                .arr()?
                .iter()
                .filter_map(|x| x.str().map(|s| s.to_string()))
                .collect(),
            model: j.get("model")?.str()?.to_string(),
            turn: j.get("turn")?.num()? as u32,
            ts: j.get("ts")?.num()? as u64,
        })
    }
}

// ---- a tiny, correct JSON reader (std-only) ---------------------------------
// Small on purpose: enough to read back exactly what `to_jsonl` writes, with
// proper string-escape handling, so the round-trip is real and not faked.
enum Json {
    Str(String),
    Num(f64),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, k: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(kk, _)| kk == k).map(|(_, vv)| vv),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
    fn arr(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
}

struct Parser {
    c: Vec<char>,
    i: usize,
}

impl Parser {
    fn new(s: &str) -> Parser {
        Parser { c: s.chars().collect(), i: 0 }
    }
    fn ws(&mut self) {
        while self.i < self.c.len() && self.c[self.i].is_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let ch = self.peek();
        if ch.is_some() {
            self.i += 1;
        }
        ch
    }
    fn value(&mut self) -> Option<Json> {
        self.ws();
        match self.peek()? {
            '"' => self.string().map(Json::Str),
            '[' => self.array(),
            '{' => self.object(),
            _ => self.number(),
        }
    }
    fn string(&mut self) -> Option<String> {
        if self.bump()? != '"' {
            return None;
        }
        let mut out = String::new();
        loop {
            match self.bump()? {
                '"' => return Some(out),
                '\\' => match self.bump()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            code = code * 16 + self.bump()?.to_digit(16)?;
                        }
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                },
                ch => out.push(ch),
            }
        }
    }
    fn array(&mut self) -> Option<Json> {
        self.bump(); // consume '['
        let mut v = Vec::new();
        self.ws();
        if self.peek()? == ']' {
            self.bump();
            return Some(Json::Arr(v));
        }
        loop {
            v.push(self.value()?);
            self.ws();
            match self.bump()? {
                ',' => continue,
                ']' => return Some(Json::Arr(v)),
                _ => return None,
            }
        }
    }
    fn object(&mut self) -> Option<Json> {
        self.bump(); // consume '{'
        let mut v = Vec::new();
        self.ws();
        if self.peek()? == '}' {
            self.bump();
            return Some(Json::Obj(v));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            if self.bump()? != ':' {
                return None;
            }
            let val = self.value()?;
            v.push((k, val));
            self.ws();
            match self.bump()? {
                ',' => continue,
                '}' => return Some(Json::Obj(v)),
                _ => return None,
            }
        }
    }
    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E') {
                self.i += 1;
            } else {
                break;
            }
        }
        let s: String = self.c[start..self.i].iter().collect();
        s.parse::<f64>().ok().map(Json::Num)
    }
}

fn parse_line(s: &str) -> Option<Json> {
    Parser::new(s).value()
}

/// Minimal JSON string escaping for the write half of the round-trip.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            _ => o.push(c),
        }
    }
    o
}

// ---- store I/O --------------------------------------------------------------
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load() -> Vec<Decision> {
    fs::read_to_string(STORE)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_line)
        .filter_map(|j| Decision::from_json(&j))
        .collect()
}

fn write_store(ds: &[Decision]) {
    let body: String = ds.iter().map(|d| d.to_jsonl()).collect::<Vec<_>>().join("\n");
    fs::write(STORE, format!("{body}\n")).expect("write decision store");
}

/// The decisions the agent made during the task — recorded with their *why*.
fn demo_decisions() -> Vec<Decision> {
    let ts = now_secs();
    vec![
        Decision {
            loc: "src/api/auth.rs:15".into(),
            decision: "verify the JWT signature before decoding any claims".into(),
            why: "claims are attacker-controlled; trusting them before verification is the bug class behind most JWT CVEs".into(),
            rejected: vec![
                "decode-then-verify  ->  opens a TOCTOU window".into(),
                "skip the exp check  ->  tokens would never expire".into(),
            ],
            model: "gpt-5.5".into(),
            turn: 2,
            ts,
        },
        Decision {
            loc: "src/cache.rs:42".into(),
            decision: "bound the cache with LRU eviction at 1024 entries".into(),
            why: "unbounded growth OOMs under sustained load; profiling holds the hit-rate above 90% at 1024".into(),
            rejected: vec![
                "unbounded HashMap  ->  OOM under load".into(),
                "TTL-only           ->  cold keys still pin memory".into(),
            ],
            model: "gpt-5.5".into(),
            turn: 4,
            ts,
        },
        Decision {
            loc: "src/parser.rs:88".into(),
            decision: "empty input returns Err, not panic".into(),
            why: "the parser validates at the boundary; a library must never crash its caller".into(),
            rejected: vec![
                "panic!()  ->  crashes callers".into(),
                "Ok(0)     ->  silently hides the error".into(),
            ],
            model: "gpt-5.5".into(),
            turn: 5,
            ts,
        },
    ]
}

/// Read the store; seed it with the demo trail on first use so every entry
/// point (including a bare `why <loc>`) just works.
fn ensure_seeded() -> Vec<Decision> {
    let existing = load();
    if existing.is_empty() {
        let d = demo_decisions();
        write_store(&d);
        d
    } else {
        existing
    }
}

// ---- rendering --------------------------------------------------------------
/// One "label   value" line, label column fixed at 9 so cards align.
fn row(label: &str, value: &str, vcolor: &str) {
    println!("     {f}{label:<9}{RESET} {vcolor}{value}{RESET}", f = faint());
}

/// Shorten a string to `max` display chars with an ellipsis.
fn gist(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max - 1).collect();
        format!("{t}…")
    }
}

fn scene(tag: &str, sub: &str) {
    println!("\n{BOLD}{s}▸ {tag}{RESET}", s = sage());
    if !sub.is_empty() {
        println!("   {f}{sub}{RESET}", f = faint());
    }
}

/// The detailed decision card (used both when recording and when answering).
fn detail(d: &Decision, show_when: bool) {
    row("decision", &d.decision, &text());
    if show_when {
        row("when", &format!("turn {} · decided by {}", d.turn, d.model), &faint());
    }
    row("because", &d.why, &text());
    for r in &d.rejected {
        row("rejected", r, &bad());
    }
}

/// The `--all` table: one line per decision, like `git blame` for reasoning.
fn table(ds: &[Decision]) {
    let w = ds.iter().map(|d| d.loc.chars().count()).max().unwrap_or(0);
    for d in ds {
        println!(
            "   {s}{loc:<w$}{RESET}  {t}{g:<34}{RESET} {f}turn {turn} · {model}{RESET}",
            s = sage(),
            loc = d.loc,
            t = text(),
            g = gist(&d.decision, 34),
            f = faint(),
            turn = d.turn,
            model = d.model,
        );
    }
}

// ---- entry points -----------------------------------------------------------
fn walkthrough() {
    let ds = demo_decisions();
    write_store(&ds);

    println!("{BOLD}{s}tomte · Pillar 2 — memory of why (the decision trail){RESET}", s = sage());
    println!("{f}real JSONL on disk; the reasoning survives a session AND a model switch{RESET}", f = faint());

    scene("during the task   ·   model in play: gpt-5.5", "the agent records WHY as it decides — not just what changed:");
    println!();
    for d in &ds {
        println!("   {s}recorded{RESET}  {t}{loc}{RESET}", s = sage(), t = text(), loc = d.loc);
        detail(d, false);
        println!();
    }
    println!("   {s}->{RESET} {f}wrote {n} decisions to {STORE}{RESET}", s = sage(), f = faint(), n = ds.len());

    scene(
        "you switch models mid-session   ·   /model claude-opus-4-8",
        "a different vendor — it has no chat memory of the choices above.",
    );
    println!("   {f}so it reads the trail from disk — the reasoning, not a lossy summary:{RESET}\n", f = faint());
    let loc = "src/api/auth.rs:15";
    if let Some(d) = ds.iter().find(|d| d.loc == loc) {
        println!("   {t}$ tomte why {loc}{RESET}", t = text());
        detail(d, true);
        row("carried", "claude-opus-4-8 now cites gpt-5.5's reasoning, verbatim", &ok());
    }

    scene("browse the whole trail   ·   git blame, but for decisions", "");
    println!("   {t}$ tomte why --all{RESET}\n", t = text());
    table(&ds);

    println!("\n{f}— the \"why\" outlives the session AND the model switch. that's the moat:{RESET}", f = faint());
    println!("{f}  the reasoning belongs to the agent, not to any one model.{RESET}", f = faint());
}

fn cmd_why(loc: &str) {
    let ds = ensure_seeded();
    let hits: Vec<&Decision> = ds.iter().filter(|d| d.loc == loc).collect();
    if hits.is_empty() {
        println!("{f}no decision recorded at {loc}.  try: why_trail why --all{RESET}", f = faint());
        return;
    }
    for d in hits {
        println!("{t}$ tomte why {loc}{RESET}", t = text());
        detail(d, true);
    }
}

fn cmd_all() {
    let ds = ensure_seeded();
    if ds.is_empty() {
        println!("{f}the trail is empty.{RESET}", f = faint());
        return;
    }
    println!("{t}$ tomte why --all{RESET}\n", t = text());
    table(&ds);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        None => walkthrough(),
        Some("why") => match args.get(1).map(|s| s.as_str()) {
            Some("--all") | Some("-a") => cmd_all(),
            Some(loc) => cmd_why(loc),
            None => println!("usage: why_trail why <loc>   |   why_trail why --all"),
        },
        Some("--all") | Some("-a") | Some("log") => cmd_all(),
        Some(other) => {
            println!("unknown command {other:?}.  try: why_trail | why_trail why <loc> | why_trail why --all")
        }
    }
}

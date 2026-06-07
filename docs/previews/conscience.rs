//! Standalone preview of the custodian's conscience — the active decision trail.
//! Not part of tomte's build: std-only, under docs/, compiled by hand:
//!   rustc -D warnings docs/previews/conscience.rs -o /tmp/conscience && /tmp/conscience
//!
//! The decision trail (see why_trail.rs) makes tomte REMEMBER; the conscience makes it ACTIVE.
//! This is not a mockup: it writes real source files AND a real `decisions.jsonl`
//! to disk, then RECONCILES the trail against the code by CONTENT (not a frozen
//! file:line), confronts an edit about to reverse a recorded decision, and logs the
//! human override on the record. The on-disk shape is exactly the `DecisionRecord`
//! the design specifies — loc / decision / why / rejected[] / model / ts, plus the
//! two additive fields the conscience needs:
//!   anchor:     a snapshot of the line at record time   (A1 — Drift Watch)
//!   supersedes: the ts of the decision this overturns    (A3 — On the Record)
//!
//! Behaves like a tiny conscience CLI:
//!   conscience                 guided walkthrough: drift → reckoning → override
//!   conscience reconcile       audit the on-disk trail against the code (A1)
//!   conscience why <loc>       query a location, following the supersede chain
//!   conscience why --all       the whole trail — git-blame for conscience

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

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
fn bad() -> String {
    fg(216, 131, 131)
}
fn amber() -> String {
    fg(210, 180, 120)
} // a calm caution — never a gate

// ---- the workspace this demo audits (real files on disk) --------------------
const ROOT: &str = "/tmp/tomte_conscience_demo";
fn store_path() -> String {
    format!("{ROOT}/.tomte/decisions.jsonl")
}
fn code_path(rel: &str) -> String {
    format!("{ROOT}/{rel}")
}

/// A decision GPT-5.5 made two weeks ago is the whole point: the conscience makes
/// a promise from the past confront an edit in the present.
const WEEKS_AGO: u64 = 15 * 24 * 3600;

// ---- the decision record (the real DecisionRecord shape) --------------------
struct Decision {
    loc: String,
    decision: String,
    why: String,
    rejected: Vec<String>,
    model: String,
    ts: u64,
    anchor: Option<String>,    // A1: the trimmed line(s) at `loc` when recorded
    supersedes: Option<u64>,   // A3: ts of the decision this one overturns
}

impl Decision {
    /// Serialize to one JSONL line. The two new fields are emitted only when set —
    /// exactly the additive `serde(default)` discipline the design promises, so an
    /// old line without them still round-trips.
    fn to_jsonl(&self) -> String {
        let rej: Vec<String> = self.rejected.iter().map(|r| format!("\"{}\"", esc(r))).collect();
        let mut s = format!(
            "{{\"loc\":\"{}\",\"decision\":\"{}\",\"why\":\"{}\",\"rejected\":[{}],\"model\":\"{}\",\"ts\":{}",
            esc(&self.loc),
            esc(&self.decision),
            esc(&self.why),
            rej.join(","),
            esc(&self.model),
            self.ts,
        );
        if let Some(a) = &self.anchor {
            s.push_str(&format!(",\"anchor\":\"{}\"", esc(a)));
        }
        if let Some(sp) = self.supersedes {
            s.push_str(&format!(",\"supersedes\":{sp}"));
        }
        s.push('}');
        s
    }

    /// Rebuild from a parsed object — the read half. Absent optional fields → None,
    /// proving the format is backward-compatible (the read side never requires them).
    fn from_json(j: &Json) -> Option<Decision> {
        Some(Decision {
            loc: j.get("loc")?.str()?.to_string(),
            decision: j.get("decision")?.str()?.to_string(),
            why: j.get("why")?.str()?.to_string(),
            rejected: j
                .get("rejected")
                .and_then(|x| x.arr())
                .map(|a| a.iter().filter_map(|x| x.str().map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            model: j.get("model")?.str()?.to_string(),
            ts: j.get("ts")?.num()? as u64,
            anchor: j.get("anchor").and_then(|x| x.str()).map(|s| s.to_string()),
            supersedes: j.get("supersedes").and_then(|x| x.num()).map(|n| n as u64),
        })
    }
}

// ---- a tiny, correct JSON reader (std-only, from why_trail.rs) --------------
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

// ---- store + workspace I/O --------------------------------------------------
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn ago(ts: u64) -> String {
    let d = now_secs().saturating_sub(ts);
    if d < 90 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else if d < 14 * 86_400 {
        format!("{}d ago", d / 86_400)
    } else {
        format!("{} weeks ago", d / (7 * 86_400))
    }
}

fn load() -> Vec<Decision> {
    fs::read_to_string(store_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(parse_line)
        .filter_map(|j| Decision::from_json(&j))
        .collect()
}

fn write_store(ds: &[Decision]) {
    let dir = format!("{ROOT}/.tomte");
    fs::create_dir_all(&dir).expect("create store dir");
    let body: String = ds.iter().map(|d| d.to_jsonl()).collect::<Vec<_>>().join("\n");
    fs::write(store_path(), format!("{body}\n")).expect("write decision store");
}

fn write_code(rel: &str, contents: &str) {
    let path = code_path(rel);
    if let Some(slash) = path.rfind('/') {
        fs::create_dir_all(&path[..slash]).expect("create code dir");
    }
    fs::write(&path, contents).expect("write code file");
}

fn read_code_lines(rel: &str) -> Vec<String> {
    fs::read_to_string(code_path(rel)).unwrap_or_default().lines().map(|l| l.to_string()).collect()
}

/// "src/auth.rs:7" -> ("src/auth.rs", 7)
fn parse_loc(loc: &str) -> Option<(String, usize)> {
    let (f, l) = loc.rsplit_once(':')?;
    Some((f.to_string(), l.parse().ok()?))
}

// ---- A1 · Drift Watch — reconcile the trail against the code ----------------
// The original slop was a frozen file:line. The fix is a CONTENT anchor: find the
// remembered line wherever it moved to, and only worry when it truly vanished.
enum Drift {
    Present,            // the line at `loc` still matches the anchor — silent
    Moved(usize),       // found uniquely elsewhere — re-anchor, silent (a tidy house)
    Gone,               // not found at all — surface ONE calm line, never a gate
    Ambiguous(usize),   // found in N places — can't auto-pick; surface for review
    Legacy,             // no anchor (old record) — left untouched, never dropped
}

fn check(d: &Decision) -> Drift {
    let Some(anchor) = d.anchor.as_deref() else {
        return Drift::Legacy;
    };
    let Some((file, line)) = parse_loc(&d.loc) else {
        return Drift::Gone;
    };
    let lines = read_code_lines(&file);
    if lines.get(line - 1).map(|l| l.trim()) == Some(anchor) {
        return Drift::Present;
    }
    let hits: Vec<usize> =
        lines.iter().enumerate().filter(|(_, l)| l.trim() == anchor).map(|(i, _)| i + 1).collect();
    match hits.len() {
        0 => Drift::Gone,
        1 => Drift::Moved(hits[0]),
        n => Drift::Ambiguous(n),
    }
}

struct Report {
    present: usize,
    moved: Vec<(String, String)>, // (old loc, new loc)
    gone: Vec<(String, String)>,  // (loc, decision gist)
    ambiguous: Vec<(String, usize)>,
}

/// Walk the trail, re-anchor what merely moved (silently), and report the rest.
/// Rewrites `decisions.jsonl` only when a `loc` actually changed.
fn reconcile(ds: &mut [Decision]) -> Report {
    let mut r =
        Report { present: 0, moved: vec![], gone: vec![], ambiguous: vec![] };
    let mut changed = false;
    for d in ds.iter_mut() {
        match check(d) {
            Drift::Present | Drift::Legacy => r.present += 1,
            Drift::Moved(new_line) => {
                let (file, _) = parse_loc(&d.loc).unwrap();
                let new_loc = format!("{file}:{new_line}");
                r.moved.push((d.loc.clone(), new_loc.clone()));
                d.loc = new_loc;
                changed = true;
            }
            Drift::Gone => r.gone.push((d.loc.clone(), gist(&d.decision, 38))),
            Drift::Ambiguous(n) => r.ambiguous.push((d.loc.clone(), n)),
        }
    }
    if changed {
        write_store(ds);
    }
    r
}

/// A2 Tier 1 — every decision recorded for a file, matched on the FILE part of
/// `loc`, not the frozen file:line. Pure recall; it can never be wrong.
fn for_file<'a>(ds: &'a [Decision], file: &str) -> Vec<&'a Decision> {
    ds.iter().filter(|d| parse_loc(&d.loc).map(|(f, _)| f == file).unwrap_or(false)).collect()
}

// ---- rendering --------------------------------------------------------------
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

/// One "label   value" line, label column fixed at 10 so cards align.
fn row(label: &str, value: &str, vcolor: &str) {
    println!("     {f}{label:<10}{RESET}{vcolor}{value}{RESET}", f = faint());
}

/// The full decision card (used when answering a `why`).
fn detail(d: &Decision) {
    row("decision", &d.decision, &text());
    row("because", &d.why, &text());
    for rj in &d.rejected {
        row("rejected", rj, &bad());
    }
    row("by", &format!("{} · {}", d.model, ago(d.ts)), &faint());
}

fn render_report(r: &Report) {
    for (old, new) in &r.moved {
        println!(
            "   {s}moved{RESET}     {f}{old}{RESET} {s}->{RESET} {t}{new}{RESET}  {f}· re-anchored silently (a tidy house needs no announcement){RESET}",
            s = sage(),
            f = faint(),
            t = text(),
        );
    }
    for (loc, g) in &r.ambiguous_pairs() {
        println!("   {a}ambiguous{RESET} {t}{loc}{RESET}  {f}· {g}{RESET}", a = amber(), t = text(), f = faint());
    }
    let stale = r.gone.len();
    if stale > 0 {
        // The whole A1 promise: not a stack trace, not a block — ONE calm opener line.
        let (word, verb, poss) =
            if stale == 1 { ("decision", "matches", "its") } else { ("decisions", "match", "their") };
        println!(
            "   {a}{stale} {word} no longer {verb} {poss} code{RESET} {f}— conscience reconcile{RESET}",
            a = amber(),
            f = faint(),
        );
        for (loc, g) in &r.gone {
            println!("       {f}{loc}{RESET}  {f}{g}{RESET}", f = faint());
        }
    }
    if r.moved.is_empty() && r.gone.is_empty() && r.ambiguous.is_empty() {
        println!("   {ok}all {n} decisions still match their code{RESET}", ok = ok(), n = r.present);
    } else {
        println!("   {f}{n} still present (silent){RESET}", f = faint(), n = r.present);
    }
}

impl Report {
    // Tiny helper so `render_report` can iterate ambiguous as (loc, "found N×").
    fn ambiguous_pairs(&self) -> Vec<(String, String)> {
        self.ambiguous.iter().map(|(loc, n)| (loc.clone(), format!("found in {n} places"))).collect()
    }
}

// ---- the three acts ---------------------------------------------------------
const AUTH_V1: &str = "\
use crate::hash::argon2id;

pub fn hash_password(pw: &str) -> String {
    // argon2id: memory-hard, the modern default
    argon2id(pw, default_params())
}
";

// drift: two imports added above — the argon2 line slides from 5 to 7.
const AUTH_V2: &str = "\
use crate::hash::argon2id;
use crate::config::Policy;
use std::time::Instant;

pub fn hash_password(pw: &str) -> String {
    // argon2id: memory-hard, the modern default
    argon2id(pw, default_params())
}
";

// after the human-approved supersede: argon2 -> bcrypt at the re-anchored line.
const AUTH_V3: &str = "\
use crate::hash::bcrypt;
use crate::config::Policy;
use std::time::Instant;

pub fn hash_password(pw: &str) -> String {
    // bcrypt: argon2 dep dropped to shrink the binary
    bcrypt(pw, cost(12))
}
";

const PARSER_V1: &str = "\
pub fn parse(s: &str) -> Result<Ast, Error> {
    if s.is_empty() {
        return Err(Error::Empty);
    }
    real_parse(s)
}
";

// gone: the empty-input guard is refactored out entirely.
const PARSER_V2: &str = "\
pub fn parse(s: &str) -> Result<Ast, Error> {
    real_parse(s)
}
";

/// Lay down the workspace + the trail two models built over two weeks.
fn seed() -> Vec<Decision> {
    let _ = fs::remove_dir_all(ROOT); // fresh every run, so the demo is repeatable
    write_code("src/auth.rs", AUTH_V1);
    write_code("src/parser.rs", PARSER_V1);
    let old = now_secs().saturating_sub(WEEKS_AGO);
    let ds = vec![
        Decision {
            loc: "src/auth.rs:5".into(),
            decision: "hash passwords with argon2id".into(),
            why: "memory-hard, resists GPU/ASIC cracking; bcrypt caps input at 72 bytes and is weaker under parallel attack".into(),
            rejected: vec![
                "bcrypt   ->  72-byte cap, weaker vs GPU farms".into(),
                "sha-256  ->  fast == crackable; not a KDF".into(),
            ],
            model: "gpt-5.5".into(),
            ts: old,
            anchor: Some("argon2id(pw, default_params())".into()),
            supersedes: None,
        },
        Decision {
            loc: "src/parser.rs:3".into(),
            decision: "empty input returns Err, never panic".into(),
            why: "a library must validate at the boundary and never crash its caller".into(),
            rejected: vec!["panic!()  ->  crashes callers".into()],
            model: "gpt-5.5".into(),
            ts: old + 18 * 3600, // recorded later in the session — supersede links by ts, so it must be unique
            anchor: Some("return Err(Error::Empty);".into()),
            supersedes: None,
        },
    ];
    write_store(&ds);
    ds
}

fn walkthrough() {
    println!(
        "{BOLD}{s}tomte · Pillar 5 — the custodian's conscience (the active decision trail){RESET}",
        s = sage()
    );
    println!(
        "{f}real code + real JSONL on disk; the trail audits itself, then confronts the edit{RESET}",
        f = faint()
    );

    let mut ds = seed();
    println!(
        "\n{f}seeded {ROOT}/ — two files and a {n}-record trail gpt-5.5 wrote two weeks ago.{RESET}",
        f = faint(),
        n = ds.len()
    );

    // ---- A1 · Drift Watch -----------------------------------------------
    scene(
        "A1 · Drift Watch   ·   the trail audits itself against the code",
        "Pillar 2 froze a file:line. Here the record carries a CONTENT anchor, so it",
    );
    println!("   {f}follows its line when the code moves — and only complains when it truly vanishes.{RESET}", f = faint());

    println!("\n   {t}someone adds two imports to src/auth.rs — the argon2 line slides 5 → 7.{RESET}", t = text());
    write_code("src/auth.rs", AUTH_V2); // a real edit to a real file
    println!("   {t}$ conscience reconcile{RESET}", t = text());
    let r = reconcile(&mut ds);
    render_report(&r);
    println!(
        "   {f}note: a frozen file:line would now point tomte at the WRONG line and silently rot.{RESET}",
        f = faint()
    );

    println!("\n   {t}then the empty-input guard in src/parser.rs is refactored away entirely.{RESET}", t = text());
    write_code("src/parser.rs", PARSER_V2); // the anchored line is now gone
    println!("   {t}$ conscience reconcile{RESET}", t = text());
    let r = reconcile(&mut ds);
    render_report(&r);

    // ---- A2 · The Reckoning ---------------------------------------------
    scene(
        "A2 · The Reckoning   ·   an old promise confronts the edit about to break it",
        "you switch models mid-session  ·  /model claude-opus-4-8  — it never saw the argon2 call.",
    );
    let auth = for_file(&ds, "src/auth.rs");
    println!(
        "\n   {t}opus is about to edit src/auth.rs. before it runs, the pre-flight surfaces{RESET}",
        t = text()
    );
    println!("   {f}every decision recorded for this file — Tier 1, render-only, cannot be wrong:{RESET}\n", f = faint());
    println!("   {s}house rules · src/auth.rs{RESET}", s = sage());
    for d in &auth {
        println!(
            "     {t}{}{RESET} {f}— {} (by {}){RESET}",
            gist(&d.decision, 34),
            gist(&d.why, 46),
            d.model,
            t = text(),
            f = faint()
        );
    }

    println!("\n   {t}the proposed diff:{RESET}", t = text());
    println!("   {s}src/auth.rs:7{RESET}", s = sage());
    println!("   {f} 7{RESET} {bad}-   argon2id(pw, default_params()){RESET}", f = faint(), bad = bad());
    println!("   {f} 7{RESET} {ok}+   bcrypt(pw, cost(12)){RESET}", f = faint(), ok = ok());

    // Tier 2: the SAME editing model judges its own diff against the trail.
    // (In tomte this is one real completion to ctx.config.model — provider-agnostic.
    //  Here we show its shape and the answer it returns; the harness parses one line.)
    let star = ds.iter().find(|d| d.loc.starts_with("src/auth.rs")).unwrap();
    println!("\n   {f}Tier 2 — one cheap self-check to the editing model (it judges, not a substring lint):{RESET}", f = faint());
    println!(
        "   {t}self-check (claude-opus-4-8){RESET}  {bad}CONFLICT {ts} — swaps argon2id for bcrypt,{RESET}",
        t = text(),
        bad = bad(),
        ts = star.ts
    );
    println!("                                 {bad}the exact reversal gpt-5.5 recorded against.{RESET}", bad = bad());
    println!(
        "\n   {a}⚠ conflict{RESET}  {t}the edit reverses a recorded decision{RESET}",
        a = amber(),
        t = text()
    );
    println!(
        "   {f}[a] abort     [s] supersede + record why     [e] edit anyway (logged){RESET}",
        f = faint()
    );

    // ---- A3 · On the Record ---------------------------------------------
    scene(
        "A3 · On the Record   ·   only a human overturns a decision, and it is logged as one",
        "the human picks [s] — argon2 was dropped from the build, so bcrypt is the real call now.",
    );
    write_code("src/auth.rs", AUTH_V3); // apply the approved edit for real
    let new_ts = now_secs();
    ds.push(Decision {
        loc: "src/auth.rs:7".into(),
        decision: "hash with bcrypt(cost 12)".into(),
        why: "argon2 dep dropped from the build to shrink the binary; bcrypt is the team's vetted fallback".into(),
        rejected: vec![],
        model: "claude-opus-4-8".into(),
        ts: new_ts,
        anchor: Some("bcrypt(pw, cost(12))".into()),
        supersedes: Some(star.ts), // links to the promise it overturns
    });
    write_store(&ds);

    println!("\n   {f}the agent may NOT silently clear its own gate — the override lands in the{RESET}", f = faint());
    println!("   {f}Pillar-4 end-of-turn summary, where you can see it:{RESET}\n", f = faint());
    println!(
        "   {ok}✓{RESET} {t}edited src/auth.rs{RESET} {f}·{RESET} 1 file {f}·{RESET} {a}overturned a decision{RESET}",
        ok = ok(),
        t = text(),
        f = faint(),
        a = amber()
    );
    println!(
        "       {a}overturned{RESET} {t}gpt-5.5's \"hash with argon2id\"{RESET} {f}· why:{RESET} {t}argon2 dep dropped{RESET} {f}· by claude-opus-4-8{RESET}",
        a = amber(),
        t = text(),
        f = faint()
    );

    println!("\n   {f}the trail is now git-blame for conscience — the old promise is KEPT, not erased:{RESET}", f = faint());
    println!("   {t}$ conscience why src/auth.rs:7{RESET}\n", t = text());
    show_chain(&ds, "src/auth.rs:7");

    println!(
        "\n{f}— a promise one model made, another must answer to — and only you can clear it.{RESET}",
        f = faint()
    );
    println!(
        "{f}  the trail stops being a write-only graveyard and starts keeping its word.{RESET}",
        f = faint()
    );
    println!(
        "{f}  (headless? it narrates-and-proceeds and logs the override — readable after the fact.){RESET}",
        f = faint()
    );
}

/// Render a location and walk its supersede chain to the promise(s) it overturned.
fn show_chain(ds: &[Decision], loc: &str) {
    // Several records can share a loc after a supersede; show the CURRENT one
    // (newest ts) and walk back from it through the chain it overturned.
    let Some(cur) = ds.iter().filter(|d| d.loc == loc).max_by_key(|d| d.ts) else {
        println!("   {f}no decision recorded at {loc}.  try: conscience why --all{RESET}", f = faint());
        return;
    };
    detail(cur);
    let mut sup = cur.supersedes;
    while let Some(ts) = sup {
        match ds.iter().find(|d| d.ts == ts) {
            Some(old) => {
                println!(
                    "     {s}supersedes{RESET} {f}↑ {}'s \"{}\" ({}) — kept on the record, not deleted{RESET}",
                    old.model,
                    gist(&old.decision, 30),
                    ago(old.ts),
                    s = sage(),
                    f = faint()
                );
                sup = old.supersedes;
            }
            None => break,
        }
    }
}

// ---- standalone entry points (seed on demand, like why_trail) ---------------
fn ensure_seeded() -> Vec<Decision> {
    let existing = load();
    if existing.is_empty() {
        seed()
    } else {
        existing
    }
}

fn cmd_reconcile() {
    let mut ds = ensure_seeded();
    println!("{t}$ conscience reconcile{RESET}", t = text());
    let r = reconcile(&mut ds);
    render_report(&r);
}

fn cmd_why(loc: &str) {
    let ds = ensure_seeded();
    println!("{t}$ conscience why {loc}{RESET}", t = text());
    show_chain(&ds, loc);
}

fn cmd_all() {
    let ds = ensure_seeded();
    if ds.is_empty() {
        println!("{f}the trail is empty.{RESET}", f = faint());
        return;
    }
    let superseded: std::collections::HashSet<u64> =
        ds.iter().filter_map(|d| d.supersedes).collect();
    println!("{t}$ conscience why --all{RESET}\n", t = text());
    let w = ds.iter().map(|d| d.loc.chars().count()).max().unwrap_or(0);
    for d in &ds {
        let mark = if superseded.contains(&d.ts) {
            format!("{a}(overturned){RESET}", a = amber())
        } else if d.supersedes.is_some() {
            format!("{ok}(current){RESET}", ok = ok())
        } else {
            String::new()
        };
        println!(
            "   {s}{loc:<w$}{RESET}  {t}{g:<34}{RESET} {f}{model} · {when}{RESET} {mark}",
            s = sage(),
            loc = d.loc,
            t = text(),
            g = gist(&d.decision, 34),
            f = faint(),
            model = d.model,
            when = ago(d.ts),
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        None => walkthrough(),
        Some("reconcile") => cmd_reconcile(),
        Some("why") => match args.get(1).map(|s| s.as_str()) {
            Some("--all") | Some("-a") => cmd_all(),
            Some(loc) => cmd_why(loc),
            None => println!("usage: conscience why <loc>   |   conscience why --all"),
        },
        Some("--all") | Some("-a") => cmd_all(),
        Some(other) => {
            println!("unknown command {other:?}.  try: conscience | conscience reconcile | conscience why <loc>")
        }
    }
}

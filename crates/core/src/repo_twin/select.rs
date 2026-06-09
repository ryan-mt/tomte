//! The `why-context` engine — the headline query over the twin.
//!
//! Given a seed (a file, a `file:line` from a stack trace, or a symbol name), it
//! returns the files a maintainer *would* pull in and the nearby ones it leaves
//! out — every inclusion tagged with the index it came from (import / symbol /
//! test / git / decision) and every exclusion with the reason it's unreachable.
//! Nothing is invented: each reason points at a real edge, a real definition, a
//! real test, a real commit, or a recorded decision.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;

use super::RepoTwin;

/// Files in the ranked context, excluding the seed.
const MAX_SELECTED: usize = 15;
/// Symbols to trace references for when the seed is a *file* (prefer types, then
/// functions) — bounded so the reference scan stays cheap.
const MAX_REF_NAMES: usize = 6;
/// Don't read a file larger than this during reference scanning.
const MAX_SCAN_BYTES: u64 = 256 * 1024;
/// Nearby files to list as deliberately-excluded.
const MAX_IGNORED: usize = 6;
/// Convention rules to surface.
const MAX_RULES: usize = 6;

// Reason scoring weights.
const W_IMPORT_DEP: i64 = 50; // seed imports this file
const W_IMPORT_USE: i64 = 45; // this file imports the seed
const W_SYMBOL: i64 = 30; // this file references a seed symbol
const W_TEST: i64 = 25; // this file tests a selected file
const W_GIT_MAX: i64 = 10; // churn boost ceiling

/// One reason a file is in (or out of) the context, tagged with its source
/// index so the user can tell code-derived from git-derived from doc-derived.
#[derive(Debug, Clone, Serialize)]
pub struct Reason {
    /// `seed` | `import` | `symbol` | `test` | `git`.
    pub source: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeededFile {
    pub path: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectedFile {
    pub path: String,
    pub score: i64,
    pub reasons: Vec<Reason>,
    /// The subject of the most recent commit touching the file, if any.
    pub last_change: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IgnoredFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppliedRule {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionRef {
    pub loc: String,
    pub decision: String,
    pub why: String,
    pub model: String,
    /// `fresh` | `drifted` | `gone` | `unknown` — answers "which memory is stale?"
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    pub seed: String,
    /// `file` | `symbol` | `missing`.
    pub seed_kind: String,
    pub resolved_seeds: Vec<SeededFile>,
    pub selected: Vec<SelectedFile>,
    pub ignored: Vec<IgnoredFile>,
    pub rules: Vec<AppliedRule>,
    pub decisions: Vec<DecisionRef>,
    pub notes: Vec<String>,
}

/// Compute the context selection for `seed` against the built `twin`. `cwd` is
/// only used to read the decision trail for the seed files.
pub fn why_context(twin: &RepoTwin, cwd: &Path, seed: &str) -> Selection {
    let lookup = Lookup::new(twin);
    let seed = seed.trim();

    let (seed_kind, seed_files, symbol_names) = resolve_seed(twin, &lookup, seed);

    let mut notes = Vec::new();
    if twin.truncated {
        notes.push("repo is large — the index was truncated, so some edges may be missing".into());
    }
    if twin.git.is_empty() {
        notes.push("no git history available — the recency signal is off".into());
    }
    if seed_kind == "missing" {
        notes.push(format!(
            "could not resolve `{seed}` to a tracked file or symbol — run `tomte twin build` if the file is new"
        ));
    }

    let resolved_seeds: Vec<SeededFile> = seed_files
        .iter()
        .map(|f| SeededFile {
            path: f.clone(),
            detail: if seed_kind == "symbol" {
                format!("defines `{seed}`")
            } else {
                "seed file".into()
            },
        })
        .collect();

    // Accumulate reasons + scores per candidate file (excluding seed files).
    let mut acc: HashMap<String, (i64, Vec<Reason>)> = HashMap::new();
    let seed_set: HashSet<&str> = seed_files.iter().map(|s| s.as_str()).collect();
    let add = |acc: &mut HashMap<String, (i64, Vec<Reason>)>,
               path: &str,
               score: i64,
               source: &str,
               detail: String| {
        if seed_set.contains(path) {
            return;
        }
        let e = acc.entry(path.to_string()).or_insert((0, Vec::new()));
        e.0 += score;
        e.1.push(Reason {
            source: source.to_string(),
            detail,
        });
    };

    // Imports: dependencies (seed imports X) and dependents (X imports seed).
    for sf in &seed_files {
        for edge in lookup.imports_out.get(sf.as_str()).into_iter().flatten() {
            if let Some(to) = &edge.to {
                add(
                    &mut acc,
                    to,
                    W_IMPORT_DEP,
                    "import",
                    format!("{sf}:{} imports it", edge.line),
                );
            }
        }
        for (importer, line) in lookup.imports_in.get(sf.as_str()).into_iter().flatten() {
            add(
                &mut acc,
                importer,
                W_IMPORT_USE,
                "import",
                format!("imports the seed ({importer}:{line})"),
            );
        }
    }

    // Symbol graph: files that reference a seed symbol.
    for hit in scan_references(twin, &symbol_names, &seed_set) {
        let def = lookup
            .def_file_of(&hit.name)
            .map(|f| format!(" (defined in {f})"))
            .unwrap_or_default();
        add(
            &mut acc,
            &hit.file,
            W_SYMBOL,
            "symbol",
            format!("{}:{} references `{}`{def}", hit.file, hit.line, hit.name),
        );
    }

    // Tests: separate test files covering a file already in play. Inline
    // (`#[cfg(test)]`) edges are skipped — a file testing *itself* isn't another
    // file to pull in, and surfacing it on every selected file is pure noise.
    let in_play: HashSet<String> = acc
        .keys()
        .cloned()
        .chain(seed_files.iter().cloned())
        .collect();
    for covered in &in_play {
        for edge in lookup
            .tests_covering
            .get(covered.as_str())
            .into_iter()
            .flatten()
            .filter(|e| e.via != "inline")
        {
            add(
                &mut acc,
                &edge.test,
                W_TEST,
                "test",
                format!("regression coverage for {covered} (via {})", edge.via),
            );
        }
    }

    // Git churn boost for everything in the running.
    for (path, (score, _)) in acc.iter_mut() {
        if let Some(stat) = lookup.git.get(path.as_str()) {
            *score += (stat.commits as i64).min(W_GIT_MAX);
        }
    }

    // Rank and cap.
    let mut selected: Vec<SelectedFile> = acc
        .into_iter()
        .map(|(path, (score, reasons))| {
            let last_change = lookup
                .git
                .get(path.as_str())
                .map(|s| s.last_subject.clone())
                .filter(|s| !s.is_empty());
            SelectedFile {
                path,
                score,
                reasons,
                last_change,
            }
        })
        .collect();
    selected.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
    selected.truncate(MAX_SELECTED);

    let selected_paths: HashSet<&str> = selected.iter().map(|s| s.path.as_str()).collect();
    let ignored = ignored_neighbors(twin, &seed_files, &seed_set, &selected_paths);
    let rules = applicable_rules(twin, &seed_files);
    let decisions = seed_decisions(cwd, &seed_files);

    Selection {
        seed: seed.to_string(),
        seed_kind,
        resolved_seeds,
        selected,
        ignored,
        rules,
        decisions,
        notes,
    }
}

// ---- seed resolution --------------------------------------------------------

/// Resolve the seed into `(kind, seed_files, symbol_names_to_trace)`.
fn resolve_seed(
    twin: &RepoTwin,
    lookup: &Lookup,
    seed: &str,
) -> (String, Vec<String>, Vec<String>) {
    let norm = super::normalize(seed);
    // Strip a trailing `:NN` line suffix (a stack-trace location).
    let path_part = match norm.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => file,
        _ => norm.as_str(),
    };

    // File seed: exact path, else a unique suffix match (`session.ts` →
    // `src/auth/session.ts`).
    let mut file_matches: Vec<String> = Vec::new();
    if lookup.file_set.contains(path_part) {
        file_matches.push(path_part.to_string());
    } else {
        let suffix = format!("/{path_part}");
        for f in &twin.files {
            if f.path == path_part || f.path.ends_with(&suffix) {
                file_matches.push(f.path.clone());
            }
        }
    }
    if !file_matches.is_empty() {
        // Distinctive symbols defined in the seed files become the names we trace.
        let names = distinctive_symbols(twin, &file_matches);
        return ("file".into(), file_matches, names);
    }

    // Symbol seed: files that define a symbol with this name.
    let def_files: Vec<String> = twin
        .symbols
        .iter()
        .filter(|s| s.name == seed)
        .map(|s| s.file.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if !def_files.is_empty() {
        let mut files = def_files;
        files.sort();
        return ("symbol".into(), files, vec![seed.to_string()]);
    }

    // Unknown: maybe a brand-new file path not yet indexed.
    if path_part.contains('/') || path_part.contains('.') {
        return ("missing".into(), vec![path_part.to_string()], Vec::new());
    }
    ("missing".into(), Vec::new(), Vec::new())
}

/// The symbols worth tracing references for, for a file seed. To keep the symbol
/// graph *honest*, only **distinctive** names are traced: those defined in
/// exactly one file across the whole repo (so a reference unambiguously points
/// back to the seed) and long/Pascal-case enough not to collide with stdlib
/// method names like `.append()` or `.load()`. Types/classes first (they "flow"
/// across files), then sufficiently-specific functions, capped. Without this
/// filter a generic name like `append` produced false "references" at every
/// `vec.append(…)` call site.
fn distinctive_symbols(twin: &RepoTwin, files: &[String]) -> Vec<String> {
    // name → the set of files that define it, for global-uniqueness.
    let mut def_files: HashMap<&str, HashSet<&str>> = HashMap::new();
    for s in &twin.symbols {
        def_files
            .entry(s.name.as_str())
            .or_default()
            .insert(s.file.as_str());
    }

    let fileset: HashSet<&str> = files.iter().map(|s| s.as_str()).collect();
    let mut types = Vec::new();
    let mut fns = Vec::new();
    let mut seen = HashSet::new();
    for s in &twin.symbols {
        if !fileset.contains(s.file.as_str()) || !seen.insert(s.name.clone()) {
            continue;
        }
        // A reference to a name defined in more than one file can't be attributed
        // to the seed without guessing — skip it.
        if def_files.get(s.name.as_str()).map(|f| f.len()).unwrap_or(0) != 1 {
            continue;
        }
        match s.kind.as_str() {
            "type" | "class" if s.name.len() >= 4 => types.push(s.name.clone()),
            "fn" if s.name.len() >= 8 => fns.push(s.name.clone()),
            _ => {}
        }
    }
    types.extend(fns);
    types.truncate(MAX_REF_NAMES);
    types
}

// ---- symbol reference scan --------------------------------------------------

struct RefHit {
    file: String,
    line: usize,
    name: String,
}

/// Scan source files for whole-word references to any of `names`, skipping the
/// seed files themselves. One alternation regex over the names keeps it to a
/// single pass per file. Bounded by file size; the twin's file list is already
/// capped at build time.
fn scan_references(twin: &RepoTwin, names: &[String], seed_set: &HashSet<&str>) -> Vec<RefHit> {
    if names.is_empty() {
        return Vec::new();
    }
    let pattern = format!(
        r"\b(?:{})\b",
        names
            .iter()
            .map(|n| regex::escape(n))
            .collect::<Vec<_>>()
            .join("|")
    );
    let Ok(re) = Regex::new(&pattern) else {
        return Vec::new();
    };
    let root = Path::new(&twin.root);
    let mut hits = Vec::new();
    let mut counted: HashSet<(String, String)> = HashSet::new();
    for f in &twin.files {
        if !f.lang.is_source() || seed_set.contains(f.path.as_str()) {
            continue;
        }
        let abs = root.join(&f.path);
        if std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0) > MAX_SCAN_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&abs) else {
            continue;
        };
        // First reference that isn't a method/field access (`x.name`) — a
        // leading `.` means it's `obj.name`, not a use of the seed's symbol.
        if let Some(m) = re
            .find_iter(&text)
            .find(|m| !preceded_by_dot(&text, m.start()))
        {
            let name = m.as_str().to_string();
            // One hit per (file, symbol) — the first real reference justifies it.
            if counted.insert((f.path.clone(), name.clone())) {
                hits.push(RefHit {
                    file: f.path.clone(),
                    line: super::line_at(&text, m.start()),
                    name,
                });
            }
        }
    }
    hits
}

/// Whether the byte just before `at` is a `.` — i.e. the match is `obj.name`
/// (a field/method access), not a standalone reference to the symbol. A `::`
/// path (`module::name`) ends in `:`, not `.`, so it's correctly kept.
fn preceded_by_dot(text: &str, at: usize) -> bool {
    text[..at].bytes().next_back() == Some(b'.')
}

// ---- ignored neighbors ------------------------------------------------------

static LEGACY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(old|legacy|deprecated|backup|bak|copy|unused|v\d+)\b").unwrap()
});

/// Source files sitting next to the seed (same directory) that the graph did
/// NOT pull in — the honest "examined and excluded" set. A name that looks
/// superseded (old/legacy/…) gets a sharper reason.
fn ignored_neighbors(
    twin: &RepoTwin,
    seed_files: &[String],
    seed_set: &HashSet<&str>,
    selected: &HashSet<&str>,
) -> Vec<IgnoredFile> {
    let dirs: HashSet<&str> = seed_files
        .iter()
        .map(|f| f.rsplit_once('/').map(|(d, _)| d).unwrap_or(""))
        .collect();
    let mut out = Vec::new();
    for f in &twin.files {
        if out.len() >= MAX_IGNORED {
            break;
        }
        if !f.lang.is_source() {
            continue;
        }
        let path = f.path.as_str();
        if seed_set.contains(path) || selected.contains(path) {
            continue;
        }
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        if !dirs.contains(dir) {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        let reason = if LEGACY_RE.is_match(name) {
            "name looks superseded, and no import or symbol path reaches the seed".to_string()
        } else {
            "sits beside the seed but no import or symbol path reaches it".to_string()
        };
        out.push(IgnoredFile {
            path: path.to_string(),
            reason,
        });
    }
    out
}

// ---- rules + decisions ------------------------------------------------------

/// Convention rules relevant to the seed: those whose text mentions a seed
/// file's name or directory, then top project-wide rules to fill out the list.
fn applicable_rules(twin: &RepoTwin, seed_files: &[String]) -> Vec<AppliedRule> {
    let mut tokens: HashSet<String> = HashSet::new();
    for f in seed_files {
        let name = f.rsplit('/').next().unwrap_or(f);
        let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
        tokens.insert(stem.to_ascii_lowercase());
        for seg in f.split('/') {
            if seg.len() >= 3 && !seg.contains('.') {
                tokens.insert(seg.to_ascii_lowercase());
            }
        }
    }

    let mut out = Vec::new();
    let mut seen: HashSet<(String, usize)> = HashSet::new();
    // Pass 1: rules that mention a seed token.
    for doc in &twin.rules {
        for r in &doc.rules {
            let lc = r.text.to_ascii_lowercase();
            if let Some(tok) = tokens.iter().find(|t| lc.contains(t.as_str())) {
                if seen.insert((doc.file.clone(), r.line)) {
                    out.push(AppliedRule {
                        file: doc.file.clone(),
                        line: r.line,
                        text: r.text.clone(),
                        why: format!("mentions `{tok}`"),
                    });
                }
            }
            if out.len() >= MAX_RULES {
                return out;
            }
        }
    }
    // Pass 2: fill with the first project-wide rules from the top-priority doc.
    for doc in &twin.rules {
        for r in &doc.rules {
            if out.len() >= MAX_RULES {
                return out;
            }
            if seen.insert((doc.file.clone(), r.line)) {
                out.push(AppliedRule {
                    file: doc.file.clone(),
                    line: r.line,
                    text: r.text.clone(),
                    why: "project-wide convention".into(),
                });
            }
        }
    }
    out
}

/// Recorded decisions on the seed files, each with a freshness state so the user
/// can see which "memory" has drifted from the code it describes.
fn seed_decisions(cwd: &Path, seed_files: &[String]) -> Vec<DecisionRef> {
    let mut out = Vec::new();
    for f in seed_files {
        for rec in crate::decisions::for_file(cwd, f) {
            out.push(DecisionRef {
                state: decision_state(cwd, &rec).into(),
                loc: rec.loc,
                decision: rec.decision,
                why: rec.why,
                model: rec.model,
            });
        }
    }
    out
}

/// Freshness of a decision against the working tree, using only public record
/// fields: `fresh` (anchor still on its line), `drifted` (anchor moved
/// elsewhere in the file), `gone` (anchor not found), `unknown` (no anchor or no
/// line to check). The read-only sibling of `tomte why --reconcile`.
fn decision_state(cwd: &Path, rec: &crate::decisions::DecisionRecord) -> &'static str {
    let Some(anchor) = rec.anchor.as_deref() else {
        return "unknown";
    };
    let (file, line) = match rec.loc.rsplit_once(':') {
        Some((f, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (f, n.parse::<usize>().unwrap_or(0))
        }
        _ => return "unknown",
    };
    if line == 0 {
        return "unknown";
    }
    let Ok(text) = std::fs::read_to_string(cwd.join(file)) else {
        return "gone";
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.get(line - 1).map(|l| l.trim()) == Some(anchor) {
        return "fresh";
    }
    if lines.iter().any(|l| l.trim() == anchor) {
        "drifted"
    } else {
        "gone"
    }
}

// ---- lookups ----------------------------------------------------------------

/// Pre-built indices over a `RepoTwin` for O(1) lookups during selection.
struct Lookup<'a> {
    file_set: HashSet<&'a str>,
    imports_out: HashMap<&'a str, Vec<&'a super::ImportEdge>>,
    imports_in: HashMap<&'a str, Vec<(&'a str, usize)>>,
    def_files_by_name: HashMap<&'a str, &'a str>,
    tests_covering: HashMap<&'a str, Vec<&'a super::TestEdge>>,
    git: HashMap<&'a str, &'a super::GitStat>,
}

impl<'a> Lookup<'a> {
    fn new(twin: &'a RepoTwin) -> Self {
        let file_set: HashSet<&str> = twin.files.iter().map(|f| f.path.as_str()).collect();
        let mut imports_out: HashMap<&str, Vec<&super::ImportEdge>> = HashMap::new();
        let mut imports_in: HashMap<&str, Vec<(&str, usize)>> = HashMap::new();
        for e in &twin.imports {
            if let Some(to) = &e.to {
                imports_out.entry(e.from.as_str()).or_default().push(e);
                imports_in
                    .entry(to.as_str())
                    .or_default()
                    .push((e.from.as_str(), e.line));
            }
        }
        let mut def_files_by_name: HashMap<&str, &str> = HashMap::new();
        for s in &twin.symbols {
            def_files_by_name
                .entry(s.name.as_str())
                .or_insert(s.file.as_str());
        }
        let mut tests_covering: HashMap<&str, Vec<&super::TestEdge>> = HashMap::new();
        for t in &twin.tests {
            tests_covering.entry(t.covers.as_str()).or_default().push(t);
        }
        let git: HashMap<&str, &super::GitStat> =
            twin.git.iter().map(|g| (g.file.as_str(), g)).collect();
        Lookup {
            file_set,
            imports_out,
            imports_in,
            def_files_by_name,
            tests_covering,
            git,
        }
    }

    fn def_file_of(&self, name: &str) -> Option<&str> {
        self.def_files_by_name.get(name).copied()
    }
}

// ---- rendering --------------------------------------------------------------

/// Render the selection as the human `tomte why-context` card.
pub fn render(sel: &Selection) -> String {
    let mut out = String::new();
    out.push_str(&format!("Context X-Ray for `{}`\n", sel.seed));

    if sel.resolved_seeds.is_empty() {
        out.push_str("\nCould not resolve the seed to any file or symbol.\n");
    } else {
        out.push_str("\nSeed:\n");
        for s in &sel.resolved_seeds {
            out.push_str(&format!("  • {} — {}\n", s.path, s.detail));
        }
    }

    out.push_str("\nSelected (would pull into context):\n");
    if sel.selected.is_empty() {
        out.push_str("  (nothing else is connected to the seed)\n");
    }
    for f in &sel.selected {
        out.push_str(&format!("  • {}\n", f.path));
        for r in &f.reasons {
            out.push_str(&format!("      because {} [{}]\n", r.detail, r.source));
        }
        if let Some(c) = &f.last_change {
            out.push_str(&format!("      last change: {c} [git]\n"));
        }
    }

    if !sel.ignored.is_empty() {
        out.push_str("\nIgnored (nearby but left out):\n");
        for f in &sel.ignored {
            out.push_str(&format!("  • {} — {}\n", f.path, f.reason));
        }
    }

    if !sel.rules.is_empty() {
        out.push_str("\nProject conventions in effect:\n");
        for r in &sel.rules {
            out.push_str(&format!(
                "  • {} ({}:{}) — {}\n",
                r.text, r.file, r.line, r.why
            ));
        }
    }

    if !sel.decisions.is_empty() {
        out.push_str("\nRecorded decisions on the seed:\n");
        for d in &sel.decisions {
            let flag = match d.state.as_str() {
                "drifted" => " ⚠ drifted",
                "gone" => " ⚠ stale (code gone)",
                _ => "",
            };
            out.push_str(&format!(
                "  • {} — {} (why: {}; by {}){flag}\n",
                d.loc, d.decision, d.why, d.model
            ));
        }
    }

    for n in &sel.notes {
        out.push_str(&format!("\nnote: {n}\n"));
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_twin::build;

    /// A small TS project mirroring the pitch: a session importing a User type,
    /// a test covering the session, and a legacy file that nothing reaches.
    fn fixture() -> (tempfile::TempDir, RepoTwin) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/auth")).unwrap();
        std::fs::create_dir_all(root.join("src/db")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("src/auth/session.ts"),
            "import { User } from '../db/user';\nexport function createSession(u: User) { return u; }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/db/user.ts"),
            "export interface User { id: string }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/auth.test.ts"),
            "import { createSession } from '../src/auth/session';\ntest('x', () => createSession({id:'1'}));\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/auth/auth-old.ts"),
            "export function legacyLogin() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("AGENTS.md"), "- session tokens expire in 1h\n").unwrap();
        let twin = build(root).unwrap();
        (tmp, twin)
    }

    #[test]
    fn file_seed_pulls_deps_dependents_and_tests() {
        let (tmp, twin) = fixture();
        let sel = why_context(&twin, tmp.path(), "src/auth/session.ts");
        assert_eq!(sel.seed_kind, "file");
        assert_eq!(sel.resolved_seeds[0].path, "src/auth/session.ts");

        let paths: Vec<&str> = sel.selected.iter().map(|s| s.path.as_str()).collect();
        // Dependency: session imports the User type's file.
        assert!(paths.contains(&"src/db/user.ts"), "deps: {paths:?}");
        // Dependent + test: the test imports the session.
        assert!(paths.contains(&"tests/auth.test.ts"), "tests: {paths:?}");

        // The User-type file's reason names the import edge.
        let user = sel
            .selected
            .iter()
            .find(|s| s.path == "src/db/user.ts")
            .unwrap();
        assert!(user.reasons.iter().any(|r| r.source == "import"));
    }

    #[test]
    fn legacy_neighbor_is_listed_as_ignored() {
        let (tmp, twin) = fixture();
        let sel = why_context(&twin, tmp.path(), "src/auth/session.ts");
        let ignored: Vec<&str> = sel.ignored.iter().map(|f| f.path.as_str()).collect();
        assert!(
            ignored.contains(&"src/auth/auth-old.ts"),
            "ignored: {ignored:?}"
        );
        let legacy = sel
            .ignored
            .iter()
            .find(|f| f.path == "src/auth/auth-old.ts")
            .unwrap();
        assert!(legacy.reason.contains("superseded"));
    }

    #[test]
    fn symbol_seed_resolves_to_its_definition_file() {
        let (tmp, twin) = fixture();
        let sel = why_context(&twin, tmp.path(), "User");
        assert_eq!(sel.seed_kind, "symbol");
        assert_eq!(sel.resolved_seeds[0].path, "src/db/user.ts");
        // The session references `User`, so it's selected via the symbol graph.
        let session = sel
            .selected
            .iter()
            .find(|s| s.path == "src/auth/session.ts");
        assert!(session.is_some(), "session should reference User");
        assert!(session
            .unwrap()
            .reasons
            .iter()
            .any(|r| r.source == "symbol" || r.source == "import"));
    }

    #[test]
    fn conventions_surface_for_the_seed() {
        let (tmp, twin) = fixture();
        let sel = why_context(&twin, tmp.path(), "src/auth/session.ts");
        // The AGENTS.md rule mentioning "session" is surfaced.
        assert!(sel.rules.iter().any(|r| r.text.contains("session tokens")));
    }

    #[test]
    fn missing_seed_is_reported_not_panicked() {
        let (tmp, twin) = fixture();
        let sel = why_context(&twin, tmp.path(), "doesNotExistAnywhere");
        assert_eq!(sel.seed_kind, "missing");
        assert!(sel.notes.iter().any(|n| n.contains("could not resolve")));
        // Rendering a missing selection is still valid text.
        assert!(render(&sel).contains("Could not resolve"));
    }
}

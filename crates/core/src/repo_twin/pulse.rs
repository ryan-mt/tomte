//! Repo Pulse — the hearth report: which files are most likely to break next,
//! measured from the twin's own indexes rather than guessed by a model.
//!
//! The score is deliberately simple and fully shown on the card:
//!
//! ```text
//! risk = commits-in-window × (importers + 1) × (2 when untested)
//! ```
//!
//! - **change heat** — commits touching the file in the twin's recent git
//!   window ([`super::GitStat::commits`]); code that churns is code in play.
//! - **blast radius** — how many repo files import it
//!   ([`super::ImportEdge`] fan-in); a hot file other files lean on breaks
//!   louder.
//! - **missing tests** — no [`super::TestEdge`] covers it, so a regression
//!   lands silently; that doubles the score.
//!
//! Every factor is a real index entry, so the verdict is reproducible — rerun
//! it, get the same list, argue with the numbers instead of with a vibe.

use serde::Serialize;

use super::RepoTwin;

/// How many files the rendered card lists. The JSON report carries the same
/// cap — pulse is a "where do I look first" answer, not a database dump.
const MAX_ENTRIES: usize = 10;

/// One scored file on the pulse card.
#[derive(Debug, Clone, Serialize)]
pub struct PulseEntry {
    pub file: String,
    /// Commits touching the file in the twin's recent git window.
    pub commits: u32,
    /// Subject line of the most recent commit that touched it.
    pub last_subject: String,
    /// Repo files with a resolved import of this file (fan-in).
    pub importers: usize,
    /// True when at least one test→source edge covers the file.
    pub tested: bool,
    pub loc: usize,
    /// `commits × (importers + 1) × (2 when untested)` — shown on the card.
    pub risk: u32,
}

/// The whole pulse: the top-risk files plus the two one-line vitals.
#[derive(Debug, Clone, Serialize)]
pub struct PulseReport {
    pub root: String,
    /// Top entries, sorted by risk (desc), capped at [`MAX_ENTRIES`].
    pub entries: Vec<PulseEntry>,
    /// Source files with ≥2 recent commits and no covering test.
    pub hot_untested: usize,
    /// The most-imported source file and its importer count.
    pub widest_blast: Option<WidestBlast>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WidestBlast {
    pub file: String,
    pub importers: usize,
}

/// Score the twin. Pure — reads only the already-built indexes, so it costs
/// nothing beyond the cache load and never shells out.
pub fn pulse(twin: &RepoTwin) -> PulseReport {
    use std::collections::{HashMap, HashSet};

    // Fan-in per file from resolved import edges.
    let mut importers: HashMap<&str, HashSet<&str>> = HashMap::new();
    for e in &twin.imports {
        if let Some(to) = &e.to {
            importers.entry(to).or_default().insert(e.from.as_str());
        }
    }
    // Covered files from the test map.
    let covered: HashSet<&str> = twin.tests.iter().map(|t| t.covers.as_str()).collect();
    // Git heat by file.
    let heat: HashMap<&str, &super::GitStat> =
        twin.git.iter().map(|g| (g.file.as_str(), g)).collect();

    let mut entries: Vec<PulseEntry> = Vec::new();
    let mut hot_untested = 0usize;
    for f in &twin.files {
        if !f.lang.is_source() || f.is_test {
            continue;
        }
        let Some(stat) = heat.get(f.path.as_str()) else {
            continue; // no recent commits — not in play
        };
        if stat.commits == 0 {
            continue;
        }
        let fan_in = importers.get(f.path.as_str()).map_or(0, |s| s.len());
        let tested = covered.contains(f.path.as_str());
        if stat.commits >= 2 && !tested {
            hot_untested += 1;
        }
        let untested_factor = if tested { 1 } else { 2 };
        let risk = stat.commits * (fan_in as u32 + 1) * untested_factor;
        entries.push(PulseEntry {
            file: f.path.clone(),
            commits: stat.commits,
            last_subject: stat.last_subject.clone(),
            importers: fan_in,
            tested,
            loc: f.loc,
            risk,
        });
    }

    // Deterministic order: risk, then heat, then path — equal twins render
    // byte-identical cards.
    entries.sort_by(|a, b| {
        b.risk
            .cmp(&a.risk)
            .then(b.commits.cmp(&a.commits))
            .then(a.file.cmp(&b.file))
    });
    entries.truncate(MAX_ENTRIES);

    let widest_blast = {
        let mut widest: Option<WidestBlast> = None;
        for f in &twin.files {
            if !f.lang.is_source() || f.is_test {
                continue;
            }
            let n = importers.get(f.path.as_str()).map_or(0, |s| s.len());
            let beats = widest
                .as_ref()
                .is_none_or(|w| n > w.importers || (n == w.importers && f.path < w.file));
            // A fan-in of 1 says nothing (in a Rust mod tree every file has
            // exactly one declaring parent) — the vital only earns its line
            // when something genuinely concentrates dependents.
            if n >= 2 && beats {
                widest = Some(WidestBlast {
                    file: f.path.clone(),
                    importers: n,
                });
            }
        }
        widest
    };

    PulseReport {
        root: twin.root.clone(),
        entries,
        hot_untested,
        widest_blast,
    }
}

/// The text card `tomte pulse` and `/pulse` share.
pub fn render(report: &PulseReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Repo Pulse — {}\n", report.root));
    if report.entries.is_empty() {
        out.push_str(
            "  No recent change activity in the twin's git window — nothing is in play.\n  \
             (After new commits, `tomte twin --rebuild` refreshes the map.)",
        );
        return out;
    }
    out.push_str(
        "  Files most likely to break next — measured, not guessed:\n  \
         risk = commits in the recent window × (importers + 1) × 2 when untested\n\n",
    );
    for (i, e) in report.entries.iter().enumerate() {
        let cover = if e.tested { "tested" } else { "untested ⚠" };
        out.push_str(&format!(
            "  {:>2}. {}\n      risk {:>4} = {}c × {}i × {} · {} loc\n      last: {}\n",
            i + 1,
            e.file,
            e.risk,
            e.commits,
            e.importers + 1,
            cover,
            e.loc,
            e.last_subject,
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "  hot & untested : {} source file(s) changed ≥2× recently with no covering test\n",
        report.hot_untested
    ));
    if let Some(w) = &report.widest_blast {
        out.push_str(&format!(
            "  widest blast   : {} — imported by {} file(s)\n",
            w.file, w.importers
        ));
    }
    out.push_str("  (scored from the cached twin; `tomte twin --rebuild` re-scans)");
    out
}

#[cfg(test)]
mod tests {
    use super::super::{FileNode, GitStat, ImportEdge, Lang, RepoTwin, TestEdge};
    use super::*;

    fn file(path: &str, is_test: bool) -> FileNode {
        FileNode {
            path: path.into(),
            lang: Lang::of(path),
            is_test,
            loc: 100,
        }
    }

    fn stat(file: &str, commits: u32) -> GitStat {
        GitStat {
            file: file.into(),
            commits,
            last_ts: 1,
            last_subject: format!("touch {file}"),
        }
    }

    fn import(from: &str, to: &str) -> ImportEdge {
        ImportEdge {
            from: from.into(),
            raw: to.into(),
            to: Some(to.into()),
            line: 1,
        }
    }

    fn twin(
        files: Vec<FileNode>,
        imports: Vec<ImportEdge>,
        tests: Vec<TestEdge>,
        git: Vec<GitStat>,
    ) -> RepoTwin {
        RepoTwin {
            version: 1,
            root: "/repo".into(),
            built_at_ms: 0,
            fingerprint: String::new(),
            truncated: false,
            files,
            imports,
            symbols: vec![],
            tests,
            git,
            rules: vec![],
        }
    }

    // The headline property: risk = commits × (importers+1) × 2-when-untested,
    // sorted descending — a hot, depended-on, untested file tops the card.
    #[test]
    fn scores_compose_heat_fanin_and_missing_tests() {
        let t = twin(
            vec![
                file("a.rs", false),
                file("b.rs", false),
                file("c.rs", false),
            ],
            // a.rs imported by b.rs and c.rs; b.rs imported by c.rs.
            vec![
                import("b.rs", "a.rs"),
                import("c.rs", "a.rs"),
                import("c.rs", "b.rs"),
            ],
            // b.rs is covered by a test; a.rs and c.rs are not.
            vec![TestEdge {
                test: "tests/b.rs".into(),
                covers: "b.rs".into(),
                via: "name".into(),
            }],
            vec![stat("a.rs", 3), stat("b.rs", 5), stat("c.rs", 1)],
        );
        let p = pulse(&t);
        // a.rs: 3 × (2+1) × 2 = 18; b.rs: 5 × (1+1) × 1 = 10; c.rs: 1 × 1 × 2 = 2.
        assert_eq!(p.entries[0].file, "a.rs");
        assert_eq!(p.entries[0].risk, 18);
        assert!(!p.entries[0].tested);
        assert_eq!(p.entries[1].file, "b.rs");
        assert_eq!(p.entries[1].risk, 10);
        assert!(p.entries[1].tested);
        assert_eq!(p.entries[2].risk, 2);
        // a.rs is the only hot (≥2 commits) untested source file.
        assert_eq!(p.hot_untested, 1);
        let w = p.widest_blast.expect("a.rs has fan-in");
        assert_eq!((w.file.as_str(), w.importers), ("a.rs", 2));
    }

    // Test files and non-source files never appear on the card, and a file with
    // no recent commits isn't "in play" no matter how depended-on it is.
    #[test]
    fn ignores_tests_non_source_and_cold_files() {
        let t = twin(
            vec![
                file("hot.rs", false),
                file("cold.rs", false),
                file("tests/hot.rs", true),
                file("notes.txt", false),
            ],
            vec![
                import("hot.rs", "cold.rs"),
                import("tests/hot.rs", "cold.rs"),
            ],
            vec![],
            vec![
                stat("hot.rs", 2),
                stat("tests/hot.rs", 9),
                stat("notes.txt", 9),
            ],
        );
        let p = pulse(&t);
        assert_eq!(p.entries.len(), 1, "only hot.rs is a scored source file");
        assert_eq!(p.entries[0].file, "hot.rs");
        // cold.rs (0 recent commits) is still the widest blast radius.
        let w = p.widest_blast.expect("cold.rs has importers");
        assert_eq!((w.file.as_str(), w.importers), ("cold.rs", 2));
    }

    // Equal-risk entries order by path so the card is byte-stable run to run,
    // and the list is capped.
    #[test]
    fn deterministic_order_and_cap() {
        let mut files = vec![];
        let mut git = vec![];
        for i in 0..15 {
            let name = format!("m{i:02}.rs");
            files.push(file(&name, false));
            git.push(stat(&name, 1));
        }
        let t = twin(files, vec![], vec![], git);
        let p = pulse(&t);
        assert_eq!(p.entries.len(), 10, "capped at MAX_ENTRIES");
        let order: Vec<_> = p.entries.iter().map(|e| e.file.clone()).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "ties break by path");
    }

    // A fan-in of exactly 1 (every Rust file's declaring parent) earns no
    // widest-blast vital — the line appears only when dependents concentrate.
    #[test]
    fn widest_blast_needs_at_least_two_importers() {
        let t = twin(
            vec![file("a.rs", false), file("b.rs", false)],
            vec![import("b.rs", "a.rs")],
            vec![],
            vec![stat("a.rs", 1)],
        );
        assert!(pulse(&t).widest_blast.is_none());
    }

    // An empty/cold twin renders the calm no-activity card instead of an empty
    // table, and a populated one shows the formula and vitals.
    #[test]
    fn render_covers_empty_and_populated() {
        let cold = twin(vec![file("a.rs", false)], vec![], vec![], vec![]);
        let card = render(&pulse(&cold));
        assert!(card.contains("No recent change activity"));

        let hot = twin(
            vec![file("a.rs", false)],
            vec![],
            vec![],
            vec![stat("a.rs", 4)],
        );
        let card = render(&pulse(&hot));
        assert!(card.contains("risk = commits"));
        assert!(card.contains("a.rs"));
        assert!(card.contains("4c × 1i × untested ⚠"));
        assert!(card.contains("hot & untested : 1"));
    }
}

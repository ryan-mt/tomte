//! `tomte why diff <base>` — review the *reasoning*, not just the code.
//!
//! A PR review reads the diff; this reads the decision trail against the same
//! range and answers the three questions a reviewer can't get from the diff
//! alone:
//!
//! - **which decisions are new** — recorded since the merge-base, each flagged
//!   when it points outside the changed files (reasoning with no matching code
//!   is worth a look too);
//! - **which earlier decisions were superseded** in this range — promises
//!   deliberately broken, shown as old → new;
//! - **which changed files carry no recorded why at all** — code that moved
//!   with no reasoning on the trail, the gaps a reviewer should poke at.
//!
//! Everything is computed from real state: the merge-base and changed files
//! from git, the decisions from the project's own trail. The analysis core is
//! pure ([`analyze`]) so every rule is unit-tested without a repo.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::decisions::{self, DecisionRecord};

/// One superseded pair: the decision that was overturned and the newer one
/// (recorded in this range) that overturned it.
#[derive(Debug, Clone, Serialize)]
pub struct SupersededPair {
    pub old: DecisionRecord,
    pub new: DecisionRecord,
}

/// The reasoning-review report for one diff range.
#[derive(Debug, Clone, Serialize)]
pub struct WhyDiff {
    /// The base the user asked about (e.g. `main`).
    pub base: String,
    /// `<short-hash>` of the merge-base the range was computed from.
    pub merge_base: String,
    /// Files changed between the merge-base and the working tree (committed
    /// and uncommitted), plus untracked files — sorted.
    pub changed_files: Vec<String>,
    /// Decisions recorded since the merge-base commit, oldest first. The bool
    /// is true when the decision's file is part of the diff.
    pub new_decisions: Vec<(DecisionRecord, bool)>,
    /// Decisions superseded by a decision recorded in this range.
    pub superseded: Vec<SupersededPair>,
    /// Changed files with no decision anywhere on the trail.
    pub files_without_why: Vec<String>,
}

/// Run git in `root`; trimmed stdout on success, the trimmed stderr as `Err`.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(root);
    crate::secret_env::scrub_secret_env_std(&mut cmd);
    let out = cmd.output().map_err(|e| format!("git did not run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Bases tried, in order, when the user names none.
const DEFAULT_BASES: &[&str] = &["origin/main", "main", "origin/master", "master"];

/// Collect the report for `base` (or the first default base that resolves).
/// Errors carry the exact git complaint — outside a repo, unknown base — so the
/// CLI can print it verbatim.
pub fn collect(cwd: &Path, base: Option<&str>) -> Result<WhyDiff, String> {
    let base = match base {
        Some(b) => b.trim().to_string(),
        None => DEFAULT_BASES
            .iter()
            .find(|b| {
                git(
                    cwd,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{b}^{{commit}}"),
                    ],
                )
                .is_ok()
            })
            .map(|b| b.to_string())
            .ok_or_else(|| {
                format!(
                    "no default base found (tried {}) — name one: `tomte why diff <base>`",
                    DEFAULT_BASES.join(", ")
                )
            })?,
    };

    let merge_base = git(cwd, &["merge-base", &base, "HEAD"]).map_err(|e| {
        if e.is_empty() {
            format!(
                "`{base}` has no merge-base with HEAD (not a git repository, or no such revision)"
            )
        } else {
            e
        }
    })?;
    // Merge-base commit time (seconds → ms): the line between "decided before
    // this range" and "decided in it".
    let base_ts_ms = git(cwd, &["show", "-s", "--format=%ct", &merge_base])?
        .parse::<u64>()
        .map(|s| s * 1000)
        .map_err(|e| format!("unreadable merge-base timestamp: {e}"))?;

    // Changed = tracked changes vs the merge-base (committed AND uncommitted,
    // one query) + untracked files (a brand-new file is part of the diff too).
    let mut changed: Vec<String> = git(cwd, &["diff", "--name-only", &merge_base])?
        .lines()
        .map(str::to_string)
        .collect();
    changed.extend(
        git(cwd, &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .map(str::to_string),
    );
    changed.retain(|f| !f.is_empty());
    changed.sort();
    changed.dedup();

    let records = decisions::load(cwd);
    let short = git(cwd, &["rev-parse", "--short", &merge_base]).unwrap_or(merge_base);
    Ok(analyze(base, short, records, changed, base_ts_ms))
}

/// The pure analysis: split the trail around `base_ts_ms`, pair supersedes,
/// and find changed files with no recorded why. Sorted and deterministic.
pub fn analyze(
    base: String,
    merge_base: String,
    records: Vec<DecisionRecord>,
    changed_files: Vec<String>,
    base_ts_ms: u64,
) -> WhyDiff {
    let changed_set: HashSet<String> = changed_files
        .iter()
        .map(|f| decisions::normalize_file(f))
        .collect();
    let in_diff =
        |loc: &str| changed_set.contains(&decisions::normalize_file(decisions::parse_loc(loc).0));

    let mut new_decisions: Vec<(DecisionRecord, bool)> = records
        .iter()
        .filter(|d| d.ts > base_ts_ms)
        .map(|d| (d.clone(), in_diff(&d.loc)))
        .collect();
    new_decisions.sort_by_key(|(d, _)| d.ts);

    // A pair counts when the SUPERSEDING decision lands in this range; the
    // superseded one may be arbitrarily old.
    let mut superseded: Vec<SupersededPair> = records
        .iter()
        .filter(|d| d.ts > base_ts_ms)
        .filter_map(|new| {
            let old_ts = new.supersedes?;
            let old = records.iter().find(|d| d.ts == old_ts)?;
            Some(SupersededPair {
                old: old.clone(),
                new: new.clone(),
            })
        })
        .collect();
    superseded.sort_by_key(|p| p.new.ts);

    // Whole-trail file coverage: any decision on the file (old or new) counts
    // as a recorded why; the gap list is for files with none at all.
    let covered: HashSet<String> = records
        .iter()
        .map(|d| decisions::normalize_file(decisions::parse_loc(&d.loc).0))
        .collect();
    let files_without_why: Vec<String> = changed_files
        .iter()
        .filter(|f| !covered.contains(&decisions::normalize_file(f)))
        .cloned()
        .collect();

    WhyDiff {
        base,
        merge_base,
        changed_files,
        new_decisions,
        superseded,
        files_without_why,
    }
}

/// Render the report as the review card. Calm when there is nothing to say.
pub fn render(r: &WhyDiff) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Why diff — vs {} (merge-base {}) · {} changed file(s)\n",
        r.base,
        r.merge_base,
        r.changed_files.len()
    ));

    out.push_str(&format!(
        "\nNew decisions in this range ({}):\n",
        r.new_decisions.len()
    ));
    if r.new_decisions.is_empty() {
        out.push_str("  none recorded — if this diff makes a non-obvious call, record it (record_decision)\n");
    } else {
        for (d, in_diff) in &r.new_decisions {
            let mark = if *in_diff {
                ""
            } else {
                "  (outside this diff)"
            };
            out.push_str(&format!(
                "  {} — {} (why: {}; by {}){mark}\n",
                d.loc, d.decision, d.why, d.model
            ));
        }
    }

    if !r.superseded.is_empty() {
        out.push_str(&format!(
            "\nSuperseded in this range ({}) — promises deliberately broken:\n",
            r.superseded.len()
        ));
        for p in &r.superseded {
            out.push_str(&format!(
                "  {} — was: {}\n            now: {} (why: {}; by {})\n",
                p.old.loc, p.old.decision, p.new.decision, p.new.why, p.new.model
            ));
        }
    }

    out.push_str(&format!(
        "\nChanged without a recorded why ({} of {}):\n",
        r.files_without_why.len(),
        r.changed_files.len()
    ));
    if r.files_without_why.is_empty() {
        out.push_str("  every changed file carries at least one recorded decision\n");
    } else {
        const MAX_FILES: usize = 30;
        for f in r.files_without_why.iter().take(MAX_FILES) {
            out.push_str(&format!("  {f}\n"));
        }
        if r.files_without_why.len() > MAX_FILES {
            out.push_str(&format!(
                "  … and {} more\n",
                r.files_without_why.len() - MAX_FILES
            ));
        }
        out.push_str("  (the reviewer's gap list — `tomte blame <file>` reads a file's trail)\n");
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests;

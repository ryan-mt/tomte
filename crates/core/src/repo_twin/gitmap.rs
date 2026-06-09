//! Index 4 — the git recent-change map.
//!
//! One `git log` over a bounded recent window gives, per file: how many of those
//! commits touched it (churn) and the most recent commit's time and subject.
//! This is the "failing areas / recently-changed" signal — a file the team is
//! actively editing is far more likely to be the right context than a dormant
//! one. Best-effort: outside a git repo, or without git on PATH, it's empty and
//! the rest of the twin still builds.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use super::{normalize, GitStat};

/// How many recent commits to scan. Enough to surface real churn without making
/// the log walk a noticeable cost on a large history.
const WINDOW: usize = 400;

/// Field/record separators unlikely to appear in a commit subject, so parsing
/// can't be fooled by punctuation in the message.
const FS: char = '\u{1}';

/// Per-file change stats over the last [`WINDOW`] commits, newest commit first
/// determining each file's `last_*`. Empty when not a git repo or git is absent.
pub fn recent_changes(root: &Path) -> Vec<GitStat> {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "core.quotepath=false",
        "log",
        "--no-merges",
        &format!("-n{WINDOW}"),
        "--name-only",
        &format!("--pretty=format:{FS}%ct{FS}%s"),
    ])
    .current_dir(root);
    crate::secret_env::scrub_secret_env_std(&mut cmd);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_log(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `git log --name-only --pretty=format:\x01%ct\x01%s`. A header line
/// starts with the field separator and carries the commit time + subject; every
/// other non-empty line is a path touched by the most recent header seen. Pure,
/// so it's tested without a live repo.
fn parse_log(stdout: &str) -> Vec<GitStat> {
    let mut stats: HashMap<String, GitStat> = HashMap::new();
    let mut cur_ts: u64 = 0;
    let mut cur_subject = String::new();

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix(FS) {
            let mut it = rest.splitn(2, FS);
            cur_ts = it.next().unwrap_or("").trim().parse().unwrap_or(0);
            cur_subject = it.next().unwrap_or("").to_string();
            continue;
        }
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        let path = normalize(path);
        stats
            .entry(path.clone())
            .and_modify(|s| s.commits += 1)
            .or_insert_with(|| GitStat {
                file: path,
                commits: 1,
                // First sighting wins: the log is newest-first, so this is the
                // file's most recent commit.
                last_ts: cur_ts,
                last_subject: cur_subject.clone(),
            });
    }

    let mut out: Vec<GitStat> = stats.into_values().collect();
    // Hottest first, then newest, then path — deterministic order for the cache.
    out.sort_by(|a, b| {
        b.commits
            .cmp(&a.commits)
            .then(b.last_ts.cmp(&a.last_ts))
            .then(a.file.cmp(&b.file))
    });
    out
}

/// Read the module path from `go.mod` (`module github.com/u/r`) so Go imports
/// can be placed inside the tree. `None` when there's no `go.mod`.
pub fn read_go_module(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("go.mod")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module ") {
            let m = rest.trim();
            if !m.is_empty() {
                return Some(m.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_counts_churn_and_keeps_newest_metadata() {
        // Two commits, newest first. src/a.rs changed in both.
        let log =
            format!("{FS}200{FS}fix auth\nsrc/a.rs\nsrc/b.rs\n\n{FS}100{FS}initial\nsrc/a.rs\n");
        let stats = parse_log(&log);
        let a = stats.iter().find(|s| s.file == "src/a.rs").unwrap();
        assert_eq!(a.commits, 2);
        // Newest commit (ts 200) wins for last_*.
        assert_eq!(a.last_ts, 200);
        assert_eq!(a.last_subject, "fix auth");
        let b = stats.iter().find(|s| s.file == "src/b.rs").unwrap();
        assert_eq!(b.commits, 1);
        // Hottest file is sorted first.
        assert_eq!(stats[0].file, "src/a.rs");
    }

    #[test]
    fn parse_handles_empty_and_subjectless() {
        assert!(parse_log("").is_empty());
        // A subject can be empty; the file still counts.
        let log = format!("{FS}100{FS}\nsrc/x.rs\n");
        let stats = parse_log(&log);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].last_subject, "");
    }

    #[test]
    fn windows_backslash_paths_are_normalized() {
        let log = format!("{FS}100{FS}msg\nsrc\\win\\file.rs\n");
        let stats = parse_log(&log);
        assert_eq!(stats[0].file, "src/win/file.rs");
    }

    #[test]
    fn reads_go_module_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("go.mod"),
            "module github.com/me/app\n\ngo 1.22\n",
        )
        .unwrap();
        assert_eq!(
            read_go_module(tmp.path()).as_deref(),
            Some("github.com/me/app")
        );
        // No go.mod → None.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(read_go_module(empty.path()), None);
    }
}

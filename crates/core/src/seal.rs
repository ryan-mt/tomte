//! Commit Seal — the Proof Capsule that travels with the commit.
//!
//! `tomte prove` answers "is the tree verified *right now*?" and the answer
//! evaporates the moment anyone moves on. `tomte seal` notarizes that answer
//! onto the commit itself: it collects the capsule at a **clean** HEAD (a seal
//! describes a commit, not a drifting tree) and attaches it as a git note under
//! `refs/notes/tomte-seal`, bound to the commit and tree ids it was collected
//! at. Notes are ordinary git objects, so the seal is pushed/fetched like any
//! ref — the proof crosses machines with the history it certifies.
//!
//! The same honesty rules as the capsule apply: the CLI gathers everything
//! itself (a model is never consulted), a red capsule seals as red (a seal is a
//! notarized observation, not an award), and `seal verify` gates green only
//! when the note's recorded commit/tree match the revision asked about AND at
//! least one check actually ran and passed — a copied or edited note, or a
//! project with nothing to check, never verifies.

use std::path::Path;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::proof::{self, ProofCapsule};

/// The notes ref the seal lives under (i.e. `refs/notes/tomte-seal`).
pub const NOTES_REF: &str = "tomte-seal";

/// A Proof Capsule notarized onto one commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seal {
    /// Full id of the commit the capsule was collected at.
    pub commit: String,
    /// That commit's tree object id — binds the seal to the exact content, so
    /// a note whose JSON was edited onto another commit can't verify.
    pub tree: String,
    /// When the seal was collected (local time).
    pub sealed_at: String,
    /// The evidence bundle the CLI gathered at that commit.
    pub capsule: ProofCapsule,
}

/// A seal read back from a commit, with the revision's *resolved* ids so the
/// caller can check the binding (the note's content vs. the object it hangs on).
#[derive(Debug, Clone)]
pub struct FoundSeal {
    pub seal: Seal,
    /// The commit id the requested revision resolved to.
    pub commit: String,
    /// That commit's tree id.
    pub tree: String,
}

/// Why a seal couldn't be read.
#[derive(Debug)]
pub enum ReadError {
    /// Git itself refused — not a repository, unknown revision, no commits yet.
    Git(String),
    /// The revision resolved, but no seal note hangs on it.
    NoSeal { commit: String },
    /// A note exists but its content isn't a seal this version can read.
    Unreadable { commit: String, error: String },
}

/// First 8 characters of an object id — the short form the cards print.
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Collect and notarize a seal at HEAD. Refuses a dirty working tree — the
/// capsule would describe the tree, not the commit the note hangs on.
pub async fn create(cwd: &Path) -> Result<Seal, String> {
    let (commit, tree) = resolve(cwd, "HEAD")
        .await
        .map_err(|e| format!("cannot seal: {e}"))?;

    let dirty = dirty_lines(cwd)
        .await
        .map_err(|e| format!("cannot seal: {e}"))?;
    if !dirty.is_empty() {
        let mut msg = String::from(
            "cannot seal: working tree not clean — a seal notarizes a commit, and these changes aren't in HEAD:\n",
        );
        for line in dirty.iter().take(5) {
            msg.push_str(&format!("  {line}\n"));
        }
        if dirty.len() > 5 {
            msg.push_str(&format!("  … +{} more\n", dirty.len() - 5));
        }
        msg.push_str("commit (or stash) first, then run `tomte seal` again");
        return Err(msg);
    }

    let capsule = proof::collect(cwd).await;
    let seal = Seal {
        commit,
        tree,
        sealed_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        capsule,
    };
    write_note(cwd, &seal).await?;
    Ok(seal)
}

/// Read the seal hanging on `rev` (any git revision spelling).
pub async fn read(cwd: &Path, rev: &str) -> Result<FoundSeal, ReadError> {
    let (commit, tree) = resolve(cwd, rev).await.map_err(ReadError::Git)?;

    let out = git(cwd, &["notes", "--ref", NOTES_REF, "show", &commit]).await;
    let text = match out {
        Ok(stdout) => stdout,
        Err(stderr) => {
            // After a successful resolve, "no note found" is the expected miss;
            // anything else is a real git failure worth surfacing verbatim.
            if stderr.to_ascii_lowercase().contains("no note found") {
                return Err(ReadError::NoSeal { commit });
            }
            return Err(ReadError::Git(stderr));
        }
    };

    match serde_json::from_str::<Seal>(text.trim()) {
        Ok(seal) => Ok(FoundSeal { seal, commit, tree }),
        Err(e) => Err(ReadError::Unreadable {
            commit,
            error: e.to_string(),
        }),
    }
}

/// Why this seal does NOT gate green for the resolved `commit`/`tree` — `None`
/// means sealed and verified. Pure, so every branch is unit-tested.
pub fn verify_failure(seal: &Seal, commit: &str, tree: &str) -> Option<String> {
    if seal.commit != commit {
        return Some(format!(
            "the seal was collected at a different commit ({}, asked about {})",
            short(&seal.commit),
            short(commit)
        ));
    }
    if seal.tree != tree {
        return Some(format!(
            "the seal's tree id doesn't match the commit's ({} ≠ {})",
            short(&seal.tree),
            short(tree)
        ));
    }
    if !seal.capsule.any_check_ran() {
        return Some("the sealed capsule ran no checks — nothing was verified".to_string());
    }
    if !seal.capsule.verified() {
        return Some("a sealed check failed — the capsule is red".to_string());
    }
    None
}

/// The seal card: a header naming the commit, the capsule it notarized, and the
/// share lines — notes travel by ref, not by default with `git push`.
pub fn render(seal: &Seal, subject: Option<&str>) -> String {
    let mut out = String::new();
    match subject {
        Some(s) => out.push_str(&format!("Commit Seal  ·  {}  {s}\n", short(&seal.commit))),
        None => out.push_str(&format!("Commit Seal  ·  {}\n", short(&seal.commit))),
    }
    out.push_str(&format!(
        "sealed {}  ·  notes ref refs/notes/{NOTES_REF}\n\n",
        seal.sealed_at
    ));
    out.push_str(&seal.capsule.render());
    out.push_str(&format!(
        "\nThe seal travels with the repo:\n  push:  git push origin refs/notes/{NOTES_REF}\n  fetch: git fetch origin refs/notes/{NOTES_REF}:refs/notes/{NOTES_REF}\n"
    ));
    out
}

/// The commit's one-line subject, for the card header. Best-effort.
pub async fn commit_subject(cwd: &Path, commit: &str) -> Option<String> {
    git(cwd, &["show", "-s", "--format=%s", commit])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve `rev` to its full commit id and that commit's tree id.
async fn resolve(cwd: &Path, rev: &str) -> Result<(String, String), String> {
    let spec = format!("{rev}^{{commit}}");
    let commit = git(cwd, &["rev-parse", "--verify", "--quiet", &spec])
        .await
        .map_err(|e| {
            if e.is_empty() {
                format!("`{rev}` is not a commit here (not a git repository, or no such revision)")
            } else {
                e
            }
        })?;
    let tree_spec = format!("{commit}^{{tree}}");
    let tree = git(cwd, &["rev-parse", "--verify", "--quiet", &tree_spec]).await?;
    Ok((commit, tree))
}

/// `git status --porcelain` lines — non-empty means the tree differs from HEAD.
async fn dirty_lines(cwd: &Path) -> Result<Vec<String>, String> {
    let out = git(cwd, &["status", "--porcelain", "--untracked-files=all"]).await?;
    Ok(out.lines().map(|l| l.to_string()).collect())
}

/// Attach `seal` as the note on its commit, replacing any earlier seal there.
/// The JSON goes in compact on stdin — single-line, so git's message cleanup
/// (comment stripping, blank-line collapsing) can never touch it.
pub async fn write_note(cwd: &Path, seal: &Seal) -> Result<(), String> {
    let json = serde_json::to_string(seal).map_err(|e| format!("cannot seal: {e}"))?;
    let mut cmd = Command::new("git");
    cmd.args([
        "notes",
        "--ref",
        NOTES_REF,
        "add",
        "-f",
        "-F",
        "-",
        &seal.commit,
    ])
    .current_dir(cwd)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    crate::secret_env::scrub_secret_env(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot write the seal note: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(json.as_bytes())
            .await
            .map_err(|e| format!("cannot write the seal note: {e}"))?;
        // Dropping stdin closes the pipe so git sees EOF.
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| format!("cannot write the seal note: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "cannot write the seal note: {}",
            stderr.trim().lines().next().unwrap_or("git notes failed")
        ));
    }
    Ok(())
}

/// Run git with `args`, returning trimmed stdout on success and trimmed stderr
/// on a non-zero exit (empty when git printed nothing).
async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    crate::secret_env::scrub_secret_env(&mut cmd);
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("git did not run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests;

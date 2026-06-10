use std::path::Path;

use super::*;
use crate::proof::{CheckResult, Outcome, ProjectKind};

fn run_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo with one commit and a configured identity, so `git notes` can write.
/// No project manifest on purpose: proof detection sees `Unknown` and plans
/// zero checks, so `create()` stays fast in tests.
fn init_repo(dir: &Path) {
    run_git(dir, &["init"]);
    run_git(dir, &["config", "user.email", "tomte@test"]);
    run_git(dir, &["config", "user.name", "tomte"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("README.txt"), "hello").unwrap();
    run_git(dir, &["add", "."]);
    run_git(dir, &["commit", "-m", "first commit"]);
}

fn capsule_with(checks: Vec<CheckResult>) -> ProofCapsule {
    ProofCapsule {
        timestamp: "2026-06-09 12:00:00".into(),
        project_kind: ProjectKind::Rust,
        files_changed: vec![],
        checks,
        reproduce: vec!["cargo test".into()],
    }
}

fn check(name: &str, outcome: Outcome) -> CheckResult {
    CheckResult {
        name: name.to_string(),
        command: "cargo test".into(),
        outcome,
        tail: String::new(),
    }
}

fn green_seal(commit: &str, tree: &str) -> Seal {
    Seal {
        commit: commit.to_string(),
        tree: tree.to_string(),
        sealed_at: "2026-06-09 12:00:00".into(),
        capsule: capsule_with(vec![check("test", Outcome::Passed)]),
    }
}

#[tokio::test]
async fn write_note_then_read_round_trips_the_seal() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let (commit, tree) = resolve(tmp.path(), "HEAD").await.unwrap();

    let seal = green_seal(&commit, &tree);
    write_note(tmp.path(), &seal).await.unwrap();

    let found = read(tmp.path(), "HEAD").await.unwrap();
    assert_eq!(found.seal.commit, commit);
    assert_eq!(found.seal.tree, tree);
    assert_eq!(found.commit, commit);
    assert_eq!(found.tree, tree);
    assert_eq!(found.seal.capsule.checks.len(), 1);
    assert!(found.seal.capsule.verified());
    assert!(verify_failure(&found.seal, &found.commit, &found.tree).is_none());
}

#[tokio::test]
async fn write_note_replaces_an_earlier_seal() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let (commit, tree) = resolve(tmp.path(), "HEAD").await.unwrap();

    let mut red = green_seal(&commit, &tree);
    red.capsule = capsule_with(vec![check("test", Outcome::Failed { code: 1 })]);
    write_note(tmp.path(), &red).await.unwrap();
    write_note(tmp.path(), &green_seal(&commit, &tree))
        .await
        .unwrap();

    let found = read(tmp.path(), "HEAD").await.unwrap();
    assert!(found.seal.capsule.verified(), "re-seal overwrites the note");
}

#[tokio::test]
async fn read_without_a_note_reports_no_seal() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let (commit, _) = resolve(tmp.path(), "HEAD").await.unwrap();

    match read(tmp.path(), "HEAD").await {
        Err(ReadError::NoSeal { commit: c }) => assert_eq!(c, commit),
        other => panic!("expected NoSeal, got {other:?}"),
    }
}

#[tokio::test]
async fn read_a_non_seal_note_reports_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    run_git(
        tmp.path(),
        &["notes", "--ref", NOTES_REF, "add", "-m", "not json"],
    );

    match read(tmp.path(), "HEAD").await {
        Err(ReadError::Unreadable { .. }) => {}
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

#[tokio::test]
async fn read_an_unknown_revision_reports_git_error() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    match read(tmp.path(), "no-such-branch").await {
        Err(ReadError::Git(_)) => {}
        other => panic!("expected Git error, got {other:?}"),
    }
}

#[tokio::test]
async fn create_refuses_a_dirty_tree() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::write(tmp.path().join("uncommitted.txt"), "drift").unwrap();

    let err = create(tmp.path()).await.unwrap_err();
    assert!(err.contains("not clean"), "got: {err}");
    assert!(err.contains("uncommitted.txt"), "names the change: {err}");
    // Refusal happens before any note write.
    assert!(matches!(
        read(tmp.path(), "HEAD").await,
        Err(ReadError::NoSeal { .. })
    ));
}

#[tokio::test]
async fn create_seals_a_clean_tree_and_verify_demands_a_ran_check() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let seal = create(tmp.path()).await.unwrap();
    let found = read(tmp.path(), "HEAD").await.unwrap();
    assert_eq!(found.seal.commit, seal.commit);

    // No manifest → zero planned checks → sealed, but never gate-green:
    // "nothing failed because nothing ran" must not verify.
    assert!(!found.seal.capsule.any_check_ran());
    let reason = verify_failure(&found.seal, &found.commit, &found.tree).unwrap();
    assert!(reason.contains("no checks"), "got: {reason}");
}

#[test]
fn verify_failure_rejects_a_rebound_or_red_seal() {
    let seal = green_seal("aaaa1111aaaa1111", "bbbb2222bbbb2222");

    // Green seal on its own commit verifies.
    assert!(verify_failure(&seal, "aaaa1111aaaa1111", "bbbb2222bbbb2222").is_none());

    // The note answers for a different commit (copied/moved note).
    let other = verify_failure(&seal, "cccc3333cccc3333", "bbbb2222bbbb2222").unwrap();
    assert!(other.contains("different commit"), "got: {other}");

    // Same commit id claimed, but the tree doesn't match (edited JSON).
    let forged = verify_failure(&seal, "aaaa1111aaaa1111", "dddd4444dddd4444").unwrap();
    assert!(forged.contains("tree"), "got: {forged}");

    // A red capsule sealed honestly still never gates green.
    let mut red = seal.clone();
    red.capsule = capsule_with(vec![check("test", Outcome::Failed { code: 101 })]);
    let failed = verify_failure(&red, "aaaa1111aaaa1111", "bbbb2222bbbb2222").unwrap();
    assert!(failed.contains("failed"), "got: {failed}");
}

#[test]
fn render_names_the_commit_the_verdict_and_the_share_lines() {
    let seal = green_seal("aaaa1111ffff0000", "bbbb2222bbbb2222");
    let card = render(&seal, Some("fix: the thing"));

    assert!(card.contains("Commit Seal"));
    assert!(card.contains("aaaa1111"), "short commit id on the header");
    assert!(card.contains("fix: the thing"));
    assert!(card.contains("✅ Verified"), "the capsule card is embedded");
    assert!(card.contains("git push origin refs/notes/tomte-seal"));
    assert!(card.contains("git fetch origin refs/notes/tomte-seal:refs/notes/tomte-seal"));

    // Without a subject the header still stands.
    let bare = render(&seal, None);
    assert!(bare.contains("Commit Seal  ·  aaaa1111\n"));
}

#[test]
fn short_id_survives_a_tiny_or_non_ascii_boundary_input() {
    assert_eq!(short("aaaa1111ffff0000"), "aaaa1111");
    assert_eq!(short("abc"), "abc");
    // Not a real oid, but byte 8 lands mid-char here ("€" is 3 bytes) — the
    // helper must fall back to the whole string, never panic.
    assert_eq!(short("€€€"), "€€€");
}

//! Tests for the `memory` tool: name validation/sandbox, the six commands,
//! index injection, and the headless write guard.

use super::inject::{
    apply_store_at, cap_index, index_block, INDEX_FILE, INDEX_MAX_BYTES, INDEX_MAX_LINES,
    STORE_BLOCK_BEGIN,
};
use super::*;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::tools::{ApprovalMode, SessionState, ToolContext};

fn ctx(cwd: std::path::PathBuf, non_interactive: bool, require_approval: bool) -> ToolContext {
    ToolContext {
        cwd,
        approval: ApprovalMode::Auto,
        require_approval,
        auto_approve_edits: false,
        non_interactive,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: crate::config::Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

// ---- name validation & sandbox ---------------------------------------------

#[test]
fn normalize_name_appends_md_and_passes_plain() {
    assert_eq!(normalize_name("notes").unwrap(), "notes.md");
    assert_eq!(normalize_name("user_role.md").unwrap(), "user_role.md");
    assert_eq!(normalize_name("  context-1.md  ").unwrap(), "context-1.md");
}

#[test]
fn normalize_name_strips_memories_root_prefix() {
    // Models trained on Anthropic's memory tool prefix their conceptual root.
    assert_eq!(normalize_name("/memories/plan.md").unwrap(), "plan.md");
    assert_eq!(normalize_name("memories/plan.md").unwrap(), "plan.md");
}

#[test]
fn normalize_name_rejects_traversal_and_dirs_and_abs() {
    assert!(normalize_name("../secret.md").is_err());
    assert!(normalize_name("sub/x.md").is_err());
    assert!(normalize_name("/etc/passwd").is_err()); // strip '/' -> "etc/passwd" -> has '/'
    assert!(normalize_name("..").is_err());
    assert!(normalize_name(".hidden.md").is_err());
    assert!(normalize_name("notes.txt").is_err());
    assert!(normalize_name("").is_err());
    assert!(normalize_name("a b.md").is_err()); // space not allowed
}

#[test]
fn resolve_file_lands_directly_under_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let p = resolve_file(&root, "notes").unwrap();
    assert_eq!(p.parent(), Some(root.as_path()));
    assert_eq!(p.file_name().unwrap(), "notes.md");
    assert!(resolve_file(&root, "../escape.md").is_err());
}

#[cfg(unix)]
#[test]
fn resolve_file_rejects_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("memory");
    std::fs::create_dir_all(&root).unwrap();
    // Plant a symlink inside the store pointing to a file outside it.
    let outside = tmp.path().join("secret.txt");
    std::fs::write(&outside, "TOPSECRET").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("notes.md")).unwrap();
    let err = resolve_file(&root, "notes.md").unwrap_err();
    assert!(err.to_string().contains("symlink"));
}

#[test]
fn project_key_sanitizes_path_separators() {
    let key = project_key(std::path::Path::new("/home/ryan/projects/cli"));
    assert!(!key.contains('/'));
    assert!(key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
    assert!(key.contains("cli"));
}

#[test]
fn store_dir_is_under_config_projects() {
    let dir = store_dir(std::path::Path::new("/tmp/whatever"));
    assert!(dir.ends_with("memory"));
    assert!(dir.to_string_lossy().contains("projects"));
}

// ---- commands ---------------------------------------------------------------

#[tokio::test]
async fn create_then_view_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let msg = cmd_create(root, Some("notes"), Some("hello world"))
        .await
        .unwrap();
    assert!(msg.contains("notes.md"));
    let view = cmd_view(root, Some("notes.md"), None).unwrap();
    assert!(view.contains("hello world"));
    // viewing by bare name (auto-.md) works too
    assert!(cmd_view(root, Some("notes"), None)
        .unwrap()
        .contains("hello world"));
}

#[tokio::test]
async fn view_range_reversed_is_error_not_panic() {
    // A model-supplied reversed range (start > end after clamping) must surface
    // an error, not panic the `lines[start..=end]` slice.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    cmd_create(root, Some("notes"), Some("l1\nl2\nl3\nl4"))
        .await
        .unwrap();
    // [2, 0] clamps to start=2, end=0 — previously `lines[2..=0]` panicked.
    assert!(cmd_view(root, Some("notes.md"), Some([2, 0])).is_err());
    // A negative end (clamped to 0) below a positive start is likewise rejected.
    assert!(cmd_view(root, Some("notes.md"), Some([3, -5])).is_err());
    // A valid forward range still works (0-based inclusive).
    let ok = cmd_view(root, Some("notes.md"), Some([1, 2])).unwrap();
    assert!(ok.contains("l2") && ok.contains("l3"));
}

#[tokio::test]
async fn create_twice_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    cmd_create(root, Some("a.md"), Some("x")).await.unwrap();
    let err = cmd_create(root, Some("a.md"), Some("y")).await.unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn str_replace_unique_not_found_and_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    cmd_create(root, Some("a.md"), Some("one two two"))
        .await
        .unwrap();

    // not found
    assert!(cmd_str_replace(root, Some("a.md"), Some("zzz"), Some("q"))
        .await
        .is_err());
    // ambiguous (two occurrences)
    assert!(cmd_str_replace(root, Some("a.md"), Some("two"), Some("q"))
        .await
        .is_err());
    // unique
    cmd_str_replace(root, Some("a.md"), Some("one"), Some("1"))
        .await
        .unwrap();
    assert!(cmd_view(root, Some("a.md"), None)
        .unwrap()
        .contains("1 two two"));
}

#[tokio::test]
async fn str_replace_missing_file_errors() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        cmd_str_replace(tmp.path(), Some("nope.md"), Some("a"), Some("b"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn insert_places_line_at_index() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    cmd_create(root, Some("a.md"), Some("l0\nl1\nl2"))
        .await
        .unwrap();
    cmd_insert(root, Some("a.md"), Some(1), Some("NEW"))
        .await
        .unwrap();
    let v = cmd_view(root, Some("a.md"), None).unwrap();
    assert!(v.contains("l0\nNEW\nl1\nl2"));
    // out-of-range clamps to end rather than erroring
    cmd_insert(root, Some("a.md"), Some(999), Some("END"))
        .await
        .unwrap();
    assert!(cmd_view(root, Some("a.md"), None).unwrap().contains("END"));
}

#[tokio::test]
async fn delete_and_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    cmd_create(root, Some("a.md"), Some("x")).await.unwrap();
    cmd_rename(root, Some("a.md"), Some("b.md")).unwrap();
    assert!(cmd_view(root, Some("b.md"), None).is_ok());
    assert!(cmd_view(root, Some("a.md"), None).is_err());
    // rename onto an existing target fails
    cmd_create(root, Some("c.md"), Some("y")).await.unwrap();
    assert!(cmd_rename(root, Some("b.md"), Some("c.md")).is_err());
    // delete
    cmd_delete(root, Some("b.md")).unwrap();
    assert!(cmd_view(root, Some("b.md"), None).is_err());
    assert!(cmd_delete(root, Some("b.md")).is_err());
}

#[tokio::test]
async fn view_range_slices_inclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    cmd_create(root, Some("a.md"), Some("l0\nl1\nl2\nl3"))
        .await
        .unwrap();
    let v = cmd_view(root, Some("a.md"), Some([1, 2])).unwrap();
    assert!(v.contains("l1\nl2"));
    assert!(!v.contains("l0"));
    assert!(!v.contains("l3"));
}

#[tokio::test]
async fn view_no_path_lists_store() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(cmd_view(root, None, None).unwrap().contains("empty"));
    cmd_create(root, Some("a.md"), Some("x")).await.unwrap();
    cmd_create(root, Some("b.md"), Some("y")).await.unwrap();
    let listing = cmd_view(root, None, None).unwrap();
    assert!(listing.contains("a.md") && listing.contains("b.md"));
}

#[tokio::test]
async fn oversized_note_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let big = "x".repeat(FILE_MAX_BYTES + 1);
    assert!(cmd_create(tmp.path(), Some("a.md"), Some(&big))
        .await
        .is_err());
}

// ---- prompt injection -------------------------------------------------------

#[test]
fn index_block_prefers_memory_md() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join(INDEX_FILE), "- a.md: notes about a").unwrap();
    let block = index_block(root).unwrap();
    assert!(block.contains("notes about a"));
    assert!(block.contains("Project memory"));
}

#[test]
fn index_block_falls_back_to_listing_without_memory_md() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("topic.md"), "body").unwrap();
    let block = index_block(root).unwrap();
    assert!(block.contains("topic.md"));
    assert!(block.contains("view"));
}

#[test]
fn index_block_none_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(index_block(tmp.path()).is_none());
}

#[test]
fn apply_store_at_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join(INDEX_FILE), "- a.md").unwrap();
    let mut prompt = "BASE".to_string();
    apply_store_at(&mut prompt, root);
    let once = prompt.len();
    apply_store_at(&mut prompt, root);
    assert_eq!(prompt.len(), once);
    assert_eq!(prompt.matches(STORE_BLOCK_BEGIN).count(), 1);
    assert!(prompt.starts_with("BASE"));
}

#[test]
fn cap_index_truncates_by_lines() {
    let many = (0..500)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let capped = cap_index(&many);
    assert!(capped.contains("truncated"));
    assert!(capped.lines().count() <= INDEX_MAX_LINES + 2);
}

#[test]
fn cap_index_stays_within_byte_cap_including_marker() {
    // 100 KiB across few long lines: the byte cap (not the line cap) triggers,
    // and the appended marker must not push the result past INDEX_MAX_BYTES.
    let many = (0..100)
        .map(|_| "x".repeat(1000))
        .collect::<Vec<_>>()
        .join("\n");
    let capped = cap_index(&many);
    assert!(capped.contains("truncated"));
    assert!(
        capped.len() <= INDEX_MAX_BYTES,
        "capped index {} exceeds cap {}",
        capped.len(),
        INDEX_MAX_BYTES
    );
}

#[test]
fn apply_store_at_preserves_a_preceding_inherited_block() {
    // The inherited-memory block (distinct marker) must survive store injection.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(INDEX_FILE), "- a.md").unwrap();
    let mut prompt = format!(
        "BASE{}INHERITED{}",
        crate::memory::MEMORY_BLOCK_BEGIN,
        crate::memory::MEMORY_BLOCK_END
    );
    let before = prompt.clone();
    apply_store_at(&mut prompt, tmp.path());
    assert!(
        prompt.starts_with(&before),
        "store injection must not disturb the inherited block"
    );
    assert!(prompt.contains("tomte-memory-store:start"));
}

// ---- headless guard via execute --------------------------------------------

#[tokio::test]
async fn execute_blocks_writes_when_headless() {
    let tmp = tempfile::tempdir().unwrap();
    // Unattended headless: non_interactive + require_approval => writes blocked.
    let c = ctx(tmp.path().to_path_buf(), true, true);
    let err = Memory
        .execute(
            json!({"command": "create", "path": "a.md", "file_text": "x"}),
            &c,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("headless"));
}

#[tokio::test]
async fn execute_allows_view_when_headless() {
    // A fresh tempdir cwd maps to a store path that does not exist, so view is
    // a safe read that returns the empty-store message — no global state touched.
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(tmp.path().to_path_buf(), true, true);
    let out = Memory
        .execute(json!({"command": "view", "path": null, "old_path": null, "new_path": null, "file_text": null, "old_text": null, "new_text": null, "insert_line": null, "view_range": null}), &c)
        .await
        .unwrap();
    assert!(out.contains("empty") || out.contains("Memory store"));
}

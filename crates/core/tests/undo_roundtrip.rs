//! Integration tests for the in-session undo stack: write a file, edit it,
//! then verify `UndoLastEdit` rolls each step back in LIFO order.

use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;
use tomte_core::tools::{
    fs::{EditFile, MultiEdit, UndoLastEdit, WriteFile},
    ApprovalMode, BuiltinTool, SessionState, ToolContext,
};

fn ctx(cwd: std::path::PathBuf) -> ToolContext {
    ToolContext {
        cwd,
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: tomte_core::config::Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    }
}

/// Register `rel` as read this session so the read-before-write/edit guard
/// (write_file/edit_file refuse to touch a file the model never read) doesn't
/// block these undo tests, which seed files straight to disk. Keyed exactly
/// like `fs::resolve`: the canonical path of the existing file. Works for
/// binary files too, which `read_file` can't load as UTF-8.
async fn mark_read(ctx: &ToolContext, rel: &str) {
    let p = std::fs::canonicalize(ctx.cwd.join(rel)).expect("file exists to mark read");
    let mut session = ctx.session.lock().await;
    // Simulate a full read: write_file gates overwrites on fully_read_files.
    session.read_files.insert(p.clone());
    session.fully_read_files.insert(p);
}

async fn force_mtime_change(path: &std::path::Path, expected: Option<SystemTime>) {
    let mut current = expected;
    for i in 0..20 {
        tokio::time::sleep(Duration::from_millis(25)).await;
        tokio::fs::write(path, format!("manual change {i}"))
            .await
            .unwrap();
        current = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if current != expected {
            return;
        }
    }
    assert_ne!(
        current, expected,
        "mtime did not change after repeated writes"
    );
}

#[tokio::test]
async fn undo_restores_overwritten_content() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    tokio::fs::write(tmp.path().join("a.txt"), b"original\n")
        .await
        .unwrap();
    mark_read(&ctx, "a.txt").await;

    WriteFile
        .execute(json!({"path": "a.txt", "content": "overwritten\n"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "overwritten\n"
    );

    let msg = UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert!(msg.contains("Restored"), "got: {msg}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn undo_removes_newly_created_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    WriteFile
        .execute(json!({"path": "new.txt", "content": "fresh\n"}), &ctx)
        .await
        .unwrap();
    assert!(tmp.path().join("new.txt").exists());

    let msg = UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert!(msg.contains("Removed"), "got: {msg}");
    assert!(!tmp.path().join("new.txt").exists());
}

#[tokio::test]
async fn undo_unwinds_in_lifo_order() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    tokio::fs::write(tmp.path().join("a.txt"), b"a0")
        .await
        .unwrap();
    tokio::fs::write(tmp.path().join("b.txt"), b"b0")
        .await
        .unwrap();
    mark_read(&ctx, "a.txt").await;
    mark_read(&ctx, "b.txt").await;

    WriteFile
        .execute(json!({"path": "a.txt", "content": "a1"}), &ctx)
        .await
        .unwrap();
    WriteFile
        .execute(json!({"path": "b.txt", "content": "b1"}), &ctx)
        .await
        .unwrap();

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
        "b0"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "a1"
    );

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "a0"
    );
}

#[tokio::test]
async fn undo_with_empty_stack_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    let err = UndoLastEdit.execute(json!({}), &ctx).await.unwrap_err();
    assert!(err.to_string().contains("no edits to undo"), "got: {err}");
}

#[tokio::test]
async fn undo_reverts_edit_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    tokio::fs::write(tmp.path().join("c.txt"), b"foo bar baz")
        .await
        .unwrap();
    mark_read(&ctx, "c.txt").await;

    EditFile
        .execute(
            json!({
                "path": "c.txt",
                "old_string": "bar",
                "new_string": "BAR",
                "replace_all": false,
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("c.txt")).unwrap(),
        "foo BAR baz"
    );

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("c.txt")).unwrap(),
        "foo bar baz"
    );
}

#[tokio::test]
async fn edit_file_rejects_empty_old_string() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    tokio::fs::write(tmp.path().join("empty-old.txt"), b"abc")
        .await
        .unwrap();

    let err = EditFile
        .execute(
            json!({
                "path": "empty-old.txt",
                "old_string": "",
                "new_string": "X",
                "replace_all": true,
            }),
            &ctx,
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("old_string must not be empty"),
        "got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("empty-old.txt")).unwrap(),
        "abc"
    );
}

#[tokio::test]
async fn undo_reverts_multi_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    tokio::fs::write(tmp.path().join("d.txt"), b"alpha beta gamma")
        .await
        .unwrap();
    mark_read(&ctx, "d.txt").await;

    MultiEdit
        .execute(
            json!({
                "path": "d.txt",
                "edits": [
                    {"old_string": "alpha", "new_string": "A", "replace_all": false},
                    {"old_string": "gamma", "new_string": "G", "replace_all": false}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("d.txt")).unwrap(),
        "A beta G"
    );

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("d.txt")).unwrap(),
        "alpha beta gamma"
    );
}

#[tokio::test]
async fn multi_edit_rejects_empty_old_string() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    tokio::fs::write(tmp.path().join("multi-empty-old.txt"), b"abc")
        .await
        .unwrap();
    mark_read(&ctx, "multi-empty-old.txt").await;

    let err = MultiEdit
        .execute(
            json!({
                "path": "multi-empty-old.txt",
                "edits": [
                    {"old_string": "", "new_string": "X", "replace_all": true}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("old_string must not be empty"),
        "got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("multi-empty-old.txt")).unwrap(),
        "abc"
    );
}

#[tokio::test]
async fn failed_undo_keeps_entry_on_stack() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    let path = tmp.path().join("race.txt");
    tokio::fs::write(&path, b"before").await.unwrap();
    mark_read(&ctx, "race.txt").await;

    EditFile
        .execute(
            json!({
                "path": "race.txt",
                "old_string": "before",
                "new_string": "after",
                "replace_all": false,
            }),
            &ctx,
        )
        .await
        .unwrap();

    let expected = {
        let session = ctx.session.lock().await;
        assert_eq!(session.undo_stack.len(), 1);
        session.undo_stack.back().unwrap().post_edit_mtime
    };
    force_mtime_change(&path, expected).await;

    let err = UndoLastEdit.execute(json!({}), &ctx).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("file has been modified since the edit"),
        "got: {err}"
    );
    let session = ctx.session.lock().await;
    assert_eq!(
        session.undo_stack.len(),
        1,
        "failed undo must remain retryable"
    );
}

#[tokio::test]
async fn undo_refuses_when_size_changed_despite_matching_mtime() {
    // On filesystems with coarse (1s) mtime resolution, a same-second external
    // edit can leave the mtime unchanged. The size check is the backstop. We
    // pin the stored mtime to the file's current mtime so ONLY the size branch
    // can fire, then confirm undo still refuses.
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());
    let path = tmp.path().join("size.txt");

    WriteFile
        .execute(json!({"path": "size.txt", "content": "hello world"}), &ctx)
        .await
        .unwrap();

    // External edit with a different length (2 bytes vs the 11 we wrote).
    tokio::fs::write(&path, b"hi").await.unwrap();
    let current_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    {
        let mut session = ctx.session.lock().await;
        session.undo_stack.back_mut().unwrap().post_edit_mtime = current_mtime;
    }

    let err = UndoLastEdit.execute(json!({}), &ctx).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("file has been modified since the edit"),
        "size mismatch must block undo; got: {err}"
    );
}

#[tokio::test]
async fn undo_restores_overwritten_binary_file() {
    // Regression: overwriting an existing NON-UTF-8 (binary) file then undoing
    // must restore the original bytes — not delete the file. The undo snapshot
    // previously used read_to_string().ok(), which returned None for binary
    // content, so undo mistook it for a newly-created file and remove_file'd it.
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx(tmp.path().to_path_buf());

    let original: &[u8] = &[0x00, 0xFF, 0xFE, 0x42, 0x00]; // invalid UTF-8
    tokio::fs::write(tmp.path().join("img.bin"), original)
        .await
        .unwrap();
    mark_read(&ctx, "img.bin").await;

    WriteFile
        .execute(json!({"path": "img.bin", "content": "overwritten"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(tmp.path().join("img.bin")).unwrap(),
        b"overwritten"
    );

    let msg = UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert!(
        msg.contains("Restored"),
        "must restore, not remove; got: {msg}"
    );
    assert!(
        tmp.path().join("img.bin").exists(),
        "binary file must still exist after undo"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("img.bin")).unwrap(),
        original,
        "original binary bytes must be restored exactly"
    );
}

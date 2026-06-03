//! fs tool tests (part 2: edit/undo/permissions), split out of `fs`.

use super::test_support::ctx;
use super::*;
use crate::tools::BuiltinTool;
use serde_json::json;

#[tokio::test]
async fn write_file_refuses_unread_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), "important").unwrap();
    let err = WriteFile
        .execute(
            json!({"path": "keep.txt", "content": "clobbered"}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not read this session"),
        "got: {err}"
    );
    // The original survives — the write was refused, not partially applied.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keep.txt")).unwrap(),
        "important"
    );
}

#[tokio::test]
async fn write_file_allows_new_file_without_read() {
    let dir = tempfile::tempdir().unwrap();
    WriteFile
        .execute(
            json!({"path": "fresh.txt", "content": "hello"}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fresh.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn read_then_write_overwrites_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "v1").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    ReadFile
        .execute(json!({"path": "doc.txt"}), &ctx)
        .await
        .unwrap();
    WriteFile
        .execute(json!({"path": "doc.txt", "content": "v2"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("doc.txt")).unwrap(),
        "v2"
    );
}

#[tokio::test]
async fn write_edit_and_multi_edit_accept_common_argument_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    let path = dir.path().join("doc.txt");
    let file_path = path.to_string_lossy();
    let other_path = dir.path().join("other.txt");
    let other_file_path = other_path.to_string_lossy();

    WriteFile
        .execute(
            json!({"file_path": file_path, "text": "one two three"}),
            &ctx,
        )
        .await
        .unwrap();
    WriteFile
        .execute(
            json!({"filePath": other_file_path, "contents": "alias content"}),
            &ctx,
        )
        .await
        .unwrap();
    EditFile
        .execute(
            json!({"file_path": file_path, "old_text": "two", "new_text": "2"}),
            &ctx,
        )
        .await
        .unwrap();
    EditFile
        .execute(
            json!({
                "filePath": file_path,
                "oldText": "2",
                "newText": "two",
                "replaceAll": "false"
            }),
            &ctx,
        )
        .await
        .unwrap();
    MultiEdit
        .execute(
            json!({
                "filePath": file_path,
                "edits": [
                    {"old_text": "one", "new_text": "1"},
                    {"oldText": "three", "newText": "3"}
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "1 two 3");
    assert_eq!(
        std::fs::read_to_string(other_path).unwrap(),
        "alias content"
    );
}

#[tokio::test]
async fn write_then_edit_without_reread_succeeds() {
    // A file the model just authored counts as read, so a follow-up
    // edit_file must not spuriously demand a read_file.
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    WriteFile
        .execute(json!({"path": "gen.txt", "content": "fn main() {}"}), &ctx)
        .await
        .unwrap();
    EditFile
        .execute(
            json!({"path": "gen.txt", "old_string": "main", "new_string": "run"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("gen.txt")).unwrap(),
        "fn run() {}"
    );
}

#[tokio::test]
async fn edit_file_refuses_unread_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("e.txt"), "foo").unwrap();
    let err = EditFile
        .execute(
            json!({"path": "e.txt", "old_string": "foo", "new_string": "bar"}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("requires reading"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("e.txt")).unwrap(),
        "foo"
    );
}

#[tokio::test]
async fn edit_file_rejects_identical_old_and_new() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("id.txt"), "same").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    ReadFile
        .execute(json!({"path": "id.txt"}), &ctx)
        .await
        .unwrap();
    let err = EditFile
        .execute(
            json!({"path": "id.txt", "old_string": "same", "new_string": "same"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("identical"), "got: {err}");
}

#[tokio::test]
async fn read_file_describes_binary_instead_of_erroring() {
    let dir = tempfile::tempdir().unwrap();
    // PNG magic header + a 0xFF byte → not valid UTF-8.
    let png = [
        0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xFF, 0x00,
    ];
    std::fs::write(dir.path().join("logo.png"), png).unwrap();
    let out = ReadFile
        .execute(json!({"path": "logo.png"}), &ctx(dir.path().to_path_buf()))
        .await
        .unwrap();
    assert!(out.contains("PNG image"), "got: {out}");
    assert!(out.contains("recorded as read"), "got: {out}");
}

#[tokio::test]
async fn read_binary_then_write_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let png = [
        0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xFF, 0x00,
    ];
    std::fs::write(dir.path().join("img.png"), png).unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    // Reading a binary records it as read even though contents aren't shown,
    // so a deliberate overwrite is allowed (the read-before-write guard,
    // which can't read binary as text, no longer blocks regeneration).
    ReadFile
        .execute(json!({"path": "img.png"}), &ctx)
        .await
        .unwrap();
    WriteFile
        .execute(json!({"path": "img.png", "content": "regenerated"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("img.png")).unwrap(),
        "regenerated"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_preserves_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ctx = ctx(dir.path().to_path_buf());

    ReadFile
        .execute(json!({"path": "script.sh"}), &ctx)
        .await
        .unwrap();
    WriteFile
        .execute(
            json!({"path": "script.sh", "content": "#!/bin/sh\necho new\n"}),
            &ctx,
        )
        .await
        .unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
}

#[cfg(unix)]
#[tokio::test]
async fn edit_and_undo_preserve_existing_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.sh");
    std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ctx = ctx(dir.path().to_path_buf());

    ReadFile
        .execute(json!({"path": "script.sh"}), &ctx)
        .await
        .unwrap();
    EditFile
        .execute(
            json!({
                "path": "script.sh",
                "old_string": "old",
                "new_string": "new",
                "replace_all": false
            }),
            &ctx,
        )
        .await
        .unwrap();
    let mode_after_edit = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_after_edit, 0o755);

    UndoLastEdit.execute(json!({}), &ctx).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "#!/bin/sh\necho old\n"
    );
    let mode_after_undo = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_after_undo, 0o755);
}

#[tokio::test]
async fn edit_file_refuses_after_external_modification() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.txt");
    std::fs::write(&path, "alpha beta\n").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    // Read records the (mtime, size) snapshot.
    ReadFile
        .execute(json!({"path": "m.txt"}), &ctx)
        .await
        .unwrap();
    // Something else changes the file on disk after the read. Changing the
    // length means staleness is caught even when the mtime resolution is
    // too coarse to distinguish a same-second write.
    std::fs::write(&path, "alpha beta gamma delta\n").unwrap();
    let err = EditFile
        .execute(
            json!({"path": "m.txt", "old_string": "alpha", "new_string": "ALPHA"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("changed on disk"), "got: {err}");
    // The refused edit left the on-disk content untouched.
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alpha beta gamma delta\n"
    );
    // Re-reading refreshes the snapshot, so the edit then goes through.
    ReadFile
        .execute(json!({"path": "m.txt"}), &ctx)
        .await
        .unwrap();
    EditFile
        .execute(
            json!({"path": "m.txt", "old_string": "alpha", "new_string": "ALPHA"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("ALPHA"));
}

#[tokio::test]
async fn consecutive_edits_after_one_read_are_allowed() {
    // The model's own edit changes (mtime, size); the staleness guard must
    // not fire on that, so a second edit after a single read still works.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.txt");
    std::fs::write(&path, "one two three").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    ReadFile
        .execute(json!({"path": "c.txt"}), &ctx)
        .await
        .unwrap();
    EditFile
        .execute(
            json!({"path": "c.txt", "old_string": "one", "new_string": "1"}),
            &ctx,
        )
        .await
        .unwrap();
    EditFile
        .execute(
            json!({"path": "c.txt", "old_string": "three", "new_string": "3"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "1 two 3");
}

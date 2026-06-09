//! fs tool tests: `edit_file`/`multi_edit`, undo, and permission preservation.

use super::test_support::ctx;
use super::*;
use crate::tools::BuiltinTool;
use serde_json::json;

#[cfg(unix)]
#[tokio::test]
async fn write_file_refuses_out_of_sandbox_parent_symlink() {
    // A parent path component that is a symlink escaping the sandbox (as a
    // swapped-in TOCTOU symlink would be) must make the write refuse, never
    // landing outside cwd. resolve() rejects it; the post-create_dir_all
    // re-resolve closes the same hole when the symlink appears after the first
    // resolve.
    let outside = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), cwd.path().join("escape")).unwrap();
    let err = WriteFile
        .execute(
            json!({"path": "escape/pwned.txt", "content": "x"}),
            &ctx(cwd.path().to_path_buf()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("sandbox"), "got: {err}");
    assert!(
        !outside.path().join("pwned.txt").exists(),
        "write escaped the sandbox through a symlinked parent"
    );
}

#[tokio::test]
async fn write_file_creates_nested_new_dirs() {
    // The re-resolve after create_dir_all must not break the common case: a new
    // file in a brand-new nested subdir still writes correctly.
    let cwd = tempfile::tempdir().unwrap();
    WriteFile
        .execute(
            json!({"path": "a/b/c/new.txt", "content": "hello"}),
            &ctx(cwd.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("a/b/c/new.txt")).unwrap(),
        "hello"
    );
}

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
        err.to_string().contains("read all of it this session"),
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
async fn partial_read_does_not_authorize_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("doc.txt"), "l1\nl2\nl3\nl4").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    // A partial (limit) read must NOT satisfy the read-before-overwrite guard:
    // the model never saw lines 2-4.
    ReadFile
        .execute(json!({"path": "doc.txt", "limit": 1}), &ctx)
        .await
        .unwrap();
    let err = WriteFile
        .execute(json!({"path": "doc.txt", "content": "x"}), &ctx)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("read all of it"),
        "partial read must not authorize overwrite: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("doc.txt")).unwrap(),
        "l1\nl2\nl3\nl4",
        "file must be untouched after the refused overwrite"
    );
    // A full read (no offset/limit) then authorizes the overwrite.
    ReadFile
        .execute(json!({"path": "doc.txt"}), &ctx)
        .await
        .unwrap();
    WriteFile
        .execute(json!({"path": "doc.txt", "content": "x"}), &ctx)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("doc.txt")).unwrap(),
        "x"
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
async fn edit_file_matches_across_crlf_line_endings() {
    // A CRLF file: read_file strips the `\r` (str::lines), so the model can only
    // ever build an `\n`-joined old_string. edit_file must still match it against
    // the CRLF bytes on disk and preserve CRLF on write — otherwise every
    // multi-line edit to a Windows-style file fails with "old_string not found".
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("crlf.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    ReadFile
        .execute(json!({"path": "crlf.txt"}), &ctx)
        .await
        .unwrap();
    EditFile
        .execute(
            json!({"path": "crlf.txt", "old_string": "alpha\nbeta", "new_string": "ALPHA\nBETA"}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("crlf.txt")).unwrap(),
        "ALPHA\r\nBETA\r\ngamma\r\n",
        "the edited region is rewritten and CRLF endings are preserved"
    );
}

#[tokio::test]
async fn multi_edit_matches_across_crlf_line_endings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("crlf.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    ReadFile
        .execute(json!({"path": "crlf.txt"}), &ctx)
        .await
        .unwrap();
    MultiEdit
        .execute(
            json!({"path": "crlf.txt", "edits": [
                {"old_string": "one\ntwo", "new_string": "1\n2"},
                {"old_string": "three", "new_string": "3"},
            ]}),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("crlf.txt")).unwrap(),
        "1\r\n2\r\n3\r\n"
    );
}

#[tokio::test]
async fn edit_file_refuses_ambiguous_mixed_line_endings() {
    // A mixed-ending file holds the same text once with CRLF and once with LF.
    // read_file strips `\r`, so the model's LF old_string can't tell the two
    // regions apart. Counting only the verbatim-LF form would report 1 and
    // silently edit the LF occurrence; both forms must be counted so the edit
    // refuses as ambiguous and asks for more context, leaving the file untouched.
    let dir = tempfile::tempdir().unwrap();
    let original = "a\r\nb\r\nMID\na\nb\nEND\n";
    std::fs::write(dir.path().join("mixed.txt"), original).unwrap();
    let ctx = ctx(dir.path().to_path_buf());
    ReadFile
        .execute(json!({"path": "mixed.txt"}), &ctx)
        .await
        .unwrap();
    let err = EditFile
        .execute(
            json!({"path": "mixed.txt", "old_string": "a\nb", "new_string": "Z"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("occurs 2 times"),
        "ambiguous cross-encoding target must be reported, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mixed.txt")).unwrap(),
        original,
        "an ambiguous edit must not modify the file"
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
async fn read_large_binary_describes_instead_of_erroring() {
    let dir = tempfile::tempdir().unwrap();
    // A >5MB file whose leading bytes aren't valid UTF-8 must be summarized as
    // binary (matching the small-file path), not surface a UTF-8 decode error.
    let mut data = vec![
        0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0xFF, 0x00,
    ];
    data.resize(5_000_001, 0u8);
    std::fs::write(dir.path().join("big.png"), &data).unwrap();
    // Large files require an explicit limit.
    let out = ReadFile
        .execute(
            json!({"path": "big.png", "limit": 10}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("PNG image"), "got: {out}");
    assert!(out.contains("recorded as read"), "got: {out}");
    // The true file size is reported, not the sniff-chunk length.
    assert!(out.contains("5000001"), "got: {out}");
}

#[tokio::test]
async fn read_large_text_with_a_later_invalid_byte_renders_lossily() {
    let dir = tempfile::tempdir().unwrap();
    // The binary sniff only sees the leading ~8KB. A >5MB file that is valid
    // UTF-8 up front but carries a stray invalid byte well past that window must
    // render that line lossily, not fail the whole read with a UTF-8 error.
    let mut data = Vec::new();
    for _ in 0..1000 {
        data.extend_from_slice(b"valid text line\n");
    }
    data.extend_from_slice(b"corrupt\xFFline\n");
    while data.len() < 5_000_001 {
        data.extend_from_slice(b"padding\n");
    }
    std::fs::write(dir.path().join("big.log"), &data).unwrap();
    let out = ReadFile
        .execute(
            json!({"path": "big.log", "limit": 1100}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(
        out.contains("valid text line"),
        "leading text should render"
    );
    assert!(
        out.contains("corrupt"),
        "the stray-byte line should survive lossily"
    );
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

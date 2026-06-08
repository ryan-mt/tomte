//! fs tool tests: `read_file` and `write_file`.

use super::test_support::{ctx, read_args};
use super::*;
use crate::tools::BuiltinTool;
use serde_json::json;

#[tokio::test]
async fn bad_args_surface_as_arg_schema_error() {
    // `path` is a required string; a number makes serde reject the call. The
    // agent relies on this being an ArgSchemaError (not a runtime error like
    // "file not found") to attach a schema hint, so verify the type survives
    // the tool boundary.
    let tmp = tempfile::tempdir().unwrap();
    let err = ReadFile
        .execute(json!({"path": 123}), &ctx(tmp.path().to_path_buf()))
        .await
        .expect_err("a non-string path must be rejected");
    assert!(
        err.downcast_ref::<crate::tools::ArgSchemaError>().is_some(),
        "expected ArgSchemaError, got: {err}"
    );
    assert!(err.to_string().contains("argument schema mismatch"));
}

#[tokio::test]
async fn atomic_write_removes_temp_on_failed_swap() {
    let dir = tempfile::tempdir().unwrap();
    // Make the destination an existing directory so `rename(file -> dir)`
    // fails (EISDIR) after the temp file has already been written.
    let path = dir.path().join("dest");
    std::fs::create_dir(&path).unwrap();
    let tmp = dir.path().join("dest.tmp");

    let res = atomic_write_preserving_permissions(&path, &tmp, b"payload").await;

    assert!(res.is_err(), "rename onto a directory should fail");
    assert!(
        !tmp.exists(),
        "temp file must be cleaned up after a failed swap"
    );
}

#[tokio::test]
async fn read_file_missing_path_gives_clear_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let err = ReadFile
        .execute(
            read_args("nope.py", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("file not found"), "got: {err}");
    assert!(err.contains("nope.py"), "got: {err}");
    assert!(
        !err.contains("stat "),
        "must not leak the syscall name: {err}"
    );
}

#[tokio::test]
async fn read_file_empty_returns_system_reminder() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty.txt"), "").unwrap();
    let out = ReadFile
        .execute(
            read_args("empty.txt", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("exists but is empty"), "got: {out}");
    assert!(out.contains("<system-reminder>"), "got: {out}");
}

#[tokio::test]
async fn read_file_renders_ipynb_as_cells() {
    let dir = tempfile::tempdir().unwrap();
    let nb = r##"{
      "cells": [
        {"cell_type":"markdown","id":"md1","metadata":{},"source":["# Heading One\n"]},
        {"cell_type":"code","id":"code1","metadata":{},"execution_count":1,
         "source":["compute()\n"],
         "outputs":[
           {"output_type":"stream","name":"stdout","text":["answer=42\n"]},
           {"output_type":"display_data","data":{"image/png":"iVBORw0KGgo="},"metadata":{}}
         ]}
      ],
      "metadata":{}, "nbformat":4, "nbformat_minor":5
    }"##;
    std::fs::write(dir.path().join("nb.ipynb"), nb).unwrap();
    let out = ReadFile
        .execute(
            read_args("nb.ipynb", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("Jupyter notebook (2 cells)"), "got: {out}");
    assert!(out.contains("[cell 0 id=md1] markdown"), "got: {out}");
    assert!(out.contains("# Heading One"), "got: {out}");
    assert!(out.contains("[cell 1 id=code1] code"), "got: {out}");
    assert!(out.contains("compute()"), "got: {out}");
    assert!(out.contains("--- output ---"), "got: {out}");
    // `answer=42` only appears in the rendered stream output.
    assert!(out.contains("answer=42"), "got: {out}");
    // The base64 PNG is omitted, not dumped.
    assert!(out.contains("[image/png output omitted]"), "got: {out}");
    assert!(
        !out.contains("iVBORw0KGgo="),
        "image bytes must not leak: {out}"
    );
    // Rendered as cells, not the raw JSON.
    assert!(
        !out.contains("\"cell_type\""),
        "should not be raw JSON: {out}"
    );
}

#[tokio::test]
async fn read_file_ipynb_with_limit_returns_raw_json() {
    let dir = tempfile::tempdir().unwrap();
    let nb = r#"{"cells":[{"cell_type":"code","id":"c0","source":["x=1\n"],"outputs":[]}],"metadata":{},"nbformat":4}"#;
    std::fs::write(dir.path().join("nb.ipynb"), nb).unwrap();
    // A sliced read (explicit limit) bypasses cell rendering and shows raw JSON.
    let out = ReadFile
        .execute(
            read_args("nb.ipynb", None, Some(2000)),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(
        out.contains("\"cell_type\""),
        "expected raw JSON slice: {out}"
    );
}

#[tokio::test]
async fn read_file_attaches_image_as_media() {
    let dir = tempfile::tempdir().unwrap();
    // PNG magic bytes + payload; sniffed as image/png by execute_rich.
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[0u8; 32]);
    std::fs::write(dir.path().join("pic.png"), &bytes).unwrap();
    let out = ReadFile
        .execute_rich(
            read_args("pic.png", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert_eq!(out.media.len(), 1, "expected one image attached");
    assert_eq!(out.media[0].media_type, "image/png");
    // base64 of the PNG signature begins with iVBORw0KGgo.
    assert!(
        out.media[0].data_base64.starts_with("iVBORw0KGgo"),
        "got: {}",
        out.media[0].data_base64
    );
    assert!(
        out.text.contains("image"),
        "text note should mention the image: {}",
        out.text
    );
}

#[tokio::test]
async fn read_file_image_sliced_stays_text_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[0u8; 32]);
    std::fs::write(dir.path().join("pic.png"), &bytes).unwrap();
    // A sliced read (explicit limit) must NOT attach media — it defers to the
    // text path (which describes the binary).
    let out = ReadFile
        .execute_rich(
            read_args("pic.png", None, Some(10)),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.media.is_empty(), "sliced read must not attach media");
}

#[tokio::test]
async fn read_file_on_directory_errors_clearly() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    let err = ReadFile
        .execute(
            read_args("subdir", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("is a directory"), "got: {err}");
    assert!(
        err.contains("list_dir"),
        "should point to the right tool: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_on_fifo_errors_instead_of_hanging() {
    use std::ffi::CString;
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe");
    let c = CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo failed");
    // Without the is_file guard the eager read blocks forever (no writer), so a
    // timeout makes a regression fail loudly instead of hanging the suite.
    let fut = ReadFile.execute(read_args("pipe", None, None), &ctx(dir.path().to_path_buf()));
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), fut)
        .await
        .expect("read_file hung on a FIFO instead of rejecting it")
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a regular file"), "got: {err}");
}

#[tokio::test]
async fn list_dir_on_file_errors_clearly() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let err = ListDir
        .execute(json!({"path": "a.txt"}), &ctx(dir.path().to_path_buf()))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("is a file"), "got: {err}");
    assert!(
        err.contains("read_file"),
        "should point to the right tool: {err}"
    );
}

#[tokio::test]
async fn read_file_caps_at_default_limit_and_emits_continuation_notice() {
    let dir = tempfile::tempdir().unwrap();
    // 2500 lines: hits the 2000-line default cap, leaves 500 remaining.
    let content: String = (1..=2500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), &content).unwrap();
    let out = ReadFile
        .execute(
            read_args("big.txt", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    // First and last printed lines fall inside [1, 2000].
    assert!(out.contains("\tline 1\n"), "missing first line");
    assert!(out.contains("\tline 2000\n"), "missing 2000th line");
    assert!(
        !out.contains("\tline 2001\n"),
        "should have stopped at default cap"
    );
    assert!(
        out.contains("500 more line"),
        "missing continuation hint: {out}"
    );
    assert!(out.contains("offset=2000"), "missing offset hint: {out}");
}

#[tokio::test]
async fn read_file_truncates_long_lines() {
    let dir = tempfile::tempdir().unwrap();
    // 3000 'a' characters on one line → must be truncated to 2000 + marker.
    let huge_line: String = "a".repeat(3000);
    std::fs::write(dir.path().join("min.js"), &huge_line).unwrap();
    let out = ReadFile
        .execute(
            read_args("min.js", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(
        out.contains("[line truncated]"),
        "missing truncation marker: {out}"
    );
    // The full 3000-char line must NOT have been emitted verbatim.
    assert!(
        !out.contains(&"a".repeat(3000)),
        "long line was not truncated"
    );
}

#[tokio::test]
async fn read_file_respects_explicit_offset_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let content: String = (1..=10).map(|i| format!("L{i}\n")).collect();
    std::fs::write(dir.path().join("small.txt"), &content).unwrap();
    let out = ReadFile
        .execute(
            read_args("small.txt", Some(3), Some(2)),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    // Lines 4 and 5 (offset is zero-indexed in the slice, displayed 1-indexed).
    assert!(out.contains("\tL4\n"), "got: {out}");
    assert!(out.contains("\tL5\n"), "got: {out}");
    assert!(!out.contains("\tL3\n"), "should not include L3");
    assert!(!out.contains("\tL6\n"), "should not include L6");
    // 5 lines after offset 5 remain → continuation notice expected.
    assert!(out.contains("more line"), "missing continuation: {out}");
}

#[tokio::test]
async fn read_file_accepts_string_offset_and_limit_args() {
    let dir = tempfile::tempdir().unwrap();
    let content: String = (1..=5).map(|i| format!("L{i}\n")).collect();
    std::fs::write(dir.path().join("small.txt"), &content).unwrap();

    let out = ReadFile
        .execute(
            json!({"path": "small.txt", "offset": "2", "limit": "1"}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();

    assert!(out.contains("\tL3\n"), "got: {out}");
    assert!(!out.contains("\tL2\n"), "got: {out}");
    assert!(!out.contains("\tL4\n"), "got: {out}");
}

#[tokio::test]
async fn read_file_accepts_claude_file_path_alias_and_absolute_path_inside_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.txt");
    std::fs::write(&path, "hello\n").unwrap();

    let out = ReadFile
        .execute(
            json!({"file_path": path.to_string_lossy()}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();

    assert!(out.contains("\thello\n"), "got: {out}");
}

#[tokio::test]
async fn absolute_path_outside_cwd_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("secret.txt");
    std::fs::write(&path, "secret\n").unwrap();

    let err = ReadFile
        .execute(
            json!({"file_path": path.to_string_lossy()}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("escapes the sandbox"),
        "got: {err}"
    );
}

#[tokio::test]
async fn read_file_rejects_zero_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("small.txt"), "hello\n").unwrap();

    let err = ReadFile
        .execute(
            read_args("small.txt", None, Some(0)),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("limit must be greater than 0"));
}

#[tokio::test]
async fn failed_read_file_does_not_authorize_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("small.txt"), "original\n").unwrap();
    let ctx = ctx(dir.path().to_path_buf());

    ReadFile
        .execute(read_args("small.txt", None, Some(0)), &ctx)
        .await
        .unwrap_err();

    let err = WriteFile
        .execute(
            json!({"path": "small.txt", "content": "replacement\n"}),
            &ctx,
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("read all of it this session"),
        "got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("small.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn read_file_rejects_limit_above_hard_cap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("small.txt"), "hello\n").unwrap();

    let err = ReadFile
        .execute(
            read_args("small.txt", None, Some(2001)),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("limit must be <= 2000"));
}

#[tokio::test]
async fn list_dir_accepts_common_directory_aliases() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "mod tests;\n").unwrap();

    let out = ListDir
        .execute(json!({"directory": "src"}), &ctx(dir.path().to_path_buf()))
        .await
        .unwrap();

    assert_eq!(out.trim(), "lib.rs");
}

#[tokio::test]
async fn read_file_streams_large_file_when_limit_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.log");
    let mut content = String::new();
    for i in 1..=470_000 {
        content.push_str(&format!("line {i:06}\n"));
    }
    assert!(content.len() > 5_000_000);
    std::fs::write(&path, content).unwrap();

    let out = ReadFile
        .execute(
            read_args("large.log", Some(2), Some(2)),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();

    assert!(out.contains("\tline 000003\n"), "got: {out}");
    assert!(out.contains("\tline 000004\n"), "got: {out}");
    assert!(!out.contains("\tline 000005\n"), "got: {out}");
    assert!(out.contains("More lines remain"), "got: {out}");
    assert!(out.contains("offset=4"), "got: {out}");
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_rejects_symlink_escape() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();

    let err = ReadFile
        .execute(
            read_args("outside/secret.txt", None, None),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("path escapes the sandbox"));
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_rejects_symlink_escape_through_parent() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("outside")).unwrap();

    let err = WriteFile
        .execute(
            json!({"path": "outside/owned.txt", "content": "owned"}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("path escapes the sandbox"));
    assert!(!outside.path().join("owned.txt").exists());
}

#[tokio::test]
async fn write_file_rejects_directory_targets() {
    let dir = tempfile::tempdir().unwrap();

    let err = WriteFile
        .execute(
            json!({"path": ".", "content": "not a directory"}),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("cannot write file over directory"));
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_refuses_existing_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("write-only.txt");
    std::fs::write(&path, "secret").unwrap();
    let ctx = ctx(dir.path().to_path_buf());

    // Read it first so the read-before-write guard is satisfied, then revoke
    // read permission to exercise the "can't snapshot original" guard — a
    // TOCTOU where the file becomes unreadable between read and write.
    ReadFile
        .execute(json!({"path": "write-only.txt"}), &ctx)
        .await
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();

    let err = WriteFile
        .execute(
            json!({"path": "write-only.txt", "content": "replacement"}),
            &ctx,
        )
        .await
        .unwrap_err();

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(err.to_string().contains("read original"), "got: {err}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "secret");
}

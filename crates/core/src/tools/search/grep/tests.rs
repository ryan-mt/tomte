//! `grep` tool tests, split out of `search`.

use super::super::test_support::{ctx, grep_available, missing_rg, rg_available, write};
use super::*;

fn grep_args(pattern: &str) -> GrepArgs {
    GrepArgs {
        pattern: pattern.to_string(),
        path: None,
        glob: None,
        case_insensitive: false,
        output_mode: None,
        head_limit: None,
        offset: None,
        context_after: None,
        context_before: None,
        context: None,
        multiline: None,
        file_type: None,
    }
}

#[test]
fn grep_args_accept_camel_case_aliases() {
    let args: GrepArgs = serde_json::from_value(json!({
        "pattern": "needle",
        "caseInsensitive": "true",
        "outputMode": "files-with-matches",
        "headLimit": "10",
        "offset": "3",
        "contextAfter": "2",
        "contextBefore": 1,
        "contextLines": null,
        "multiLine": "yes",
        "fileType": "rust"
    }))
    .unwrap();

    assert!(args.case_insensitive);
    assert_eq!(args.output_mode.as_deref(), Some("files-with-matches"));
    assert_eq!(args.head_limit, Some(10));
    assert_eq!(args.offset, Some(3));
    assert_eq!(args.context_after, Some(2));
    assert_eq!(args.context_before, Some(1));
    assert_eq!(args.context, None);
    assert_eq!(args.multiline, Some(true));
    assert_eq!(args.file_type.as_deref(), Some("rust"));
}

#[test]
fn grep_output_mode_accepts_common_model_aliases() {
    assert_eq!(normalize_grep_output_mode(None), Some("content"));
    assert_eq!(normalize_grep_output_mode(Some("matches")), Some("content"));
    assert_eq!(
        normalize_grep_output_mode(Some("files-with-matches")),
        Some("files_with_matches")
    );
    assert_eq!(
        normalize_grep_output_mode(Some("paths_only")),
        Some("files_with_matches")
    );
    assert_eq!(normalize_grep_output_mode(Some("counts")), Some("count"));
    assert_eq!(normalize_grep_output_mode(Some("wat")), None);
}

#[tokio::test]
async fn grep_files_with_matches_mode_returns_paths_only() {
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", "hello\nworld\n");
    write(dir.path(), "b.txt", "no match here\n");
    write(dir.path(), "c/d.txt", "hello again\n");
    let out = Grep
        .execute(
            json!({
                "pattern": "hello",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "files_with_matches",
                "head_limit": null, "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("a.txt"), "got: {out}");
    assert!(out.contains("d.txt"), "got: {out}");
    assert!(!out.contains("b.txt"), "got: {out}");
    // No lineno:content shape — just paths.
    assert!(!out.contains("hello"), "got: {out}");
}

#[tokio::test]
async fn grep_fallback_files_with_matches_mode_returns_paths_only() {
    if !grep_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", "hello\nworld\n");
    write(dir.path(), "b.txt", "no match here\n");
    write(dir.path(), "c/d.txt", "hello again\n");

    let mut args = grep_args("hello");
    args.output_mode = Some("files_with_matches".to_string());
    let out = execute_grep_with_commands(
        &args,
        &ctx(dir.path().to_path_buf()),
        &missing_rg(dir.path()),
        "grep",
    )
    .await
    .unwrap();

    assert!(out.contains("a.txt"), "got: {out}");
    assert!(out.contains("d.txt"), "got: {out}");
    assert!(!out.contains("b.txt"), "got: {out}");
    assert!(!out.contains("hello"), "got: {out}");
}

#[tokio::test]
async fn grep_count_mode_returns_path_colon_count() {
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", "x\nx\nx\n");
    write(dir.path(), "b.txt", "x\n");
    let out = Grep
        .execute(
            json!({
                "pattern": "x",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "count",
                "head_limit": null, "offset": null, "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("a.txt:3"), "got: {out}");
    assert!(out.contains("b.txt:1"), "got: {out}");
}

#[tokio::test]
async fn grep_fallback_count_mode_returns_matching_files_only() {
    if !grep_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", "x\nx\n");
    write(dir.path(), "b.txt", "no hit\n");

    let mut args = grep_args("x");
    args.output_mode = Some("count".to_string());
    let out = execute_grep_with_commands(
        &args,
        &ctx(dir.path().to_path_buf()),
        &missing_rg(dir.path()),
        "grep",
    )
    .await
    .unwrap();

    assert!(out.contains("a.txt:2"), "got: {out}");
    assert!(!out.contains("b.txt:0"), "got: {out}");
}

#[tokio::test]
async fn grep_fallback_rejects_unsupported_glob_instead_of_ignoring_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut args = grep_args("x");
    args.glob = Some("*.rs".to_string());

    let err = execute_grep_with_commands(
        &args,
        &ctx(dir.path().to_path_buf()),
        &missing_rg(dir.path()),
        "grep",
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("does not support 'glob'"),
        "got: {err}"
    );
}

#[tokio::test]
async fn grep_head_limit_caps_output_lines() {
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=50).map(|i| format!("hit line {i}\n")).collect();
    write(dir.path(), "big.txt", &body);
    let out = Grep
        .execute(
            json!({
                "pattern": "hit",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "content",
                "head_limit": 5,
                "offset": null,
                "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("head_limit hit"), "got: {out}");
    // First 5 lines present, line 6+ NOT.
    assert!(out.contains("hit line 5"), "got: {out}");
    assert!(!out.contains("hit line 6"), "got: {out}");
}

#[tokio::test]
async fn grep_offset_skips_lines_before_head_limit() {
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=8).map(|i| format!("hit line {i}\n")).collect();
    write(dir.path(), "big.txt", &body);
    let out = Grep
        .execute(
            json!({
                "pattern": "hit",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "content",
                "head_limit": 2,
                "offset": 3,
                "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("offset skipped 3"), "got: {out}");
    assert!(!out.contains("hit line 3"), "got: {out}");
    assert!(out.contains("hit line 4"), "got: {out}");
    assert!(out.contains("hit line 5"), "got: {out}");
    assert!(!out.contains("hit line 6"), "got: {out}");
}

#[tokio::test]
async fn grep_context_after_includes_following_lines() {
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "anchor\nline2\nline3\nline4\n");
    let out = Grep
        .execute(
            json!({
                "pattern": "anchor",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "content",
                "head_limit": null,
                "offset": null,
                "context_after": 2,
                "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(out.contains("anchor"), "got: {out}");
    assert!(out.contains("line2"), "got: {out}");
    assert!(out.contains("line3"), "got: {out}");
    assert!(!out.contains("line4"), "got: {out}");
}

#[tokio::test]
async fn grep_accepts_claude_flag_aliases() {
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "before\nALPHA\nafter\n");

    let out = Grep
        .execute(
            json!({
                "pattern": "alpha",
                "output_mode": "content",
                "-i": "true",
                "-C": "1"
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();

    assert!(out.contains("before"), "got: {out}");
    assert!(out.contains("ALPHA"), "got: {out}");
    assert!(out.contains("after"), "got: {out}");
}

#[tokio::test]
async fn grep_with_path_returns_paths_relative_to_cwd() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/lib.rs", "needle\n");

    let out = Grep
        .execute(
            json!({
                "pattern": "needle",
                "path": "src",
                "glob": null,
                "case_insensitive": false,
                "output_mode": "content",
                "head_limit": null,
                "offset": null,
                "context_after": null,
                "context_before": null,
                "multiline": null,
                "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();

    assert!(out.contains("src/lib.rs:1:needle"), "got: {out}");
    assert!(
        !out.contains(&dir.path().display().to_string()),
        "grep output should be cwd-relative: {out}"
    );
}

#[tokio::test]
async fn grep_rejects_invalid_output_mode() {
    let dir = tempfile::tempdir().unwrap();
    let err = Grep
        .execute(
            json!({
                "pattern": "x",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "wat",
                "head_limit": null, "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("output_mode"), "got: {err}");
}

#[tokio::test]
async fn grep_dash_pattern_is_searched_literally() {
    // Regression: a pattern starting with `-` must be searched literally,
    // not parsed as ripgrep flags (fixed by the `--` separator).
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "this line has -rf in it\nplain line\n");
    let out = Grep
        .execute(
            json!({
                "pattern": "-rf",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "content",
                "head_limit": null, "offset": null, "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
    assert!(
        out.contains("-rf"),
        "dash pattern must match literally; got: {out}"
    );
}

#[tokio::test]
async fn grep_invalid_regex_surfaces_error_not_empty() {
    // Regression: an invalid regex (rg exit 2) must surface an error, not an
    // empty string the model reads as "no matches found".
    if !rg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "f.txt", "content\n");
    let res = Grep
        .execute(
            json!({
                "pattern": "(",
                "path": null, "glob": null, "case_insensitive": false,
                "output_mode": "content",
                "head_limit": null, "offset": null, "context_after": null, "context_before": null,
                "multiline": null, "file_type": null,
            }),
            &ctx(dir.path().to_path_buf()),
        )
        .await;
    assert!(
        res.is_err(),
        "invalid regex must surface an error, not empty output; got: {res:?}"
    );
}

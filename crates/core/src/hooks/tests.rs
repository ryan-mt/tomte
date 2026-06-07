use super::*;

#[test]
fn matches_wildcard_and_exact() {
    assert!(matches("*", "any", None));
    assert!(matches("run_shell", "run_shell", None));
    assert!(!matches("run_shell", "read_file", None));
}

#[test]
fn matches_regex_via_re_prefix() {
    assert!(matches("re:^edit_", "edit_file", None));
    assert!(!matches("re:^edit_", "multi_edit_file", None)); // anchored start
    assert!(!matches("re:^edit_", "read_file", None));
    assert!(matches("re:^run_shell$", "run_shell", None)); // anchored exact
    assert!(!matches("re:^run_shell$", "run_shell_extra", None));
}

#[test]
fn matches_file_glob_against_path_hint() {
    assert!(matches("file:**/*.rs", "edit_file", Some("src/foo.rs")));
    assert!(matches("file:**/*.rs", "edit_file", Some("a/b/c.rs")));
    assert!(matches("file:**/*.rs", "edit_file", Some("main.rs")));
    assert!(!matches("file:**/*.rs", "edit_file", Some("src/foo.ts")));
    assert!(!matches("file:**/*.rs", "edit_file", None));
}

#[test]
fn path_hint_from_args_includes_notebook_path() {
    let args = serde_json::json!({
        "notebook_path": "analysis/demo.ipynb",
        "edit_mode": "replace"
    });

    assert_eq!(
        HookSet::path_hint_from_args(&args).as_deref(),
        Some("analysis/demo.ipynb")
    );
}

#[test]
fn path_hint_from_args_includes_camel_case_and_directory_aliases() {
    let args = serde_json::json!({
        "filePath": "src/lib.rs"
    });
    assert_eq!(
        HookSet::path_hint_from_args(&args).as_deref(),
        Some("src/lib.rs")
    );

    let args = serde_json::json!({
        "notebookPath": "analysis/demo.ipynb"
    });
    assert_eq!(
        HookSet::path_hint_from_args(&args).as_deref(),
        Some("analysis/demo.ipynb")
    );

    let args = serde_json::json!({
        "directory": "src"
    });
    assert_eq!(HookSet::path_hint_from_args(&args).as_deref(), Some("src"));
}

#[test]
fn glob_match_basic() {
    assert!(glob_match("*.rs", "main.rs"));
    assert!(glob_match("src/*.rs", "src/lib.rs"));
    assert!(!glob_match("*.rs", "src/main.rs"));
    assert!(glob_match("**/*.rs", "main.rs"));
    assert!(glob_match("**/*.rs", "deep/nested/file.rs"));
    assert!(!glob_match("*.rs", "main.ts"));
    assert!(glob_match("?ello", "hello"));
    assert!(!glob_match("?ello", "yyello"));
}

#[cfg(unix)]
fn sh_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(unix)]
#[tokio::test]
async fn run_hook_timeout_kills_background_descendants() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("survived-hook-timeout");
    let command = format!(
        "(sleep 0.5; printf survived > {}) & wait",
        sh_quote(&marker)
    );

    let err = run_hook_with_timeout(
        &command,
        &serde_json::json!({"hook": "test"}),
        Duration::from_millis(80),
    )
    .await
    .unwrap_err();

    assert!(err.to_string().contains("elapsed"), "got: {err}");
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(
        !marker.exists(),
        "hook timeout killed only the shell; a background descendant survived"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn run_hook_caps_stdout_but_keeps_exit_code() {
    let (code, stdout) = run_hook_with_timeout(
        "head -c 70000 /dev/zero | tr '\\0' x",
        &serde_json::json!({"hook": "test"}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();

    assert_eq!(code, 0);
    assert!(stdout.starts_with("xxx"), "got: {stdout:?}");
    assert!(stdout.contains("truncated hook stdout"), "got: {stdout:?}");
    assert!(stdout.len() < HOOK_STDOUT_MAX_BYTES + 256);
}

#[cfg(unix)]
#[tokio::test]
async fn run_hook_drains_noisy_stderr_without_blocking() {
    let (code, stdout) = run_hook_with_timeout(
        "head -c 200000 /dev/zero >&2",
        &serde_json::json!({"hook": "test"}),
        Duration::from_secs(2),
    )
    .await
    .unwrap();

    assert_eq!(code, 0);
    assert!(stdout.is_empty(), "stderr should not leak into stdout");
}

// Regression: a hook that emits a large stdout BEFORE reading its (large)
// stdin used to deadlock — we blocked writing the payload while the hook
// blocked writing output. With readers spawned first and stdin written on
// its own task, this completes well within the timeout.
#[cfg(unix)]
#[tokio::test]
async fn run_hook_does_not_deadlock_when_output_precedes_stdin_read() {
    // Payload larger than a pipe buffer (~64 KiB), like a real PostToolUse
    // payload carrying tool output.
    let payload = serde_json::json!({ "tool_output": "x".repeat(256 * 1024) });
    let (code, _stdout) = run_hook_with_timeout(
        // Write 200 KiB to stdout first, then drain stdin.
        "head -c 200000 /dev/zero; cat >/dev/null",
        &payload,
        Duration::from_secs(10),
    )
    .await
    .expect("must not hang or error");
    assert_eq!(code, 0);
}

#[tokio::test]
async fn probe_command_runs_cross_platform_and_reports_exit_code() {
    // `echo hi` and `exit N` behave identically under `sh -c` and `cmd /C`, so
    // this test is meaningful on Linux, macOS, and Windows alike.
    let (code, out) = probe_command("echo hi", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(code, 0);
    assert!(out.contains("hi"), "got: {out:?}");

    let (code, _out) = probe_command("exit 3", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(code, 3);
}

#[test]
fn shell_invocation_is_cross_platform() {
    // Unix always runs hooks via `sh -c`. Windows uses `sh -c` when Git Bash is
    // present and falls back to `cmd /C` otherwise — so a hook runs on every OS,
    // not just the box that happens to ship a POSIX shell.
    if cfg!(windows) {
        assert_eq!(shell_invocation(true), ("sh", "-c"));
        assert_eq!(shell_invocation(false), ("cmd", "/C"));
    } else {
        assert_eq!(shell_invocation(true), ("sh", "-c"));
        assert_eq!(shell_invocation(false), ("sh", "-c"));
    }
}

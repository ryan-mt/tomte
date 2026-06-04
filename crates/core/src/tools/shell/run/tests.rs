//! `run_shell` tests, split out of `shell`.

use super::super::test_support::*;
use super::*;
use crate::tools::{ApprovalMode, SessionState};
use tokio::sync::Mutex;

#[test]
fn danger_reason_flags_destructive_and_passes_safe() {
    // The approval gate calls this to force a human prompt for destructive
    // commands even under an allow rule, so it must mirror classify_danger and
    // read the command via both the `command` field and the `cmd` alias.
    assert!(RunShell
        .danger_reason(&json!({"command": "git push --force origin main"}))
        .is_some());
    assert!(RunShell
        .danger_reason(&json!({"command": "rm -rf /*/"}))
        .is_some());
    assert!(RunShell
        .danger_reason(&json!({"cmd": "rm -rf ~"}))
        .is_some());
    assert!(RunShell
        .danger_reason(&json!({"command": "git status"}))
        .is_none());
    assert!(RunShell.danger_reason(&json!({})).is_none());
}

#[tokio::test]
async fn background_run_returns_id_and_captures_output() {
    let ctx = ctx();
    // Print, then exit fast.
    let out = RunShell
        .execute(
            json!({
                "command": print_command("hello-bg"),
                "timeout_ms": null,
                "run_in_background": true,
                "dangerous_override": null,
            }),
            &ctx,
        )
        .await
        .unwrap();
    let id = parse_bash_id(&out);
    let final_out = wait_until_status(&ctx, &id, "exited", 5000).await;
    let v: serde_json::Value = serde_json::from_str(&final_out).unwrap();
    let stdout = v.get("stdout").unwrap().as_str().unwrap();
    assert!(stdout.contains("hello-bg"), "got: {final_out}");
    assert!(v
        .get("status")
        .unwrap()
        .as_str()
        .unwrap()
        .contains("exited(0)"));
}

#[cfg(unix)]
#[tokio::test]
async fn run_shell_timeout_kills_background_descendants() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("survived-timeout");
    let ctx = ctx_at(tmp.path().to_path_buf());
    let command = format!(
        "(sleep 0.5; printf survived > {}) & wait",
        sh_quote(&marker)
    );

    let err = RunShell
        .execute(
            json!({
                "command": command,
                "timeout_ms": 80,
                "run_in_background": false,
                "dangerous_override": null,
            }),
            &ctx,
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("timed out"), "got: {err}");
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    assert!(
        !marker.exists(),
        "timeout killed only the shell; a background descendant survived"
    );
}

// Regression: when the shell exits but a backgrounded grandchild keeps the
// stdout pipe open, the foreground call must stay bounded by the timeout
// (the drain used to run outside it and hang for the descendant's lifetime).
#[cfg(unix)]
#[tokio::test]
async fn foreground_does_not_hang_when_descendant_holds_the_pipe() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ctx_at(tmp.path().to_path_buf());
    let start = std::time::Instant::now();
    // `sleep 30 &` inherits stdout and outlives the shell, which exits at once.
    let err = RunShell
        .execute(
            json!({
                "command": "sleep 30 &",
                "timeout_ms": 300,
                "run_in_background": false,
                "dangerous_override": null,
            }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "call must be bounded by the timeout, not the descendant's lifetime"
    );
}

// Regression: a background shell must be killed when the session ends, not
// leaked as an orphan (nothing else fires its kill switch on shutdown).
#[cfg(unix)]
#[tokio::test]
async fn dropping_session_kills_background_shells() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("survived");
    let ctx = ctx_at(tmp.path().to_path_buf());
    RunShell
        .execute(
            json!({
                "command": format!("sleep 0.5; printf x > {}", sh_quote(&marker)),
                "run_in_background": true,
                "dangerous_override": null,
            }),
            &ctx,
        )
        .await
        .unwrap();
    // Dropping the only Arc<Mutex<SessionState>> ends the session, which must
    // SIGKILL the background shell's process group before the sleep elapses.
    drop(ctx);
    tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    assert!(
        !marker.exists(),
        "background shell survived the session ending"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn foreground_run_shell_caps_large_stdout() {
    let tmp = tempfile::tempdir().unwrap();
    let big = tmp.path().join("big.txt");
    std::fs::write(
        &big,
        vec![b'x'; FOREGROUND_OUTPUT_MAX_BYTES_PER_STREAM + 8192],
    )
    .unwrap();
    let ctx = ctx_at(tmp.path().to_path_buf());
    let command = format!("cat {}", sh_quote(&big));

    let out = RunShell
        .execute(
            json!({
                "command": command,
                "timeout_ms": null,
                "run_in_background": false,
                "dangerous_override": null,
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.contains("stdout truncated"), "got: {out}");
    assert!(
        out.len() < FOREGROUND_OUTPUT_MAX_BYTES_PER_STREAM + 4096,
        "foreground output was not bounded: {} bytes",
        out.len()
    );
}

#[test]
fn run_shell_outer_timeout_honors_timeout_ms() {
    // Default: the 180s backstop.
    assert_eq!(
        RunShell.timeout(&json!({"command": "x"})),
        std::time::Duration::from_secs(180)
    );
    // A long foreground build must get headroom above its requested budget,
    // not be capped at the 180s default.
    assert_eq!(
        RunShell.timeout(&json!({"command": "x", "timeout_ms": 600000})),
        std::time::Duration::from_secs(630)
    );
    // String form is accepted like the deserializer.
    assert_eq!(
        RunShell.timeout(&json!({"command": "x", "timeout_ms": "600000"})),
        std::time::Duration::from_secs(630)
    );
    // The camelCase/short aliases the deserializer accepts must be honored
    // too, or an alias-using long build would be killed at the default.
    assert_eq!(
        RunShell.timeout(&json!({"command": "x", "timeoutMs": 600000})),
        std::time::Duration::from_secs(630)
    );
    assert_eq!(
        RunShell.timeout(&json!({"command": "x", "timeout": 600000})),
        std::time::Duration::from_secs(630)
    );
    // Background returns immediately, so the default backstop is fine.
    assert_eq!(
        RunShell.timeout(&json!({"command": "x", "timeout_ms": 600000, "run_in_background": true})),
        std::time::Duration::from_secs(180)
    );
}

#[tokio::test]
async fn run_shell_refuses_dangerous_command_without_override() {
    let ctx = ctx();
    let err = RunShell.execute(json!({"command": "rm -rf /", "timeout_ms": null, "run_in_background": false, "dangerous_override": null}), &ctx).await.unwrap_err();
    assert!(err.to_string().contains("refused"));
}

#[tokio::test]
async fn run_shell_accepts_claude_timeout_and_semantic_boolean_args() {
    let ctx = ctx();
    let out = RunShell
        .execute(
            json!({
                "command": print_command("shell-ok"),
                "timeout": "5000",
                "run_in_background": "false",
                "description": "Print marker"
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.contains("shell-ok"), "got: {out}");
}

#[tokio::test]
async fn run_shell_accepts_cmd_alias() {
    let ctx = ctx();
    let out = RunShell
        .execute(
            json!({
                "cmd": print_command("cmd-alias-ok"),
                "timeout_ms": 5000,
                "run_in_background": false,
                "dangerous_override": null
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.contains("cmd-alias-ok"), "got: {out}");
}

#[tokio::test]
async fn run_shell_accepts_camel_case_aliases() {
    let ctx = ctx();
    let out = RunShell
        .execute(
            json!({
                "command": print_command("camel-shell-ok"),
                "timeoutMs": "5000",
                "runInBackground": "false",
                "dangerousOverride": null
            }),
            &ctx,
        )
        .await
        .unwrap();

    assert!(out.contains("camel-shell-ok"), "got: {out}");
}

#[tokio::test]
async fn run_shell_allows_dangerous_command_with_override() {
    // Run in an isolated temp dir so the dangerous command can never touch
    // the real working tree. Previously this used cwd = repo root, so
    // `cargo test` executed `git reset --hard HEAD` against this repo and
    // wiped any uncommitted work.
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ToolContext {
        cwd: tmp.path().to_path_buf(),
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: false,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: crate::config::Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    };
    let out = RunShell
            .execute(
                json!({"command": "git reset --hard HEAD", "timeout_ms": 5000, "run_in_background": false, "dangerous_override": true}),
                &ctx,
            )
            .await
            .unwrap();
    assert!(out.contains("exit_code:"));
}

#[tokio::test]
async fn run_shell_ignores_model_override_in_non_interactive_run() {
    // In a headless run a prompt-injected model could set dangerous_override
    // itself, so it must NOT clear the destructive-command guard. The command
    // is refused outright (never executed) even with the override set.
    let tmp = tempfile::tempdir().unwrap();
    let ctx = ToolContext {
        cwd: tmp.path().to_path_buf(),
        approval: ApprovalMode::Auto,
        require_approval: false,
        auto_approve_edits: false,
        non_interactive: true,
        session: Arc::new(Mutex::new(SessionState::default())),
        config: crate::config::Config::default(),
        cwd_override: Arc::new(Mutex::new(None)),
        events: None,
    };
    let err = RunShell
            .execute(
                json!({"command": "git reset --hard HEAD", "timeout_ms": 5000, "run_in_background": false, "dangerous_override": true}),
                &ctx,
            )
            .await
            .expect_err("destructive command must be refused in a non-interactive run");
    let msg = err.to_string();
    assert!(msg.contains("refused"), "got: {msg}");
    assert!(msg.contains("non-interactive"), "got: {msg}");
}

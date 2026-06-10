
use super::super::friendly_body;
use serde_json::json;

fn text(lines: &[ratatui::text::Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn shell_output(code: i32, stdout: &str, stderr: &str) -> String {
    format!("exit_code: {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")
}

#[test]
fn failed_command_shows_red_stderr_and_error_footer_no_box() {
    let out = shell_output(101, "", "error: no such command: audit");
    // A non-zero exit is NOT a tool error — run_shell returns Ok with the
    // exit code embedded, so `error` is false and the run_shell formatter runs.
    let lines = friendly_body(
        "run_shell",
        &json!({"command": "cargo audit"}),
        Some(&out),
        false,
        80,
        false,
    );
    let rendered = text(&lines);
    assert!(
        rendered.contains("error: no such command: audit"),
        "got: {rendered}"
    );
    assert!(rendered.contains("Error (exit 101)"), "got: {rendered}");
    // No yellow "─ stderr ─" separator box.
    assert!(!rendered.contains("─ stderr ─"), "got: {rendered}");
}

#[test]
fn successful_command_has_no_exit_footer() {
    let out = shell_output(0, "all good", "");
    let lines = friendly_body(
        "run_shell",
        &json!({"command": "echo hi"}),
        Some(&out),
        false,
        80,
        false,
    );
    let rendered = text(&lines);
    assert!(rendered.contains("all good"), "got: {rendered}");
    assert!(
        !rendered.contains("exit"),
        "success must not show an exit line: {rendered}"
    );
    assert!(!rendered.contains("Error"), "got: {rendered}");
}

#[test]
fn failed_command_shows_more_than_the_success_preview() {
    // 20 stdout lines: the collapsed failure budget (15) shows far more than
    // the 3-line success preview, still bounded with a "more" hint.
    let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let out = shell_output(1, body.trim_end(), "");
    let lines = friendly_body(
        "run_shell",
        &json!({"command": "cargo fmt --check"}),
        Some(&out),
        false,
        80,
        false,
    );
    let rendered = text(&lines);
    assert!(
        rendered.contains("line 15"),
        "should show ~15 lines on failure: {rendered}"
    );
    assert!(
        !rendered.contains("line 16"),
        "should cap at the failure budget: {rendered}"
    );
    assert!(rendered.contains("+5 more line"), "got: {rendered}");
}

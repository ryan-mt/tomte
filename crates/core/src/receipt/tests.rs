use super::*;
use crate::openai::InputItem;
use crate::proof::{CheckResult, ProjectKind};

fn call(name: &str, arguments: &str) -> InputItem {
    InputItem::FunctionCall {
        call_id: "c1".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn capsule(checks: Vec<CheckResult>, files: Vec<&str>) -> ProofCapsule {
    ProofCapsule {
        timestamp: "2026-06-09 12:00".into(),
        project_kind: ProjectKind::Rust,
        files_changed: files.into_iter().map(str::to_string).collect(),
        checks,
        reproduce: vec!["cargo test".into()],
    }
}

fn check(name: &str, outcome: Outcome) -> CheckResult {
    CheckResult {
        name: name.into(),
        command: "cargo test".into(),
        outcome,
        tail: String::new(),
    }
}

fn receipt() -> Receipt {
    Receipt {
        generated: "2026-06-09 12:00".into(),
        root: "/repo".into(),
        branch: "main".into(),
        head: "abc1234 fix the thing".into(),
        capsule: capsule(vec![check("test", Outcome::Passed)], vec![" M src/a.rs"]),
        seal: Some(SealBrief {
            commit: "abc12345".into(),
            sealed_at: "2026-06-09 11:00:00".into(),
            verified: true,
            status: "verified".into(),
        }),
        decisions: vec![DecisionBrief {
            loc: "src/a.rs:10".into(),
            decision: "keep the retry".into(),
            why: "the API flakes".into(),
            model: "gpt-5.5".into(),
        }],
        decisions_total: 7,
        drift: DriftBrief {
            present: 4,
            healed: 2,
            stale: 1,
        },
        session: Some(SessionBrief {
            id: "s-1".into(),
            model: "gpt-5.5".into(),
            turns: 3,
            commands: vec!["cargo build".into()],
            commands_total: 25,
            files_edited: vec!["src/a.rs".into()],
            files_edited_total: 1,
            cost: vec![CostLine {
                model: "gpt-5.5".into(),
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: 0.02,
            }],
            total_cost_usd: 0.02,
        }),
    }
}

// The extraction rules: run_shell contributes its command line, the edit tools
// contribute their target path under any accepted spelling, paths dedupe in
// first-touch order, and unparseable arguments are skipped, never a panic.
#[test]
fn session_activity_extracts_commands_and_edited_files() {
    let history = vec![
        call("run_shell", r#"{"command":"cargo test"}"#),
        call("run_shell", r#"{"command":"  "}"#), // blank → dropped
        call("write_file", r#"{"path":"src/a.rs","content":"x"}"#),
        call("edit_file", r#"{"file_path":"src/b.rs"}"#),
        call("notebook_edit", r#"{"notebook_path":"nb.ipynb"}"#),
        call("multi_edit", r#"{"path":"src/a.rs"}"#), // dup → deduped
        call("read_file", r#"{"path":"src/ignored.rs"}"#), // read tool → not an edit
        call("run_shell", "not json"),                // unparseable → skipped
        call("run_shell", r#"{"command":"git status"}"#),
    ];
    let (commands, files) = session_activity(&history);
    assert_eq!(commands, vec!["cargo test", "git status"]);
    assert_eq!(files, vec!["src/a.rs", "src/b.rs", "nb.ipynb"]);
}

// The populated receipt renders every section with its data: verdict, seal,
// git'd file list, check lines, session activity (with the over-cap count),
// cost, decisions, and the drift watch.
#[test]
fn markdown_renders_all_sections() {
    let md = render_markdown(&receipt());
    assert!(md.contains("## Verdict: ✅ Verified"));
    assert!(md.contains("branch `main` · HEAD `abc1234 fix the thing`"));
    assert!(md.contains("seal: ✅ HEAD is sealed and verified (`abc12345`"));
    assert!(md.contains("- ` M src/a.rs`"));
    assert!(md.contains("- ✅ test — passed — `cargo test`"));
    assert!(md.contains("session `s-1`, 3 turn(s), model gpt-5.5"));
    assert!(md.contains("- `cargo build`"));
    assert!(md.contains("… and 24 more"));
    assert!(md.contains("total: $0.0200"));
    assert!(md.contains("`src/a.rs:10` — keep the retry — because the API flakes"));
    assert!(md.contains("… 6 more on the trail"));
    assert!(md.contains("4 anchor(s) hold · 2 healed · 1 need eyes"));
    assert!(md.contains("done means verified"));
}

// A failing check turns the verdict red and the seal's verify failure is
// surfaced verbatim — a receipt reports, it never embellishes.
#[test]
fn markdown_reports_red_state_honestly() {
    let mut r = receipt();
    r.capsule = capsule(vec![check("test", Outcome::Failed { code: 1 })], vec![]);
    r.seal = Some(SealBrief {
        commit: "abc12345".into(),
        sealed_at: "2026-06-09 11:00:00".into(),
        verified: false,
        status: "a sealed check failed — the capsule is red".into(),
    });
    let md = render_markdown(&r);
    assert!(md.contains("## Verdict: ❌ Not verified"));
    assert!(md.contains("- ❌ test — failed (exit 1) — `cargo test`"));
    assert!(
        md.contains("seal: ⚠️ HEAD carries a seal that does not verify — a sealed check failed")
    );
    assert!(md.contains("working tree clean"));
}

// Empty stores degrade to calm pointers, never to errors or blank headers.
#[test]
fn markdown_renders_cold_start_gracefully() {
    let r = Receipt {
        generated: "2026-06-09 12:00".into(),
        root: "/repo".into(),
        branch: String::new(),
        head: String::new(),
        capsule: ProofCapsule {
            timestamp: "2026-06-09 12:00".into(),
            project_kind: ProjectKind::Unknown,
            files_changed: vec![],
            checks: vec![],
            reproduce: vec![],
        },
        seal: None,
        decisions: vec![],
        decisions_total: 0,
        drift: DriftBrief::default(),
        session: None,
    };
    let md = render_markdown(&r);
    assert!(md.contains("## Verdict: ⚠️ Unverified"));
    assert!(md.contains("not a git repository"));
    assert!(md.contains("no recognized verification scripts"));
    assert!(md.contains("no decisions recorded yet"));
    assert!(!md.contains("What the session did"));
}

// In a repo whose HEAD simply has no seal yet, the receipt points at the tool.
#[test]
fn markdown_points_at_seal_when_head_is_unsealed() {
    let mut r = receipt();
    r.seal = None;
    let md = render_markdown(&r);
    assert!(md.contains("HEAD is not sealed (`tomte seal`"));
}

// The HTML page carries the same sections and escapes interpolated text, so a
// hostile path or commit subject can't inject markup.
#[test]
fn html_renders_sections_and_escapes() {
    let mut r = receipt();
    r.head = "abc1234 fix <script>alert(1)</script>".into();
    let html = render_html(&r);
    assert!(html.contains("<!doctype html>"));
    assert!(html.contains("Verdict: ✅ Verified"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("What the session did"));
    assert!(html.contains("done means verified"));
}

// `collect` on a plain temp dir (no git repo, no sessions, no stores) must not
// error — every section degrades. The capsule may detect Unknown and run
// nothing; what matters is no panic and the git/session fields are empty.
#[tokio::test]
async fn collect_outside_a_repo_degrades() {
    let tmp = tempfile::tempdir().unwrap();
    let r = collect(tmp.path(), None).await;
    assert!(r.branch.is_empty());
    assert!(r.head.is_empty());
    assert!(r.seal.is_none());
    assert!(r.decisions.is_empty());
}

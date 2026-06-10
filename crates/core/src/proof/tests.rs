use super::*;

/// A `package.json` scripts list, as `read_node_scripts` would return it.
fn node(pm: &'static str, scripts: &[&str]) -> (&'static str, Vec<String>) {
    (pm, scripts.iter().map(|s| s.to_string()).collect())
}

#[test]
fn detect_kind_prefers_rust_then_node_then_go_then_python() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    assert_eq!(detect_kind(p), ProjectKind::Unknown);

    std::fs::write(p.join("requirements.txt"), "").unwrap();
    assert_eq!(detect_kind(p), ProjectKind::Python);
    std::fs::write(p.join("go.mod"), "module x").unwrap();
    assert_eq!(detect_kind(p), ProjectKind::Go);
    std::fs::write(p.join("package.json"), "{}").unwrap();
    assert_eq!(detect_kind(p), ProjectKind::Node);
    std::fs::write(p.join("Cargo.toml"), "[package]").unwrap();
    assert_eq!(detect_kind(p), ProjectKind::Rust);
}

#[test]
fn python_kind_detected_from_any_of_its_manifests() {
    for manifest in [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(manifest), "").unwrap();
        assert_eq!(detect_kind(dir.path()), ProjectKind::Python, "{manifest}");
    }
}

#[test]
fn rust_plans_all_four_cargo_checks() {
    let checks = plan_for_kind(ProjectKind::Rust, None, &|_| false);
    let names: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["test", "typecheck", "lint", "build"]);
    assert!(checks.iter().all(|c| c.present));
    assert_eq!(checks[0].command, "cargo test");
    assert_eq!(checks[1].command, "cargo check");
    assert_eq!(checks[2].command, "cargo clippy");
    assert_eq!(checks[3].command, "cargo build");
}

#[test]
fn node_present_scripts_invoke_via_the_detected_package_manager() {
    let n = node("pnpm", &["test", "build", "lint", "typecheck"]);
    let checks = plan_for_kind(ProjectKind::Node, Some(&n), &|_| false);
    let by = |name: &str| checks.iter().find(|c| c.name == name).unwrap();
    assert_eq!(by("test").command, "pnpm test"); // first-class `test` verb
    assert_eq!(by("build").command, "pnpm run build");
    assert_eq!(by("lint").command, "pnpm run lint");
    assert_eq!(by("typecheck").command, "pnpm run typecheck");
    assert!(checks.iter().all(|c| c.present));
}

#[test]
fn node_absent_scripts_are_not_verified_not_dropped() {
    // Only `test` defined → the other three are present=false (⚠️ not verified),
    // and always exactly the four categories are reported.
    let n = node("npm", &["test"]);
    let checks = plan_for_kind(ProjectKind::Node, Some(&n), &|_| false);
    assert_eq!(checks.len(), 4);
    let by = |name: &str| checks.iter().find(|c| c.name == name).unwrap();
    assert!(by("test").present);
    assert_eq!(by("test").command, "npm test");
    assert!(!by("typecheck").present);
    assert!(!by("lint").present);
    assert!(!by("build").present);
}

#[test]
fn node_typecheck_resolves_common_aliases() {
    // `type-check` (hyphen) and `tsc` are accepted as the typecheck script.
    for alias in ["typecheck", "type-check", "tsc"] {
        let n = node("yarn", &[alias]);
        let checks = plan_for_kind(ProjectKind::Node, Some(&n), &|_| false);
        let tc = checks.iter().find(|c| c.name == "typecheck").unwrap();
        assert!(tc.present, "{alias}");
        assert_eq!(tc.command, format!("yarn run {alias}"));
    }
}

#[test]
fn node_with_no_scripts_reports_all_four_unverified() {
    let n = node("npm", &[]);
    let checks = plan_for_kind(ProjectKind::Node, Some(&n), &|_| false);
    assert_eq!(checks.len(), 4);
    assert!(checks.iter().all(|c| !c.present));
}

#[test]
fn go_plans_test_vet_build() {
    let checks = plan_for_kind(ProjectKind::Go, None, &|_| false);
    let names: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["test", "lint", "build"]);
    assert_eq!(checks[0].command, "go test ./...");
    assert_eq!(checks[1].command, "go vet ./...");
    assert_eq!(checks[2].command, "go build ./...");
}

#[test]
fn python_checks_present_only_when_their_tool_is_installed() {
    // Only mypy on PATH → typecheck present, test/lint not verified.
    let only_mypy = |bin: &str| bin == "mypy";
    let checks = plan_for_kind(ProjectKind::Python, None, &only_mypy);
    let by = |name: &str| checks.iter().find(|c| c.name == name).unwrap();
    assert!(!by("test").present);
    assert!(by("typecheck").present);
    assert_eq!(by("typecheck").command, "mypy .");
    assert!(!by("lint").present);

    // All three installed → all present.
    let all = |_: &str| true;
    let checks = plan_for_kind(ProjectKind::Python, None, &all);
    assert!(checks.iter().all(|c| c.present));
}

#[test]
fn unknown_project_plans_nothing() {
    assert!(plan_for_kind(ProjectKind::Unknown, None, &|_| false).is_empty());
}

#[test]
fn reproduce_line_joins_only_present_checks() {
    let checks = vec![
        PlannedCheck::present("test", "cargo test"),
        PlannedCheck::missing("lint"),
        PlannedCheck::present("build", "cargo build"),
    ];
    assert_eq!(reproduce_line(&checks), vec!["cargo test && cargo build"]);
    assert!(reproduce_line(&[PlannedCheck::missing("test")]).is_empty());

    // A sub-project's command is wrapped so the pasted line runs it in place.
    let mut sub = PlannedCheck::present("website:lint", "npm run lint");
    sub.subdir = Some("website".into());
    assert_eq!(
        reproduce_line(&[PlannedCheck::present("test", "cargo test"), sub]),
        vec!["cargo test && (cd website && npm run lint)"]
    );
}

// Monorepo: an immediate sub-directory of a *different* ecosystem is planned
// too (prefixed, run in its dir); same-kind members, hidden dirs, and
// dependency/build dirs are left to the root toolchain.
#[test]
fn plan_checks_appends_different_kind_sub_projects() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::write(p.join("Cargo.toml"), "[package]").unwrap();
    // A Node site beside the Rust workspace → planned, prefixed.
    std::fs::create_dir(p.join("website")).unwrap();
    std::fs::write(
        p.join("website").join("package.json"),
        r#"{"scripts":{"lint":"eslint","build":"next build"}}"#,
    )
    .unwrap();
    // Same-kind sub-project → covered by the root cargo workspace, not re-run.
    std::fs::create_dir(p.join("member")).unwrap();
    std::fs::write(p.join("member").join("Cargo.toml"), "[package]").unwrap();
    // Never project roots: hidden and dependency/build output dirs.
    for never in [".hidden", "node_modules", "target"] {
        std::fs::create_dir(p.join(never)).unwrap();
        std::fs::write(p.join(never).join("package.json"), "{}").unwrap();
    }

    let (kind, checks) = plan_checks(p);
    assert_eq!(kind, ProjectKind::Rust);
    let names: Vec<_> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "test",
            "typecheck",
            "lint",
            "build",
            "website:test",
            "website:typecheck",
            "website:lint",
            "website:build",
        ]
    );
    let lint = checks.iter().find(|c| c.name == "website:lint").unwrap();
    assert!(lint.present);
    assert_eq!(lint.command, "npm run lint");
    assert_eq!(lint.subdir.as_deref(), Some("website"));
    // The website has no test script → honest "not verified", never dropped.
    let test = checks.iter().find(|c| c.name == "website:test").unwrap();
    assert!(!test.present);
    // Root checks run at the root.
    assert!(checks.iter().take(4).all(|c| c.subdir.is_none()));
}

#[test]
fn parse_porcelain_normalizes_status_and_skips_short_lines() {
    let stdout = " M src/a.rs\n?? new.txt\nA  added.rs\n\nMM both.rs\n";
    assert_eq!(
        parse_porcelain(stdout),
        vec!["M src/a.rs", "?? new.txt", "A added.rs", "MM both.rs"]
    );
    assert!(parse_porcelain("").is_empty());
}

#[test]
fn tail_of_keeps_last_n_lines() {
    assert_eq!(tail_of("a\nb\nc\nd", 2), "c\nd");
    assert_eq!(tail_of("only", 5), "only");
    assert_eq!(tail_of("a\nb\n\n", 1), "b"); // trailing blank trimmed
}

/// Build a capsule with a fixed timestamp so `render` is deterministic.
fn capsule(checks: Vec<CheckResult>, files: Vec<String>) -> ProofCapsule {
    ProofCapsule {
        timestamp: "2026-06-08 12:00:00".into(),
        project_kind: ProjectKind::Rust,
        files_changed: files,
        checks,
        reproduce: vec!["cargo test".into()],
    }
}

fn result(name: &'static str, outcome: Outcome) -> CheckResult {
    CheckResult {
        name: name.to_string(),
        command: "cargo test".into(),
        outcome,
        tail: String::new(),
    }
}

#[test]
fn verified_is_true_only_when_no_check_failed_or_errored() {
    assert!(capsule(vec![result("test", Outcome::Passed)], vec![]).verified());
    assert!(capsule(
        vec![
            result("test", Outcome::Passed),
            result("lint", Outcome::Skipped)
        ],
        vec![]
    )
    .verified());
    assert!(!capsule(vec![result("test", Outcome::Failed { code: 101 })], vec![]).verified());
    assert!(!capsule(
        vec![result(
            "test",
            Outcome::Errored {
                message: "boom".into()
            }
        )],
        vec![]
    )
    .verified());
}

#[test]
fn any_check_ran_ignores_skipped() {
    assert!(!capsule(vec![result("test", Outcome::Skipped)], vec![]).any_check_ran());
    assert!(capsule(vec![result("test", Outcome::Passed)], vec![]).any_check_ran());
    assert!(capsule(vec![result("test", Outcome::Failed { code: 1 })], vec![]).any_check_ran());
}

#[test]
fn render_shows_pass_verdict_files_and_checks() {
    let c = capsule(
        vec![
            result("test", Outcome::Passed),
            result("lint", Outcome::Skipped),
        ],
        vec!["M src/a.rs".into()],
    );
    let out = c.render();
    assert!(out.contains("✅ Verified"));
    assert!(out.contains("Files changed (1):"));
    assert!(out.contains("M src/a.rs"));
    assert!(out.contains("✅ test"));
    assert!(out.contains("⚠️ lint"));
    assert!(out.contains("not verified"));
    assert!(out.contains("Reproduce:"));
    assert!(out.contains("cargo test"));
}

#[test]
fn render_shows_fail_verdict_and_tail() {
    let mut failed = result("test", Outcome::Failed { code: 101 });
    failed.tail = "error[E0001]: boom".into();
    let out = capsule(vec![failed], vec![]).render();
    assert!(out.contains("❌ Not verified — a check failed"));
    assert!(out.contains("(exit 101)"));
    assert!(out.contains("--- test output (tail) ---"));
    assert!(out.contains("error[E0001]: boom"));
    assert!(out.contains("(working tree clean)"));
}

#[test]
fn render_unverified_when_nothing_ran() {
    let out = capsule(vec![result("test", Outcome::Skipped)], vec![]).render();
    assert!(out.contains("⚠️ Unverified — no verification checks to run"));
}

#[test]
fn empty_checks_render_a_no_scripts_note() {
    let mut c = capsule(vec![], vec![]);
    c.project_kind = ProjectKind::Unknown;
    let out = c.render();
    assert!(out.contains("no recognized verification scripts for a unknown project"));
}

// The real exec path: `run_check` actually spawns the platform shell. `exit N`
// behaves the same under `sh -c` and `cmd /C`, so these run on every OS and
// pin that a zero exit reads as Passed and a non-zero as Failed{code}.
#[tokio::test]
async fn run_check_records_a_zero_exit_as_passed() {
    let dir = tempfile::tempdir().unwrap();
    let r = run_check(&PlannedCheck::present("test", "exit 0"), dir.path()).await;
    assert_eq!(r.outcome, Outcome::Passed);
    assert!(r.tail.is_empty());
    assert_eq!(r.name, "test");
}

#[tokio::test]
async fn run_check_records_a_nonzero_exit_as_failed_with_its_code() {
    let dir = tempfile::tempdir().unwrap();
    let r = run_check(&PlannedCheck::present("lint", "exit 3"), dir.path()).await;
    assert_eq!(r.outcome, Outcome::Failed { code: 3 });
}

#[tokio::test]
async fn run_check_keeps_a_failing_command_s_output_tail() {
    let dir = tempfile::tempdir().unwrap();
    // `echo` then a non-zero exit — the captured tail must carry the message so
    // the card can explain the failure. Same syntax under sh -c and cmd /C.
    let r = run_check(
        &PlannedCheck::present("test", "echo boom && exit 1"),
        dir.path(),
    )
    .await;
    assert!(matches!(r.outcome, Outcome::Failed { code: 1 }));
    assert!(r.tail.contains("boom"), "tail was: {:?}", r.tail);
}

#[tokio::test]
async fn collect_on_an_unknown_project_runs_nothing_and_is_unverified() {
    // No manifest → no checks to run, so the collection is fast and offline. It
    // still stamps a timestamp and reports "nothing ran" rather than green.
    let dir = tempfile::tempdir().unwrap();
    let capsule = collect(dir.path()).await;
    assert_eq!(capsule.project_kind, ProjectKind::Unknown);
    assert!(capsule.checks.is_empty());
    assert!(!capsule.any_check_ran());
    assert!(!capsule.timestamp.is_empty());
}

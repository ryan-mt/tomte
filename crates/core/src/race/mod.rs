//! Agent Tournament — "don't trust one agent; make several compete."
//!
//! A coding task has many solutions. Handing it to a single model is a gamble on
//! one path; a small, clean, test-passing patch usually beats a clever one that
//! rewrites twelve files. `tomte race "<task>" --agents N` runs the task with N
//! contestants (varying model / effort / style), each in its **own git
//! worktree** so they can never touch each other's tree, then judges them on
//! *evidence* — not the models' word:
//!
//! - the project's own tests / typecheck / lint / build (the [`crate::proof`]
//!   capsule, run by the CLI in each worktree),
//! - diff size and files touched (smaller is safer),
//! - whether a regression test was added (coverage),
//! - and how many risky shell commands the contestant ran (the same
//!   [`crate::tools::shell::classify_danger`] guard the live agent uses).
//!
//! The judge is **deterministic and decides the winner** ([`score`]); an LLM is
//! never the referee — at most it could later *explain* a verdict the CLI already
//! reached. The explanation here is generated from the metrics, so the result is
//! reproducible and can't be talked into a different answer.
//!
//! Isolation and cleanup are load-bearing: worktrees are created from `HEAD`,
//! every contestant runs sandboxed (`workspace-write`, no network for its shell),
//! and the worktrees are always torn down — even when a contestant errors.

pub mod judge;
pub mod score;
pub mod strategy;

use std::path::{Path, PathBuf};

use serde::Serialize;

pub use strategy::{build_strategies, Strategy, Style};

/// How long a single contestant may run before it's killed. A real task can take
/// a while; this only stops a wedged agent from hanging the whole race.
const AGENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

/// Options for a race, from the CLI flags.
#[derive(Debug, Clone)]
pub struct RaceOptions {
    pub agents: usize,
    pub models: Vec<String>,
    /// Apply the winning patch to the working tree when the race finishes.
    pub apply: bool,
}

/// Progress events emitted while a race runs, so the CLI can narrate a
/// multi-minute tournament instead of going silent until the final report.
#[derive(Debug, Clone)]
pub enum RaceEvent {
    /// Worktrees are ready; the field is about to run.
    Starting { contestants: usize },
    /// A contestant's worktree could not be created — it can never win.
    WorktreeFailed { label: String, error: String },
    /// A contestant's agent process began.
    AgentStarted { label: String, model: String },
    /// A contestant's agent process ended; `error` carries its run failure.
    AgentFinished {
        label: String,
        secs: u64,
        error: Option<String>,
    },
    /// A contestant's worktree is being verified (the project's own checks).
    Verifying { label: String },
    /// Verification finished for a contestant.
    Verified {
        label: String,
        passed: usize,
        failed: usize,
    },
}

/// The evidence gathered for one contestant — every field is measured by the
/// CLI, never supplied by the model.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    pub files_changed: usize,
    pub insertions: u64,
    pub deletions: u64,
    pub added_test: bool,
    pub risky_commands: u32,
    pub checks_total: usize,
    pub checks_passed: usize,
    pub checks_failed: usize,
    /// At least one defined check actually ran (vs. a project with no checks).
    pub any_check_ran: bool,
    /// Every defined check passed (no fail/error). The Proof Capsule verdict.
    pub verified: bool,
}

/// One contestant's raw result, before ranking. `diff` is the full unified patch
/// (kept out of the serialized report — it can be large).
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub label: String,
    pub model: String,
    pub diff: String,
    pub metrics: Metrics,
    /// Set when the contestant couldn't be run or judged; such an entry can never
    /// win.
    pub run_error: Option<String>,
}

impl AgentOutcome {
    /// Whether the contestant actually changed anything.
    pub fn has_changes(&self) -> bool {
        self.metrics.files_changed > 0
    }
}

/// A contestant after the deterministic judge has scored it.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub label: String,
    pub model: String,
    pub metrics: Metrics,
    pub score: i64,
    /// 0 = passed verification, 1 = changed but a check failed, 2 = no change /
    /// errored. Ranking is by tier first, then score.
    pub tier: u8,
    /// One-line reasons (deterministic, from the metrics) for the card.
    pub reasons: Vec<String>,
}

/// The full race result.
#[derive(Debug, Clone, Serialize)]
pub struct RaceReport {
    pub task: String,
    /// Contestants, best first.
    pub verdicts: Vec<Verdict>,
    /// The winning label, or `None` when no contestant produced a usable change.
    pub winner: Option<String>,
    /// Where the winning patch was written (so it can be applied by hand).
    pub patch_path: Option<String>,
    /// Whether the winning patch was applied to the working tree.
    pub applied: bool,
    pub notes: Vec<String>,
    /// The winner's full patch — consumed by the orchestrator to save/apply it;
    /// kept out of the serialized report because it can be large.
    #[serde(skip)]
    pub winning_diff: Option<String>,
}

// ---- orchestration ----------------------------------------------------------

/// Run the tournament: build the line-up, run each contestant in its own
/// worktree, judge by evidence, rank, and (optionally) apply the winner. Worktrees
/// are always cleaned up. Progress is narrated through `on_event`.
pub async fn run_race(
    cwd: &Path,
    task: &str,
    opts: &RaceOptions,
    // Shared by the concurrent contestants, hence `Send + Sync`.
    on_event: &(dyn Fn(RaceEvent) + Send + Sync),
) -> anyhow::Result<RaceReport> {
    use anyhow::Context as _;

    if !crate::doctor::binary_on_path("git") {
        anyhow::bail!("`tomte race` needs git on PATH to create isolated worktrees");
    }
    let root = crate::memory::git_root_from(cwd)
        .context("`tomte race` must run inside a git repository (worktrees branch from HEAD)")?;

    let exe =
        std::env::current_exe().context("could not locate the tomte binary to spawn agents")?;

    let strategies = build_strategies(opts.agents, &opts.models);
    let mut notes = Vec::new();
    if working_tree_dirty(&root).await {
        notes.push(
            "the working tree has uncommitted changes — contestants race from HEAD, so those changes are not included".to_string(),
        );
    }

    // A unique base directory for this race's worktrees, removed at the end.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!("tomte-race-{}-{stamp}", std::process::id()));
    std::fs::create_dir_all(&base).with_context(|| format!("create {}", base.display()))?;

    // Create one worktree per contestant. A creation failure becomes a run_error
    // for that contestant rather than sinking the whole race.
    let mut planned: Vec<(Strategy, Option<PathBuf>, Option<String>)> = Vec::new();
    for s in strategies {
        let wt = base.join(&s.label);
        match add_worktree(&root, &wt).await {
            Ok(()) => planned.push((s, Some(wt), None)),
            Err(e) => {
                let err = format!("could not create worktree: {e:#}");
                on_event(RaceEvent::WorktreeFailed {
                    label: s.label.clone(),
                    error: err.clone(),
                });
                planned.push((s, None, Some(err)));
            }
        }
    }
    on_event(RaceEvent::Starting {
        contestants: planned.iter().filter(|(_, wt, _)| wt.is_some()).count(),
    });

    // Run every contestant that has a worktree, concurrently.
    let futures = planned.into_iter().map(|(s, wt, err)| {
        let exe = exe.clone();
        let task = task.to_string();
        async move {
            match (wt, err) {
                (Some(wt), _) => run_one(&exe, &wt, &s, &task, on_event).await,
                (None, err) => AgentOutcome {
                    label: s.label,
                    model: s.model.unwrap_or_else(|| "default".into()),
                    diff: String::new(),
                    metrics: Metrics::default(),
                    run_error: err,
                },
            }
        }
    });
    let outcomes = futures::future::join_all(futures).await;

    // Tear every worktree down before building the report — even on error above.
    cleanup(&root, &base).await;

    let mut report = score::rank(task.to_string(), outcomes);
    report.notes.extend(notes);

    // Persist the winning patch and, if asked, apply it.
    if let Some(winner) = &report.winner {
        if let Some(diff) = report.winning_diff.take() {
            match save_patch(&root, winner, &diff) {
                Ok(path) => {
                    report.patch_path = Some(path.to_string_lossy().to_string());
                    if opts.apply {
                        match apply_patch(&root, &path).await {
                            Ok(()) => report.applied = true,
                            Err(e) => report
                                .notes
                                .push(format!("could not apply the winning patch: {e:#}")),
                        }
                    }
                }
                Err(e) => report
                    .notes
                    .push(format!("could not save the winning patch: {e:#}")),
            }
        }
    }

    Ok(report)
}

/// Run one contestant in its worktree and gather its evidence.
async fn run_one(
    exe: &Path,
    wt: &Path,
    strategy: &Strategy,
    task: &str,
    on_event: &(dyn Fn(RaceEvent) + Send + Sync),
) -> AgentOutcome {
    let model = strategy.model.clone().unwrap_or_else(|| "default".into());
    let prompt = format!("{task}{}", strategy.prompt_suffix());

    on_event(RaceEvent::AgentStarted {
        label: strategy.label.clone(),
        model: model.clone(),
    });
    let started = std::time::Instant::now();
    let events = match spawn_agent(exe, wt, strategy, &prompt).await {
        Ok(stdout) => stdout,
        Err(e) => {
            let err = format!("agent run failed: {e:#}");
            on_event(RaceEvent::AgentFinished {
                label: strategy.label.clone(),
                secs: started.elapsed().as_secs(),
                error: Some(err.clone()),
            });
            return AgentOutcome {
                label: strategy.label.clone(),
                model,
                diff: String::new(),
                metrics: Metrics::default(),
                run_error: Some(err),
            };
        }
    };
    on_event(RaceEvent::AgentFinished {
        label: strategy.label.clone(),
        secs: started.elapsed().as_secs(),
        error: None,
    });

    let (diff, numstat) = capture_diff(wt).await;
    let (files_changed, insertions, deletions) = judge::parse_numstat(&numstat);
    let changed = judge::changed_paths(&numstat);
    let added_test = judge::detect_added_test(&diff, &changed);
    let risky_commands = judge::count_risky_commands(&events);

    // The deterministic verification: run the project's own checks in the worktree.
    on_event(RaceEvent::Verifying {
        label: strategy.label.clone(),
    });
    let capsule = crate::proof::collect(wt).await;
    let checks_total = capsule.checks.len();
    let checks_passed = capsule
        .checks
        .iter()
        .filter(|c| matches!(c.outcome, crate::proof::Outcome::Passed))
        .count();
    let checks_failed = capsule
        .checks
        .iter()
        .filter(|c| {
            matches!(
                c.outcome,
                crate::proof::Outcome::Failed { .. } | crate::proof::Outcome::Errored { .. }
            )
        })
        .count();
    on_event(RaceEvent::Verified {
        label: strategy.label.clone(),
        passed: checks_passed,
        failed: checks_failed,
    });

    AgentOutcome {
        label: strategy.label.clone(),
        model,
        diff,
        metrics: Metrics {
            files_changed,
            insertions,
            deletions,
            added_test,
            risky_commands,
            checks_total,
            checks_passed,
            checks_failed,
            any_check_ran: capsule.any_check_ran(),
            verified: capsule.verified(),
        },
        run_error: None,
    }
}

/// Spawn `tomte run` in the worktree, capturing its JSON event stream (stdout).
async fn spawn_agent(
    exe: &Path,
    wt: &Path,
    strategy: &Strategy,
    prompt: &str,
) -> anyhow::Result<String> {
    use tokio::process::Command;
    let mut cmd = Command::new(exe);
    cmd.arg("run")
        .arg("--cwd")
        .arg(wt)
        .arg("--output-format")
        .arg("json")
        .arg("--dangerously-skip-permissions")
        .arg("--sandbox")
        .arg("workspace-write");
    if let Some(m) = &strategy.model {
        cmd.arg("--model").arg(m);
    }
    if let Some(r) = &strategy.reasoning {
        cmd.arg("--reasoning").arg(r);
    }
    cmd.arg(prompt);
    cmd.current_dir(wt)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    let out = tokio::time::timeout(AGENT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("timed out after {}s", AGENT_TIMEOUT.as_secs()))??;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Stage everything in the worktree and capture `(unified diff, numstat)` vs
/// HEAD — staging first so new untracked files are included in both.
async fn capture_diff(wt: &Path) -> (String, String) {
    let _ = git(wt, &["add", "-A"]).await;
    let diff = git(wt, &["diff", "--cached"]).await.unwrap_or_default();
    let numstat = git(wt, &["diff", "--cached", "--numstat"])
        .await
        .unwrap_or_default();
    (diff, numstat)
}

async fn add_worktree(root: &Path, wt: &Path) -> anyhow::Result<()> {
    let wt_str = wt.to_string_lossy().to_string();
    let out = git_output(root, &["worktree", "add", "--detach", &wt_str, "HEAD"]).await?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Remove every worktree under `base`, then the base directory. Best-effort — a
/// leftover directory is pruned with `git worktree prune` as a backstop.
async fn cleanup(root: &Path, base: &Path) {
    if let Ok(entries) = std::fs::read_dir(base) {
        for entry in entries.flatten() {
            let p = entry.path();
            let _ = git(
                root,
                &["worktree", "remove", "--force", &p.to_string_lossy()],
            )
            .await;
        }
    }
    let _ = std::fs::remove_dir_all(base);
    let _ = git(root, &["worktree", "prune"]).await;
}

/// Write the winning patch beside the project's other tomte state, so it can be
/// applied by hand even without `--apply`.
fn save_patch(root: &Path, label: &str, diff: &str) -> anyhow::Result<PathBuf> {
    use anyhow::Context as _;
    let dir = crate::tools::memory::store_dir(root)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(crate::config::config_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("race-winner-{label}.patch"));
    std::fs::write(&path, diff).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

async fn apply_patch(root: &Path, patch: &Path) -> anyhow::Result<()> {
    let out = git_output(root, &["apply", &patch.to_string_lossy()]).await?;
    if !out.status.success() {
        anyhow::bail!(
            "{} (apply it by hand: git apply {})",
            String::from_utf8_lossy(&out.stderr).trim(),
            patch.display()
        );
    }
    Ok(())
}

async fn working_tree_dirty(root: &Path) -> bool {
    git(root, &["status", "--porcelain"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Run a git command in `dir`, returning trimmed stdout on success, else `None`.
async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_output(dir, args).await.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn git_output(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    use tokio::process::Command;
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    crate::secret_env::scrub_secret_env(&mut cmd);
    cmd.output().await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The progress narration around one contestant: `AgentStarted` is always
    /// followed by an erroring `AgentFinished` when the agent binary can't
    /// spawn — and the outcome carries the same failure. Hermetic (no LLM, no
    /// git): the exe path simply doesn't exist.
    #[tokio::test]
    async fn run_one_emits_started_then_failed_on_spawn_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let events: std::sync::Mutex<Vec<String>> = Default::default();
        let on_event = |ev: RaceEvent| {
            let tag = match ev {
                RaceEvent::AgentStarted { .. } => "started",
                RaceEvent::AgentFinished { error: Some(_), .. } => "failed",
                RaceEvent::AgentFinished { error: None, .. } => "finished",
                _ => "other",
            };
            events.lock().unwrap().push(tag.to_string());
        };
        let strategy = build_strategies(1, &[]).remove(0);
        let missing = tmp.path().join("definitely-not-a-binary");
        let out = run_one(&missing, tmp.path(), &strategy, "task", &on_event).await;
        assert!(out.run_error.is_some(), "spawn failure must be carried");
        assert_eq!(events.into_inner().unwrap(), vec!["started", "failed"]);
    }

    /// Exercise the git plumbing the orchestrator depends on — worktree create,
    /// diff capture (including a new untracked file), patch save, apply, and
    /// teardown — end to end against a real temp repo, WITHOUT spawning an LLM
    /// agent (the one part that can't run in CI). This is the riskiest non-LLM
    /// path; the LLM-driven `run_race` is verified by hand.
    #[tokio::test]
    async fn worktree_lifecycle_capture_apply_and_cleanup() {
        if !crate::doctor::binary_on_path("git") {
            return; // no git on this box — nothing to verify
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // A repo with one commit, so HEAD is valid for `worktree add`.
        assert!(git_output(&root, &["init"]).await.unwrap().status.success());
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        git_output(&root, &["add", "-A"]).await.unwrap();
        let commit = git_output(
            &root,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "commit",
                "-m",
                "init",
            ],
        )
        .await
        .unwrap();
        assert!(commit.status.success(), "commit failed");

        // The main tree is clean before any worktree runs.
        assert!(!working_tree_dirty(&root).await);

        // Create a worktree and make a change in it: a new file + an edit.
        let base = tmp.path().join("race-base");
        std::fs::create_dir_all(&base).unwrap();
        let wt = base.join("agent-a");
        add_worktree(&root, &wt).await.expect("worktree add");
        assert!(wt.join("file.txt").exists(), "worktree checked out HEAD");
        std::fs::write(wt.join("new.rs"), "pub fn added() {}\n").unwrap();
        std::fs::write(wt.join("file.txt"), "hello\nworld\n").unwrap();

        // Capture the diff — it must include the untracked new file.
        let (diff, numstat) = capture_diff(&wt).await;
        assert!(
            diff.contains("new.rs"),
            "diff must include the new file: {diff}"
        );
        let (files, ins, _del) = judge::parse_numstat(&numstat);
        assert_eq!(files, 2, "two files changed");
        assert!(ins >= 2, "insertions counted");

        // Save the patch, then apply it onto the (clean) main tree.
        let patch = save_patch(&root, "agent-a", &diff).unwrap();
        assert!(patch.exists());
        apply_patch(&root, &patch)
            .await
            .expect("apply winning patch");
        assert!(
            root.join("new.rs").exists(),
            "the winning patch created the new file in the main tree"
        );
        // Substring, not exact bytes: git autocrlf can rewrite line endings on
        // Windows, so assert the edit landed without pinning the EOL style.
        assert!(
            std::fs::read_to_string(root.join("file.txt"))
                .unwrap()
                .contains("world"),
            "the winning patch applied the edit"
        );

        // Teardown removes the worktree.
        cleanup(&root, &base).await;
        assert!(!wt.exists(), "worktree removed by cleanup");

        let _ = std::fs::remove_file(patch);
    }
}

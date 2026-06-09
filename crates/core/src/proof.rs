//! Proof Capsule — "done means verified."
//!
//! A task isn't done because the model *says* it is; it's done when the tooling
//! that can actually fail has run and passed. This module builds a deterministic
//! evidence bundle the CLI gathers itself — the files git reports changed, and
//! the exit codes of the project's own verification scripts (test / typecheck /
//! lint / build) that the CLI *actually runs*. The model never supplies these
//! numbers; it can only explain a capsule the CLI already collected. That's the
//! whole point: a capsule can't be hallucinated into existence.
//!
//! Detection (`plan_for_kind`) is pure — given the project kind and, for Node,
//! the parsed `package.json` scripts, it returns the checks to run — so the
//! per-ecosystem matrix is unit-tested without touching disk. [`collect`] then
//! runs git + each planned check and records what it observed; [`ProofCapsule::render`]
//! turns that into the ✅/❌/⚠️ card. The same capsule backs the `/prove` slash
//! command and the headless `tomte prove` subcommand, so the verdict logic lives
//! in one place.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

use crate::doctor::binary_on_path;

/// Per-check hard timeout. A build or test suite can legitimately run for a
/// while; this only stops a runaway/hung command from wedging the capsule.
const CHECK_TIMEOUT: Duration = Duration::from_secs(600);

/// How many lines of a failing check's output to keep for the card. Enough to
/// see the first error; not so much it floods the transcript.
const TAIL_LINES: usize = 20;

/// The recognized project ecosystems. The capsule verifies one primary
/// ecosystem (chosen by [`detect_kind`] priority) — a polyglot repo verifies its
/// primary toolchain, which is the one whose checks the user most likely means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectKind {
    Rust,
    Node,
    Go,
    Python,
    Unknown,
}

impl ProjectKind {
    pub fn label(self) -> &'static str {
        match self {
            ProjectKind::Rust => "rust",
            ProjectKind::Node => "node",
            ProjectKind::Go => "go",
            ProjectKind::Python => "python",
            ProjectKind::Unknown => "unknown",
        }
    }
}

/// A check the CLI intends to run: which category, and the exact command line.
/// `present` is false when the project *could* define this check but doesn't
/// (e.g. a Node project with no `typecheck` script) — those surface as a
/// deterministic "not verified", never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCheck {
    pub name: &'static str,
    pub command: String,
    pub present: bool,
}

impl PlannedCheck {
    fn present(name: &'static str, command: impl Into<String>) -> Self {
        Self {
            name,
            command: command.into(),
            present: true,
        }
    }
    fn missing(name: &'static str) -> Self {
        Self {
            name,
            command: String::new(),
            present: false,
        }
    }
}

/// What the CLI observed when it ran (or couldn't run) a planned check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    /// The command ran and exited 0.
    Passed,
    /// The command ran and exited non-zero.
    Failed { code: i32 },
    /// The project doesn't define this check — nothing to run.
    Skipped,
    /// The command couldn't be launched at all (binary missing, spawn error).
    Errored { message: String },
}

/// One check, after the CLI ran it.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub command: String,
    #[serde(flatten)]
    pub outcome: Outcome,
    /// Last lines of output, kept only for a failure so the card can show why.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tail: String,
}

/// The collected evidence bundle.
#[derive(Debug, Clone, Serialize)]
pub struct ProofCapsule {
    pub timestamp: String,
    pub project_kind: ProjectKind,
    /// Porcelain-style `<code> <path>` lines from `git status` (e.g. `M src/x`).
    pub files_changed: Vec<String>,
    pub checks: Vec<CheckResult>,
    /// Copy-pasteable command(s) to re-run the verification by hand.
    pub reproduce: Vec<String>,
}

impl ProofCapsule {
    /// True only when every check the project *defines* passed. A skipped check
    /// (the project has no such script) doesn't fail the verdict, but it is
    /// surfaced as "not verified" so an absent test suite can't masquerade as a
    /// green one.
    pub fn verified(&self) -> bool {
        self.checks
            .iter()
            .all(|c| !matches!(c.outcome, Outcome::Failed { .. } | Outcome::Errored { .. }))
    }

    /// True when at least one check actually ran and passed — so "verified" means
    /// "something was checked", not just "nothing failed because nothing ran".
    pub fn any_check_ran(&self) -> bool {
        self.checks
            .iter()
            .any(|c| matches!(c.outcome, Outcome::Passed | Outcome::Failed { .. }))
    }

    /// The ✅/❌/⚠️ card shown in the TUI and printed by `tomte prove`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let verdict = if !self.any_check_ran() {
            "⚠️ Unverified — no verification checks to run"
        } else if self.verified() {
            "✅ Verified"
        } else {
            "❌ Not verified — a check failed"
        };
        out.push_str(&format!("Proof Capsule  ·  {}\n", self.timestamp));
        out.push_str(&format!("{verdict}\n"));

        out.push_str(&format!(
            "\nFiles changed ({}):\n",
            self.files_changed.len()
        ));
        if self.files_changed.is_empty() {
            out.push_str("  (working tree clean)\n");
        } else {
            for line in self.files_changed.iter().take(30) {
                out.push_str(&format!("  {line}\n"));
            }
            if self.files_changed.len() > 30 {
                out.push_str(&format!("  … +{} more\n", self.files_changed.len() - 30));
            }
        }

        out.push_str("\nChecks:\n");
        if self.checks.is_empty() {
            out.push_str(&format!(
                "  (no recognized verification scripts for a {} project)\n",
                self.project_kind.label()
            ));
        } else {
            for c in &self.checks {
                let (glyph, status, detail) = match &c.outcome {
                    Outcome::Passed => ("✅", "passed", c.command.clone()),
                    Outcome::Failed { code } => {
                        ("❌", "failed", format!("{} (exit {code})", c.command))
                    }
                    Outcome::Skipped => ("⚠️", "not verified", "no script".to_string()),
                    Outcome::Errored { message } => {
                        ("❌", "error", format!("{} ({message})", c.command))
                    }
                };
                out.push_str(&format!("  {glyph} {:<10} {status:<13} {detail}\n", c.name));
            }
        }

        // Failure tails, so the card explains why something is red.
        for c in &self.checks {
            if !c.tail.is_empty() {
                out.push_str(&format!("\n--- {} output (tail) ---\n", c.name));
                out.push_str(&c.tail);
                if !c.tail.ends_with('\n') {
                    out.push('\n');
                }
            }
        }

        if !self.reproduce.is_empty() {
            out.push_str("\nReproduce:\n");
            for r in &self.reproduce {
                out.push_str(&format!("  {r}\n"));
            }
        }
        out
    }
}

/// Pick the project's primary ecosystem by priority. A repo can carry more than
/// one manifest (a Tauri app is Rust + Node); the capsule verifies the primary
/// toolchain rather than running every ecosystem's build.
pub fn detect_kind(cwd: &Path) -> ProjectKind {
    if cwd.join("Cargo.toml").is_file() {
        ProjectKind::Rust
    } else if cwd.join("package.json").is_file() {
        ProjectKind::Node
    } else if cwd.join("go.mod").is_file() {
        ProjectKind::Go
    } else if cwd.join("pyproject.toml").is_file()
        || cwd.join("setup.py").is_file()
        || cwd.join("setup.cfg").is_file()
        || cwd.join("requirements.txt").is_file()
    {
        ProjectKind::Python
    } else {
        ProjectKind::Unknown
    }
}

/// First Node package manager whose lockfile is present, else `npm`. Decides how
/// a `package.json` script is invoked (`pnpm run build` vs `npm run build`).
fn detect_node_pm(cwd: &Path) -> &'static str {
    if cwd.join("bun.lockb").is_file() {
        "bun"
    } else if cwd.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if cwd.join("yarn.lock").is_file() {
        "yarn"
    } else {
        "npm"
    }
}

/// Build the planned checks for `cwd`. Pure given the inputs it reads: it touches
/// disk only to learn the project kind, the Node package manager + scripts, and
/// (for Python) which tools are installed. The per-kind matrix itself is the
/// testable [`plan_for_kind`].
pub fn plan_checks(cwd: &Path) -> (ProjectKind, Vec<PlannedCheck>) {
    let kind = detect_kind(cwd);
    let node = if kind == ProjectKind::Node {
        let scripts = read_node_scripts(cwd);
        Some((detect_node_pm(cwd), scripts))
    } else {
        None
    };
    let py_tool = |bin: &str| binary_on_path(bin);
    (kind, plan_for_kind(kind, node.as_ref(), &py_tool))
}

/// Read the `scripts` object of `package.json` as name→command. Missing file or
/// malformed JSON yields an empty map (every Node check then reports "no script").
fn read_node_scripts(cwd: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(cwd.join("package.json")) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    json.get("scripts")
        .and_then(|s| s.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// The pure per-ecosystem check matrix. `node` carries `(package_manager,
/// script_names)`; `py_tool` reports whether a Python tool is on PATH. Kept free
/// of disk access so every branch is unit-tested directly.
pub fn plan_for_kind(
    kind: ProjectKind,
    node: Option<&(&'static str, Vec<String>)>,
    py_tool: &dyn Fn(&str) -> bool,
) -> Vec<PlannedCheck> {
    match kind {
        ProjectKind::Rust => vec![
            PlannedCheck::present("test", "cargo test"),
            PlannedCheck::present("typecheck", "cargo check"),
            PlannedCheck::present("lint", "cargo clippy"),
            PlannedCheck::present("build", "cargo build"),
        ],
        ProjectKind::Node => {
            let empty: Vec<String> = Vec::new();
            let (pm, scripts) = match node {
                Some((pm, s)) => (*pm, s),
                None => ("npm", &empty),
            };
            let has = |name: &str| scripts.iter().any(|s| s == name);
            // `test` runs via the manager's first-class `test` verb; the rest go
            // through `run <script>`. A category maps to the first script name it
            // finds, so common aliases (`type-check`, `tsc`) still resolve.
            let mut checks = Vec::new();
            checks.push(if has("test") {
                PlannedCheck::present("test", format!("{pm} test"))
            } else {
                PlannedCheck::missing("test")
            });
            checks.push(plan_node_script(
                "typecheck",
                pm,
                &["typecheck", "type-check", "tsc"],
                &has,
            ));
            checks.push(plan_node_script("lint", pm, &["lint"], &has));
            checks.push(plan_node_script("build", pm, &["build"], &has));
            checks
        }
        ProjectKind::Go => vec![
            PlannedCheck::present("test", "go test ./..."),
            PlannedCheck::present("lint", "go vet ./..."),
            PlannedCheck::present("build", "go build ./..."),
        ],
        ProjectKind::Python => {
            // Python verification depends on which tools are actually installed —
            // running a missing `pytest` would just error noisily, so an absent
            // tool is an honest "not verified" instead.
            let tool = |name: &'static str, cmd: &str, bin: &str| {
                if py_tool(bin) {
                    PlannedCheck::present(name, cmd)
                } else {
                    PlannedCheck::missing(name)
                }
            };
            vec![
                tool("test", "pytest", "pytest"),
                tool("typecheck", "mypy .", "mypy"),
                tool("lint", "ruff check .", "ruff"),
            ]
        }
        ProjectKind::Unknown => Vec::new(),
    }
}

/// Map a Node category to the first matching script name, invoked via `run`.
fn plan_node_script(
    name: &'static str,
    pm: &str,
    candidates: &[&str],
    has: &dyn Fn(&str) -> bool,
) -> PlannedCheck {
    match candidates.iter().find(|c| has(c)) {
        Some(script) => PlannedCheck::present(name, format!("{pm} run {script}")),
        None => PlannedCheck::missing(name),
    }
}

/// The reproduce line: the present checks' commands joined with `&&`, so the user
/// can paste one line to re-run exactly what the capsule ran.
fn reproduce_line(checks: &[PlannedCheck]) -> Vec<String> {
    let cmds: Vec<&str> = checks
        .iter()
        .filter(|c| c.present)
        .map(|c| c.command.as_str())
        .collect();
    if cmds.is_empty() {
        Vec::new()
    } else {
        vec![cmds.join(" && ")]
    }
}

/// Gather the proof capsule for `cwd`: the git-reported file changes plus the
/// observed result of every planned check the CLI runs itself.
pub async fn collect(cwd: &Path) -> ProofCapsule {
    let (kind, planned) = plan_checks(cwd);
    let files_changed = git_changed_files(cwd).await;
    let reproduce = reproduce_line(&planned);

    let mut checks = Vec::with_capacity(planned.len());
    for p in &planned {
        if !p.present {
            checks.push(CheckResult {
                name: p.name,
                command: String::new(),
                outcome: Outcome::Skipped,
                tail: String::new(),
            });
            continue;
        }
        checks.push(run_check(p, cwd).await);
    }

    ProofCapsule {
        timestamp: now_local(),
        project_kind: kind,
        files_changed,
        checks,
        reproduce,
    }
}

fn now_local() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `git status --porcelain` → `<code> <path>` lines (e.g. `M src/x`, `?? new`).
/// Empty (clean tree) or no-git both yield an empty list; the capsule then shows
/// "working tree clean".
async fn git_changed_files(cwd: &Path) -> Vec<String> {
    let mut git = Command::new("git");
    git.args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(cwd);
    crate::secret_env::scrub_secret_env(&mut git);
    let Ok(out) = git.output().await else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_porcelain(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `git status --porcelain` stdout into `<code> <path>` lines. Porcelain
/// v1 is a 2-char status, a space, then the path; the status is normalized to a
/// single trimmed token so ` M` and `M ` both read `M`. Pure, so it's tested
/// without a live repo.
fn parse_porcelain(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            let code = line[..2].trim();
            let path = line[3..].trim();
            Some(format!("{code} {path}"))
        })
        .collect()
}

/// Run one planned check through the platform shell and record the outcome. The
/// command is the project's own script (not model-supplied), so it runs in a
/// plain shell like the user would invoke it — only secret-looking env vars are
/// scrubbed.
async fn run_check(check: &PlannedCheck, cwd: &Path) -> CheckResult {
    let mut cmd = platform_shell(&check.command);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::secret_env::scrub_secret_env(&mut cmd);

    let result = tokio::time::timeout(CHECK_TIMEOUT, cmd.output()).await;
    let (outcome, tail) = match result {
        Err(_) => (
            Outcome::Errored {
                message: format!("timed out after {}s", CHECK_TIMEOUT.as_secs()),
            },
            String::new(),
        ),
        Ok(Err(e)) => (
            Outcome::Errored {
                message: e.to_string(),
            },
            String::new(),
        ),
        Ok(Ok(out)) => {
            if out.status.success() {
                (Outcome::Passed, String::new())
            } else {
                let code = out.status.code().unwrap_or(-1);
                let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
                combined.push_str(&String::from_utf8_lossy(&out.stderr));
                (Outcome::Failed { code }, tail_of(&combined, TAIL_LINES))
            }
        }
    };
    CheckResult {
        name: check.name,
        command: check.command.clone(),
        outcome,
        tail,
    }
}

/// Last `n` non-trailing-empty lines of `text`, for a failing check's card.
fn tail_of(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// A `tokio::process::Command` that runs `command` through the platform shell
/// (`cmd /C` on Windows, `sh -c` elsewhere) — the same convention `run_shell`
/// uses, kept local so this module doesn't reach into the shell tool's internals.
fn platform_shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

#[cfg(test)]
mod tests;

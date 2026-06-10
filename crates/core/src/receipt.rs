//! The work receipt — one artifact that *proves* a stretch of work instead of
//! transcribing it.
//!
//! `tomte receipt` (headless) bundles, into a single Markdown / HTML / JSON
//! document you can attach to a PR:
//!
//! - **the verdict** — a fresh Proof Capsule: the files git reports changed and
//!   the REAL exit codes of the project's own test/typecheck/lint/build, run by
//!   the CLI right now;
//! - **the seal** — whether HEAD carries a verified Commit Seal
//!   (`refs/notes/tomte-seal`), checked with the same binding rules as
//!   `tomte seal verify`;
//! - **what the session actually did** — the shell commands run and the files
//!   edited, read from the persisted session log (the CLI's own record of the
//!   tool calls that executed, not a model's recollection), plus the per-model
//!   token/cost receipt;
//! - **why** — the newest recorded decisions with the drift-watch counts.
//!
//! Every line is collected by the CLI from real state. The difference from
//! sharing a transcript: a transcript shows what was *said*; the receipt shows
//! what was *verified*. It never gates (always renders, red or green) — the
//! gates are `tomte prove` and `tomte seal verify`.

use std::path::Path;

use serde::Serialize;

use crate::handoff::{DecisionBrief, DriftBrief};
use crate::proof::{Outcome, ProofCapsule};
use crate::session::ModelUsage;

/// Caps, in the same spirit as the handoff: a receipt is a briefing with
/// evidence attached, not an archive. The full stores stay one command away.
const MAX_DECISIONS: usize = 5;
const MAX_COMMANDS: usize = 20;
const MAX_FILES_EDITED: usize = 20;

/// The seal standing on HEAD, summarized with the verify verdict.
#[derive(Debug, Clone, Serialize)]
pub struct SealBrief {
    /// Short commit id the seal is bound to.
    pub commit: String,
    pub sealed_at: String,
    /// True when `tomte seal verify HEAD` would gate green.
    pub verified: bool,
    /// "verified" or the exact reason verify would refuse.
    pub status: String,
}

/// One model's token usage priced at published API rates.
#[derive(Debug, Clone, Serialize)]
pub struct CostLine {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// USD at API rates (an estimate for subscription auth).
    pub cost_usd: f64,
}

/// What one persisted session actually did — extracted from the CLI's own
/// record of executed tool calls, never from model prose.
#[derive(Debug, Clone, Serialize)]
pub struct SessionBrief {
    pub id: String,
    pub model: String,
    /// User messages in the history — one user message ≈ one turn.
    pub turns: u64,
    /// Shell command lines the session ran, oldest first, capped.
    pub commands: Vec<String>,
    pub commands_total: usize,
    /// Files touched by edit tools (write/edit/multi-edit/notebook), deduped
    /// in first-touch order, capped.
    pub files_edited: Vec<String>,
    pub files_edited_total: usize,
    pub cost: Vec<CostLine>,
    pub total_cost_usd: f64,
}

/// The whole receipt. Serializes for `--json`; renders for humans (markdown)
/// and for sharing (a standalone HTML page).
#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    /// Local wall-clock time the receipt was collected.
    pub generated: String,
    pub root: String,
    /// Current branch, empty outside a git repo.
    pub branch: String,
    /// `<short-hash> <subject>` of HEAD, empty outside a git repo.
    pub head: String,
    /// Fresh evidence: files changed + real check exit codes, collected now.
    pub capsule: ProofCapsule,
    /// The seal on HEAD; `None` when HEAD carries no seal (or not a repo).
    pub seal: Option<SealBrief>,
    /// Newest decisions first, capped at [`MAX_DECISIONS`].
    pub decisions: Vec<DecisionBrief>,
    pub decisions_total: usize,
    pub drift: DriftBrief,
    /// The newest (or `--session`-chosen) persisted session; `None` when the
    /// project has no saved sessions or the id doesn't load.
    pub session: Option<SessionBrief>,
}

/// Run a git command in `root` and return trimmed stdout, `None` on any
/// failure (outside a repo, no git on PATH) — the receipt degrades to the
/// sections that do exist instead of erroring. Same shape as the handoff's.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(root);
    crate::secret_env::scrub_secret_env_std(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Extract the session activity (commands run, files edited) from a persisted
/// history. Pure, so the extraction rules are unit-testable: a `run_shell`
/// call contributes its `command` line; the edit tools contribute their target
/// path (the spellings the tools actually accept).
pub fn session_activity(history: &[crate::openai::InputItem]) -> (Vec<String>, Vec<String>) {
    let mut commands = Vec::new();
    let mut files = Vec::new();
    for item in history {
        let crate::openai::InputItem::FunctionCall {
            name, arguments, ..
        } = item
        else {
            continue;
        };
        let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
            continue;
        };
        match name.as_str() {
            "run_shell" => {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    let cmd = cmd.trim();
                    if !cmd.is_empty() {
                        commands.push(cmd.to_string());
                    }
                }
            }
            "write_file" | "edit_file" | "multi_edit" | "notebook_edit" => {
                let path = ["path", "file_path", "notebook_path"]
                    .iter()
                    .find_map(|k| args.get(*k).and_then(|v| v.as_str()));
                if let Some(p) = path {
                    let p = p.trim();
                    if !p.is_empty() && !files.iter().any(|f| f == p) {
                        files.push(p.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    (commands, files)
}

/// Price a session's usage into cost lines + total, at published API rates.
fn cost_lines(usage: &[ModelUsage]) -> (Vec<CostLine>, f64) {
    let mut lines = Vec::new();
    let mut total = 0.0;
    for u in usage {
        let cost = crate::pricing::pricing_for(&u.model).cost_of(u);
        total += cost;
        lines.push(CostLine {
            model: u.model.clone(),
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_write_tokens: u.cache_write_tokens,
            cost_usd: cost,
        });
    }
    (lines, total)
}

/// Summarize one persisted session record into the receipt's brief.
fn session_brief(record: &crate::session::SessionRecord) -> SessionBrief {
    let turns = record
        .history
        .iter()
        .filter(
            |item| matches!(item, crate::openai::InputItem::Message { role, .. } if role == "user"),
        )
        .count() as u64;
    let (mut commands, mut files) = session_activity(&record.history);
    let commands_total = commands.len();
    let files_edited_total = files.len();
    commands.truncate(MAX_COMMANDS);
    files.truncate(MAX_FILES_EDITED);
    let (cost, total_cost_usd) = cost_lines(&record.state.usage);
    SessionBrief {
        id: record.meta.id.clone(),
        model: record.meta.model.clone(),
        turns,
        commands,
        commands_total,
        files_edited: files,
        files_edited_total,
        cost,
        total_cost_usd,
    }
}

/// Collect the receipt: run the proof checks (real exit codes), read the seal
/// on HEAD, load the decision trail, and summarize the newest (or chosen)
/// persisted session. Degrades per section — outside a repo, with no sessions,
/// or with an empty trail, the receipt says so instead of erroring.
pub async fn collect(cwd: &Path, session_id: Option<&str>) -> Receipt {
    let capsule = crate::proof::collect(cwd).await;

    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let head = git(cwd, &["log", "-1", "--pretty=%h %s"]).unwrap_or_default();

    let seal = match crate::seal::read(cwd, "HEAD").await {
        Ok(found) => {
            let failure = crate::seal::verify_failure(&found.seal, &found.commit, &found.tree);
            Some(SealBrief {
                commit: crate::seal::short(&found.seal.commit).to_string(),
                sealed_at: found.seal.sealed_at.clone(),
                verified: failure.is_none(),
                status: failure.unwrap_or_else(|| "verified".to_string()),
            })
        }
        Err(_) => None,
    };

    let mut records = crate::decisions::load(cwd);
    let decisions_total = records.len();
    records.sort_by_key(|r| std::cmp::Reverse(r.ts));
    let decisions = records
        .into_iter()
        .take(MAX_DECISIONS)
        .map(|r| DecisionBrief {
            loc: r.loc,
            decision: r.decision,
            why: r.why,
            model: r.model,
        })
        .collect();
    let rec = crate::decisions::reconcile(cwd);
    let drift = DriftBrief {
        present: rec.present,
        healed: rec.moved.len(),
        stale: rec.stale(),
    };

    let session = {
        let id = match session_id {
            Some(id) => Some(id.to_string()),
            None => crate::session::list(cwd).first().map(|m| m.id.clone()),
        };
        id.and_then(|id| crate::session::load(cwd, &id).ok())
            .map(|record| session_brief(&record))
    };

    Receipt {
        generated: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        root: cwd.display().to_string(),
        branch,
        head,
        capsule,
        seal,
        decisions,
        decisions_total,
        drift,
        session,
    }
}

/// The one-line verdict the receipt leads with, from the fresh capsule.
fn verdict(capsule: &ProofCapsule) -> &'static str {
    if !capsule.any_check_ran() {
        "⚠️ Unverified — no verification checks to run"
    } else if capsule.verified() {
        "✅ Verified"
    } else {
        "❌ Not verified — a check failed"
    }
}

mod html;
mod markdown;

pub use html::*;
pub use markdown::*;

#[cfg(test)]
mod tests;

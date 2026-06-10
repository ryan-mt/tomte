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

/// Render the receipt as paste-ready markdown (the PR attachment).
pub fn render_markdown(r: &Receipt) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Receipt — {}\n\n", r.root));
    out.push_str(&format!(
        "_Collected by the tomte CLI on {} — real git state, real check exit \
         codes, the CLI's own log of what ran. Nothing here is a model's \
         summary._\n\n",
        r.generated
    ));

    out.push_str(&format!("## Verdict: {}\n\n", verdict(&r.capsule)));
    if !r.branch.is_empty() {
        out.push_str(&format!("- branch `{}` · HEAD `{}`\n", r.branch, r.head));
    }
    match &r.seal {
        Some(s) if s.verified => out.push_str(&format!(
            "- seal: ✅ HEAD is sealed and verified (`{}`, sealed {})\n",
            s.commit, s.sealed_at
        )),
        Some(s) => out.push_str(&format!(
            "- seal: ⚠️ HEAD carries a seal that does not verify — {}\n",
            s.status
        )),
        None if !r.branch.is_empty() => {
            out.push_str("- seal: HEAD is not sealed (`tomte seal` notarizes the proof)\n")
        }
        None => out.push_str("- not a git repository (or git is not installed)\n"),
    }

    out.push_str(&format!(
        "\n## What changed ({} file(s), per git)\n\n",
        r.capsule.files_changed.len()
    ));
    if r.capsule.files_changed.is_empty() {
        out.push_str("- working tree clean\n");
    } else {
        for line in r.capsule.files_changed.iter().take(30) {
            out.push_str(&format!("- `{line}`\n"));
        }
        if r.capsule.files_changed.len() > 30 {
            out.push_str(&format!(
                "- … and {} more\n",
                r.capsule.files_changed.len() - 30
            ));
        }
    }

    out.push_str("\n## Checks (run by the CLI, real exit codes)\n\n");
    if r.capsule.checks.is_empty() {
        out.push_str(&format!(
            "- no recognized verification scripts for a {} project\n",
            r.capsule.project_kind.label()
        ));
    } else {
        for c in &r.capsule.checks {
            let line = match &c.outcome {
                Outcome::Passed => format!("- ✅ {} — passed — `{}`", c.name, c.command),
                Outcome::Failed { code } => {
                    format!("- ❌ {} — failed (exit {code}) — `{}`", c.name, c.command)
                }
                Outcome::Skipped => format!("- ⚠️ {} — not verified — no script", c.name),
                Outcome::Errored { message } => {
                    format!("- ❌ {} — error ({message}) — `{}`", c.name, c.command)
                }
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !r.capsule.reproduce.is_empty() {
        out.push_str("\nReproduce:\n\n```\n");
        for cmd in &r.capsule.reproduce {
            out.push_str(&format!("{cmd}\n"));
        }
        out.push_str("```\n");
    }

    if let Some(s) = &r.session {
        out.push_str(&format!(
            "\n## What the session did (session `{}`, {} turn(s), model {})\n\n",
            s.id, s.turns, s.model
        ));
        if s.files_edited.is_empty() {
            out.push_str("- no files edited through tomte's edit tools\n");
        } else {
            out.push_str(&format!("- files edited ({}):\n", s.files_edited_total));
            for f in &s.files_edited {
                out.push_str(&format!("  - `{f}`\n"));
            }
            if s.files_edited_total > s.files_edited.len() {
                out.push_str(&format!(
                    "  - … and {} more\n",
                    s.files_edited_total - s.files_edited.len()
                ));
            }
        }
        if s.commands.is_empty() {
            out.push_str("- no shell commands run\n");
        } else {
            out.push_str(&format!("- commands run ({}):\n", s.commands_total));
            for c in &s.commands {
                out.push_str(&format!("  - `{c}`\n"));
            }
            if s.commands_total > s.commands.len() {
                out.push_str(&format!(
                    "  - … and {} more\n",
                    s.commands_total - s.commands.len()
                ));
            }
        }
        if !s.cost.is_empty() {
            out.push_str("- cost (API-rate estimate):\n");
            for l in &s.cost {
                out.push_str(&format!(
                    "  - {} — ${:.4} (in {} · out {} · cache read {} · cache write {})\n",
                    l.model,
                    l.cost_usd,
                    l.input_tokens,
                    l.output_tokens,
                    l.cache_read_tokens,
                    l.cache_write_tokens
                ));
            }
            out.push_str(&format!("  - total: ${:.4}\n", s.total_cost_usd));
        }
    }

    out.push_str("\n## Why (decision trail)\n\n");
    if r.decisions.is_empty() {
        out.push_str(
            "- no decisions recorded yet — `record_decision` writes the why, \
             `tomte why <loc>` reads it back\n",
        );
    } else {
        for d in &r.decisions {
            out.push_str(&format!(
                "- `{}` — {} — because {} _(recorded by {})_\n",
                d.loc, d.decision, d.why, d.model
            ));
        }
        if r.decisions_total > r.decisions.len() {
            out.push_str(&format!(
                "- … {} more on the trail: `tomte why --all`\n",
                r.decisions_total - r.decisions.len()
            ));
        }
        out.push_str(&format!(
            "- drift watch: {} anchor(s) hold · {} healed · {} need eyes\n",
            r.drift.present, r.drift.healed, r.drift.stale
        ));
    }

    out.push_str(
        "\n---\n_Re-verify yourself: `tomte prove` · gate CI on the seal: \
         `tomte seal verify` — done means verified._\n",
    );
    out
}

/// Minimal HTML escaping for text interpolated into the HTML receipt.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render the receipt as one standalone HTML page (no external assets), for
/// sharing outside a markdown context. Same data as the markdown view.
pub fn render_html(r: &Receipt) -> String {
    let mut body = String::new();
    let push_li = |body: &mut String, text: &str| {
        body.push_str(&format!("<li>{}</li>\n", esc(text)));
    };

    body.push_str(&format!("<h1>Receipt — {}</h1>\n", esc(&r.root)));
    body.push_str(&format!(
        "<p class=\"sub\">Collected by the tomte CLI on {} — real git state, \
         real check exit codes, the CLI's own log of what ran.</p>\n",
        esc(&r.generated)
    ));

    body.push_str(&format!(
        "<h2>Verdict: {}</h2>\n<ul>\n",
        verdict(&r.capsule)
    ));
    if !r.branch.is_empty() {
        push_li(&mut body, &format!("branch {} · HEAD {}", r.branch, r.head));
    }
    match &r.seal {
        Some(s) if s.verified => push_li(
            &mut body,
            &format!(
                "seal: ✅ HEAD is sealed and verified ({}, sealed {})",
                s.commit, s.sealed_at
            ),
        ),
        Some(s) => push_li(
            &mut body,
            &format!(
                "seal: ⚠️ HEAD carries a seal that does not verify — {}",
                s.status
            ),
        ),
        None if !r.branch.is_empty() => push_li(&mut body, "seal: HEAD is not sealed"),
        None => push_li(&mut body, "not a git repository (or git is not installed)"),
    }
    body.push_str("</ul>\n");

    body.push_str(&format!(
        "<h2>What changed ({} file(s), per git)</h2>\n<ul>\n",
        r.capsule.files_changed.len()
    ));
    if r.capsule.files_changed.is_empty() {
        push_li(&mut body, "working tree clean");
    } else {
        for line in r.capsule.files_changed.iter().take(30) {
            push_li(&mut body, line);
        }
        if r.capsule.files_changed.len() > 30 {
            push_li(
                &mut body,
                &format!("… and {} more", r.capsule.files_changed.len() - 30),
            );
        }
    }
    body.push_str("</ul>\n");

    body.push_str("<h2>Checks (run by the CLI, real exit codes)</h2>\n<ul>\n");
    if r.capsule.checks.is_empty() {
        push_li(
            &mut body,
            &format!(
                "no recognized verification scripts for a {} project",
                r.capsule.project_kind.label()
            ),
        );
    } else {
        for c in &r.capsule.checks {
            let text = match &c.outcome {
                Outcome::Passed => format!("✅ {} — passed — {}", c.name, c.command),
                Outcome::Failed { code } => {
                    format!("❌ {} — failed (exit {code}) — {}", c.name, c.command)
                }
                Outcome::Skipped => format!("⚠️ {} — not verified — no script", c.name),
                Outcome::Errored { message } => {
                    format!("❌ {} — error ({message}) — {}", c.name, c.command)
                }
            };
            push_li(&mut body, &text);
        }
    }
    body.push_str("</ul>\n");

    if let Some(s) = &r.session {
        body.push_str(&format!(
            "<h2>What the session did (session {}, {} turn(s), model {})</h2>\n<ul>\n",
            esc(&s.id),
            s.turns,
            esc(&s.model)
        ));
        if s.files_edited.is_empty() {
            push_li(&mut body, "no files edited through tomte's edit tools");
        } else {
            push_li(
                &mut body,
                &format!("files edited ({}):", s.files_edited_total),
            );
            for f in &s.files_edited {
                push_li(&mut body, &format!("· {f}"));
            }
        }
        if s.commands.is_empty() {
            push_li(&mut body, "no shell commands run");
        } else {
            push_li(&mut body, &format!("commands run ({}):", s.commands_total));
            for c in &s.commands {
                push_li(&mut body, &format!("· {c}"));
            }
        }
        if !s.cost.is_empty() {
            push_li(
                &mut body,
                &format!("estimated cost: ${:.4} (API rates)", s.total_cost_usd),
            );
        }
        body.push_str("</ul>\n");
    }

    body.push_str("<h2>Why (decision trail)</h2>\n<ul>\n");
    if r.decisions.is_empty() {
        push_li(&mut body, "no decisions recorded yet");
    } else {
        for d in &r.decisions {
            push_li(
                &mut body,
                &format!(
                    "{} — {} — because {} (recorded by {})",
                    d.loc, d.decision, d.why, d.model
                ),
            );
        }
        push_li(
            &mut body,
            &format!(
                "drift watch: {} anchor(s) hold · {} healed · {} need eyes",
                r.drift.present, r.drift.healed, r.drift.stale
            ),
        );
    }
    body.push_str("</ul>\n");

    body.push_str(
        "<p class=\"sub\">Re-verify yourself: <code>tomte prove</code> · gate CI \
         on the seal: <code>tomte seal verify</code> — done means verified.</p>\n",
    );

    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>tomte receipt</title>\n<style>\n\
         body{{max-width:48rem;margin:2rem auto;padding:0 1rem;\
         font-family:ui-monospace,SFMono-Regular,Consolas,monospace;\
         background:#101412;color:#e6e4dc;line-height:1.5}}\n\
         h1{{font-size:1.2rem}} h2{{font-size:1rem;margin-top:1.5rem}}\n\
         ul{{padding-left:1.2rem}} li{{margin:.15rem 0}}\n\
         code{{color:#ffb86c}} .sub{{color:#9a9a8f}}\n\
         </style>\n</head>\n<body>\n{body}</body>\n</html>\n"
    )
}

#[cfg(test)]
mod tests;

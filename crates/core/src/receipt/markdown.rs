use super::*;

/// Collapse session/model-authored text onto one capped line. The receipt
/// sells itself as CLI-collected truth, so a multi-line shell command or
/// decision `why` must not be able to inject raw markdown (e.g. a forged
/// `## Verdict` heading) into it.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let t: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
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
            "- no recognized verification scripts for this {} project\n",
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
                out.push_str(&format!("  - `{}`\n", one_line(f, 160)));
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
                out.push_str(&format!("  - `{}`\n", one_line(c, 160)));
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
                one_line(&d.loc, 120),
                one_line(&d.decision, 200),
                one_line(&d.why, 300),
                one_line(&d.model, 60)
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

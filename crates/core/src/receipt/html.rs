use super::*;

/// Minimal HTML escaping for text interpolated into the HTML receipt.
pub(super) fn esc(s: &str) -> String {
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
                "no recognized verification scripts for this {} project",
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

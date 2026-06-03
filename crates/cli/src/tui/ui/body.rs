//! Tool-call body rendering and its length limits. Split out of `ui`; logic unchanged.

use super::*;

pub(super) fn friendly_body<'a>(
    name: &str,
    args: &serde_json::Value,
    output: Option<&str>,
    error: bool,
    width: usize,
    expanded: bool,
) -> Vec<Line<'a>> {
    let mut out: Vec<Line> = Vec::new();
    let avail = width.saturating_sub(4); // minus branch "  │ "
    let Some(text) = output else {
        out.push(Line::from(Span::styled(
            "…",
            Style::default().fg(Color::Yellow),
        )));
        return out;
    };

    if error {
        // Always show full error text (regardless of compact mode) so the model
        // and the user can diagnose. Cap to avoid runaway output.
        let max_err = if expanded { usize::MAX } else { 30 };
        let total = text.lines().count();
        for raw in text.lines().take(max_err) {
            for w in wrap(raw, avail) {
                out.push(Line::from(Span::styled(w, Style::default().fg(Color::Red))));
            }
        }
        if total > max_err {
            out.push(Line::from(Span::styled(
                format!("… +{} lines", total - max_err),
                Style::default().fg(Color::Rgb(160, 160, 160)),
            )));
        }
        return out;
    }

    let style_summary = Style::default().fg(Color::Gray);
    let style_meta = Style::default().fg(Color::Rgb(160, 160, 160));
    let style_code = Style::default().fg(Color::White);
    let style_lineno = Style::default().fg(Color::Rgb(160, 160, 160));

    // Per-tool limits: (compact, expanded).
    let limits = BodyLimits::for_mode(expanded);

    match name {
        "write_file" => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let line_count = if content.is_empty() {
                0
            } else {
                content.lines().count().max(1)
            };
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("(file)");
            out.push(Line::from(Span::styled(
                format!("Wrote {} lines to {}", line_count, pretty_path(path)),
                style_summary,
            )));
            append_numbered(
                &mut out,
                content,
                limits.write_preview,
                style_lineno,
                style_code,
                avail,
            );
        }
        "edit_file" => {
            let old = args
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_ = args
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let added = if new_.is_empty() {
                0
            } else {
                new_.lines().count()
            };
            let removed = if old.is_empty() {
                0
            } else {
                old.lines().count()
            };
            let summary_text = match (added, removed) {
                (a, 0) => format!("Added {a} line{}", plural(a)),
                (0, r) => format!("Removed {r} line{}", plural(r)),
                (a, r) => format!("Added {a} line{}, removed {r} line{}", plural(a), plural(r)),
            };
            out.push(Line::from(Span::styled(summary_text, style_summary)));

            // Determine starting line number by trying to locate old_string in the file.
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let start_line = locate_line_number(path, old).unwrap_or(1);

            let removed_bg = Style::default()
                .bg(Color::Rgb(60, 0, 0))
                .fg(Color::Rgb(255, 120, 120));
            let added_bg = Style::default()
                .bg(Color::Rgb(0, 50, 0))
                .fg(Color::Rgb(160, 255, 160));
            let lineno_removed = Style::default()
                .bg(Color::Rgb(60, 0, 0))
                .fg(Color::Rgb(200, 80, 80));
            let lineno_added = Style::default()
                .bg(Color::Rgb(0, 50, 0))
                .fg(Color::Rgb(120, 200, 120));

            let mut shown = 0usize;
            let max_diff = limits.edit_diff;

            for (i, line) in old.lines().enumerate() {
                if shown >= max_diff {
                    break;
                }
                let n = start_line + i;
                out.push(diff_line(n, "-", line, lineno_removed, removed_bg, avail));
                shown += 1;
            }
            for (i, line) in new_.lines().enumerate() {
                if shown >= max_diff {
                    break;
                }
                let n = start_line + i;
                out.push(diff_line(n, "+", line, lineno_added, added_bg, avail));
                shown += 1;
            }
            let total = old.lines().count() + new_.lines().count();
            if total > max_diff {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines", total - max_diff),
                    style_meta,
                )));
            }
        }
        "multi_edit" => {
            // Render a per-edit diff like edit_file so an Update from multi_edit
            // shows what changed (previously it fell through to the raw "Applied
            // N edits" text — the "sometimes the diff shows, sometimes not"
            // inconsistency between edit_file and multi_edit).
            let edits = args.get("edits").and_then(|v| v.as_array());
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let removed_bg = Style::default()
                .bg(Color::Rgb(60, 0, 0))
                .fg(Color::Rgb(255, 120, 120));
            let added_bg = Style::default()
                .bg(Color::Rgb(0, 50, 0))
                .fg(Color::Rgb(160, 255, 160));
            let lineno_removed = Style::default()
                .bg(Color::Rgb(60, 0, 0))
                .fg(Color::Rgb(200, 80, 80));
            let lineno_added = Style::default()
                .bg(Color::Rgb(0, 50, 0))
                .fg(Color::Rgb(120, 200, 120));

            let (mut total_added, mut total_removed) = (0usize, 0usize);
            if let Some(edits) = edits {
                for e in edits {
                    let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new_ = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    total_added += if new_.is_empty() {
                        0
                    } else {
                        new_.lines().count()
                    };
                    total_removed += if old.is_empty() {
                        0
                    } else {
                        old.lines().count()
                    };
                }
            }
            let n_edits = edits.map(|e| e.len()).unwrap_or(0);
            out.push(Line::from(Span::styled(
                format!(
                    "{n_edits} edit{}: added {total_added} line{}, removed {total_removed} line{}",
                    plural(n_edits),
                    plural(total_added),
                    plural(total_removed)
                ),
                style_summary,
            )));

            let max_diff = limits.edit_diff;
            let mut shown = 0usize;
            if let Some(edits) = edits {
                'edits: for e in edits {
                    let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                    let new_ = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                    let start_line = locate_line_number(path, old).unwrap_or(1);
                    for (i, line) in old.lines().enumerate() {
                        if shown >= max_diff {
                            break 'edits;
                        }
                        out.push(diff_line(
                            start_line + i,
                            "-",
                            line,
                            lineno_removed,
                            removed_bg,
                            avail,
                        ));
                        shown += 1;
                    }
                    for (i, line) in new_.lines().enumerate() {
                        if shown >= max_diff {
                            break 'edits;
                        }
                        out.push(diff_line(
                            start_line + i,
                            "+",
                            line,
                            lineno_added,
                            added_bg,
                            avail,
                        ));
                        shown += 1;
                    }
                }
            }
            let total_lines = total_added + total_removed;
            if total_lines > max_diff {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines", total_lines - max_diff),
                    style_meta,
                )));
            }
        }
        "read_file" => {
            // Just the line count — never dump file contents into the chat.
            // The model already has the file in its context; the user only
            // needs to know that the read happened.
            let total = text.lines().count();
            out.push(Line::from(Span::styled(
                format!("Read {total} line{}", plural(total)),
                style_summary,
            )));
        }
        "run_shell" => {
            // Output format: "exit_code: N\n--- stdout ---\n…\n--- stderr ---\n…"
            let (code, stdout, stderr) = parse_shell_output(text);
            let success = code == 0;
            // A failed command's output is the whole point, so when collapsed we
            // show a bigger slice than the 3-line success preview — enough to
            // read the error inline, still bounded with a "Ctrl+O for more" hint.
            const FAILED_SHELL_PREVIEW: usize = 15;
            let (stdout_budget, stderr_budget) = if !success && !expanded {
                (FAILED_SHELL_PREVIEW, FAILED_SHELL_PREVIEW)
            } else {
                (limits.shell_stdout, limits.shell_stderr)
            };
            if !stdout.is_empty() {
                let total = stdout.lines().count();
                for raw in stdout.lines().take(stdout_budget) {
                    for w in wrap(raw, avail) {
                        out.push(Line::from(Span::styled(w, style_code)));
                    }
                }
                let extra = total.saturating_sub(stdout_budget);
                if extra > 0 {
                    out.push(Line::from(Span::styled(
                        format!("… +{extra} more line{} (Ctrl+O for more)", plural(extra)),
                        style_meta,
                    )));
                }
            }
            let stderr_trim = stderr.trim();
            if !stderr_trim.is_empty() {
                if !success || expanded {
                    // Claude Code style: stderr rendered in red, with no
                    // separator box — the colour alone sets it apart from stdout.
                    let err_style = Style::default().fg(Color::Red);
                    let total_err = stderr.lines().count();
                    for raw in stderr.lines().take(stderr_budget) {
                        for w in wrap(raw, avail) {
                            out.push(Line::from(Span::styled(w, err_style)));
                        }
                    }
                    if total_err > stderr_budget {
                        out.push(Line::from(Span::styled(
                            format!(
                                "… +{} stderr line{} (Ctrl+O for more)",
                                total_err - stderr_budget,
                                plural(total_err - stderr_budget)
                            ),
                            style_meta,
                        )));
                    }
                } else {
                    // Success but with stderr noise (warnings, etc.). One-line
                    // hint keeps things clean without losing the signal.
                    let n = stderr.lines().filter(|l| !l.trim().is_empty()).count();
                    out.push(Line::from(Span::styled(
                        format!(
                            "(+ {n} stderr line{} suppressed — Ctrl+O to view)",
                            plural(n)
                        ),
                        style_meta,
                    )));
                }
            }
            // Claude Code shows no "exit 0" on success; only a compact red
            // footer when the command failed.
            if !success {
                out.push(Line::from(Span::styled(
                    format!("Error (exit {code})"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
        "grep" => {
            let total = text.lines().filter(|l| !l.is_empty()).count();
            out.push(Line::from(Span::styled(
                format!("{total} match{}", if total == 1 { "" } else { "es" }),
                style_summary,
            )));
            for raw in text.lines().take(limits.grep_preview) {
                for w in wrap(raw, avail) {
                    out.push(Line::from(Span::styled(w, style_code)));
                }
            }
            if total > limits.grep_preview {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines (Ctrl+O for more)", total - limits.grep_preview),
                    style_meta,
                )));
            }
        }
        "todo_write" => {
            // Render the canonical todo list as a checklist, mirroring the
            // Claude Code CLI presentation. Falls back to the summary text
            // when the args JSON hasn't fully arrived yet.
            let Some(todos) = args.get("todos").and_then(|v| v.as_array()) else {
                out.push(Line::from(Span::styled(
                    text.lines().next().unwrap_or("").to_string(),
                    style_summary,
                )));
                return out;
            };

            let done_text = Style::default()
                .fg(Color::Rgb(130, 130, 130))
                .add_modifier(Modifier::CROSSED_OUT);
            let pending_text = Style::default().fg(Color::Gray);
            let active_text = Style::default()
                .fg(Color::Rgb(255, 184, 108))
                .add_modifier(Modifier::BOLD);
            let check_done = Style::default().fg(Color::Green);
            let check_pending = Style::default().fg(Color::Rgb(160, 160, 160));
            let check_active = Style::default().fg(Color::Rgb(255, 184, 108));

            for todo in todos {
                let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let active = todo
                    .get("active_form")
                    .and_then(|v| v.as_str())
                    .or_else(|| todo.get("activeForm").and_then(|v| v.as_str()))
                    .unwrap_or(content);
                let status = todo
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending");
                let (symbol, sym_style, body_style, label) = match status {
                    "completed" => ("☒", check_done, done_text, content),
                    "in_progress" => ("☐", check_active, active_text, active),
                    _ => ("☐", check_pending, pending_text, content),
                };
                let label_wrapped = wrap(label, avail.saturating_sub(2));
                let mut first = true;
                for piece in label_wrapped {
                    if first {
                        out.push(Line::from(vec![
                            Span::styled(format!("{symbol} "), sym_style),
                            Span::styled(piece, body_style),
                        ]));
                        first = false;
                    } else {
                        out.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(piece, body_style),
                        ]));
                    }
                }
            }
        }
        "ask_user_question" | "enter_plan_mode" | "exit_plan_mode" => {
            // No inline body: the System block rendered right below this tool
            // result already shows the question or plan approval prompt.
        }
        "glob" | "list_dir" => {
            let total = text.lines().filter(|l| !l.is_empty()).count();
            out.push(Line::from(Span::styled(
                format!("{total} entr{}", if total == 1 { "y" } else { "ies" }),
                style_summary,
            )));
            for raw in text.lines().take(limits.list_preview) {
                if raw.is_empty() {
                    continue;
                }
                for w in wrap(raw, avail) {
                    out.push(Line::from(Span::styled(w, style_code)));
                }
            }
            if total > limits.list_preview {
                out.push(Line::from(Span::styled(
                    format!("… +{} lines (Ctrl+O for more)", total - limits.list_preview),
                    style_meta,
                )));
            }
        }
        _ => {
            let total = text.lines().count();
            for raw in text.lines().take(limits.default_preview) {
                for w in wrap(raw, avail) {
                    out.push(Line::from(Span::styled(w, style_code)));
                }
            }
            if total > limits.default_preview {
                out.push(Line::from(Span::styled(
                    format!(
                        "… +{} lines (Ctrl+O for more)",
                        total - limits.default_preview
                    ),
                    style_meta,
                )));
            }
        }
    }
    out
}

pub(super) struct BodyLimits {
    write_preview: usize,
    edit_diff: usize,
    shell_stdout: usize,
    shell_stderr: usize,
    grep_preview: usize,
    list_preview: usize,
    default_preview: usize,
}

impl BodyLimits {
    fn for_mode(expanded: bool) -> Self {
        if expanded {
            Self {
                write_preview: 60,
                edit_diff: 80,
                shell_stdout: 80,
                shell_stderr: 30,
                grep_preview: 60,
                list_preview: 60,
                default_preview: 60,
            }
        } else {
            Self {
                write_preview: 6,
                edit_diff: 8,
                shell_stdout: 3,
                shell_stderr: 6,
                grep_preview: 5,
                list_preview: 5,
                default_preview: 5,
            }
        }
    }
}

//! End-of-turn "left in order" summary — SOUL.md Pillar 4 (the calm, tidy
//! terminal). When a turn completes, the custodian leaves a tidy one-line
//! receipt of what it changed in the house: files touched, commands/tests run
//! (with pass/fail read from the shell's own exit code, not the tool-error
//! flag), and the *why* it recorded (a Pillar-2 decision seed). A pure
//! question-and-answer turn that changed nothing produces no receipt — the
//! custodian reports only when it actually did something.

use super::*;
use crate::tui::palette;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Tools that write to a file on disk.
fn is_file_write(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "multi_edit" | "notebook_edit"
    )
}

/// First present, non-empty string field from a tool's JSON args, trying each
/// key in turn (tools accept aliases like `file_path`/`filePath`).
fn arg_str(args: &str, keys: &[&str]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    keys.iter().find_map(|k| {
        v.get(*k)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    })
}

/// Heuristic: does this shell command run a test suite? Used only to label the
/// receipt ("test" vs "cmd"); a miss just shows it as a generic command.
fn looks_like_test(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    const RUNNERS: &[&str] = &[
        "cargo test",
        "cargo nextest",
        "nextest run",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "jest",
        "vitest",
        "mvn test",
        "gradle test",
        "rspec",
        "phpunit",
    ];
    RUNNERS.iter().any(|p| c.contains(p)) || c.split_whitespace().any(|t| t == "test")
}

/// Parse `run_shell`'s `exit_code: N` header (its output always starts with it —
/// see `tools::shell::run`). `Some(true)` = exit 0, `Some(false)` = nonzero,
/// `None` = couldn't tell (no output captured yet).
fn shell_ok(output: Option<&String>) -> Option<bool> {
    let first = output?.lines().next()?;
    let code = first.strip_prefix("exit_code:")?.trim();
    Some(code == "0")
}

/// File name of a path for the compact receipt, falling back to the whole path.
fn short_path(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(p)
        .to_string()
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Append a ` · <text>` segment to the headline line.
fn push_seg(spans: &mut Vec<Span<'static>>, text: String, color: Color) {
    spans.push(Span::styled(
        " · ",
        Style::default().fg(palette::TEXT_FAINT),
    ));
    spans.push(Span::styled(text, Style::default().fg(color)));
}

/// Build the "left in order" receipt for the just-finished turn — the blocks
/// after the last `Block::User`, so it reports only the current turn's work.
/// Returns `None` when the turn changed nothing worth reporting.
pub fn build_turn_summary(blocks: &[Block]) -> Option<Block> {
    let start = blocks
        .iter()
        .rposition(|b| matches!(b, Block::User(_)))
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut files: Vec<String> = Vec::new();
    let mut tests_total = 0usize;
    let mut tests_failed = 0usize;
    let mut cmds = 0usize;
    let mut tool_errors = 0usize;
    let mut decisions: Vec<(String, String)> = Vec::new();

    for b in &blocks[start..] {
        let Block::Tool {
            name,
            args,
            output,
            error,
            ..
        } = b
        else {
            continue;
        };
        if *error {
            tool_errors += 1;
        }
        if is_file_write(name) {
            if let Some(p) = arg_str(args, &["path", "file_path", "filePath", "notebook_path"]) {
                if !files.contains(&p) {
                    files.push(p);
                }
            }
        } else if name == "run_shell" {
            let cmd = arg_str(args, &["command", "cmd"]).unwrap_or_default();
            if looks_like_test(&cmd) {
                tests_total += 1;
                if shell_ok(output.as_ref()) == Some(false) {
                    tests_failed += 1;
                }
            } else {
                cmds += 1;
            }
        } else if name == "record_decision" {
            if let Some(d) = arg_str(args, &["decision"]) {
                let loc = arg_str(args, &["loc"]).unwrap_or_default();
                decisions.push((loc, d));
            }
        }
    }

    if files.is_empty() && tests_total == 0 && cmds == 0 && decisions.is_empty() {
        return None;
    }

    // --- headline ---
    let trouble = tool_errors > 0 || tests_failed > 0;
    let (glyph, glyph_color) = if trouble {
        ("⚠", palette::WARNING)
    } else {
        ("✓", palette::SUCCESS)
    };
    let mut spans = vec![
        Span::styled(format!("{glyph} "), Style::default().fg(glyph_color)),
        Span::styled("left in order", Style::default().fg(palette::TEXT_MUTED)),
    ];
    if !files.is_empty() {
        let label = if files.len() <= 3 {
            files
                .iter()
                .map(|p| short_path(p))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            format!("{} files", files.len())
        };
        push_seg(&mut spans, label, palette::ACCENT);
    }
    if tests_total > 0 {
        let (txt, color) = if tests_failed > 0 {
            (
                format!(
                    "{tests_total} test{} ({tests_failed} failed)",
                    plural(tests_total)
                ),
                palette::DANGER,
            )
        } else {
            (
                format!("{tests_total} test{} passed", plural(tests_total)),
                palette::SUCCESS,
            )
        };
        push_seg(&mut spans, txt, color);
    }
    if cmds > 0 {
        push_seg(
            &mut spans,
            format!("{cmds} cmd{}", plural(cmds)),
            palette::TEXT_MUTED,
        );
    }
    if tool_errors > 0 {
        push_seg(
            &mut spans,
            format!("{tool_errors} error{}", plural(tool_errors)),
            palette::DANGER,
        );
    }

    let mut lines = vec![Line::from(spans)];

    // --- why lines (a Pillar-2 decision seed), capped so the receipt stays tidy ---
    for (loc, decision) in decisions.iter().take(3) {
        let mut s = vec![Span::raw("    ")];
        if !loc.is_empty() {
            s.push(Span::styled(
                loc.clone(),
                Style::default().fg(palette::ACCENT),
            ));
            s.push(Span::raw("  "));
        }
        s.push(Span::styled(
            "why: ",
            Style::default().fg(palette::TEXT_FAINT),
        ));
        s.push(Span::styled(
            decision.clone(),
            Style::default().fg(palette::TEXT_MUTED),
        ));
        lines.push(Line::from(s));
    }

    Some(Block::Rich(lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, args: &str, output: Option<&str>, error: bool) -> Block {
        Block::Tool {
            call_id: "c".into(),
            name: name.into(),
            args: args.into(),
            output: output.map(|s| s.into()),
            error,
            preflight: None,
        }
    }

    fn assistant(text: &str) -> Block {
        Block::Assistant {
            text: text.into(),
            reasoning: String::new(),
            done: true,
            thought_for_secs: None,
            reasoning_started_at: None,
        }
    }

    fn lines_text(b: &Block) -> String {
        match b {
            Block::Rich(lines) => lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => panic!("expected a Rich block"),
        }
    }

    #[test]
    fn qa_turn_with_no_actions_has_no_receipt() {
        let blocks = vec![Block::User("hi".into()), assistant("hello")];
        assert!(build_turn_summary(&blocks).is_none());
    }

    #[test]
    fn read_only_turn_has_no_receipt() {
        let blocks = vec![
            Block::User("look".into()),
            tool("read_file", r#"{"path":"src/lib.rs"}"#, Some("..."), false),
        ];
        assert!(build_turn_summary(&blocks).is_none());
    }

    #[test]
    fn reports_files_tests_and_why() {
        let blocks = vec![
            Block::User("fix parser".into()),
            tool(
                "edit_file",
                r#"{"path":"src/parser.rs"}"#,
                Some("ok"),
                false,
            ),
            tool(
                "write_file",
                r#"{"file_path":"src/lib.rs"}"#,
                Some("ok"),
                false,
            ),
            tool(
                "run_shell",
                r#"{"command":"cargo test"}"#,
                Some("exit_code: 0\n--- stdout ---\nok"),
                false,
            ),
            tool(
                "record_decision",
                r#"{"loc":"src/parser.rs:88","decision":"cover the empty-input case","why":"x","rejected":[]}"#,
                Some("recorded"),
                false,
            ),
        ];
        let b = build_turn_summary(&blocks).expect("receipt expected");
        let t = lines_text(&b);
        assert!(t.starts_with('✓'), "{t}");
        assert!(t.contains("left in order"), "{t}");
        assert!(t.contains("parser.rs"), "{t}");
        assert!(t.contains("lib.rs"), "{t}");
        assert!(t.contains("1 test passed"), "{t}");
        assert!(t.contains("why: cover the empty-input case"), "{t}");
        assert!(t.contains("src/parser.rs:88"), "{t}");
    }

    #[test]
    fn failed_test_flags_trouble() {
        let blocks = vec![
            Block::User("run tests".into()),
            tool(
                "run_shell",
                r#"{"command":"cargo test"}"#,
                Some("exit_code: 101\n--- stdout ---\nFAILED"),
                false,
            ),
        ];
        let b = build_turn_summary(&blocks).expect("receipt expected");
        let t = lines_text(&b);
        assert!(t.starts_with('⚠'), "{t}");
        assert!(t.contains("failed"), "{t}");
    }

    #[test]
    fn only_counts_the_current_turn() {
        let blocks = vec![
            Block::User("first".into()),
            tool("edit_file", r#"{"path":"a.rs"}"#, Some("ok"), false),
            Block::User("second".into()),
            tool(
                "run_shell",
                r#"{"command":"ls"}"#,
                Some("exit_code: 0\n"),
                false,
            ),
        ];
        let b = build_turn_summary(&blocks).expect("receipt expected");
        let t = lines_text(&b);
        assert!(!t.contains("a.rs"), "must exclude the previous turn: {t}");
        assert!(t.contains("1 cmd"), "{t}");
    }

    #[test]
    fn dedupes_repeated_file_edits() {
        let blocks = vec![
            Block::User("edit twice".into()),
            tool("edit_file", r#"{"path":"src/x.rs"}"#, Some("ok"), false),
            tool("edit_file", r#"{"path":"src/x.rs"}"#, Some("ok"), false),
        ];
        let b = build_turn_summary(&blocks).expect("receipt expected");
        let t = lines_text(&b);
        // One file name, not two.
        assert_eq!(t.matches("x.rs").count(), 1, "{t}");
    }
}

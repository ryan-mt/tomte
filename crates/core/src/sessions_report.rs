//! `tomte sessions` — the saved-session ledger: list this project's persisted
//! sessions, render one as a readable transcript, and plan a prune of old
//! ones. All selection and rendering logic is pure (timestamps are injected)
//! so it is unit-testable; the CLI command does the filesystem work.

use crate::openai::{InputItem, MessageContent};
use crate::session::{SessionMeta, SessionRecord};

/// Human age of a timestamp: "just now", "5m ago", "3h ago", "12d ago".
/// A future timestamp (clock skew, a copied store) reads "just now" instead
/// of underflowing.
pub fn format_age(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// A millisecond timestamp as UTC text; "unknown" when it can't represent a
/// real date (e.g. a corrupt store carrying u64::MAX).
fn format_utc(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Render the session list card. `metas` must be newest-first (the
/// `session::list` contract).
pub fn render_list(metas: &[SessionMeta], now_ms: u64) -> String {
    if metas.is_empty() {
        return "No saved sessions for this project yet — `tomte` starts one, and every \
                session is saved automatically."
            .to_string();
    }
    let noun = if metas.len() == 1 {
        "session"
    } else {
        "sessions"
    };
    let mut out = format!(
        "Saved {noun} — {} for this project (newest first)\n",
        metas.len()
    );
    for m in metas {
        out.push_str(&format!(
            "\n  {}  ·  {}  ·  {}  ·  {} msgs\n    {}\n",
            m.id,
            format_age(now_ms, m.updated_at_ms),
            m.model,
            m.message_count,
            m.preview
        ));
    }
    out.push_str(
        "\nResume:  tomte --continue (newest) · tomte resume (picker)\
         \nInspect: tomte sessions show <id> · tomte cost --session <id>",
    );
    out
}

/// Keys probed, in order, for a tool call's one-line brief — the argument a
/// human would recognize the call by. The edit/shell spellings mirror
/// `receipt::session_activity`; the rest cover the common read/search shapes.
const BRIEF_KEYS: [&str; 7] = [
    "command",
    "path",
    "file_path",
    "notebook_path",
    "pattern",
    "url",
    "seed",
];

fn tool_brief(arguments: &str) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return String::new();
    };
    let Some(brief) = BRIEF_KEYS
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
    else {
        return String::new();
    };
    truncate_chars(&brief.trim().replace('\n', " "), 100)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

/// Render one persisted session as a readable markdown transcript: user and
/// assistant messages in full, each tool call as a one-line `> tool:` note.
/// Tool *results* are omitted (they dominate the bytes and the `--json` form
/// carries them); reasoning items are provider-internal and skipped.
pub fn render_transcript(record: &SessionRecord) -> String {
    let meta = &record.meta;
    let turns = record
        .history
        .iter()
        .filter(|item| matches!(item, InputItem::Message { role, .. } if role == "user"))
        .count();
    let omitted = record
        .history
        .iter()
        .filter(|item| matches!(item, InputItem::FunctionCallOutput { .. }))
        .count();

    let mut out = format!("# tomte session {}\n\n", meta.id);
    out.push_str(&format!("- model: `{}`\n", meta.model));
    out.push_str(&format!(
        "- created: {} · updated: {}\n",
        format_utc(meta.created_at_ms),
        format_utc(meta.updated_at_ms)
    ));
    out.push_str(&format!(
        "- messages: {} · turns: {turns}\n",
        record.history.len()
    ));
    if omitted > 0 {
        out.push_str(&format!(
            "- {omitted} tool result{} omitted — `--json` carries the full record\n",
            if omitted == 1 { "" } else { "s" }
        ));
    }

    for item in &record.history {
        match item {
            InputItem::Message { role, content } => {
                let heading = match role.as_str() {
                    "user" => "You",
                    "assistant" => "tomte",
                    other => other,
                };
                out.push_str(&format!("\n## {heading}\n\n"));
                for c in content {
                    match c {
                        MessageContent::InputText { text }
                        | MessageContent::OutputText { text } => {
                            out.push_str(text.trim_end());
                            out.push('\n');
                        }
                        MessageContent::InputImage { .. } => {
                            out.push_str("*(image attached)*\n");
                        }
                    }
                }
            }
            InputItem::FunctionCall {
                name, arguments, ..
            } => {
                let brief = tool_brief(arguments);
                if brief.is_empty() {
                    out.push_str(&format!("\n> tool: {name}\n"));
                } else {
                    out.push_str(&format!("\n> tool: {name} — {brief}\n"));
                }
            }
            // Tool outputs are counted in the header; reasoning items are
            // provider-internal continuity, not conversation.
            _ => {}
        }
    }
    out
}

/// Select which sessions a prune would delete. `metas` must be newest-first
/// (the `session::list` contract). The rules union: `keep` selects everything
/// beyond the newest N, `older_than_days` selects everything last updated
/// before the cutoff. With neither rule nothing is selected — the CLI
/// requires at least one flag, so a bare prune can never select the store.
pub fn plan_prune(
    metas: &[SessionMeta],
    keep: Option<usize>,
    older_than_days: Option<u64>,
    now_ms: u64,
) -> Vec<SessionMeta> {
    let cutoff_ms = older_than_days.map(|d| now_ms.saturating_sub(d.saturating_mul(86_400_000)));
    metas
        .iter()
        .enumerate()
        .filter(|(i, m)| {
            let beyond_keep = keep.is_some_and(|k| *i >= k);
            let too_old = cutoff_ms.is_some_and(|c| m.updated_at_ms < c);
            beyond_keep || too_old
        })
        .map(|(_, m)| m.clone())
        .collect()
}

/// Render the prune plan: the selected sessions, and — on a dry run — the
/// reminder that nothing was touched.
pub fn render_prune_plan(
    victims: &[SessionMeta],
    total: usize,
    dry_run: bool,
    now_ms: u64,
) -> String {
    if victims.is_empty() {
        return format!("Nothing to prune — all {total} saved sessions are within the rules.");
    }
    let mut out = format!("{} of {total} saved sessions selected:\n", victims.len());
    for m in victims {
        out.push_str(&format!(
            "  {}  ·  {}  ·  {}\n",
            m.id,
            format_age(now_ms, m.updated_at_ms),
            m.preview
        ));
    }
    if dry_run {
        out.push_str("\nDry run — nothing deleted. Re-run with --yes to delete these sessions.");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionSnapshot;
    use std::path::PathBuf;

    fn meta(id: &str, updated_at_ms: u64) -> SessionMeta {
        SessionMeta {
            id: id.to_string(),
            cwd: PathBuf::from("/p"),
            model: "claude-fable-5".to_string(),
            created_at_ms: updated_at_ms,
            updated_at_ms,
            message_count: 4,
            preview: format!("preview of {id}"),
        }
    }

    const HOUR_MS: u64 = 3_600_000;
    const DAY_MS: u64 = 24 * HOUR_MS;

    #[test]
    fn format_age_bands_and_future_safe() {
        let now = 100 * DAY_MS;
        assert_eq!(format_age(now, now - 30_000), "just now");
        assert_eq!(format_age(now, now - 5 * 60_000), "5m ago");
        assert_eq!(format_age(now, now - 3 * HOUR_MS), "3h ago");
        assert_eq!(format_age(now, now - 12 * DAY_MS), "12d ago");
        // A timestamp from the future must not underflow.
        assert_eq!(format_age(now, now + DAY_MS), "just now");
    }

    #[test]
    fn render_list_empty_points_at_starting_a_session() {
        let card = render_list(&[], 0);
        assert!(card.contains("No saved sessions"));
    }

    #[test]
    fn render_list_shows_id_age_model_and_preview() {
        let now = 10 * DAY_MS;
        let metas = vec![
            meta("new-1", now - HOUR_MS),
            meta("old-2", now - 2 * DAY_MS),
        ];
        let card = render_list(&metas, now);
        assert!(card.contains("2 for this project"));
        assert!(card.contains("new-1"));
        assert!(card.contains("1h ago"));
        assert!(card.contains("old-2"));
        assert!(card.contains("2d ago"));
        assert!(card.contains("claude-fable-5"));
        assert!(card.contains("preview of new-1"));
        assert!(card.contains("tomte sessions show <id>"));
    }

    fn record_with_history(history: Vec<InputItem>) -> SessionRecord {
        SessionRecord {
            meta: meta("s-1", 5 * DAY_MS),
            state: SessionSnapshot::default(),
            history,
        }
    }

    #[test]
    fn transcript_renders_messages_tools_and_omits_outputs() {
        let history = vec![
            InputItem::Message {
                role: "user".to_string(),
                content: vec![
                    MessageContent::InputText {
                        text: "fix the bug".to_string(),
                    },
                    MessageContent::InputImage {
                        image_url: "data:image/png;base64,xxxx".to_string(),
                        detail: None,
                    },
                ],
            },
            InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "run_shell".to_string(),
                arguments: r#"{"command":"cargo test"}"#.to_string(),
            },
            InputItem::FunctionCallOutput {
                call_id: "c1".to_string(),
                output: "a giant wall of test output".to_string(),
                error: false,
                media: Vec::new(),
            },
            InputItem::Message {
                role: "assistant".to_string(),
                content: vec![MessageContent::OutputText {
                    text: "done, tests pass".to_string(),
                }],
            },
        ];
        let md = render_transcript(&record_with_history(history));
        assert!(md.contains("# tomte session s-1"));
        assert!(md.contains("model: `claude-fable-5`"));
        assert!(md.contains("messages: 4 · turns: 1"));
        assert!(md.contains("## You"));
        assert!(md.contains("fix the bug"));
        assert!(md.contains("*(image attached)*"));
        assert!(md.contains("> tool: run_shell — cargo test"));
        assert!(md.contains("## tomte"));
        assert!(md.contains("done, tests pass"));
        // The tool result body never lands in the transcript — only the count.
        assert!(!md.contains("a giant wall of test output"));
        assert!(md.contains("1 tool result omitted"));
    }

    #[test]
    fn transcript_tool_brief_probes_paths_and_handles_bad_args() {
        let history = vec![
            InputItem::FunctionCall {
                call_id: "c1".to_string(),
                name: "edit_file".to_string(),
                arguments: r#"{"file_path":"src/main.rs","old":"a","new":"b"}"#.to_string(),
            },
            InputItem::FunctionCall {
                call_id: "c2".to_string(),
                name: "think".to_string(),
                arguments: "not json at all".to_string(),
            },
        ];
        let md = render_transcript(&record_with_history(history));
        assert!(md.contains("> tool: edit_file — src/main.rs"));
        // Unparseable arguments degrade to the bare tool name, never a panic.
        assert!(md.contains("> tool: think\n"));
    }

    #[test]
    fn transcript_briefs_are_single_line_and_capped() {
        let long = "x".repeat(300);
        let history = vec![InputItem::FunctionCall {
            call_id: "c1".to_string(),
            name: "run_shell".to_string(),
            arguments: serde_json::json!({ "command": format!("echo a\necho {long}") }).to_string(),
        }];
        let md = render_transcript(&record_with_history(history));
        let line = md
            .lines()
            .find(|l| l.starts_with("> tool: run_shell"))
            .expect("tool line rendered");
        assert!(line.contains("echo a echo"), "newlines flattened to spaces");
        assert!(line.chars().count() < 130, "brief capped: {line}");
        assert!(line.ends_with('…'));
    }

    #[test]
    fn plan_prune_keep_selects_beyond_newest_n() {
        let now = 10 * DAY_MS;
        let metas = vec![
            meta("a", now - HOUR_MS),
            meta("b", now - 2 * HOUR_MS),
            meta("c", now - 3 * HOUR_MS),
        ];
        let victims = plan_prune(&metas, Some(2), None, now);
        assert_eq!(
            victims.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["c"]
        );
        // keep ≥ count selects nothing.
        assert!(plan_prune(&metas, Some(3), None, now).is_empty());
    }

    #[test]
    fn plan_prune_older_than_selects_before_cutoff() {
        let now = 100 * DAY_MS;
        let metas = vec![
            meta("fresh", now - DAY_MS),
            meta("stale", now - 31 * DAY_MS),
        ];
        let victims = plan_prune(&metas, None, Some(30), now);
        assert_eq!(
            victims.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["stale"]
        );
    }

    #[test]
    fn plan_prune_rules_union_and_no_rules_selects_nothing() {
        let now = 100 * DAY_MS;
        let metas = vec![
            meta("a", now - DAY_MS),
            meta("b", now - 2 * DAY_MS),
            meta("c", now - 40 * DAY_MS),
        ];
        // keep=2 selects c (beyond newest 2); older-than-35d also selects c —
        // union, not intersection, and no duplicate entries.
        let victims = plan_prune(&metas, Some(2), Some(35), now);
        assert_eq!(
            victims.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["c"]
        );
        // keep=1 + older-than-35d: b from the keep rule, c from both.
        let victims = plan_prune(&metas, Some(1), Some(35), now);
        assert_eq!(
            victims.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        // No rules → nothing selected, ever.
        assert!(plan_prune(&metas, None, None, now).is_empty());
    }

    #[test]
    fn render_prune_plan_dry_run_says_nothing_deleted() {
        let now = 10 * DAY_MS;
        let victims = vec![meta("old", now - 5 * DAY_MS)];
        let dry = render_prune_plan(&victims, 3, true, now);
        assert!(dry.contains("1 of 3 saved sessions selected"));
        assert!(dry.contains("old"));
        assert!(dry.contains("Dry run — nothing deleted"));
        assert!(dry.contains("--yes"));

        let wet = render_prune_plan(&victims, 3, false, now);
        assert!(!wet.contains("Dry run"));

        let none = render_prune_plan(&[], 3, true, now);
        assert!(none.contains("Nothing to prune"));
    }
}

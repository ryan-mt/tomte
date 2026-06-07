//! Pillar 5 — A2 (The Reckoning): the conscience self-check.
//!
//! Pure, provider-agnostic logic for asking the editing model whether a pending
//! edit contradicts a decision recorded for the file, and parsing its one-line
//! verdict. The harness *asks*; the model *answers as itself* (SOUL.md §4), so
//! the judgment survives a mid-session model switch and never special-cases a
//! provider. The wiring (when to fire, the model call, the card) lives in the
//! agent tool phase; the brain is here so it is unit-testable in isolation.
//!
//! Fail-open by design: only a clearly-formed `CONFLICT` blocks. A malformed or
//! garbage answer parses to [`ConscienceVerdict::Clear`] — the conscience must
//! never fail *shut* on a model quirk and wedge an edit.

use crate::decisions::DecisionRecord;

/// The model's verdict on whether a pending edit contradicts a recorded decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConscienceVerdict {
    /// The edit honors the recorded decisions (or is unrelated) — proceed.
    Clear,
    /// The edit contradicts the decision recorded at `ts`; `reason` is the
    /// model's one-line explanation of how.
    Conflict { ts: u64, reason: String },
}

/// How many of a file's decisions to show the model — the same budget the trail
/// injection uses, so a file with a huge trail can't blow up the check prompt.
const MAX_DECISIONS_IN_CHECK: usize = 30;

/// Build the harness-authored self-check prompt: the recorded decisions for the
/// file plus the proposed change, asking for exactly `CLEAR` or
/// `CONFLICT <ts> — <reason>`. Provider-agnostic — no model is named.
pub fn build_check_prompt(file: &str, decisions: &[DecisionRecord], change: &str) -> String {
    let mut s = format!(
        "You are about to edit `{file}`. It has recorded decisions (house rules). \
         Judge ONLY whether the proposed change CONTRADICTS one of them.\n\n\
         Recorded decisions:\n"
    );
    for d in decisions.iter().rev().take(MAX_DECISIONS_IN_CHECK) {
        s.push_str(&format!("  #{} — {} (because {})\n", d.ts, d.decision, d.why));
        for r in &d.rejected {
            s.push_str(&format!("      rejected: {r}\n"));
        }
    }
    s.push_str("\nProposed change:\n");
    s.push_str(change);
    s.push_str(
        "\n\nAnswer on ONE line, no prose:\n\
         - `CLEAR` if the change honors the decisions or is unrelated to them.\n\
         - `CONFLICT <ts> — <one sentence>` if it contradicts one, citing that decision's #ts.\n",
    );
    s
}

/// Parse the model's one-line verdict. Lenient and fail-open: scans for the
/// first line that resolves to `CLEAR` or a well-formed `CONFLICT <ts>`, after
/// stripping leading markdown (`*`, `` ` ``, `-`). Anything else — prose, a
/// `CONFLICT` with no parseable ts, an empty answer — is [`ConscienceVerdict::Clear`].
pub fn parse_check_answer(answer: &str) -> ConscienceVerdict {
    for line in answer.lines() {
        let l = line
            .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
            .trim();
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("clear") {
            return ConscienceVerdict::Clear;
        }
        if let Some(after) = lower.strip_prefix("conflict") {
            // The first integer after CONFLICT is the superseded decision's ts.
            let ts = after
                .split(|c: char| !c.is_ascii_digit())
                .find(|tok| !tok.is_empty())
                .and_then(|tok| tok.parse::<u64>().ok());
            let Some(ts) = ts else {
                // A CONFLICT with no parseable ts is malformed — keep scanning,
                // and fail open to CLEAR if nothing better turns up.
                continue;
            };
            let reason = l
                .split_once('—')
                .or_else(|| l.split_once(" - "))
                .map(|(_, r)| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "contradicts a recorded decision".to_string());
            return ConscienceVerdict::Conflict { ts, reason };
        }
    }
    ConscienceVerdict::Clear
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: u64, decision: &str, why: &str) -> DecisionRecord {
        DecisionRecord {
            loc: "src/auth.rs:10".into(),
            decision: decision.into(),
            why: why.into(),
            rejected: vec!["bcrypt -> not memory-hard".into()],
            model: "gpt-5.5".into(),
            ts,
            anchor: None,
            supersedes: None,
        }
    }

    #[test]
    fn clear_answer_parses_clear() {
        assert_eq!(parse_check_answer("CLEAR"), ConscienceVerdict::Clear);
        assert_eq!(parse_check_answer("clear\n"), ConscienceVerdict::Clear);
        assert_eq!(parse_check_answer("`CLEAR`"), ConscienceVerdict::Clear);
    }

    #[test]
    fn conflict_answer_parses_ts_and_reason() {
        assert_eq!(
            parse_check_answer("CONFLICT 1736000000000 — switches to bcrypt"),
            ConscienceVerdict::Conflict {
                ts: 1_736_000_000_000,
                reason: "switches to bcrypt".into()
            }
        );
        // Plain hyphen separator and markdown wrapping both work.
        assert_eq!(
            parse_check_answer("**CONFLICT** 42 - drops argon2"),
            ConscienceVerdict::Conflict {
                ts: 42,
                reason: "drops argon2".into()
            }
        );
    }

    #[test]
    fn malformed_or_prose_fails_open_to_clear() {
        // Garbage / prose → CLEAR (never fail shut).
        assert_eq!(
            parse_check_answer("I think this is fine, no issues."),
            ConscienceVerdict::Clear
        );
        // A CONFLICT with no ts is malformed → CLEAR.
        assert_eq!(
            parse_check_answer("CONFLICT — but I can't cite which"),
            ConscienceVerdict::Clear
        );
        // Empty answer → CLEAR.
        assert_eq!(parse_check_answer("   "), ConscienceVerdict::Clear);
        // "no conflicts, CLEAR" must not false-positive as a conflict.
        assert_eq!(
            parse_check_answer("No conflicts here — CLEAR"),
            ConscienceVerdict::Clear
        );
    }

    #[test]
    fn prompt_carries_file_decisions_and_change() {
        let decisions = vec![rec(99, "use argon2", "memory-hard")];
        let prompt = build_check_prompt("src/auth.rs", &decisions, "swap argon2 for bcrypt");
        assert!(prompt.contains("src/auth.rs"));
        assert!(prompt.contains("#99"));
        assert!(prompt.contains("use argon2"));
        assert!(prompt.contains("swap argon2 for bcrypt"));
        assert!(prompt.contains("CLEAR"));
        assert!(prompt.contains("CONFLICT"));
    }
}

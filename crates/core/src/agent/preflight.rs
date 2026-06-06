//! SOUL Pillar 1 — the glass-box pre-flight.
//!
//! Before the harness runs a *consequential* tool call — a write or a shell
//! command — it states in one calm line what the call will change and how far
//! it can reach, so an auto-approved action is **legible, not silent**. This is
//! the custodian being followable: you see WHAT it will do and HOW FAR it can
//! reach, first. Purely informational — the approval gate is unchanged (we add
//! visibility, not friction). See docs/SOUL.md (Pillar 1).
//!
//! Read-only tools (read/grep/glob/list/lsp) get no card: they are inherently
//! bounded and already shown calmly, so a "0 writes" card for each would be
//! noise, not legibility. The card is reserved for the actions that actually
//! reach into your tree.

use serde_json::Value;

/// The pre-flight card for one tool call: a one-line scope statement (the blast
/// radius) and an optional leash (a safety note for a flagged-destructive call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreFlightCard {
    /// What it writes and how far it reaches, e.g. "writes 1 file · nothing
    /// else moves" or "read-only · changes nothing in your tree".
    pub scope: String,
    /// A one-line leash when the call is flagged destructive (`danger_reason`),
    /// else `None` — the bound it runs within, shown before it acts.
    pub leash: Option<String>,
}

/// Derive the pre-flight card for a tool call, or `None` when the tool does not
/// warrant one (a read/search). Provider-agnostic and pure, so it is unit-
/// testable without the agent loop.
///
/// `effective_read_only` is the *effective* read-only verdict (so a read-only
/// `run_shell` reads `true`), and `danger` is the tool's `danger_reason` — both
/// computed by the caller from live tool metadata.
pub(crate) fn preflight_card(
    tool: &str,
    args: &Value,
    effective_read_only: bool,
    danger: Option<&str>,
) -> Option<PreFlightCard> {
    let leash = danger.map(str::to_string);
    let scope = match tool {
        "edit_file" | "write_file" => "writes 1 file · nothing else moves".to_string(),
        "multi_edit" => {
            let n = args
                .get("edits")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            if n > 0 {
                format!(
                    "writes 1 file · {n} edit{} · nothing else moves",
                    if n == 1 { "" } else { "s" }
                )
            } else {
                "writes 1 file · nothing else moves".to_string()
            }
        }
        "run_shell" => {
            if effective_read_only {
                "read-only · changes nothing in your tree".to_string()
            } else {
                "runs a shell command · may change your tree".to_string()
            }
        }
        _ => return None,
    };
    Some(PreFlightCard { scope, leash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_and_write_show_one_bounded_file() {
        let card = preflight_card("edit_file", &json!({"path": "src/a.rs"}), false, None).unwrap();
        assert_eq!(card.scope, "writes 1 file · nothing else moves");
        assert_eq!(card.leash, None);

        let card = preflight_card("write_file", &json!({"path": "x"}), false, None).unwrap();
        assert_eq!(card.scope, "writes 1 file · nothing else moves");
    }

    #[test]
    fn multi_edit_counts_its_edits_and_singularizes() {
        let two = json!({"path": "src/a.rs", "edits": [{"a": 1}, {"b": 2}]});
        assert_eq!(
            preflight_card("multi_edit", &two, false, None)
                .unwrap()
                .scope,
            "writes 1 file · 2 edits · nothing else moves"
        );
        let one = json!({"path": "src/a.rs", "edits": [{"a": 1}]});
        assert_eq!(
            preflight_card("multi_edit", &one, false, None)
                .unwrap()
                .scope,
            "writes 1 file · 1 edit · nothing else moves"
        );
        // Missing/empty edits degrade gracefully, never panic.
        let none = json!({"path": "src/a.rs"});
        assert_eq!(
            preflight_card("multi_edit", &none, false, None)
                .unwrap()
                .scope,
            "writes 1 file · nothing else moves"
        );
    }

    #[test]
    fn run_shell_distinguishes_read_only_from_writing() {
        let ro = preflight_card("run_shell", &json!({"command": "ls"}), true, None).unwrap();
        assert_eq!(ro.scope, "read-only · changes nothing in your tree");
        let rw = preflight_card("run_shell", &json!({"command": "mkdir x"}), false, None).unwrap();
        assert_eq!(rw.scope, "runs a shell command · may change your tree");
    }

    #[test]
    fn a_flagged_command_carries_its_leash() {
        let card = preflight_card(
            "run_shell",
            &json!({"command": "rm -rf /etc"}),
            false,
            Some("rm -rf on a critical path"),
        )
        .unwrap();
        assert_eq!(card.leash.as_deref(), Some("rm -rf on a critical path"));
    }

    #[test]
    fn read_only_tools_get_no_card() {
        for tool in ["read_file", "grep", "glob", "list_dir", "lsp", "web_fetch"] {
            assert!(
                preflight_card(tool, &json!({}), true, None).is_none(),
                "{tool} should not warrant a pre-flight card"
            );
        }
    }
}

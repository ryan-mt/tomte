//! Strategy generation for a race — turning `--agents N` / `--models a,b` into a
//! concrete, deterministic line-up of contestants. Pure, so the line-up is
//! unit-tested without spawning anything.

/// How a contestant is steered, beyond its model/effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Solve it well; ensure the tests pass.
    Balanced,
    /// The minimal-patch contestant: smallest, safest change; no public-API
    /// churn; add a regression test; avoid risky shell.
    Conservative,
}

/// One contestant: a label, an optional model/effort override (None → the
/// configured default), and a steering style.
#[derive(Debug, Clone)]
pub struct Strategy {
    pub label: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub style: Style,
}

impl Strategy {
    /// The instruction appended to the task for this contestant — the only place
    /// a strategy shapes the agent, kept explicit so the race stays honest about
    /// what each contestant was told.
    pub fn prompt_suffix(&self) -> &'static str {
        match self.style {
            Style::Balanced => "\n\nWhen you are done, make sure the project's own tests pass.",
            Style::Conservative => {
                "\n\nConstraints: make the smallest, safest change that fixes this. \
                 Prefer a minimal diff, do not change public APIs, and add a regression \
                 test. Avoid risky shell commands. Make sure the project's tests pass."
            }
        }
    }
}

/// Effort tiers cycled across contestants so a line-up on a single model still
/// explores different depths of reasoning.
const EFFORTS: &[&str] = &["high", "medium", "low"];

/// Build `n` contestants. With `models` given, each contestant takes the next
/// model (cycling if `n` exceeds the list); otherwise all use the configured
/// default model. The last contestant in a line-up of two or more is always the
/// Conservative minimal-patch entry — the pitch's "agent-d" — so the field always
/// includes the small-clean-diff strategy a winner often turns out to be.
pub fn build_strategies(n: usize, models: &[String]) -> Vec<Strategy> {
    let n = n.clamp(1, 8);
    (0..n)
        .map(|i| {
            let conservative = n > 1 && i == n - 1;
            Strategy {
                label: format!("agent-{}", label_letter(i)),
                model: if models.is_empty() {
                    None
                } else {
                    Some(models[i % models.len()].clone())
                },
                reasoning: Some(EFFORTS[i % EFFORTS.len()].to_string()),
                style: if conservative {
                    Style::Conservative
                } else {
                    Style::Balanced
                },
            }
        })
        .collect()
}

/// `0 → a`, `1 → b`, … capped at the 8-agent ceiling so it never overflows `z`.
fn label_letter(i: usize) -> char {
    (b'a' + (i as u8)) as char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_lineup_uses_configured_model_and_varies_effort() {
        let s = build_strategies(3, &[]);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].label, "agent-a");
        assert!(s.iter().all(|x| x.model.is_none()));
        assert_eq!(s[0].reasoning.as_deref(), Some("high"));
        assert_eq!(s[1].reasoning.as_deref(), Some("medium"));
        // Last contestant is the conservative minimal-patch entry.
        assert_eq!(s[2].style, Style::Conservative);
        assert_eq!(s[0].style, Style::Balanced);
    }

    #[test]
    fn models_are_assigned_round_robin() {
        let models = vec!["claude-opus-4-8".to_string(), "gpt-5.5".to_string()];
        let s = build_strategies(4, &models);
        assert_eq!(s[0].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s[1].model.as_deref(), Some("gpt-5.5"));
        assert_eq!(s[2].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s[3].model.as_deref(), Some("gpt-5.5"));
        assert_eq!(s[3].style, Style::Conservative);
    }

    #[test]
    fn single_agent_is_not_forced_conservative() {
        let s = build_strategies(1, &[]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].style, Style::Balanced);
    }

    #[test]
    fn count_is_clamped_to_a_sane_range() {
        assert_eq!(build_strategies(0, &[]).len(), 1);
        assert_eq!(build_strategies(99, &[]).len(), 8);
    }

    #[test]
    fn conservative_suffix_asks_for_minimal_diff_and_a_test() {
        let s = build_strategies(2, &[]);
        let suffix = s[1].prompt_suffix();
        assert!(suffix.contains("minimal diff"));
        assert!(suffix.contains("regression"));
    }
}

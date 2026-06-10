use super::*;

/// A decision parsed from the auto-capture self-check, before the harness stamps
/// the model, timestamp, and drift anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedDecision {
    pub loc: String,
    pub decision: String,
    pub why: String,
    pub rejected: Vec<String>,
}

/// Parse the self-check answer into a decision, or `None` when the model said
/// NONE or returned nothing usable. Lenient by design — models vary, so we take
/// the first `{ … }` span and parse that, tolerating surrounding prose or a
/// markdown fence. Returns `None` on any parse failure or a record missing
/// `loc`/`decision`/`why`, so a malformed answer never writes trail litter. Pure
/// and provider-agnostic (no model is special-cased), hence unit-testable.
pub fn parse_captured(answer: &str) -> Option<CapturedDecision> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    if end < start {
        return None;
    }
    #[derive(Deserialize)]
    struct Raw {
        loc: String,
        decision: String,
        why: String,
        #[serde(default, alias = "rejected_alternatives", alias = "alternatives")]
        rejected: Vec<String>,
    }
    let raw: Raw = serde_json::from_str(answer.get(start..=end)?).ok()?;
    let loc = raw.loc.trim().to_string();
    let decision = raw.decision.trim().to_string();
    let why = raw.why.trim().to_string();
    if loc.is_empty() || decision.is_empty() || why.is_empty() {
        return None;
    }
    let rejected = raw
        .rejected
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some(CapturedDecision {
        loc,
        decision,
        why,
        rejected,
    })
}

impl CapturedDecision {
    /// Stamp the live model, a timestamp, and a drift anchor onto a parsed
    /// decision, yielding the record to append — so an auto-captured decision is
    /// indistinguishable from one the `record_decision` tool wrote by hand
    /// (including the `anchor` that lets Drift Watch re-locate it later).
    pub fn into_record(self, cwd: &Path, model: &str) -> DecisionRecord {
        DecisionRecord {
            anchor: capture_anchor(cwd, &self.loc),
            loc: self.loc,
            decision: self.decision,
            why: self.why,
            rejected: self.rejected,
            model: model.to_string(),
            ts: now_ms(),
            supersedes: None,
        }
    }
}

/// Wall-clock epoch milliseconds, for stamping a freshly captured decision.
pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

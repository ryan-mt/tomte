//! The decision trail: a project-scoped, append-only log of *why* the agent
//! made a change — the decision, the reasoning, and the alternatives it
//! rejected — each stamped with the model that decided.
//!
//! It lives beside the memory store (`<config>/projects/<key>/decisions.jsonl`)
//! and reuses memory's project keying. It is a *separate*, structured store
//! rather than a freeform memory note for two reasons:
//! - It is queryable by code location (`tomte why <file:line>`).
//! - Each record carries the model in play, so the reasoning survives a mid-task
//!   model switch — a different vendor inherits the *why*, not a lossy summary.
//!   That cross-model trail is the moat.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One recorded decision: what was chosen, why, what was rejected, and which
/// model decided. Serialized as a single JSON line in `decisions.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// Where the decision lives, e.g. `src/parser.rs:88`.
    pub loc: String,
    /// The choice that was made.
    pub decision: String,
    /// The reasoning behind it.
    pub why: String,
    /// Alternatives considered and dropped (each "alt -> consequence").
    #[serde(default)]
    pub rejected: Vec<String>,
    /// The model that recorded it. Stamped by the harness, not the model.
    pub model: String,
    /// Wall-clock epoch milliseconds the decision was recorded.
    pub ts: u64,
    /// A snapshot of the trimmed source line at `loc` when the decision was
    /// recorded. Lets `reconcile` re-locate the decision after the code moves,
    /// so `tomte why` never cites a line that has drifted. `None` for older
    /// records and for file-only locations (no `:line`). Pillar 5 — Drift Watch.
    #[serde(default)]
    pub anchor: Option<String>,
    /// When this decision overturns an earlier one, the `ts` of the decision it
    /// supersedes — so the trail becomes an audit log of promises kept and
    /// deliberately broken. `None` for an ordinary, non-superseding decision.
    /// Pillar 5 — On the Record (A3).
    #[serde(default)]
    pub supersedes: Option<u64>,
}

mod capture;
mod prompt;
mod reconcile;
mod render;
mod store;

pub use capture::*;
pub use prompt::*;
pub use reconcile::*;
pub use render::*;
pub use store::*;

#[cfg(test)]
mod tests;

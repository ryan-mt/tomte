use super::*;

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call_id: String,
    pub tool_name: String,
    /// Pretty-printed JSON arguments shown inside the modal.
    pub args_json: String,
    /// Optional diff/preview rendered in a second pane (e.g. write_file).
    pub diff_preview: Option<String>,
    /// Highlighted menu option: 0 = allow once, 1 = allow this tool/command in
    /// this project (persisted to .tomte/permissions.json), 2 = deny. Driven
    /// by Up/Down; Enter commits it.
    pub selected: usize,
}

/// Pillar 5 (A2 Tier 2) — a conscience-conflict card: a pending edit the
/// self-check judged to contradict a recorded decision. The human chooses
/// abort / supersede / edit-anyway.
#[derive(Debug, Clone)]
pub struct PendingConscience {
    pub call_id: String,
    pub file: String,
    pub ts: u64,
    pub prev_decision: String,
    pub prev_model: String,
    pub reason: String,
    /// Highlighted option: 0 = abort, 1 = supersede, 2 = edit anyway. Up/Down
    /// moves it; Enter commits.
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct ActiveGoal {
    pub objective: String,
    pub turns_completed: u32,
    pub waiting_for_user: bool,
    pub last_summary: Option<String>,
    pub started_at: std::time::Instant,
    pub started_at_ms: u64,
}

impl ActiveGoal {
    pub fn new(objective: String) -> Self {
        Self {
            objective,
            turns_completed: 0,
            waiting_for_user: false,
            last_summary: None,
            started_at: std::time::Instant::now(),
            started_at_ms: tomte_core::session::now_ms(),
        }
    }

    pub fn elapsed_label(&self) -> String {
        format_goal_elapsed(self.started_at.elapsed())
    }

    pub fn to_session_snapshot(&self) -> SessionGoalSnapshot {
        SessionGoalSnapshot {
            objective: self.objective.clone(),
            turns_completed: self.turns_completed,
            waiting_for_user: self.waiting_for_user,
            last_summary: self.last_summary.clone(),
            started_at_ms: self.started_at_ms,
        }
    }

    pub fn from_session_snapshot(snapshot: SessionGoalSnapshot) -> Self {
        let elapsed = tomte_core::session::now_ms().saturating_sub(snapshot.started_at_ms);
        let started_at = std::time::Instant::now()
            .checked_sub(Duration::from_millis(elapsed))
            .unwrap_or_else(std::time::Instant::now);
        Self {
            objective: snapshot.objective,
            turns_completed: snapshot.turns_completed,
            waiting_for_user: snapshot.waiting_for_user,
            last_summary: snapshot.last_summary,
            started_at,
            started_at_ms: snapshot.started_at_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingGoalReplacement {
    pub objective: String,
}

#[derive(Debug, Clone)]
pub struct PendingPlanExit {
    pub plan: String,
}

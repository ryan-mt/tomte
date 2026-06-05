//! `impl App` methods. Split out of `app`; logic unchanged.

use super::*;

impl App {
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        let config = config::load_for_cwd(&cwd);
        let auth_mode = auth::load_auth()
            .map(|a| auth::effective_mode_with_env(&a))
            .unwrap_or_else(|_| auth_mode_from_env().unwrap_or(AuthMode::None));
        let blocks = vec![Block::Welcome];
        let screen = initial_screen(auth_mode, has_supported_env_key());
        let mut app = Self {
            screen,
            login: LoginScreen::new(),
            blocks,
            input: TextInput::default(),
            busy: false,
            cwd,
            config,
            auth_mode,
            scroll: 0,
            auto_scroll: true,
            jump_to_bottom_hint: None,
            subagents: Vec::new(),
            subagent_rows: Vec::new(),
            status_line: String::new(),
            selection: None,
            last_buffer: None,
            copy_notice: None,
            last_height: 0,
            last_width: 0,
            turn_started_at: None,
            spinner_word: String::new(),
            tokens_used: 0,
            usage_by_model: Vec::new(),
            turn_count: 0,
            last_quota: None,
            pending_images: Vec::new(),
            next_image_num: 1,
            pending_shell_context: Vec::new(),
            hatch: None,
            buddy_pet: None,
            buddy_hidden: false,
            overlay: None,
            chain_to_effort: false,
            message_queue: Vec::new(),
            is_thinking: false,
            expanded_tools: false,
            current_turn: None,
            approval: ApprovalMode::OnRequest,
            require_approval: true,
            auto_approve_edits: false,
            should_exit: false,
            pending_resume_id: None,
            pending_undo: false,
            pending_clear: false,
            pending_compact: false,
            auto_compact_done_this_window: false,
            compacting: false,
            compact_started_at: None,
            compact_done_at: None,
            compact_result_msg: None,
            start_with_resume_picker: false,
            session_todos: Vec::new(),
            todo_completed_at: HashMap::new(),
            show_todos: true,
            chat_render_cache: None,
            pending_approval: None,
            approval_handle: None,
            input_history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            active_goal: None,
            pending_goal_replacement: None,
            pending_plan_exit: None,
            pending_session_save: false,
        };
        // Restore the last-persisted permission mode (Claude Code's
        // `defaultMode`). Overrides the literal defaults above for the three
        // approval fields via the canonical setter.
        app.set_permission_mode(PermissionMode::from_config_str(
            &app.config.default_permission_mode,
        ));
        app
    }

    /// Record a submitted prompt in the input history (skipping a consecutive
    /// duplicate) and reset the browse cursor.
    pub fn record_history(&mut self, text: &str) {
        if self.input_history.last().map(String::as_str) != Some(text) {
            self.input_history.push(text.to_string());
            // Drop the oldest entries once past the cap. Safe here because
            // `history_pos` is reset to `None` right below, so no live cursor
            // can be left pointing at a shifted index.
            if self.input_history.len() > MAX_INPUT_HISTORY {
                let overflow = self.input_history.len() - MAX_INPUT_HISTORY;
                self.input_history.drain(0..overflow);
            }
        }
        self.history_pos = None;
    }

    /// Recall the previous (older) history entry into the composer.
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let target = match self.history_pos {
            None => {
                // Starting to browse — stash the in-progress draft.
                self.history_draft = self.input.buffer.clone();
                self.input_history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(target);
        self.input.set_text(self.input_history[target].clone());
    }

    /// Move toward newer history; past the newest entry restores the draft.
    pub fn history_next(&mut self) {
        let Some(p) = self.history_pos else {
            return;
        };
        if p + 1 < self.input_history.len() {
            self.history_pos = Some(p + 1);
            self.input.set_text(self.input_history[p + 1].clone());
        } else {
            self.history_pos = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.input.set_text(draft);
        }
    }

    /// Drop any active text selection and its copy confirmation.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.copy_notice = None;
    }

    /// Finalize a dragged selection: copy the highlighted text to the clipboard
    /// (read from the frame captured during the drag) and keep the highlight
    /// visible with a brief confirmation until the next action.
    pub fn finish_selection(&mut self, sel: selection::Selection) {
        if let Some(buf) = self.last_buffer.as_ref() {
            let text = selection::extract_text(buf, &sel);
            if !text.is_empty() {
                self.copy_notice = Some(match clipboard::copy_text(&text) {
                    Ok(()) => format!("✂ copied {} chars", text.chars().count()),
                    Err(e) => format!("copy failed: {e}"),
                });
            }
        }
        self.selection = Some(sel);
    }

    pub fn permission_mode(&self) -> PermissionMode {
        if self.approval == ApprovalMode::Plan {
            PermissionMode::Plan
        } else if !self.require_approval {
            PermissionMode::BypassPerms
        } else if self.auto_approve_edits {
            PermissionMode::AcceptEdits
        } else {
            PermissionMode::Default
        }
    }

    pub fn set_permission_mode(&mut self, m: PermissionMode) {
        match m {
            PermissionMode::Plan => {
                self.approval = ApprovalMode::Plan;
                self.require_approval = true;
                self.auto_approve_edits = false;
            }
            PermissionMode::Default => {
                self.approval = ApprovalMode::OnRequest;
                self.require_approval = true;
                self.auto_approve_edits = false;
            }
            PermissionMode::AcceptEdits => {
                self.approval = ApprovalMode::OnRequest;
                self.require_approval = true;
                self.auto_approve_edits = true;
            }
            PermissionMode::BypassPerms => {
                self.approval = ApprovalMode::OnRequest;
                self.require_approval = false;
                self.auto_approve_edits = false;
            }
        }
    }

    /// Whether a deferred operation that locks the agent mutex (resume load,
    /// undo) may run on this iteration. It must NOT run while a turn is streaming
    /// (`busy`) or a compaction is in flight (`compacting`): both hold the agent
    /// mutex, so locking it from the main loop would stall the loop, and a turn
    /// that then fills the bounded `agent_rx` would block on `tx.send` forever —
    /// a hard deadlock. Any new agent-locking deferred op must gate on this.
    pub fn can_run_deferred_agent_op(&self) -> bool {
        !self.compacting && !self.busy
    }

    pub fn open_overlay(&mut self, kind: OverlayKind) {
        let picker = match kind {
            OverlayKind::SlashMenu => Picker::new("commands", picker::slash_commands()),
            OverlayKind::FilePicker => {
                Picker::new("attach file (@)", composer::file_candidates(&self.cwd))
            }
            OverlayKind::ModelPicker => {
                let mut p = Picker::new("select model", picker::models());
                // pre-select current
                if let Some(i) = p.items.iter().position(|it| it.key == self.config.model) {
                    p.selected = i;
                }
                p
            }
            OverlayKind::EffortPicker => {
                let mut p = Picker::new("reasoning effort", picker::efforts(&self.config.model));
                if let Some(i) = p
                    .items
                    .iter()
                    .position(|it| it.key == self.config.reasoning_effort)
                {
                    p.selected = i;
                }
                p
            }
            OverlayKind::VerbosityPicker => {
                let mut p = Picker::new("verbosity", picker::verbosities());
                if let Some(i) = p
                    .items
                    .iter()
                    .position(|it| it.key == self.config.verbosity)
                {
                    p.selected = i;
                }
                p
            }
            OverlayKind::ResumePicker => {
                let metas = tomte_core::session::list(&self.cwd);
                Picker::new("resume session", picker::sessions(&metas))
            }
            OverlayKind::LogoutPicker => {
                Picker::new("log out — pick a credential", picker::logout_targets())
            }
        };
        self.overlay = Some((kind, picker));
    }
}

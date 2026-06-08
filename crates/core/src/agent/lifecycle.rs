//! Split out of `agent` (impl Agent block); logic unchanged.

use super::*;

impl Agent {
    pub fn new(client: LlmClient, config: Config) -> Self {
        Self {
            client,
            registry: Registry::standard(),
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            approval: ApprovalMode::OnRequest,
            history: Vec::new(),
            config,
            system_prompt: default_system_prompt(),
            session: Arc::new(Mutex::new(SessionState::default())),
            hooks: Arc::new(crate::hooks::load()),
            session_id: crate::session::new_session_id(),
            session_created_ms: crate::session::now_ms(),
            pending_approvals: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_conscience: Arc::new(Mutex::new(std::collections::HashMap::new())),
            require_approval: false,
            auto_approve_edits: false,
            non_interactive: false,
            last_input_tokens: 0,
            history_seen_len: 0,
            cost_usage: Vec::new(),
        }
    }

    /// Fold one response's billed tokens into the per-model cost tally.
    pub(super) fn record_cost(&mut self, model: &str, u: &TurnUsage) {
        let entry = match self.cost_usage.iter_mut().find(|e| e.model == model) {
            Some(e) => e,
            None => {
                self.cost_usage.push(crate::session::ModelUsage {
                    model: model.to_string(),
                    ..Default::default()
                });
                self.cost_usage.last_mut().expect("just pushed an entry")
            }
        };
        entry.input_tokens = entry.input_tokens.saturating_add(u.uncached_input);
        entry.output_tokens = entry.output_tokens.saturating_add(u.output);
        entry.cache_read_tokens = entry.cache_read_tokens.saturating_add(u.cache_read);
        entry.cache_write_tokens = entry.cache_write_tokens.saturating_add(u.cache_write);
    }

    pub async fn respond_approval(&self, call_id: &str, granted: bool) {
        let sender = {
            let mut map = self.pending_approvals.lock().await;
            map.remove(call_id)
        };
        if let Some(s) = sender {
            let _ = s.send(granted);
        }
    }

    /// Resolve a pending conscience-conflict card with the human's choice. The
    /// three-valued sibling of [`respond_approval`](Self::respond_approval).
    pub async fn respond_conscience(&self, call_id: &str, choice: ConscienceChoice) {
        let sender = {
            let mut map = self.pending_conscience.lock().await;
            map.remove(call_id)
        };
        if let Some(s) = sender {
            let _ = s.send(choice);
        }
    }

    /// Roll back the most recent file edit on the agent's session undo stack.
    /// Equivalent to the `undo_last_edit` tool but callable directly from the
    /// host (e.g. a `/undo` slash command) without round-tripping the model.
    pub async fn undo_last_edit(&self) -> anyhow::Result<String> {
        use anyhow::{anyhow, Context};
        let mut session = self.session.lock().await;
        let entry = session
            .undo_stack
            .back()
            .cloned()
            .ok_or_else(|| anyhow!("no edits to undo"))?;
        // Mirrors the TOCTOU guard in the `undo_last_edit` tool: refuse to
        // overwrite a file that has been touched since we edited it, so a
        // user's manual changes can't be silently destroyed by /undo.
        if let Some(expected) = entry.post_edit_mtime {
            let meta = std::fs::metadata(&entry.path);
            let current_mtime = meta.as_ref().ok().and_then(|m| m.modified().ok());
            let current_size = meta.as_ref().ok().map(|m| m.len());
            // Mirror the `undo_last_edit` tool exactly: at 1s mtime resolution a
            // same-second external edit can leave mtime unchanged, so the size
            // snapshot is the only signal that catches it. Checking mtime alone
            // here risked silently clobbering such an edit.
            if current_mtime != Some(expected) || current_size != entry.post_edit_size {
                return Err(anyhow!(
                    "refusing to undo {}: file has been modified since the edit",
                    entry.path.display()
                ));
            }
        }
        let message = match entry.original_content {
            Some(content) => {
                // Atomic restore (temp + rename, preserving permissions), matching
                // the undo_last_edit tool and the edit/write tools, so a crash
                // mid-restore can't leave a half-written file and the restored file
                // keeps its original permissions instead of the umask default.
                let tmp = entry
                    .path
                    .with_extension(format!("undo-{}.tmp", crate::tools::fs::rand_suffix()));
                crate::tools::fs::atomic_write_preserving_permissions(&entry.path, &tmp, &content)
                    .await
                    .with_context(|| format!("restore {}", entry.path.display()))?;
                format!("Restored {}", entry.path.display())
            }
            None => {
                tokio::fs::remove_file(&entry.path)
                    .await
                    .with_context(|| format!("remove {}", entry.path.display()))?;
                format!("Removed (was a new file): {}", entry.path.display())
            }
        };
        session.undo_stack.pop_back();
        Ok(message)
    }

    /// Replace this agent's history and identity from a stored session so
    /// `/resume` can pick up exactly where the previous run left off.
    pub fn restore_from(&mut self, record: crate::session::SessionRecord) {
        let mut state = SessionState::default();
        state.todos = record.state.todos;
        // Deliberately do NOT restore `read_files`. A tampered session file could
        // pre-seed it to satisfy write_file/edit_file's read-before-overwrite
        // guard — the runtime staleness snapshots (`read_file_meta`) are empty
        // after resume, so set membership would be the only gate. Start empty so
        // the model must actually read a file this session before overwriting it.
        self.cost_usage = record.state.usage;
        self.session = Arc::new(Mutex::new(state));
        self.history = record.history;
        self.session_id = record.meta.id;
        self.session_created_ms = record.meta.created_at_ms;
    }

    /// Reset the conversation so the next turn starts fresh: drop the history the
    /// model is re-sent and the context-occupancy accounting that drives
    /// `/context` and microcompaction. Backs the `/clear` command — clearing the
    /// transcript UI alone left the full history in context, so the model kept
    /// (and the user kept paying for) everything. The system prompt, tools,
    /// MCP/skill manifest, and the billed `/cost` tally are kept: they are not
    /// conversation turns.
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.history_seen_len = 0;
        self.last_input_tokens = 0;
    }

    /// Append inherited memory files to the system prompt (Codex / Claude Code /
    /// tomte). At most one file per directory (`AGENTS.override.md` >
    /// `AGENTS.md` > `CLAUDE.md`), project scope is limited to the git root
    /// through `cwd`, combined bodies are capped at 32 KiB, and re-applying
    /// replaces the previous block instead of duplicating it.
    pub fn apply_project_memory(&mut self) {
        crate::memory::apply_to_system_prompt(&mut self.system_prompt, &self.cwd);
    }

    /// Re-inject the project's agent-written memory index (`MEMORY.md`, or a
    /// listing of saved notes) so memory saved with the `memory` tool survives
    /// across sessions. No-ops when the store is empty. Like the skill manifest,
    /// it is rebuilt as part of `refresh_system_context`, before that block so a
    /// stray standalone call can't truncate later sections.
    pub fn apply_memory_store(&mut self) {
        crate::tools::memory::apply_store_to_prompt(&mut self.system_prompt, &self.cwd);
    }

    /// Re-inject the project's decision trail (`crate::decisions`) so the
    /// reasoning behind earlier changes survives across sessions AND model
    /// switches — a new or switched-to model inherits the *why*, not a lossy
    /// summary. Marker-block based and idempotent, like `apply_memory_store`;
    /// call it right after that so their blocks stay in a stable order. No-ops
    /// when the trail is empty.
    pub fn apply_decision_trail(&mut self) {
        // Reconcile the trail against the working tree before injecting it, so a
        // decision whose line drifted self-heals and the model inherits the
        // current `file:line` rather than a stale citation presented as authority
        // (the shipped defect this closes). The manual `tomte why --reconcile`
        // path is unchanged; this makes the custodian keep the trail tidy on its
        // own. The report is intentionally dropped here — the heal is the
        // side-effect we want; surfacing it is the CLI's job. Pillar 5 — Drift
        // Watch (A1).
        crate::decisions::reconcile(&self.cwd);
        crate::decisions::apply_trail_to_prompt(&mut self.system_prompt, &self.cwd);
    }

    /// Discover every installed skill (tomte + Claude Code + Codex + project)
    /// and append a compact manifest to the system prompt so the model knows
    /// what playbooks exist and can load any one on demand via the `skill`
    /// tool. Only `name: description` lines go in — bodies are loaded lazily —
    /// and the whole block rides the prompt cache, so even hundreds of skills
    /// cost roughly one line each after the first turn. Idempotent-ish: call
    /// once during setup, after `cwd` is set. No-ops when nothing is installed.
    pub fn apply_skill_manifest(&mut self) {
        let entries = crate::skill::discover(&self.cwd);
        if entries.is_empty() {
            return;
        }
        let count = entries.len();
        // Defang framework block markers a project-supplied SKILL.md name or
        // description might embed, the same as inherited memory content.
        let manifest = crate::memory::neutralize_block_markers(&crate::skill::manifest(&entries));
        self.system_prompt.push_str(&format!(
            "\n\n# Available skills ({count})\n\n\
             These curated playbooks are installed and available. Each is the distilled \
             approach for one kind of task. When a request clearly matches a skill's \
             description, call the `skill` tool with its exact name to load the full \
             instructions, then follow them. Load at most what you need — do not pull in \
            skills speculatively.\n\n{manifest}"
        ));
    }

    /// Rebuild the static instruction prefix after cwd-dependent context
    /// changes. Conversation history and session state are intentionally kept.
    pub fn refresh_system_context(&mut self) {
        self.system_prompt = default_system_prompt();
        self.apply_project_memory();
        self.apply_memory_store();
        self.apply_decision_trail();
        self.apply_skill_manifest();
        // The registry keeps its deferred MCP tools across a refresh, so the
        // rebuilt prompt must re-advertise them; otherwise their schemas stay
        // withheld while the model loses the manifest telling it they exist.
        // No-ops when nothing is deferred.
        self.apply_mcp_tool_manifest();
    }

    /// Build a `SessionRecord` snapshot of the current conversation and the
    /// resumable subset of runtime session state.
    pub async fn to_session_record(&self) -> crate::session::SessionRecord {
        let state = {
            let session = self.session.lock().await;
            let mut read_files = session.read_files.iter().cloned().collect::<Vec<_>>();
            read_files.sort();
            crate::session::SessionSnapshot {
                todos: session.todos.clone(),
                read_files,
                active_goal: None,
                usage: self.cost_usage.clone(),
            }
        };
        crate::session::SessionRecord {
            meta: crate::session::SessionMeta {
                id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                model: self.config.model.clone(),
                created_at_ms: self.session_created_ms,
                updated_at_ms: crate::session::now_ms(),
                message_count: self.history.len(),
                preview: crate::session::derive_preview(&self.history),
            },
            state,
            history: self.history.clone(),
        }
    }

    /// Spawn the MCP servers listed in `settings.json` and register every
    /// discovered tool into this agent's `Registry` under `mcp__<server>__<tool>`.
    /// Best-effort: a misconfigured server logs a warning but does not abort.
    pub async fn load_mcp(&mut self) -> Result<()> {
        let clients = crate::mcp::spawn_all().await;
        let mut mcp_count = 0usize;
        for client in clients {
            for info in client.tools.clone() {
                let adapter = crate::mcp::McpToolAdapter::new(client.clone(), info);
                self.registry.add(Box::new(adapter));
                mcp_count += 1;
            }
        }
        // Past the threshold, defer MCP schemas behind `tool_search` and tell
        // the model what's available via a compact manifest in the prompt.
        if mcp_count > MCP_DEFER_THRESHOLD {
            self.registry.enable_tool_search();
            self.apply_mcp_tool_manifest();
        }
        Ok(())
    }

    /// Append a manifest of deferred MCP tools to the system prompt: one
    /// `name: description` line each, mirroring `apply_skill_manifest`. Only
    /// the deferred tools' schemas are withheld — this block tells the model
    /// they exist and that `tool_search` loads them. No-ops when nothing is
    /// deferred.
    pub fn apply_mcp_tool_manifest(&mut self) {
        let summaries = self.registry.deferred_summaries();
        if summaries.is_empty() {
            return;
        }
        let count = summaries.len();
        let mut manifest = String::new();
        for (name, desc) in &summaries {
            manifest.push_str("- ");
            manifest.push_str(name);
            let one_line = desc.split_whitespace().collect::<Vec<_>>().join(" ");
            let one_line: String = one_line.chars().take(200).collect();
            // The description comes from an MCP server (untrusted); defang any
            // framework block markers it embeds, as the skill/memory paths do.
            let one_line = crate::memory::neutralize_block_markers(&one_line);
            if !one_line.is_empty() {
                manifest.push_str(": ");
                manifest.push_str(&one_line);
            }
            manifest.push('\n');
        }
        self.system_prompt.push_str(&format!(
            "\n\n# Searchable tools ({count})\n\n\
             These MCP tools are connected but their schemas are withheld to save context. \
             They are NOT directly callable yet. When a task needs one, call the `tool_search` \
             tool (e.g. with keywords, or `select:<exact-name>`) to load its schema; it then \
             becomes callable from your next message. Load only what you need.\n\n{manifest}"
        ));
    }

    pub fn push_user_message(&mut self, text: impl Into<String>) {
        self.history.push(InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(text)],
        });
    }

    /// Push a user message with text + image attachments (paths read from disk).
    pub fn push_user_message_with_images(
        &mut self,
        text: String,
        image_paths: &[std::path::PathBuf],
    ) {
        let mut content = vec![MessageContent::text(text)];
        for path in image_paths {
            match std::fs::read(path) {
                Ok(bytes) => {
                    use base64::Engine;
                    let mime = guess_mime(path);
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    content.push(MessageContent::InputImage {
                        image_url: format!("data:{};base64,{}", mime, b64),
                        detail: None,
                    });
                }
                Err(e) => {
                    tracing::warn!(?path, error = %e, "failed to read image attachment");
                    content.push(MessageContent::text(format!(
                        "[image attachment {} could not be read: {}]",
                        path.display(),
                        e
                    )));
                }
            }
        }
        self.history.push(InputItem::Message {
            role: "user".to_string(),
            content,
        });
    }
}

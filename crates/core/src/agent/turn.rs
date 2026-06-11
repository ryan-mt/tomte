//! Split out of `agent` (impl Agent block); logic unchanged.

use super::*;

impl Agent {
    /// Drive one full turn: send the current history, process tool calls until
    /// the model produces final assistant text. Emits events through `tx`.
    ///
    /// Thin wrapper so the `Stop` hook fires on EVERY exit — success or error —
    /// per its documented contract. The inner loop has several early error
    /// returns (idle timeout, stream error, response.failed); firing here covers
    /// all of them instead of only the clean-completion path.
    pub async fn run_turn(&mut self, tx: mpsc::Sender<AgentEvent>) -> Result<()> {
        let result = self.run_turn_inner(tx.clone()).await;
        if let Err(e) = &result {
            let _ = tx
                .send(AgentEvent::Error {
                    message: e.to_string(),
                })
                .await;
        }
        self.hooks.fire_stop().await;
        result
    }

    /// Replace the entire conversation history with one model-generated summary
    /// message, reclaiming context-window space. Provider-agnostic: it operates
    /// on `self.history` before any request is built, so every model benefits.
    ///
    /// On a trivially short history, an empty summary, or a stream error,
    /// `self.history` is left UNTOUCHED and an `Err` is returned. On success
    /// returns the number of history items that were compacted away.
    ///
    /// `focus` is an optional user steer from `/compact <focus>`; when present it
    /// nudges the summary to emphasize that topic without dropping the rest.
    pub async fn compact_history(&mut self, focus: Option<&str>) -> Result<usize> {
        let original_len = self.history.len();
        if !should_compact(original_len) {
            return Err(anyhow::anyhow!(
                "nothing to compact — conversation is already short"
            ));
        }

        // One-off summary request from the CURRENT history plus a summarize
        // instruction. Deliberately built WITHOUT `.with_tools(...)`: the
        // summary turn must not start editing files or running commands.
        let mut input = self.history.clone();
        input.push(InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(compact_prompt(focus))],
        });
        let request = ResponsesRequest::new(self.config.model.clone(), input)
            .with_instructions(self.system_prompt.clone())
            .with_reasoning(self.config.reasoning_effort.clone())
            .with_verbosity(self.config.verbosity.clone());

        let (summary, usage) = self.collect_text(request).await?;
        // The summary turn re-reads the whole history — the most expensive
        // side request the agent makes. Fold it into /cost like any turn.
        if let Some(u) = &usage {
            let model = self.config.model.clone();
            self.record_cost(&model, u);
        }
        if summary.trim().is_empty() {
            return Err(anyhow::anyhow!("compaction produced an empty summary"));
        }

        self.history = compacted_history(&summary);
        // History was replaced by a single summary item, so every checkpoint's
        // `history_index` now points past the end — drop them rather than let
        // `/rewind` truncate to a stale offset.
        self.checkpoints.clear();
        Ok(original_len)
    }

    /// Proactively shed stale tool-output bulk when the last request's context
    /// occupancy crossed [`MICROCOMPACT_PCT`]% of the window. Cheaper and far
    /// less lossy than the full-summary `/compact` fallback (which the TUI fires
    /// at 85%): it keeps every message, reasoning block, and the most recent
    /// tool results, dropping only old, already-acted-on tool outputs — the
    /// bulkiest, lowest-value content. A no-op unless `auto_compact` is on and
    /// we are genuinely near the limit, so it almost never costs a prompt-cache
    /// miss.
    /// Scoped to `history_seen_len` so it can never clear the just-produced batch
    /// of a multi-tool response before the model has been shown those results.
    pub(super) fn microcompact_tool_outputs(&mut self) {
        if !self.config.auto_compact {
            return;
        }
        let limit = self.config.effective_context_limit();
        if limit == 0 || self.last_input_tokens.saturating_mul(100) < limit * MICROCOMPACT_PCT {
            return;
        }
        // Only shed within the prefix the model has already seen; outputs the
        // current turn appended but hasn't sent back yet must stay intact.
        let seen = self.history_seen_len.min(self.history.len());
        let cleared = clear_stale_tool_outputs(
            &mut self.history[..seen],
            MICROCOMPACT_KEEP_RECENT,
            MICROCOMPACT_MIN_OUTPUT_BYTES,
        );
        if cleared > 0 {
            tracing::info!(
                cleared,
                input_tokens = self.last_input_tokens,
                limit,
                "microcompacted stale tool outputs to conserve context"
            );
        }
    }

    /// Last-ditch context relief when a request was already rejected for
    /// overflowing the window: clear every tool output but the two most recent,
    /// regardless of size. Free (no model call, so it cannot itself overflow) and
    /// usually sufficient because tool outputs dominate a long session's context.
    /// Returns whether it actually freed anything — `false` means the bulk is in
    /// messages/reasoning we won't auto-drop, so the caller surfaces the error.
    pub(super) fn emergency_shed_context(&mut self) -> bool {
        clear_stale_tool_outputs(&mut self.history, 2, 0) > 0
    }

    /// Try to recover from a context-overflow rejection without failing the
    /// turn: if recoveries aren't exhausted, the message looks like an overflow,
    /// and shedding stale tool outputs actually frees space, shed and signal a
    /// retry (bumping `recoveries`). Shared by the pre-stream send error and the
    /// mid-stream `Failed`/`Error` paths, so every way a provider surfaces
    /// overflow — a 4xx before the stream, or an error event during it — gets the
    /// same auto-recovery instead of only the pre-stream case.
    pub(super) fn try_recover_overflow(&mut self, message: &str, recoveries: &mut usize) -> bool {
        if *recoveries < MAX_OVERFLOW_RECOVERIES
            && is_context_overflow_message(message)
            && self.emergency_shed_context()
        {
            *recoveries += 1;
            tracing::warn!(
                attempt = *recoveries,
                "context overflow — shed stale tool outputs and retrying turn"
            );
            true
        } else {
            false
        }
    }

    /// Try to fail over to a configured fallback model when the active one is
    /// rate-limited / its provider is overloaded. Returns `true` (and swaps
    /// `self.client`/`self.config.model`, emitting [`AgentEvent::FallbackSwitched`])
    /// when a usable fallback was adopted, so the caller retries the turn; `false`
    /// otherwise (the error then surfaces as today).
    ///
    /// Provider-agnostic by construction: it knows nothing about specific models.
    /// Guards: only a genuine overload error (never a fatal 4xx, refusal, or
    /// context overflow); bounded by [`MAX_FALLBACK_ATTEMPTS`]; each candidate is
    /// built via [`LlmClient::for_config`], which fails for a built-in provider
    /// with no stored credential — such an unusable fallback is skipped rather
    /// than turning a clear rate-limit into a confusing auth error.
    pub(super) async fn try_fail_over(
        &mut self,
        error: &str,
        tried: &mut Vec<String>,
        attempts: &mut usize,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        if *attempts >= MAX_FALLBACK_ATTEMPTS {
            return false;
        }
        // Only an overload/rate-limit warrants switching models — a fatal error
        // (bad request, auth, model-not-found, refusal) or a context overflow
        // would not be helped by another model.
        if !crate::fallback::is_quota_or_overload(error) || is_context_overflow_message(error) {
            return false;
        }
        while let Some(candidate) = crate::fallback::next_fallback(&self.config, tried) {
            tried.push(candidate.clone());
            let mut trial = self.config.clone();
            trial.model = candidate.clone();
            match LlmClient::for_config(&trial).await {
                Ok(client) => {
                    let from = std::mem::replace(&mut self.config.model, candidate.clone());
                    self.client = client;
                    *attempts += 1;
                    tracing::warn!(%from, to = %candidate, "model overloaded — failing over");
                    let _ = tx
                        .send(AgentEvent::FallbackSwitched {
                            from,
                            to: candidate,
                            reason: error.to_string(),
                        })
                        .await;
                    return true;
                }
                // Unbuildable (e.g. a built-in provider with no stored
                // credential) — skip and try the next configured fallback.
                Err(_) => continue,
            }
        }
        false
    }

    /// Drive a request through the streaming path and return the accumulated
    /// assistant text plus the turn's parsed usage (when the provider reported
    /// one). A minimal recv loop for tool-free turns (used by
    /// `compact_history`): it handles only text and terminal events. It does
    /// NOT call `emit_usage`, so the summary turn's large input doesn't re-fire
    /// the 85% context warning while we are in the middle of compacting — but
    /// the usage is still returned so callers can `record_cost` it: these side
    /// turns are billed like any other and must show up in `/cost`.
    pub(super) async fn collect_text(
        &self,
        request: ResponsesRequest,
    ) -> Result<(String, Option<TurnUsage>)> {
        let mut handle = self.client.stream(request).await?;
        let mut text = String::new();
        let mut usage = None;
        loop {
            let recv = tokio::time::timeout(STREAM_IDLE_TIMEOUT, handle.rx.recv()).await;
            let ev = match recv {
                Err(_) => {
                    return Err(anyhow::anyhow!(
                        "stream idle for {}s — connection may be stale, try again",
                        STREAM_IDLE_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(e),
                Ok(Some(Ok(v))) => v,
            };
            match ev {
                ResponseStreamEvent::OutputTextDelta { delta, .. } => {
                    text.push_str(&delta);
                }
                // Fall back to the block's full text only if no deltas arrived
                // (some providers emit Done without deltas); otherwise keep the
                // accumulated deltas.
                ResponseStreamEvent::OutputTextDone { text: t, .. } if text.is_empty() => {
                    text = t;
                }
                ResponseStreamEvent::Completed { response } => {
                    usage = parse_usage(&response);
                    break;
                }
                ResponseStreamEvent::Failed { response } => {
                    let message = response
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("response.failed (no message)")
                        .to_string();
                    return Err(anyhow::anyhow!("response.failed: {message}"));
                }
                ResponseStreamEvent::Error { message } => {
                    return Err(anyhow::anyhow!(message));
                }
                _ => {}
            }
        }
        Ok((text, usage))
    }

    /// After a turn that changed files, ask the model — provider-agnostically —
    /// whether it made a non-obvious decision worth preserving, and if so append
    /// it to the trail. This makes the decision trail self-populating: the *why*
    /// survives without the model having to remember to call `record_decision`
    /// (Pillar 2 — auto-capture).
    ///
    /// Cheap and unobtrusive by construction — it returns early unless every
    /// guard passes:
    /// - a real file edit landed this turn (`turn_mutated`),
    /// - the model did NOT already record a decision itself (`turn_recorded`),
    /// - `auto_capture` is on,
    /// - the run is not unattended-headless, where the replayed trail is a
    ///   prompt-injection vector (the same gate `record_decision` enforces).
    ///
    /// Provider-agnostic: the self-check routes through the active model/client
    /// like any other turn, with no per-model special-casing, so a model added
    /// later works automatically. Fail-open: a self-check error or a `NONE`
    /// answer records nothing and never disturbs the finished turn.
    pub(super) async fn maybe_capture_decision(&mut self, turn_mutated: bool, turn_recorded: bool) {
        if !should_auto_capture(
            self.config.auto_capture,
            self.non_interactive,
            self.require_approval,
            turn_mutated,
            turn_recorded,
        ) {
            return;
        }
        let mut input = self.history.clone();
        input.push(InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(CAPTURE_PROMPT)],
        });
        // Tool-free and low-effort: a cheap classification, not a work turn — it
        // must never start editing, and a routine edit shouldn't pay for deep
        // reasoning. Built like `compact_history` so it streams through the same
        // provider-agnostic path.
        let request = ResponsesRequest::new(self.config.model.clone(), input)
            .with_instructions(self.system_prompt.clone())
            .with_reasoning("low")
            .with_verbosity("low");
        let Ok((answer, usage)) = self.collect_text(request).await else {
            return;
        };
        // The self-check replays the full history as input — billed work, so
        // it must land in /cost even though it never emits usage events.
        if let Some(u) = &usage {
            let model = self.config.model.clone();
            self.record_cost(&model, u);
        }
        let Some(captured) = crate::decisions::parse_captured(&answer) else {
            return;
        };
        let record = captured.into_record(&self.cwd, &self.config.model);
        match crate::decisions::append(&self.cwd, &record) {
            Ok(()) => tracing::info!(
                loc = %record.loc,
                model = %record.model,
                "auto-captured a decision into the trail"
            ),
            Err(e) => tracing::warn!(error = %e, "auto-capture: failed to append decision"),
        }
    }

    /// Pillar 5 (A2 Tier 2) — ask the editing model whether a pending edit to a
    /// file with recorded `decisions` contradicts one. Provider-agnostic (routes
    /// through the active model/client) and fail-open: any self-check error
    /// yields `Clear`, so the conscience never blocks an edit on a model or
    /// transport quirk. A focused, low-effort, tool-free classification call.
    pub(super) async fn conscience_verdict(
        &self,
        file: &str,
        decisions: &[crate::decisions::DecisionRecord],
        change: &str,
    ) -> crate::conscience::ConscienceVerdict {
        let prompt = crate::conscience::build_check_prompt(file, decisions, change);
        let input = vec![InputItem::Message {
            role: "user".to_string(),
            content: vec![MessageContent::text(prompt)],
        }];
        let request = ResponsesRequest::new(self.config.model.clone(), input)
            .with_instructions(crate::conscience::CHECK_INSTRUCTIONS.to_string())
            .with_reasoning("low")
            .with_verbosity("low");
        // Usage is dropped here: this caller is `&self` (the conscience
        // pre-pass borrows the agent shared) and the check is a single short
        // message at low effort — the one side turn /cost still undercounts.
        match self.collect_text(request).await {
            Ok((answer, _)) => crate::conscience::parse_check_answer(&answer),
            Err(_) => crate::conscience::ConscienceVerdict::Clear,
        }
    }
}

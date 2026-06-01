# Changelog

## 0.0.1-beta.3

Stability beta adding proactive context management and hardening streamed tool calls, reasoning portability, and `/goal` limits.

- Added proactive context "microcompaction": when the last request crosses ~75% of the context window, stale already-seen tool-output bulk is shed before the next request (keeping every message and reasoning block intact) — far cheaper and less lossy than a full `/compact`. Scoped to the history the model has already been shown, so a large parallel tool batch's fresh results are never dropped before it reads them.
- Added automatic recovery from a hard context-window overflow: shed stale tool outputs and retry the turn (bounded) instead of failing, covering both a pre-stream rejection and an overflow surfaced as an error mid-stream.
- Tracked context occupancy from provider-reported usage and stopped a failed or usage-less response from zeroing it (which had silently disabled proactive microcompaction on the next turn).
- Fixed streamed tool-call arguments dropping a bare `null`/`{}`/`[]` value mid-object (e.g. a streamed `"limit": null`), which corrupted the accumulated JSON; the leading empty-args placeholder rule now lives in one shared helper across the OpenAI Responses, Chat Completions, and Anthropic paths so they can't drift.
- Added the `forward_reasoning_effort` provider option to forward reasoning effort to OpenAI-compatible Chat Completions endpoints (off by default; `minimal`/`low`/`medium`/`high` pass through, `xhigh`/`max`/`ultracode` clamp to `high`, unknown levels omitted to avoid 400s).
- Dropped provider-foreign reasoning/thinking items before the OpenAI Responses wire, so a `/model` switch or a resumed cross-provider session no longer 400s on a foreign reasoning id.
- Added `OPENCLI_DEBUG_WIRE=1` opt-in wire diagnostics to confirm the reasoning effort and token usage actually sent to the provider.
- Bounded `/goal` objectives by both word and character count, so an overlong objective — including space-free CJK text — can't crowd out the real work on every continuation turn.
- Returned a clear "file not found" error from `read_file` instead of leaking the underlying `stat` syscall name.

## 0.0.1-beta.2

Stability beta focused on tool-call compatibility, goal recovery, and credential/model correctness.

- Fixed OpenAI strict tool schemas so `dispatch_agent` and optional tool fields satisfy required/nullable schema rules.
- Hardened OpenAI Chat Completions, OpenAI Responses, and Anthropic stream parsing across text, reasoning, and tool-call delta aliases.
- Limited ChatGPT/Codex OAuth model catalogues to verified OAuth-backed models while keeping full OpenAI catalogues for API-key mode.
- Added credential coverage reporting for OpenAI OAuth/API key and Anthropic OAuth/API key without exposing secrets.
- Preserved OAuth credentials when saving API keys, so multiple credential types can coexist and active mode can switch cleanly.
- Normalized `provider/model` config inputs consistently across config, chat, and TUI slash commands.
- Paused active `/goal` runs on provider/tool errors instead of silently continuing from a broken turn.
- Kept JSON chat output clean by suppressing stderr tracing in `json` and `stream-json` modes.
- Added local release packaging/smoke scripts, CI smoke coverage, and release artifact checksums for repeatable beta verification.

## 0.0.1-beta.1

Initial beta release candidate.

- Added the interactive TUI, headless chat, login/status/config/resume flows, and session persistence.
- Added `/goal` with active-goal continuation, replacement confirmation, footer elapsed timer, and `goal_update`.
- Added Claude-style todo UI and `todo_write` compatibility.
- Added plan-mode controls (`enter_plan_mode`, `exit_plan_mode`) with approval gating.
- Added sub-agent dispatch compatible with Claude Code agent definitions and common Task/Agent argument aliases.
- Hardened tool calling across provider shapes, streamed argument recovery, output caps, parallel read-only calls, and common schema aliases.
- Added filesystem/search/shell/web/notebook tools with undo, permission checks, hook matching, and safer destructive command handling.
- Added OpenAI and Anthropic provider adapters, model catalog handling, retry behavior, and reasoning/thinking translation support.
- Added CI for Linux, macOS, and Windows plus release artifact builds.

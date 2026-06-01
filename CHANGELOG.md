# Changelog

## 0.0.1-beta.4

Beta 4 focuses on making long agent sessions easier to run, inspect, and recover: better context/quota visibility, safer file and shell behavior, more Claude Code-compatible tools, and release-ready TUI polish.

### Highlights

- Added daemon-free code intelligence with the `lsp` tool: document/workspace symbols, go-to-definition, references, and hover for Rust, TypeScript/JavaScript, Python, and Go.
- Added isolated git worktrees through `enter_worktree`/`exit_worktree` plus `/worktree create [name]` and `/worktree exit keep|remove [--discard]` in the TUI.
- Added `/usage` for live provider quota/rate-limit status, separate from `/cost`'s local token tally and USD estimate.
- Added `/context` (`/ctx`) to show real context-window usage plus a rough breakdown of where the visible conversation is spending tokens.
- Added `tool_search` for progressive MCP tool disclosure when many MCP tools are connected, reducing per-request schema bloat.

### New tools and commands

- Added `wait`, a non-blocking 1–120s sleep for poll-and-wait loops that does not occupy a foreground shell slot.
- Added task dependencies to `todo_write` with `id` and `blockedBy`, including unblocked-task summaries and dimmed blocked items in the live todo panel.
- Added stale-file guards to `edit_file`, `multi_edit`, and `write_file` so writes are refused when the file changed after the model last read it.
- Added `/commit` and `/commit-push-pr` slash commands with a git safety protocol, Conventional Commit generation, optional branch push, and PR creation via `gh`.
- Added `opencli run` as an alias for headless `chat`, plus `--cwd` and `--prompt-file` for cron/systemd-style scheduled runs.

### Reliability and recovery

- Hardened OpenAI Responses, OpenAI Chat Completions, and Anthropic streaming against early SSE connection drops. Pre-output drops retry the turn; drops after usable text or a completed tool call finalize the streamed work, while incomplete tool calls are skipped instead of executing partial arguments.
- Captured provider quota from response headers and Codex `codex.rate_limits` events without extra API calls, covering ChatGPT/Codex OAuth, OpenAI API keys, Claude OAuth, and Anthropic API keys.
- Increased `dispatch_agent`'s outer hard timeout independently of ordinary tools, so long repo-audit subagents are not cut off by the default per-tool cap.
- Kept deferred MCP tools available after working-directory changes mid-session.
- Removed the staging temp file when an atomic file write fails after the temp is created (partial write, permission error, or cross-device/EISDIR rename), so failed `write_file`/`edit_file`/`multi_edit` operations no longer leave stray `.tmp` siblings behind.
- Stopped the `lsp` workspace-symbol walk from following directory symlinks and capped its recursion depth, so a symlink cycle in the project tree can no longer recurse until the stack overflows.
- Bounded `grep` stdout capture at ~4 MiB and killed the search process on overrun, so a pattern matching every line of a giant minified file can no longer balloon memory before the output cap trims it.
- Showed both consumed and remaining quota in `/usage` (`12.5% used (87.5% left)`), so a percentage reads unambiguously whether the provider's native UI counts up (Claude utilization) or down (ChatGPT/Codex remaining). All providers are still normalized to one "used" convention internally; this only clarifies the display.
- Capped the per-stream tool-call and content-block accumulators (orphan argument buffers in the Responses path, Anthropic content blocks, and Chat Completions tool calls), so a malformed stream emitting unboundedly many distinct ids/indices can no longer grow memory during a single turn.
- Bounded the in-memory composer recall history (Up/Down) at 1000 entries so a multi-day session can't grow it without limit.

### Safety and security

- Hardened `dispatch_agent` cwd overrides: child cwd values are canonicalized and must stay under the parent session cwd.
- Hardened `run_shell` destructive-command detection for absolute program paths such as `/bin/rm`, `/usr/bin/git`, `/sbin/mkfs.*`, and `/usr/bin/curl | /bin/sh`.
- Wrote `config.json` with owner-only permissions on Unix and redacted provider API keys from `config --show`.
- Preserved sub-agent approval safety: when nested approvals cannot be surfaced, sub-agents run in enforced plan mode rather than silently bypassing review.

### TUI polish and fixes

- Improved `run_shell` rendering to match Claude Code more closely: red stderr, no separator box, compact `Error (exit N)` footer, and more failed-command output kept inline.
- Fixed long-session TUI garbling by sanitizing ANSI codes, tabs, and carriage returns before rendering tool output.
- Fixed a plan-approval lockout where typing a follow-up while the agent was planning could make `Y` stop approving the plan.
- Updated README and in-app slash command discovery for the beta4 release line and `/usage`.

### Claude Code and Codex interoperability

- Expanded inherited memory loading to include global instruction files from `$CODEX_HOME` / `~/.codex`, `~/.claude`, and `~/.config/opencli`, then the git repository root through the session `cwd` (ancestor-first, closest directory last in the prompt).
- Fixed inherited memory to match Codex-style discovery: at most one file per directory (`AGENTS.override.md` > `AGENTS.md` > `CLAUDE.md`), stop at the git root instead of the filesystem root, cap combined bodies at 32 KiB, and replace the previous memory block on re-apply instead of duplicating it. Fixed candidate-file iteration so a missing `AGENTS.override.md` no longer prevented falling through to `AGENTS.md` / `CLAUDE.md`.
- Extended skill discovery to project `.codex/skills/`, `$CODEX_HOME/skills` / `~/.codex/skills`, and recursive search under Claude/Codex `plugins/` trees (deduplicated roots).
- Extended sub-agent discovery to project `.codex/agents/`, `~/.codex/agents`, and `$CODEX_HOME/agents` (deduplicated roots).
- Updated `dispatch_agent` tool copy, the default system prompt, and TUI `/agents` / `/skills` empty-state messages to document the expanded discovery paths.
- Added tests for git-scoped walk-up ordering, `AGENTS.override.md` precedence, repo-boundary exclusion, idempotent re-apply, the 32 KiB cap, and Codex/plugin skill and sub-agent roots.

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

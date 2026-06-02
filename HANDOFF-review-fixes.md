# Handoff — review-driven stability fixes

**Branch:** `fix/review-findings` (base: `main`)
**Ultimate goal:** make opencli **stable**. Every change is a real, verified bug fix.
**Started from:** two reviews in one session — (1) a 14-agent whole-codebase bug hunt, (2) a `bao-review` of the uncommitted `@`/`!`/`#` composer feature.

## Working conventions (MUST follow — from the user)
- **Fix all listed bugs.** Don't leave any out.
- **One commit per bug**, message `fix(area): …` (or `feat`/`refactor`), explaining the bug + the fix.
- Add a **CHANGELOG.md** bullet under `## 0.0.2 → ### Fixed` for each fix.
- Add a **regression test** whenever feasible (prefer a small pure helper so it's testable).
- **Don't grow giant files or cram one folder** — put net-new code in a focused module (we extracted the composer into `crates/cli/src/tui/composer.rs` as the first example). `app.rs` (~5.1k lines) and `agent/mod.rs` (~5.4k) are the worst offenders.
- **Ground model/API facts in current docs** (use WebFetch/WebSearch). Verified June 2026: newest Claude is **Opus 4.8** (no 4.9); Opus 4.8/4.7/4.6 + Sonnet 4.6 = 1M context, Haiku 4.5 = 200K. `catalog.rs` matches.
- **Verify after every change:** `cargo test --workspace` must stay green (currently ~632 tests, 0 failures, 0 warnings).

---

## DONE (24 commits on this branch, oldest→newest)

Composer feature (was uncommitted working tree) + its review fixes:
| commit | what |
|---|---|
| `64d3d6c` | feat: `@`/`!`/`#` composer prefixes; bump to 0.0.2 (baseline) |
| `280848d` | **B1** `!`-shell ran inline on the event loop → froze TUI on long/non-terminating cmds. Now background task + 120s timeout + kill_on_drop. |
| `c314957` | **B2** `@`-mention could read outside cwd / absolute paths. Now canonicalized + confined to cwd. |
| `6c3ff86` | **B3** `@`-expansion scanned the prepended `!` shell output too. Now scans the user prompt only. |
| `aa2487d` | **B4** `@`-picker ran `rg --files` buffering all stdout. Now streams + caps 5000 + kills rg early. |
| `044108e` | **B5** `#`-note glued onto last line of a no-trailing-newline CLAUDE.md. Now `claude_md_note_block` adds a separator. |
| `ee9cfee` | **B6** `!`/`#` typed while busy were sent to the model verbatim. Now dispatched as commands in the queue flush. |
| `829870f` | refactor: extracted the composer feature out of `app.rs` into `tui/composer.rs` (pure move). |

Whole-codebase audit fixes:
| commit | what |
|---|---|
| `78ff05d` | **A1** Resume-picker/`/undo` while a turn streamed locked the agent mutex → hard deadlock. Gated both on `App::can_run_deferred_agent_op()` (`!busy && !compacting`). |
| `f50067a` | **A2** `run_shell` perm rules keyed on the first word only → `deny(rm:*)` bypassed by `sudo rm`/`x; rm`, `allow(cargo:*)` auto-ran `cargo build; curl|sh`. Now splits on shell operators, peels wrappers, deny=any-segment, allow=clean-command-only. |
| `dfbe976` | **A5** Deny glob matched the raw path → `./.git/config` etc. bypassed `deny(.git/**)`. Now `normalize_rule_path` (drop `./`, collapse `//`, resolve `..` safely). |
| `6834778` | **A3** PostToolUse hook deadlocked: stdin written before readers spawned, outside the timeout, payload > pipe buffer. Now readers spawned first + stdin on its own task. |
| `0dacd97` | **A6** Foreground `run_shell` drained stdout/stderr *after* the timeout select → grandchild holding the pipe hung it. Now wait+drain wrapped in one timeout, kills the group on expiry. |
| `f42d8d7` | **A9** Background shells leaked as orphans on exit. Added `pid` to `BackgroundShellState` + `Drop for SessionState` that SIGKILLs the group. |
| `7111b59` | **A7** `notebook_edit` had no read-before-edit/staleness guard (could clobber unseen cells) and deleted by numeric index. Now mirrors `edit_file` guard; `delete` refuses index fallback. |
| `22fbbee` | **A8** `family_supports_1m` used fixed substrings while adaptive/xhigh are version-gated → a future Opus/Sonnet got 200K and auto-compacted early. Now version-gated; `claude_version` ignores a date as "minor". |
| `26852f5` | **A4** Manual Anthropic OAuth login never verified the returned `state` (CSRF). Now `check_returned_state` compares the pasted `code#state`. |
| `2490cae` | **A10** OpenAI Chat `finish_reason: content_filter` was reported as a clean Completed. Now surfaced as an error (like the Responses path). |
| `787dc17` | **A12** `Retry-After` only parsed delta-seconds. Now also HTTP-date (`parse_retry_after`). |
| `ac9c86a` | **A21** Backoff had no jitter (thundering herd across sub-agents). Added ≤+25% jitter (`jittered`). |
| `968776b` | **A11** Non-streaming `create()` had no total timeout → could hang forever. Added 300s `.timeout()` (OpenAI + Anthropic); streaming stays long-lived. |
| `e0e3f63` | **A14** Config dir created with default umask (0o755). Added `config::create_dir_secure` (0o700 + repairs existing). |
| `280d20b` | **A17** Paste inserted char-by-char (O(n²)). Added `TextInput::insert_str`. |
| `0cde459` | **A16** `render_fleet` registered click hit-rects for off-screen sub-agent rows (clicks on the input box toggled hidden agents). Now stops at the panel height. |

---

## REMAINING (not yet fixed — pick up here)

### Needs a product decision
- **A13 — `keyring` dependency declared but unused.** `grep -rn keyring --include=*.rs` = 0 hits; tokens are stored plaintext in `~/.config/<app>/auth.json` (mode 0o600, atomic write — sound). Declared in workspace `Cargo.toml` and `crates/core/Cargo.toml` with `features = ["sync-secret-service","vendored"]`.
  - **Decide:** (a) **remove** the unused dep (surgical; less attack surface/build cost; 0o600 file storage is already fine — what `gh`/`aws`/`gcloud` do), or (b) **implement** keyring-backed storage with a 0o600 file fallback (feature work, platform-dependent). Ask the user before doing either.

### Should fix (MEDIUM)
- **A15 — `crates/core/src/anthropic/stream.rs` two stream gaps:**
  1. **`redacted_thinking` block dropped** (~`stream.rs:293-313`). A `redacted_thinking` `content_block_start` carries an encrypted `data` payload (not via deltas); the code stores only `kind` and emits `ReasoningDone` only if signature/text is non-empty → both empty → block lost. `ContentBlock::Thinking` (`models.rs:~48`) has no field for redacted data. **Fix:** capture `content_block.data` at block-start; add a `RedactedThinking { data }` content block (or carry `data` through `ReasoningDone`) so a redacted-thinking-then-tool turn replays verbatim instead of breaking continuity.
  2. **`message_delta.stop_reason` dropped** (~`stream.rs:430-454`). Handler reads only `usage`; the `Completed` response carries no `stop_reason`, so `max_tokens` truncation / `refusal` is indistinguishable from `end_turn`. Latent (no consumer today). **Fix:** thread `stop_reason` into the `Completed` JSON so the agent can detect truncation/refusal.

### Should fix (LOW, but stability/correctness)
- **A18 — `crates/cli/src/commands/login.rs:151` `prompt_secret` missing `KeyEventKind::Press` filter.** On terminals that emit Press+Release/Repeat (Windows console; kitty enhancement) a typed key can double, corrupting a pasted/typed API key. The TUI filters this everywhere; this fn doesn't. **Fix:** in the `event::read()` loop, `if k.kind != KeyEventKind::Press { continue; }`.
- **A19 — `crates/cli/src/commands/login.rs:147-166` `prompt_secret` leaves the terminal in raw mode on panic.** No RAII guard / panic hook (unlike the TUI) between `enable_raw_mode()` and `disable_raw_mode()`. A panic in `event::read()` returns the shell raw (no echo). **Fix:** a small RAII guard struct whose `Drop` calls `disable_raw_mode()`. (Can share one commit with A18 since same fn, or split.)
- **A20 — `crates/core/src/tools/web.rs:319-355` `is_blocked_ip` SSRF gap.** Only IPv4-mapped (`::ffff:x.x.x.x`) is re-checked against v4 rules; **IPv4-compatible** (`::127.0.0.1`) and **NAT64** (`64:ff9b::7f00:1`) embedding internal IPv4 are NOT blocked. **Fix:** also handle `v6.to_ipv4_compatible()` (or decode the last 2 segments when `seg[0..6]==0`) and block the `64:ff9b::/96` prefix. (Known separate caveat: DNS-rebinding — only the first host is checked.)
- **A22 — `crates/cli/src/tui/buddy.rs:359-365` `roll_weighted` panics if a rarity tier is empty** (`tier.len()-1` underflows `usize` → `tier[huge]`). Not currently reachable (`roll_rarity` always returns a non-empty tier; guarded by a test), but fragile if a rarity is added without a pet. **Fix:** `tier.get(pick).copied().unwrap_or(0)` or early-return.
- **A23 — `crates/cli/src/tui/login.rs:118-122` + `289-303` OAuth callback race.** Esc in `WaitingForBrowser` sets stage=`PickMode`, but the spawned `start_chatgpt` task keeps running; if it completes after Esc it overwrites the stage (unexpected `Success`, or shows an abandoned-flow error). **Fix:** only apply the task result if the stage is still `WaitingForBrowser` (a generation counter / cancel flag).
- **A24 — `crates/core/src/agent/mod.rs:~1126` orphan arg-buffer cap is by count, not bytes.** `MAX_ORPHAN_ARG_BUFFERS = 256` bounds the number of distinct item-id buffers, each up to `MAX_TOOL_ARGUMENT_BYTES` (2 MiB) → ~512 MiB theoretical on a malformed stream. **Fix:** track total bytes across orphan buffers and stop accumulating past ~8–16 MiB.
- **A25 — `crates/core/src/tools/shell.rs:~95` `classify_danger` redirect-to-device gap** (this also backs the composer `!!` guard, B7). The redirect check matches only whitespace-separated `>`, so `echo x >/dev/sda` (no space) isn't flagged. **Fix:** strip a leading `>`/`>>` from each token (or regex `>>?\s*/dev/(sd|nvme|hd)\S*`) before matching. It's defense-in-depth, not a sandbox.

### Noted by the audit, low value / latent (decide whether to bother)
- `agent/mod.rs` — on a `Failed` stream event, `emit_usage` may fire `AutoCompactSuggested`/`ContextWarning` from a failed response (UI-cosmetic only).
- `openai/chat.rs` — finalized tool calls emit `output_index: 0` (harmless; agent matches by `call_id`); unindexed continuation tool-call deltas are matched by positional slot (latent hazard for a provider that omits `index` and reorders).
- `tools/search.rs:631-635` — `normalize_search_output_line` may miss a backslash after the first `-` (Windows-only, cosmetic).

---

## Verified NOT bugs (don't re-investigate)
`agent/mod.rs` had no panic/lock-across-await/infinite-loop issues; PKCE/S256/RNG in auth are correct; retry never re-sends a consumed stream; `tools/fs.rs` symlink/`..` guards are sound; MCP id-matching & process-group kill are correct; session id uniqueness + atomic write are correct; `lsp.rs` is a text-scanner (no real LSP/JSON-RPC); tool dispatch/`tool_args` parsing is well-guarded. (Full reasoning is in the session transcript.)

## How to continue
1. `git checkout fix/review-findings`
2. Pick the next item above; read the cited `file:line` with full context first.
3. Fix → add a regression test → `cargo test --workspace` (or `-p` the crate) → CHANGELOG bullet → one focused commit.
4. Keep new code in focused modules; verify model/API facts against current docs.

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

## REMAINING — ALL DONE ✅ (completed 2026-06-01, session 2)

Every audit item below is now fixed, each its own commit + CHANGELOG bullet + regression test where feasible. Workspace stays green: **648 tests, 0 fail, `clippy -D warnings` clean, release build OK**.

| item | commit | what shipped |
|---|---|---|
| (clippy) | `33a94bd` | `permissions.rs` char-array split (silence clippy 1.95 `manual_pattern_char_comparison`). |
| **A15.1** | `4b6fbf4` | redacted_thinking blocks captured (`data` at block-start) + replayed via new `RedactedThinking` event → `InputItem::Reasoning.redacted_thinking` → `ContentBlock::RedactedThinking`. Stream + translate tests. |
| **A15.2** | `022c0cf` | `message_delta.stop_reason` captured; a `refusal` stop now surfaces as an error (like OpenAI content_filter) instead of a silent empty turn. Test. |
| **A18+A19** | `960066d` | `prompt_secret`: `KeyEventKind::Press` filter + RAII guard restoring cooked mode on panic. |
| **A20** | `7dfee31` | SSRF: block IPv6 literals embedding internal IPv4 via IPv4-compatible (`::127.0.0.1`) + NAT64 (`64:ff9b::/96`); reads (`cat /dev/sda`) still allowed. Test. |
| **A22** | `c8f437b` | `/buddy roll_weighted` empty-tier underflow → pure `pick_from_tier` helper falling back to pet 0. Test (distribution unchanged). |
| **A23** | `d35609e` | ChatGPT OAuth callback race: per-flow generation counter (bumped on Esc/Ctrl+C/new flow); `finish_chatgpt` drops a stale result. Tests. |
| **A24** | `746b912` | Orphan tool-arg buffers bounded by aggregate bytes (16 MiB), not just count; gate covers the args-`done` path too. Pure-helper test. |
| **A25** | `b5e0cbe` | `classify_danger` flags `>`/`>>` glued to a block device (`echo x >/dev/sda`). Tests. |
| **A13** | `1dd34be` | **Decision: REMOVE** (user chose, 2026-06-01). Dropped the unused `keyring` dep (0 code refs); auth.json 0o600 + atomic write is retained (gh/aws/gcloud-style). Smaller build + attack surface; whole subtree leaves Cargo.lock. |

### Also shipped this session (user feature requests, not from the audit)
- `b288ad9` **feat** — left-drag text selection + clipboard copy, no Shift needed (new `tui/selection.rs`, `clipboard::copy_text`, mouse Down/Drag/Up wiring, highlight overlay). Pure geometry unit-tested.
- `f360041` **docs** — composer ↑/↓ history recall already existed & works (verified by a new test); added it + left-drag selection to the `/help` keyboard-shortcuts list so they're discoverable.

### Noted by the audit, low value / latent (decide whether to bother)
- `agent/mod.rs` — on a `Failed` stream event, `emit_usage` may fire `AutoCompactSuggested`/`ContextWarning` from a failed response (UI-cosmetic only).
- `openai/chat.rs` — finalized tool calls emit `output_index: 0` (harmless; agent matches by `call_id`); unindexed continuation tool-call deltas are matched by positional slot (latent hazard for a provider that omits `index` and reorders).
- `tools/search.rs:631-635` — `normalize_search_output_line` may miss a backslash after the first `-` (Windows-only, cosmetic).

---

## Verified NOT bugs (don't re-investigate)
`agent/mod.rs` had no panic/lock-across-await/infinite-loop issues; PKCE/S256/RNG in auth are correct; retry never re-sends a consumed stream; `tools/fs.rs` symlink/`..` guards are sound; MCP id-matching & process-group kill are correct; session id uniqueness + atomic write are correct; `lsp.rs` is a text-scanner (no real LSP/JSON-RPC); tool dispatch/`tool_args` parsing is well-guarded. (Full reasoning is in the session transcript.)

## How to continue
The audit backlog is **cleared**. Branch `fix/review-findings` is ready to merge into `main` (38 commits ahead). Open follow-ups, if desired:
- The 3 "low value / latent" notes above (UI-cosmetic / Windows-only / latent) — only if a real symptom shows up.
- Merge this branch to `main` (the beta4 work already lives in `main`).
- New work resumes from the `project_opencli_100_goal` track (UI polish, restored-src mining).

Conventions if you reopen: read the cited `file:line` with full context first; fix → regression test → `cargo test --workspace` → CHANGELOG bullet → one focused commit; keep new code in focused modules; verify model/API facts against current docs.

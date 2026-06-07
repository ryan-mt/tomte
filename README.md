<div align="center">

# `tomte`

**A calm, multi-model coding agent for your terminal.**

Rust-fast · runs any model · quiet and surgical — and it hatches a pixel companion.

`0.0.2` · MIT · built in 🦀 Rust

</div>

---

One binary. Point it at OpenAI or Anthropic, drop it into any repo, and it reads, writes,
runs, searches, and *reasons* its way through real work — streaming, with a full tool belt
and a terminal UI that stays out of the way.

```bash
tomte            # open the TUI and start working
tomte chat "explain what this repo does, then add a test for the parser"
```

## Why you might like it

- **No daemon, no ceremony.** A single `tomte` binary. Launch the TUI, or fire a one-shot
  from a script — same agent either way.
- **Bring your own brain.** Sign in with a ChatGPT or Claude subscription (OAuth) *or* drop in
  an API key. Switch models mid-session with `/model`.
- **A real tool belt, not a toy.** Files, shell, search, web, notebooks, sub-agents, todos,
  plan mode, persistent memory — 27 tools, streamed and run in parallel where it's safe.
- **Code intelligence, zero setup.** The `lsp` tool gives you symbols, go-to-definition,
  references, and hover for Rust, TypeScript/JavaScript, Python, and Go — no language server
  to install.
- **Experiment without fear.** `enter_worktree` spins the session into an isolated git
  worktree; `exit_worktree` cleans it up after a safety check so you never clobber main.
- **Knows what it's spending.** `/usage` reads your provider's live quota, `/cost` tallies
  tokens and dollars, `/context` shows where the window is going.
- **Remembers *why*, across models.** `record_decision` logs the reasoning behind a non-obvious
  change to a decision trail that's re-injected every session, so a later session — or a
  different model — inherits *why* it was done that way. Read it back with `tomte why <loc>`,
  `tomte blame <file>`, or `/why`.
- **Tells you before it acts.** A glass-box pre-flight states what a write or shell command will
  change and how far it reaches before it runs, and a file's recorded decisions surface as house
  rules so the agent re-reads its own constraints before it can break one.

## 60-second start

```bash
git clone https://github.com/ryan-mt/tomte && cd tomte
make install         # build --release + link to ~/.local/bin/tomte
tomte login        # sign in (opens a browser for OAuth)
tomte              # launch the TUI
```

Prefer a prebuilt binary? Grab the archive for your platform from the
[latest release](https://github.com/ryan-mt/tomte/releases) and put `tomte`
(or `tomte.exe`) on your `PATH`:

| Platform | Archive |
| --- | --- |
| Linux x86-64 | `tomte-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Intel | `tomte-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `tomte-aarch64-apple-darwin.tar.gz` |
| Windows x86-64 | `tomte-x86_64-pc-windows-msvc.zip` |

## Sign in your way

Four doors in — use a subscription or an API key, OpenAI or Anthropic:

```bash
tomte login                                   # OpenAI OAuth (ChatGPT Plus/Pro/Team/Enterprise)
tomte login --api-key --provider openai       # paste an OpenAI API key
tomte login --api-key --provider anthropic    # paste an Anthropic API key
tomte status                                   # who am I, and on what plan?
tomte doctor                                   # diagnose setup (auth, config, model, MCP, tools)
tomte logout
```

Anthropic OAuth (Claude Pro/Max) is available after you acknowledge the ToS notice.
Environment keys (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`) are picked up automatically.

OAuth uses PKCE with the callback `http://localhost:1455/auth/callback`. Tokens land in
`$XDG_CONFIG_HOME/tomte/auth.json` with owner-only permissions on Unix and refresh themselves
before they expire. Non-Unix builds refuse to persist credentials until owner-only storage can
be enforced there.

## Two ways to talk to it

**Interactive — the TUI** (the default):

```bash
tomte              # full terminal UI
tomte resume       # reopen with the session picker
```

**Headless — one-shot or piped**, perfect for scripts, cron, and `systemd`:

```bash
tomte chat "write a fibonacci function in Python"
tomte chat --model gpt-5.5-pro --reasoning high "refactor module X"
echo "read CLAUDE.md and summarize" | tomte chat

tomte run --cwd /srv/project --prompt-file nightly-task.md   # scheduler-friendly alias
```

## The tool belt

The model can reach for any of these — streamed, schema-validated, and executed in parallel
when read-only:

| Group | Tools |
| --- | --- |
| **Files** | `read_file` · `write_file` · `edit_file` · `multi_edit` · `list_dir` |
| **Search** | `grep` · `glob` · `lsp` |
| **Shell** | `run_shell` · `bash_output` · `kill_shell` |
| **Web** | `web_fetch` · `web_search` |
| **Flow** | `todo_write` · `goal_update` · `enter_plan_mode` · `exit_plan_mode` · `wait` |
| **Agents** | `dispatch_agent` · `ask_user_question` · `skill` · `tool_search` |
| **Memory** | `memory` · `record_decision` |
| **Git worktrees** | `enter_worktree` · `exit_worktree` |
| **Notebooks** | `notebook_edit` |

Stale-file guards refuse a write when a file changed since the model last read it, destructive
shell commands are flagged for confirmation, and incomplete streamed tool calls are dropped
rather than executed with half-finished arguments.

## Slash commands worth knowing

Inside the TUI:

| Command | Does |
| --- | --- |
| `/usage` | live provider quota / rate-limit snapshot (separate from cost) |
| `/cost` | local token tally + estimated USD for the session |
| `/context` (`/ctx`) | context-window usage and where tokens are going |
| `/worktree create [name]` · `/worktree exit keep\|remove [--discard]` | isolated git worktrees |
| `/commit` · `/commit-push-pr` | Conventional-Commit generation, push, and PR via `gh` |
| `/why` | read back the decision trail — *why* past changes were made (`tomte why <loc>` / `tomte blame <file>` from the CLI) |
| `/buddy` | hatch a pixel companion — a rarity-weighted species seeded from your account, so it's stable for you and only re-rolls on an account switch (`/buddy off`, `/buddy reset`) |

**Composer prefixes** (Claude Code / Codex-style): `@<path>` attaches a file via gitignore-aware
typeahead, `!<command>` runs a shell command inline, and `#<note>` appends a note to `CLAUDE.md`.

It also inherits memory and skills from your existing setup: `AGENTS.md` / `CLAUDE.md` from the
git root down to your cwd are folded into the system prompt, and Codex/Claude skills and agents
are discovered automatically.

## Configuration

```bash
tomte config --show
tomte config --set-model gpt-5.5-pro --set-reasoning high
```

`$XDG_CONFIG_HOME/tomte/config.json`:

```json
{
  "model": "gpt-5",
  "reasoning_effort": "medium",
  "verbosity": "medium",
  "auto_approve_read": true,
  "auto_approve_write": false
}
```

**Reasoning effort:** `low` · `medium` · `high` · `xhigh` — **Verbosity:** `low` · `medium` · `high`

**Project overrides:** drop a `.tomte/config.json` in a repo to override settings for that
project on top of the global config. Because that file ships in cloned repos, only behavioral
fields are honored — `model`, `reasoning_effort`, `verbosity`, `auto_compact`, `auto_capture`, `fallback_models`.
Security-sensitive keys (`default_permission_mode`, `auto_approve_read` / `auto_approve_write`,
`providers`) are ignored in a project file and stay global-only, so a cloned repo can't disable
approval prompts or redirect the model endpoint.

## Models

| Model | Notes |
| --- | --- |
| `gpt-5.5` | Default — largest OpenAI context window |
| `gpt-5.5-pro` | Extended reasoning for hard agent tasks |
| `gpt-5.4` | Previous frontier, stable |
| `gpt-5.4-mini` | Fast and cheaper, still strong for routine code |
| `gpt-5.4-nano` | Latency-sensitive, cheapest |
| `gpt-5.2` · `gpt-5` | Earlier frontier generations, still selectable |

Retired ids (`gpt-5.1`, `gpt-5.3`, `gpt-5-pro`, `gpt-5-mini`, `gpt-5-nano`) auto-migrate to
their current equivalent on startup, so an existing `config.json` keeps working.

## How it's built

```
tomte/
└── crates/
    ├── core/   # library: OpenAI + Anthropic clients, OAuth (PKCE), agent loop, tools
    └── cli/    # the `tomte` binary: CLI subcommands + interactive TUI
```

`crates/core` holds the streaming SSE clients, the agent loop, and every tool. `crates/cli`
wraps it in subcommands (`login`, `chat`, `status`, `config`, `resume`, …) and the terminal UI
— run with no subcommand and you land straight in the TUI.

## Build from source

**You'll need:** Rust stable (CI tracks the latest stable; this release was verified with Rust
1.96.0) and `ripgrep` (recommended — powers the `grep` tool).

```bash
git clone https://github.com/ryan-mt/tomte && cd tomte
make install      # build release + link to ~/.local/bin/tomte
make link-dev     # OR: dev mode — re-runs `cargo run` on each call, no manual rebuild
make unlink       # remove the link
```

## Development

```bash
cargo run -- chat "hello"                            # headless one-shot
cargo run                                            # interactive TUI
cargo fmt --all --check                              # formatting gate
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                               # the test suite
make package                                         # local release archive + SHA256
make smoke                                           # local release smoke checks
```

Set `TOMTE_LIVE_SMOKE=1` with `make smoke` to also exercise live OpenAI and Anthropic
chat/tool-call paths using the credentials already on the machine.

## Contributing

Bug reports, ideas, and patches are all welcome. Start with
[CONTRIBUTING.md](CONTRIBUTING.md): it covers the dev setup, the exact CI gates
to run locally (`cargo fmt`, `clippy -D warnings`, `cargo test`, `make smoke`),
the Conventional-Commit style, and the PR flow. The short version: branch off
`main`, keep the diff focused, make the gates pass, and open a PR.

## Security

- OAuth tokens refresh automatically; `auth.json` is written with owner-only permissions on Unix.
- Project permission allow-lists reject symlinked `.tomte` paths and write with `O_NOFOLLOW`
  on Unix, so an "allow in this project" decision cannot be redirected into another file.
- Headless `chat` sanitizes terminal control sequences from model/tool text before writing to
  stdout, while keeping tomte's own status styling.
- Provider parse/SSE errors use bounded, auth-redacted excerpts instead of raw response bodies
  or event payloads.
- **`run_shell` runs inside an OS-level sandbox** — default `workspace-write` with outbound
  network off. On Linux it applies Landlock + seccomp, on macOS `sandbox-exec`, so a
  prompt-injected `curl … | sh` or `rm -rf ~` can't reach the network or write outside the
  workspace. **On Windows it is best-effort process-tree cleanup only — the filesystem and
  network are not yet confined** (`tomte doctor` reports the platform as unsandboxed), so keep
  reviewing destructive prompts there. Modes: `read-only` · `workspace-write` ·
  `danger-full-access`, with per-run `--sandbox <mode>` / `--sandbox-allow-net` overrides.
- On top of the sandbox, tomte flags obvious destructive commands (`rm -rf` on system or home
  paths, `curl … | sh`, `mkfs`, raw block-device writes, force-pushes, …) and refuses them until
  you explicitly override — the permission layer and the sandbox are independent.
- Environment variables that look like secrets (names containing `TOKEN`, `SECRET`, `KEY`,
  `OPENAI`, `AWS_`, `GITHUB_`, …) are stripped from `run_shell`'s child process so the model
  can't read them back via `env`.
- `auto_approve_write = false` by default.
- Sub-agents inherit the parent's approval policy; when a nested approval can't be surfaced,
  the sub-agent is forced into plan mode rather than silently bypassing review.

## License

MIT — see [LICENSE](LICENSE).

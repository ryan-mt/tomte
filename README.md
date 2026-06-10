<div align="center">

# `tomte`

**The coding agent that proves its work.**

Calm, multi-model, Rust-fast · quiet and surgical — and it hatches a pixel companion.

`0.0.4` · MIT · built in 🦀 Rust

</div>

---

One binary. Point it at OpenAI or Anthropic, drop it into any repo, and it reads, writes,
runs, searches, and *reasons* its way through real work — streaming, with a full tool belt
and a terminal UI that stays out of the way. Named for the Nordic farm spirit who keeps the
household in order overnight: meticulous, quiet, and intolerant of sloppy work.

```bash
tomte            # open the TUI and start working
tomte chat "explain what this repo does, then add a test for the parser"
```

## The tomte way

Most coding agents *tell* you the work is done. tomte is built around four ideas no other
terminal agent ships together — each one verifiable, none of them "trust me":

- **Done means verified.** `/prove` (or `tomte prove`, exit-code-clean for CI and commit
  hooks) collects an evidence bundle the CLI gathers *itself* — the files git reports
  changed, plus the **real exit codes** of your project's own test / typecheck / lint /
  build. The model never supplies those numbers, so it can't fabricate a green capsule;
  a check your project could define but doesn't surfaces as ⚠ unverified, never silently
  dropped. `tomte seal` notarizes that capsule onto the commit itself as a git note, so
  the proof is pushed and fetched with the history it certifies — `tomte seal verify`
  gates CI on it from any clone.
- **It remembers *why* — across models.** `record_decision` appends the reasoning behind
  every non-obvious change to a decision trail that's re-injected each session, so next
  month's session — or a different model entirely — inherits the *why*, not just the diff.
  Read it back with `tomte why <loc>`, `tomte blame <file>`, or `/why`; the reconcile pass
  flags a decision the code has since drifted out from under.
- **It knows the house.** `tomte twin` builds a verifiable map of the repo — import graph,
  symbol graph, test→source map, git recency, conventions — and `tomte why-context <seed>`
  (or `/why-context` in a session) answers the question context-stuffing agents dodge:
  *which files actually belong in context, and why*. Every claim is grounded in a real
  import edge, definition, test, or commit, and the nearby files it leaves out are listed
  with the reason each is unreachable.
- **Don't trust one agent — race them.** `tomte race "<task>" --agents 4` runs the task as
  a tournament: contestants varying model, effort, and style, each in its own isolated git
  worktree, judged on *measured evidence* — the project's own checks, diff size, added
  tests, risky commands run. The judge is deterministic (an LLM is never the referee), so
  the verdict is reproducible; `--apply` lands the winning patch.

Wrapped around those: a glass-box pre-flight that states what a write or shell command will
touch *before* it runs, recorded decisions resurfacing as house rules the agent re-reads
before it could break one, and an end-of-turn receipt — files touched, tests run, the why
it recorded. And because the indexes are real data, they compose: `tomte pulse` scores
which files are most likely to break next (change heat × import fan-in × missing tests,
formula on the card), `tomte handoff` renders the whole standing — git state, newest
decisions, drift watch, map, pulse — as one paste-ready capsule, so the next session
(a colleague, tomorrow's you, or a different model entirely) starts where this one stopped,
and `tomte rounds` is the custodian's night walk: it re-checks all of it against the last
walk — pulse risers, newly untested hot spots, decision anchors that drifted, TODO marks
that appeared, the project's own checks re-run — and exits non-zero only when something is
genuinely red, so a nightly CI job can run it as the morning gate.

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
- **Recovers gracefully.** A checkpoint every turn: `/undo` reverts the last file edit,
  `/rewind` restores the session to an earlier turn *and* reverts the edits made since —
  each picker row showing its blast radius before you commit.

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
tomte login                                   # interactive picker (OpenAI/Anthropic · OAuth or API key)
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

**And the evidence commands** — no model in the loop, safe anywhere:

```bash
tomte prove --json                       # run the project's own checks; non-zero exit on failure
tomte seal                               # notarize the proof onto HEAD as a git note; `seal verify` gates CI
tomte receipt --out RECEIPT.md           # the work receipt for a PR: proof + seal + what the session ran + cost + why
tomte twin                               # build/inspect the repo's verifiable map
tomte why-context src/auth/session.rs    # which files belong in context, and why
tomte pulse                              # which files break next — scored, formula on the card
tomte handoff --out HANDOFF.md           # the shift report for the next session (or model)
tomte rounds                             # the night walk: what changed since last rounds; red exits 1
tomte race "fix the flaky retry test" --agents 4   # tournament: isolated worktrees, measured judge
tomte sessions                           # the saved-session ledger: list · show <id> transcript · prune old ones
tomte cost --all                         # one cost ledger across every saved session for this project
tomte completions zsh                    # shell completions for the whole command surface
```

## Done means verified — in CI

The same evidence commands ship as a GitHub Action: it installs the released
binary (checksum-verified), runs `tomte prove` (the project's own checks, real
exit codes) and `tomte rounds` (drift watch, hot-and-untested files), fails the
job when the evidence is red, and writes the full report to the PR check's step
summary — optionally as one self-updating PR comment.

```yaml
jobs:
  verify:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write        # only needed for `comment: "true"`
    steps:
      - uses: actions/checkout@v6
        with: { fetch-depth: 0 }  # rounds/pulse read recent git history
      - uses: dtolnay/rust-toolchain@stable   # your project's toolchain — tomte runs *its* checks
      - uses: ryan-mt/tomte@v0.0.4
        with:
          comment: "true"
```

Inputs: `version` (release tag or `latest`), `prove` / `rounds` / `seal-verify`
(pick the gates), `comment` + `github-token`, `working-directory`. Output:
`verified` (`"true"`/`"false"`).

## The tool belt

The model can reach for any of these — streamed, schema-validated, and executed in parallel
when read-only:

| Group | Tools |
| --- | --- |
| **Files** | `read_file` · `write_file` · `edit_file` · `multi_edit` · `undo_last_edit` · `list_dir` |
| **Search** | `grep` · `glob` · `lsp` |
| **Shell** | `run_shell` · `bash_output` · `kill_shell` |
| **Web** | `web_fetch` · `web_search` |
| **Flow** | `todo_write` · `goal_update` · `enter_plan_mode` · `exit_plan_mode` · `wait` |
| **Agents** | `dispatch_agent` · `ask_user_question` · `skill` |
| **Memory** | `memory` · `record_decision` |
| **Git worktrees** | `enter_worktree` · `exit_worktree` |
| **Notebooks** | `notebook_edit` |

One more — `tool_search` — appears automatically when many MCP tools are connected, so their
schemas load on demand instead of bloating every request.

**MCP servers** — wire one up from the CLI, no hand-editing JSON:

```bash
tomte mcp add filesystem -- npx -y @modelcontextprotocol/server-filesystem /tmp
tomte mcp list                       # what's configured (env values stay hidden)
tomte mcp remove filesystem
```

Servers land in `settings.json` under `mcp_servers`, and each one's tools show up to the agent as
`mcp__<server>__<tool>`. Pass `--env KEY=VALUE` (repeatable) to set per-server environment.

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
| `/why` | read back the decision trail — *why* past changes were made (`tomte why <loc>` / `tomte blame <file>` from the CLI; add `--json` for machine-readable output) |
| `/prove` | verify the work — run the project's own test/typecheck/lint/build and show the proof capsule (`tomte prove` headless; non-zero exit gates CI) |
| `/twin` · `/why-context <seed>` | the Repo Twin and the context X-ray — five verifiable indexes of the repo, and which files belong in context for a file/symbol, with why |
| `/pulse` · `/handoff` | the files most likely to break next (scored from the twin, formula shown), and the paste-ready shift report for the next session |
| `/rewind` | restore the session to an earlier turn and revert the file edits made since (each row shows its blast radius first); `/undo` reverts just the last edit |
| `/compact <focus>` | compact the conversation, steering the summary toward what you name |
| `/buddy` | hatch a pixel companion — a rarity-weighted species seeded from your account, so it's stable for you and only re-rolls on an account switch (`/buddy off`, `/buddy reset`) |

**Composer prefixes** — typed right in the chat input: `@<path>` attaches a file via gitignore-aware
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
  "model": "gpt-5.5",
  "reasoning_effort": "medium",
  "verbosity": "medium",
  "auto_approve_read": true,
  "auto_approve_write": false
}
```

**Reasoning effort:** `none` · `minimal` · `low` · `medium` · `high` · `xhigh` · `max` — **Verbosity:** `low` · `medium` · `high`

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
| `claude-fable-5` | Anthropic's top tier — 1M context, adaptive thinking, `xhigh` effort |
| `claude-opus-4-8` | Frontier Opus — most capable Opus, 1M context |
| `claude-sonnet-4-6` | Balanced speed/capability |
| `claude-haiku-4-5` | Fast and cheap for routine work |

Retired ids (`gpt-5.1`, `gpt-5.3`, `gpt-5-pro`, `gpt-5-mini`, `gpt-5-nano`) auto-migrate to
their current equivalent on startup, so an existing `config.json` keeps working. Earlier
Claude tiers (Opus 4.5–4.7, dated snapshots) stay selectable and price correctly in `/cost`.

**Other providers.** Any OpenAI-compatible endpoint works via a `<id>/<model>` spec. The common
ones are built in — `groq`, `openrouter`, `deepseek`, `xai`, `together`, `fireworks`, `cerebras`,
`mistral`, plus local `ollama` and `lmstudio` — so `tomte config --set-model groq/llama-3.3-70b`,
set `GROQ_API_KEY` (each preset reads `<ID>_API_KEY`; local servers need no key), and you're
running. Anything else: add a `providers` entry to `config.json` with its `base_url`.

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

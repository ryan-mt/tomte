# opencli

A coding-agent CLI written in **Rust** — built to be a drop-in replacement for Claude Code.

Current release line: `0.0.1-beta.4`.

Backed by OpenAI and Anthropic model adapters with multiple authentication modes:

- **OpenAI OAuth ChatGPT** — sign in with a ChatGPT Plus/Pro/Team/Enterprise account and use your subscription quota.
- **OpenAI API key** — set `OPENAI_API_KEY` or store one with `opencli login --api-key --provider openai`.
- **Anthropic OAuth** — sign in with a Claude Pro/Max account after acknowledging the ToS warning.
- **Anthropic API key** — set `ANTHROPIC_API_KEY` or store one with `opencli login --api-key --provider anthropic`.

Full **tool calling** surface: `read_file`, `write_file`, `edit_file`, `multi_edit`, `list_dir`, `grep`, `glob`, `run_shell`, `bash_output`, `kill_shell`, `todo_write`, `goal_update`, `enter_plan_mode`, `exit_plan_mode`, `dispatch_agent`, `ask_user_question`, `web_fetch`, `web_search`, `notebook_edit`, `lsp`, `wait`, `skill`, `tool_search`, `enter_worktree`, and `exit_worktree`. Streaming SSE, parallel tool execution, reasoning summary, strict JSON-schema validation, and compatibility aliases for multiple provider tool-call shapes.

## Architecture

```
opencli/
└── crates/
    ├── core/     # Library: OpenAI + Anthropic clients, OAuth (PKCE), agent loop, tools
    └── cli/      # `opencli` binary: CLI commands + interactive terminal UI (TUI)
```

The `opencli` binary:

- CLI subcommands: `login`, `chat`, `status`, `config`, `resume`, …
- When called with no subcommand: launches the interactive terminal UI (TUI).

## Install

### Option A: Download a beta build

Download the matching archive from a GitHub release:

- `opencli-x86_64-unknown-linux-gnu.tar.gz`
- `opencli-x86_64-apple-darwin.tar.gz`
- `opencli-aarch64-apple-darwin.tar.gz`
- `opencli-x86_64-pc-windows-msvc.zip`

Then put `opencli` (or `opencli.exe`) somewhere on your `PATH`.

### Option B: Build from source

### 1. System dependencies

- Rust stable (CI uses the latest stable toolchain; this beta was locally verified with Rust 1.95.0)
- `ripgrep` (recommended; used by the `grep` tool)

### 2. Build + link

```bash
git clone <repo> opencli && cd opencli
make install           # build release + link to ~/.local/bin/opencli
```

Or dev mode (wrapper runs `cargo run` on each invocation, no manual rebuild):

```bash
make link-dev
```

After linking, edit Rust code then re-run `cargo build --release` (with `make link`), or just edit and call `opencli …` (with `make link-dev`, which builds on demand).

To unlink:

```bash
make unlink
```

## Usage

### Sign in

```bash
opencli login                 # OAuth ChatGPT (opens browser)
opencli login --api-key       # paste an API key
opencli status                # show current status
opencli logout
```

OAuth uses PKCE + the callback `http://localhost:1455/auth/callback`. Tokens are stored in `$XDG_CONFIG_HOME/opencli/auth.json` (chmod 600). The access token is refreshed automatically when it gets close to expiry.

### Headless chat

```bash
opencli chat "write a fibonacci function in Python"
opencli chat --model gpt-5-pro --reasoning high "refactor module X"
echo "read CLAUDE.md and summarize" | opencli chat
```

### Interactive TUI (default)

```bash
opencli              # launches the interactive terminal UI
opencli resume       # open the TUI with the session picker
```

Useful slash commands inside the TUI:

- `/usage` shows the active provider's live quota/rate-limit snapshot after the first response.
- `/cost` shows the local token tally and estimated USD cost for the session.
- `/context` shows context-window usage and where the visible conversation is spending tokens.

### Configuration

```bash
opencli config --show
opencli config --set-model gpt-5-pro --set-reasoning high
```

Config lives at `$XDG_CONFIG_HOME/opencli/config.json`:

```json
{
  "model": "gpt-5",
  "reasoning_effort": "medium",
  "verbosity": "medium",
  "auto_approve_read": true,
  "auto_approve_write": false
}
```

## Development

```bash
cargo run -- chat "hello"     # headless one-shot
cargo run                     # interactive TUI
cargo fmt --all -- --check    # formatting gate
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace        # run the test suite
make package                  # build local release archive + SHA256
make smoke                    # run local release smoke checks
```

Set `OPENCLI_LIVE_SMOKE=1` when running `make smoke` to also verify live OpenAI
and Anthropic chat/tool-call paths using the credentials already stored on the
machine.

## Supported models

| Model          | Notes                                        |
| -------------- | -------------------------------------------- |
| `gpt-5.5`      | Default, largest OpenAI context window       |
| `gpt-5.4`      | Previous frontier, stable                    |
| `gpt-5.3`      | Older frontier                               |
| `gpt-5-pro`    | Extended reasoning for hard agent tasks      |
| `gpt-5-mini`   | Fast, cheaper, still strong for routine code |
| `gpt-5-nano`   | Latency-sensitive, cheapest                  |

Older opencli builds shipped legacy base names (`gpt-5`, `gpt-5.1`, `gpt-5.2`).
`opencli` auto-migrates those to the current default on startup, so an existing
`config.json` keeps working.

Reasoning effort: `low` · `medium` · `high` · `xhigh`.
Verbosity: `low` · `medium` · `high`.

## Security

- OAuth tokens refresh automatically; `auth.json` is written with mode `0600`.
- The `run_shell` tool runs directly on your machine. No sandbox yet — review prompts that include destructive commands.
- Environment variables that look like secrets (names containing TOKEN, SECRET, KEY, OPENAI, AWS_, GITHUB_, etc.) are stripped from `run_shell`'s child process so the model can't exfiltrate them via `env`.
- `auto_approve_write = false` is the default; future versions will show a diff and prompt before applying writes.
- Sub-agents inherit parent approval policy; when nested approvals cannot be surfaced, they are forced into plan mode instead of silently bypassing review.

## License

MIT

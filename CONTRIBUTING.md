# Contributing to tomte

Thanks for wanting to make `tomte` better. It's a young project and patches,
bug reports, and ideas are all welcome.

This guide is short on purpose. The golden rule: **make the change you'd want to
review** — small, focused, and green.

## Ground rules

- **One concern per PR.** A bug fix, a feature, or a refactor — not all three.
- **Keep diffs surgical.** Touch only what the change needs; match the
  surrounding style instead of reformatting unrelated code.
- **No new warnings.** CI runs with `-D warnings`, so anything `clippy` or
  `rustc` complains about will fail the build.
- **Tests come with behavior.** New behavior (or a fixed bug) should arrive with
  a test that would fail without your change.

## Set up your environment

You'll need:

- **Rust stable** (CI tracks the latest stable toolchain).
- **`ripgrep`** — recommended; it backs the `grep` tool and the smoke test.

```bash
git clone https://github.com/ryan-mt/tomte && cd tomte
make link-dev      # dev mode: `tomte` re-runs `cargo run` on each call
# ...or...
make install       # build --release and link to ~/.local/bin/tomte
```

Run it while you work:

```bash
cargo run                       # interactive TUI
cargo run -- chat "hello"       # headless one-shot
```

## The checks your PR must pass

CI (`.github/workflows/ci.yml`) runs **rustfmt**, **clippy (deny warnings)**,
the **test suite on Linux/macOS/Windows**, a **release build**, and a
**release smoke test**. Run the same gates locally before you push:

```bash
cargo fmt --all                                       # format (CI checks this with --check)
cargo clippy --workspace --all-targets -- -D warnings # lint, warnings = errors
cargo test --workspace                                # the full suite
make smoke                                            # build a release binary and sanity-check it
```

Shortcut: `make check` runs `cargo check` + the clippy gate, and `make fmt`
formats the tree.

> Tip: the fmt gate is the most common surprise. Run `cargo fmt --all` (or
> `make fmt`) before every commit.

## Tests

- Unit tests live next to the code in `#[cfg(test)] mod tests { … }`.
- Cross-cutting/integration tests live in `crates/core/tests/`.
- Anything OS-specific (symlinks, signals, file modes) should be gated with
  `#[cfg(unix)]` (or the relevant cfg) so the Windows/macOS CI jobs stay green.

Async tests use `#[tokio::test]`. When a test waits on external state, bound it
with `tokio::time::timeout` so a regression fails fast instead of hanging CI.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
fix: stop the lsp walk from following symlink cycles
feat: add the /buddy pixel companion
style: apply rustfmt across the tree
chore: prepare beta4 release
```

Use the imperative mood, keep the subject line tight, and put the *why* in the
body when it isn't obvious from the diff.

## Pull requests

1. Branch off `main`.
2. Make the change; keep commits meaningful.
3. Make the local gates above pass.
4. Open a PR against `main` and describe **what** changed and **why**. Link the
   issue it closes.
5. Wait for CI to go green — every job must pass before merge.

Small, well-described PRs get reviewed fastest.

## Reporting bugs & requesting features

Open an [issue](https://github.com/ryan-mt/tomte/issues). For a bug, the most
useful report includes:

- what you ran (the command or the prompt) and the model/provider in use,
- what you expected vs. what happened,
- your OS and `tomte --version`,
- any error output (scrub secrets first).

## Reporting a security issue

Please **don't** open a public issue for a vulnerability. Report it privately
through GitHub Security Advisories
([Security → Report a vulnerability](https://github.com/ryan-mt/tomte/security/advisories/new))
so it can be fixed before disclosure.

`run_shell` runs inside an OS-level sandbox (Landlock + seccomp on Linux,
`sandbox-exec` on macOS, best-effort process-tree cleanup on Windows; default
`workspace-write` with outbound network off), and a separate classifier flags
destructive commands before they run. The agent's ability to run commands is by
design — focus reports on ways those guardrails can be bypassed: sandbox or path
escapes, secret exfiltration, and the destructive-command classifier missing a
real case.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).

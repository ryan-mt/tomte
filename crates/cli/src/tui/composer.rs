//! Composer-prefix features driven from the chat input, mirroring Claude Code /
//! Codex muscle memory:
//!   - `!<command>` — run a shell command now (no model turn), output staged as
//!     context for the next message; `!!` forces past the destructive guard.
//!   - `#<note>`    — append a note to the project `CLAUDE.md` and re-apply it.
//!   - `@<path>`    — reference a file/dir; its contents are attached to the
//!     prompt (`file_candidates` powers the typeahead, `expand_at_mentions`
//!     performs the attach at send time).
//!
//! Extracted from `app.rs` to keep that file readable; the handlers take
//! `&mut App` and report back through the same channels the event loop owns.

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc;
use tomte_core::agent::Agent;

use super::app::{App, Block};
use super::picker;

/// Hard ceiling on a composer `!`-command. The command runs on a background
/// task, so the UI stays responsive regardless; this bound exists so a
/// non-terminating command (a dev server, `tail -f`, a hung pipe) is killed
/// instead of leaking a process forever.
const BANG_TIMEOUT: Duration = Duration::from_secs(120);

/// Result of a background `!`-command, sent back to the main loop so the output
/// is appended on the UI thread (never touching `App` from the spawned task).
pub(crate) struct BangResult {
    /// What to show inline in the transcript.
    pub(crate) display: String,
    /// What to stage into `pending_shell_context` for the next model turn.
    pub(crate) context: String,
}

/// Run a `!`-prefixed shell command straight from the composer (no model turn),
/// mirroring `!bash` mode in Claude Code. A leading `!` (the user typed `!!cmd`)
/// forces past the destructive-command guard.
///
/// The command runs on a **background task** and reports back over `bang_tx`, so
/// a slow or non-terminating command never blocks the event loop (the previous
/// version `.await`ed `output()` inline, freezing the whole TUI until the
/// command exited — an unrecoverable hang for e.g. a dev server). The task is
/// also bounded by [`BANG_TIMEOUT`] with `kill_on_drop`, so the child is killed
/// on timeout rather than leaking.
pub(crate) fn handle_bang_shell(app: &mut App, bang_tx: &mpsc::Sender<BangResult>, raw: &str) {
    let (force, cmd) = match raw.strip_prefix('!') {
        Some(rest) => (true, rest.trim()),
        None => (false, raw),
    };
    if cmd.is_empty() {
        app.blocks.push(Block::System(
            "usage: !<shell command>  (!! to force)".into(),
        ));
        return;
    }
    if !force {
        if let Some(reason) = tomte_core::tools::shell::classify_danger(cmd) {
            app.blocks.push(Block::System(format!(
                "⚠ refused: {reason}. Re-run as `!!{cmd}` to force."
            )));
            return;
        }
    }
    app.blocks.push(Block::System(format!("! {cmd}")));
    app.auto_scroll = true;

    let cwd = app.cwd.clone();
    let cmd = cmd.to_string();
    let tx = bang_tx.clone();
    tokio::spawn(async move {
        let result = run_bang_command(&cmd, &cwd, BANG_TIMEOUT).await;
        // Receiver gone (app exiting) → nothing to do.
        let _ = tx.send(result).await;
    });
}

/// Execute one composer `!`-command with a hard `timeout`, killing the child on
/// timeout (`kill_on_drop`). Pure w.r.t. `App` so it is safe to run off-thread
/// and straightforward to unit-test. Split out from [`handle_bang_shell`] so the
/// timeout can be exercised in tests without waiting the full ceiling.
async fn run_bang_command(cmd: &str, cwd: &Path, timeout: Duration) -> BangResult {
    let ctx =
        |body: &str| format!("[The user ran a shell command in the terminal]\n$ {cmd}\n{body}");

    #[cfg(windows)]
    let mut command = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    command.current_dir(cwd).kill_on_drop(true);

    // On timeout the `output()` future is dropped; `kill_on_drop` then kills the
    // child so a hung command can't survive past the ceiling.
    let output = match tokio::time::timeout(timeout, command.output()).await {
        Ok(r) => r,
        Err(_) => {
            let msg = format!("⏱ timed out after {}s (process killed)", timeout.as_secs());
            return BangResult {
                display: msg.clone(),
                context: ctx(&msg),
            };
        }
    };

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let mut body = String::new();
            if !stdout.trim().is_empty() {
                body.push_str(stdout.trim_end());
            }
            if !stderr.trim().is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(stderr.trim_end());
            }
            let code = o.status.code().unwrap_or(-1);
            let body = truncate_output(&body, 16 * 1024);
            let shown = if body.trim().is_empty() {
                if o.status.success() {
                    "(no output)".to_string()
                } else {
                    format!("(exit {code}, no output)")
                }
            } else {
                body
            };
            BangResult {
                context: ctx(&format!("Exit code: {code}\nOutput:\n{shown}")),
                display: shown,
            }
        }
        Err(e) => BangResult {
            display: format!("! failed to run: {e}"),
            context: ctx(&format!("failed to run: {e}")),
        },
    }
}

/// Build the text to append to a project `CLAUDE.md` for a `#`-note. Writes a
/// header for a brand-new file, and guarantees the note starts on its own line
/// even when the existing file doesn't end in a newline (otherwise the bullet
/// would be glued onto the last line, e.g. `...last line- note`).
fn claude_md_note_block(existed: bool, ends_with_newline: bool, note: &str) -> String {
    if !existed {
        format!("# CLAUDE.md\n\n- {note}\n")
    } else if ends_with_newline {
        format!("- {note}\n")
    } else {
        format!("\n- {note}\n")
    }
}

/// Append a `#`-prefixed note to the project `CLAUDE.md` and re-apply memory to
/// the live agent so it takes effect immediately (Claude Code's `#` quick-add).
pub(crate) async fn handle_hash_memory(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
    note: &str,
) {
    if note.is_empty() {
        app.blocks
            .push(Block::System("usage: #<note to remember>".into()));
        return;
    }
    let path = app.cwd.join("CLAUDE.md");
    let existed = path.exists();
    let ends_with_newline = std::fs::read_to_string(&path)
        .map(|c| c.is_empty() || c.ends_with('\n'))
        .unwrap_or(true);
    let block = claude_md_note_block(existed, ends_with_newline, note);
    use std::io::Write;
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(block.as_bytes()));
    match res {
        Ok(()) => {
            app.blocks
                .push(Block::System(format!("📝 remembered → {}", path.display())));
            // Rebuild the system context so the note lands in this session's
            // prompt. A full refresh (not a lone apply_project_memory) keeps the
            // memory-store and skill blocks, which the inherited-memory re-apply
            // would otherwise truncate. With no agent yet it's applied on the
            // first turn instead.
            let mut guard = agent.lock().await;
            if let Some(a) = guard.as_mut() {
                a.refresh_system_context();
            }
        }
        Err(e) => app
            .blocks
            .push(Block::System(format!("memory write failed: {e}"))),
    }
    app.auto_scroll = true;
}

/// Build the `@`-file picker list: project files relative to `cwd`, gitignore-
/// aware via `rg --files`, falling back to a bounded manual walk when ripgrep is
/// absent. Capped so a huge tree can't stall the UI.
pub(crate) fn file_candidates(cwd: &Path) -> Vec<picker::PickerItem> {
    const MAX: usize = 5000;
    let mut paths: Vec<String> = Vec::new();
    // Stream `rg --files` and stop at MAX, killing rg early, so a giant monorepo
    // can't make the synchronous picker-open stall enumerating millions of files
    // (the old code buffered all of rg's stdout before capping).
    use std::io::BufRead;
    let mut rg_cmd = std::process::Command::new("rg");
    rg_cmd
        .arg("--files")
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    tomte_core::secret_env::scrub_secret_env_std(&mut rg_cmd);
    let rg = rg_cmd.spawn();
    match rg {
        Ok(mut child) => {
            if let Some(out) = child.stdout.take() {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    paths.push(line.replace('\\', "/"));
                    if paths.len() >= MAX {
                        break;
                    }
                }
            }
            // We have enough (or hit EOF) — stop rg and reap it.
            let _ = child.kill();
            let _ = child.wait();
            // rg ran but produced nothing (e.g. not a usable dir) → manual walk.
            if paths.is_empty() {
                walk_files(cwd, MAX, &mut paths);
            }
        }
        // rg not installed / failed to spawn.
        Err(_) => walk_files(cwd, MAX, &mut paths),
    }
    paths
        .into_iter()
        .map(|p| picker::PickerItem {
            key: p.clone(),
            title: p,
            description: String::new(),
        })
        .collect()
}

/// Bounded, gitignore-blind directory walk used when `rg` is unavailable. Skips
/// hidden entries and a few notoriously heavy directories.
fn walk_files(root: &Path, max: usize, out: &mut Vec<String>) {
    const SKIP: &[&str] = &[".git", "node_modules", "target", ".venv", "dist", "build"];
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= max {
                return;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if ft.is_file() {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
}

/// Scan `text` for `@<path>` references that resolve to real files/dirs under
/// `cwd` and return a context block with their contents (files) or listings
/// (dirs), to append to the prompt — mirroring Claude Code's `@file` expansion.
/// Each `@` must start the string or follow whitespace, so `a@b.com` is left
/// alone. Returns `None` when nothing resolves.
pub(crate) fn expand_at_mentions(text: &str, cwd: &Path) -> Option<String> {
    const MAX_FILE_BYTES: usize = 64 * 1024;
    const MAX_TOTAL_BYTES: usize = 256 * 1024;
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut sections = String::new();

    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'@' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                end += 1;
            }
            if end > start {
                let token = text[start..end].trim_end_matches(['.', ',', ':', ';', ')']);
                if !token.is_empty() && seen.insert(token.to_string()) {
                    if let Some(section) = render_mention(token, cwd, MAX_FILE_BYTES) {
                        if sections.len() + section.len() <= MAX_TOTAL_BYTES {
                            sections.push_str(&section);
                        }
                    }
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    (!sections.is_empty())
        .then(|| format!("[Contents of files referenced with @ in the message above]\n{sections}"))
}

/// Render one resolved `@`-mention: a fenced file body or a directory listing.
///
/// The resolved path is confined to `cwd`: it is canonicalized (resolving `..`
/// and symlinks) and must stay under the canonical `cwd`, so `@/etc/passwd`,
/// `@../../secret`, or a symlink pointing outside the workspace resolve to
/// `None` rather than silently shipping out-of-tree file contents to the model.
fn render_mention(token: &str, cwd: &Path, max_bytes: usize) -> Option<String> {
    let p = Path::new(token);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    // canonicalize() also confirms the path exists; a missing path → None.
    let path = joined.canonicalize().ok()?;
    let base = cwd.canonicalize().ok()?;
    if !path.starts_with(&base) {
        return None;
    }
    let meta = std::fs::metadata(&path).ok()?;
    if meta.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&path)
            .ok()?
            .flatten()
            .map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() {
                    format!("{n}/")
                } else {
                    n
                }
            })
            .collect();
        names.sort();
        names.truncate(200);
        Some(format!("\n## @{token} (directory)\n{}\n", names.join("\n")))
    } else if meta.is_file() {
        // Skip binary: only attach valid UTF-8.
        let text = String::from_utf8(std::fs::read(&path).ok()?).ok()?;
        let body = truncate_output(&text, max_bytes);
        Some(format!("\n## @{token}\n```\n{body}\n```\n"))
    } else {
        None
    }
}

/// Truncate `s` to at most `max` bytes on a char boundary, appending a note when
/// it was cut. Shared by `!` output capture and `@file` expansion.
fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated, {} bytes total)", &s[..end], s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_at_mentions_includes_existing_file_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "hello world").unwrap();

        // A real file is attached; an email-shaped token and a missing path are
        // left untouched (no section, returns the same None when nothing resolves).
        let out = expand_at_mentions("see @notes.txt and mail a@b.com", tmp.path()).unwrap();
        assert!(out.contains("## @notes.txt"));
        assert!(out.contains("hello world"));
        assert!(!out.contains("a@b.com"));

        assert!(expand_at_mentions("ping a@b.com only", tmp.path()).is_none());
        assert!(expand_at_mentions("read @does/not/exist", tmp.path()).is_none());
    }

    // Regression: @-mentions are confined to cwd — an absolute path or a `..`
    // escape that resolves outside the workspace must not be attached.
    #[test]
    fn expand_at_mentions_refuses_paths_outside_cwd() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        // A secret living next to (not under) the project dir.
        let secret = root.path().join("secret.txt");
        std::fs::write(&secret, "TOP SECRET").unwrap();

        // Absolute path outside cwd.
        let abs = format!("look @{}", secret.display());
        assert!(expand_at_mentions(&abs, &project).is_none());
        // Parent-dir escape.
        assert!(expand_at_mentions("look @../secret.txt", &project).is_none());

        // A file genuinely under cwd still resolves.
        std::fs::write(project.join("ok.txt"), "fine").unwrap();
        let out = expand_at_mentions("see @ok.txt", &project).unwrap();
        assert!(out.contains("fine"));
    }

    #[test]
    fn claude_md_note_block_keeps_note_on_its_own_line() {
        // New file gets a header.
        assert_eq!(
            claude_md_note_block(false, true, "x"),
            "# CLAUDE.md\n\n- x\n"
        );
        // Existing file ending in newline: plain append.
        assert_eq!(claude_md_note_block(true, true, "x"), "- x\n");
        // Existing file WITHOUT a trailing newline: prepend one so the bullet
        // doesn't glue onto the last line.
        assert_eq!(claude_md_note_block(true, false, "x"), "\n- x\n");
    }

    #[tokio::test]
    async fn run_bang_command_captures_output_and_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let r = run_bang_command("echo hi", tmp.path(), Duration::from_secs(10)).await;
        assert!(r.display.contains("hi"));
        assert!(r.context.contains("$ echo hi"));
        assert!(r.context.contains("Exit code: 0"));
    }

    // Regression: a non-terminating `!`-command must be killed at the deadline,
    // never freeze the UI (the old inline `.await` hung forever on such commands).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn run_bang_command_times_out_instead_of_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let r = run_bang_command("sleep 30", tmp.path(), Duration::from_millis(200)).await;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "command must return at the deadline, not run to completion"
        );
        assert!(r.display.contains("timed out"), "got: {}", r.display);
    }
}

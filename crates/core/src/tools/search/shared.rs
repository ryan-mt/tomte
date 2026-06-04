//! Shared search helpers: capped subprocess execution, output capping, and
//! path/output normalization used by both `grep` and `glob`. Split out of
//! `search`; logic unchanged.

use anyhow::Result;
use tokio::process::Command;

/// Hard ceiling on raw search stdout captured into memory before
/// [`apply_limits`] trims it. Generous enough to preserve deep `offset`
/// pagination, but bounded so a pathological match set (e.g. every line of a
/// giant minified file) can't balloon memory. ~4 MiB.
pub(super) const SEARCH_OUTPUT_CAP_BYTES: usize = 4 * 1024 * 1024;

/// Like [`Command::output`], but stops capturing stdout at `cap` bytes and
/// kills the child if it overruns, so it can neither balloon memory nor block
/// forever writing to a full pipe once we stop reading. Returns the output
/// alongside a flag that is `true` when the stdout cap was hit (and the child
/// was therefore killed) — callers use it to tell our own cap-kill apart from a
/// genuine non-zero exit.
pub(super) async fn run_capped(
    mut cmd: Command,
    cap: usize,
) -> std::io::Result<(std::process::Output, bool)> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // Drain stderr concurrently and all the way to EOF (retaining only the
    // first 64 KiB) in its own task. A child that floods stderr — e.g. `grep`
    // emitting one "Permission denied" line per directory over a large tree —
    // would otherwise block on a full stderr pipe and never close stdout,
    // hanging the stdout read below until the outer tool timeout.
    let stderr_task = tokio::spawn(async move {
        let mut err = Vec::new();
        let _ = drain_capped(&mut stderr, &mut err, 64 * 1024).await;
        err
    });

    let mut out = Vec::new();
    let overran = read_to_cap(&mut stdout, &mut out, cap).await?;
    if overran {
        // We stopped reading; kill so the child doesn't block writing to a
        // full pipe while we wait for it to exit.
        let _ = child.start_kill();
    }
    let status = child.wait().await?;
    let err = stderr_task.await.unwrap_or_default();
    Ok((
        std::process::Output {
            status,
            stdout: out,
            stderr: err,
        },
        overran,
    ))
}

/// Read from `r` into `buf` until EOF or `cap` bytes. Returns `true` when the
/// cap was reached (more data may remain unread).
async fn read_to_cap<R>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<bool>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    while buf.len() < cap {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(false);
        }
        let room = cap - buf.len();
        buf.extend_from_slice(&chunk[..n.min(room)]);
        if n > room {
            return Ok(true);
        }
    }
    Ok(true)
}

/// Read from `r` all the way to EOF, retaining at most `cap` bytes in `buf` and
/// discarding the rest. Unlike [`read_to_cap`], this never stops early, so the
/// child can never block on a full pipe — used for stderr, which we keep only a
/// bounded prefix of but must fully drain to avoid deadlocking the stdout read.
async fn drain_capped<R>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut chunk = [0u8; 8192];
    loop {
        let n = r.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        if buf.len() < cap {
            let room = cap - buf.len();
            buf.extend_from_slice(&chunk[..n.min(room)]);
        }
    }
}

pub(super) fn resolved_relative_to_cwd(
    cwd: &std::path::Path,
    path: &str,
) -> Result<std::path::PathBuf> {
    let resolved = crate::tools::fs::resolve(cwd, path)?;
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    Ok(resolved
        .strip_prefix(&root)
        .map(|p| p.to_path_buf())
        .unwrap_or(resolved))
}

pub(super) fn path_to_slash_string(path: &std::path::Path) -> String {
    normalize_path_separators(&path.to_string_lossy())
}

pub(super) fn normalize_path_separators(path: &str) -> String {
    path.replace('\\', "/")
}

pub(super) fn normalize_search_output_paths(output: &str, mode: &str) -> String {
    output
        .lines()
        .map(|line| normalize_search_output_line(line, mode))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_search_output_line(line: &str, mode: &str) -> String {
    if line.is_empty() || line == "--" {
        return line.to_string();
    }

    if matches!(mode, "files_with_matches") {
        return normalize_path_separators(line);
    }

    let Some(idx) = line.find([':', '-']) else {
        return normalize_path_separators(line);
    };
    let (path, rest) = line.split_at(idx);
    format!("{}{}", normalize_path_separators(path), rest)
}

/// Cap an output string by offset, lines (`head_limit`), and bytes (`byte_cap`).
/// The byte cut walks back to a char boundary so we never slice mid-codepoint.
pub(super) fn apply_limits(
    s: &str,
    head_limit: Option<usize>,
    offset: Option<usize>,
    byte_cap: usize,
) -> String {
    let offset = offset.unwrap_or(0);
    let line_clipped: String = if offset > 0 || head_limit.is_some() {
        let lines: Vec<&str> = s.lines().collect();
        let total = lines.len();
        let start = offset.min(total);
        let mut end = total;
        if let Some(n) = head_limit {
            // saturating: a model-supplied head_limit near usize::MAX would
            // otherwise overflow `start + n` and wrap to `end < start`, panicking
            // the `lines[start..end]` slice below.
            end = start.saturating_add(n).min(total);
        }
        let mut out = lines[start..end].join("\n");
        if offset > 0 {
            let skipped = start;
            let note = format!("…(offset skipped {skipped} line(s))");
            if out.is_empty() {
                out = note;
            } else {
                out = format!("{note}\n{out}");
            }
        }
        if end < total {
            out.push_str(&format!(
                "\n…(head_limit hit, {} more line(s) omitted)",
                total - end
            ));
        }
        out
    } else {
        s.to_string()
    };
    if line_clipped.len() <= byte_cap {
        return line_clipped;
    }
    let mut cut = byte_cap;
    while cut > 0 && !line_clipped.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n…(truncated, {} bytes remaining)",
        &line_clipped[..cut],
        line_clipped.len() - cut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_output_paths_use_forward_slashes() {
        assert_eq!(
            normalize_search_output_line(r"src\lib.rs:1:needle", "content"),
            "src/lib.rs:1:needle"
        );
        assert_eq!(
            normalize_search_output_line(r"src\lib.rs-2-context", "content"),
            "src/lib.rs-2-context"
        );
        assert_eq!(
            normalize_search_output_line(r"src\lib.rs", "files_with_matches"),
            "src/lib.rs"
        );
    }

    #[test]
    fn apply_limits_survives_overflowing_head_limit() {
        // A head_limit near usize::MAX must not overflow `start + n` and panic
        // the `lines[start..end]` slice.
        let s = "a\nb\nc\nd";
        let out = apply_limits(s, Some(usize::MAX), Some(1), 8192);
        assert!(out.contains('b') && out.contains('d'));
    }

    #[tokio::test]
    async fn read_to_cap_stops_at_limit() {
        let data = [b'x'; 10_000];
        let mut buf = Vec::new();
        let overran = read_to_cap(&mut &data[..], &mut buf, 4096).await.unwrap();
        assert!(overran);
        assert_eq!(buf.len(), 4096);
    }

    #[tokio::test]
    async fn read_to_cap_reads_everything_under_limit() {
        let data = [b'x'; 100];
        let mut buf = Vec::new();
        let overran = read_to_cap(&mut &data[..], &mut buf, 4096).await.unwrap();
        assert!(!overran);
        assert_eq!(buf.len(), 100);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_capped_bounds_and_kills_a_stdout_flood() {
        // `yes` writes to stdout forever; run_capped must bound it and return
        // instead of hanging or growing without limit, and report the overrun.
        let (out, overran) = run_capped(Command::new("yes"), 8192).await.unwrap();
        assert!(out.stdout.len() <= 8192 + 8192);
        assert!(overran, "a stdout flood must report overran=true");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_capped_does_not_deadlock_on_a_stderr_flood() {
        // A child that floods stderr while writing little/no stdout must not
        // hang run_capped: stderr is drained concurrently to EOF. We bound the
        // test with our own timeout so a regression fails fast instead of
        // hanging the suite.
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            // ~512 KiB to stderr (well past the 64 KiB stderr cap and the OS
            // pipe buffer), a tiny bit to stdout.
            .arg("yes ERR | head -c 524288 1>&2; echo ok");
        let fut = run_capped(cmd, 4 * 1024 * 1024);
        let (out, overran) = tokio::time::timeout(std::time::Duration::from_secs(10), fut)
            .await
            .expect("run_capped must not deadlock on a stderr flood")
            .unwrap();
        assert!(!overran, "stdout stayed under cap");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ok");
        assert!(out.stderr.len() <= 64 * 1024, "stderr retained is capped");
    }
}

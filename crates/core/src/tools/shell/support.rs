//! Shell runtime support: platform shell selection, process-group control,
//! output capping, secret-env scrubbing, and UTF-8 draining. Split out of
//! `shell`; logic unchanged.

use base64::Engine;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::tools::{BackgroundShellState, BgStatus};

pub(super) fn bash_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    format!(
        "bash_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    )
}

pub(super) fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
pub(super) fn isolate_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn isolate_process_group(_cmd: &mut Command) {}

#[cfg(windows)]
pub(super) fn platform_shell_name() -> &'static str {
    "cmd"
}

#[cfg(not(windows))]
pub(super) fn platform_shell_name() -> &'static str {
    "sh"
}

#[cfg(windows)]
pub(super) fn configure_platform_shell(cmd: &mut Command, command: &str) {
    cmd.arg("/C").arg(command);
}

#[cfg(not(windows))]
pub(super) fn configure_platform_shell(cmd: &mut Command, command: &str) {
    cmd.arg("-c").arg(command);
}

#[cfg(unix)]
pub(super) fn kill_process_group(pid: Option<u32>) {
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    unsafe {
        let _ = kill(-pid, SIGKILL);
    }
}

#[cfg(not(unix))]
pub(super) fn kill_process_group(_pid: Option<u32>) {}

impl BackgroundShellState {
    /// Synchronously SIGKILL the shell's process group. Used by `SessionState`'s
    /// `Drop` so background children (and their descendants) don't leak when the
    /// session ends — the async `kill_tx` path can't be driven during teardown.
    /// Skips a shell already known to have finished, to avoid a pid-reuse race.
    pub(crate) fn kill_now(&self) {
        // Only SIGKILL the cached process group while holding the status lock and
        // confirming the child is still Running. A failed `try_lock` (the waiter
        // is mid-update) or any terminal status means the child may already be
        // reaped and its pid recycled, so killing the cached pgid could hit an
        // unrelated same-uid process group. (The old code killed regardless when
        // the lock was contended.) The residual reap→status-flip window is
        // irreducible without pidfd; this removes the larger cases.
        let Ok(status) = self.status.try_lock() else {
            return;
        };
        if matches!(*status, BgStatus::Running) {
            kill_process_group(self.pid);
        }
    }
}

/// Per-stream cap on background-shell output retention. A command like
/// `yes` or `dd if=/dev/urandom` previously filled memory at gigabytes
/// per minute because the Vec<u8> was never truncated. We retain the
/// most recent 4 MiB and drop older bytes; the cursor is adjusted so
/// already-returned bytes stay accounted for.
const BG_BUFFER_MAX_BYTES: usize = 4 * 1_048_576;
pub(super) const FOREGROUND_OUTPUT_MAX_BYTES_PER_STREAM: usize = 256 * 1024;

#[derive(Debug, Default)]
pub(super) struct CappedOutput {
    bytes: Vec<u8>,
    dropped_bytes: usize,
}

pub(super) async fn read_capped_output<R>(mut reader: R, cap: usize) -> CappedOutput
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut out = CappedOutput::default();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => append_tail_capped(&mut out.bytes, &mut out.dropped_bytes, &buf[..n], cap),
            Err(e) => {
                let msg = format!("\n[opencli: failed to read process output: {e}]");
                append_tail_capped(&mut out.bytes, &mut out.dropped_bytes, msg.as_bytes(), cap);
                break;
            }
        }
    }
    out
}

fn append_tail_capped(buf: &mut Vec<u8>, dropped_bytes: &mut usize, chunk: &[u8], cap: usize) {
    if cap == 0 {
        *dropped_bytes = dropped_bytes.saturating_add(chunk.len());
        return;
    }
    buf.extend_from_slice(chunk);
    if buf.len() > cap {
        let drop_n = buf.len() - cap;
        buf.drain(..drop_n);
        *dropped_bytes = dropped_bytes.saturating_add(drop_n);
    }
}

pub(super) fn format_capped_stream(label: &str, out: CappedOutput) -> String {
    let body = String::from_utf8_lossy(&out.bytes);
    if out.dropped_bytes == 0 {
        return body.into_owned();
    }
    format!(
        "<system-reminder>{label} truncated: omitted {} byte(s) from the start, showing the last {} byte(s). Redirect noisy output to a file and inspect smaller slices if you need the omitted content.</system-reminder>\n{body}",
        out.dropped_bytes,
        out.bytes.len()
    )
}

/// Append `chunk`, then truncate from the front if `buf` exceeds the cap.
/// Locks are acquired in the order (buf, cursor); the reader follows the
/// same order to avoid deadlock and to close the buf-then-cursor race
/// that previously let appends slip between the reader's two locks.
pub(super) async fn append_capped(
    buf: &tokio::sync::Mutex<Vec<u8>>,
    cursor: &tokio::sync::Mutex<usize>,
    chunk: &[u8],
) {
    let mut b = buf.lock().await;
    let mut c = cursor.lock().await;
    b.extend_from_slice(chunk);
    if b.len() > BG_BUFFER_MAX_BYTES {
        let drop_n = b.len() - BG_BUFFER_MAX_BYTES;
        b.drain(..drop_n);
        *c = c.saturating_sub(drop_n);
    }
}

/// Decode newly-appended bytes from `buf[*cursor..]` as UTF-8, advancing the
/// cursor only past complete characters. A multi-byte sequence split across a
/// `bash_output` read boundary is left in the buffer for the next read instead
/// of being mangled into U+FFFD (the previous code advanced the cursor to
/// `buf.len()` and lossy-decoded the partial tail). A genuinely invalid byte is
/// still consumed lossily so a single bad byte can't stall the stream forever.
pub(super) fn drain_utf8(buf: &[u8], cursor: &mut usize) -> String {
    let start = (*cursor).min(buf.len());
    let slice = &buf[start..];
    let take = match std::str::from_utf8(slice) {
        Ok(_) => slice.len(),
        // Incomplete trailing sequence: stop at the last complete char.
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        // Genuinely invalid byte(s): include them so we make progress.
        Err(e) => e.valid_up_to() + e.error_len().unwrap(),
    };
    let out = String::from_utf8_lossy(&slice[..take]).into_owned();
    *cursor = start + take;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_utf8_keeps_split_multibyte_for_next_read() {
        // "é" (0xC3 0xA9) arriving split across two reads must not be mangled.
        let mut buf = vec![0xC3u8];
        let mut cursor = 0usize;
        assert_eq!(drain_utf8(&buf, &mut cursor), "");
        assert_eq!(cursor, 0, "partial trailing byte must stay in the buffer");
        buf.push(0xA9);
        assert_eq!(drain_utf8(&buf, &mut cursor), "é");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn drain_utf8_progresses_past_an_invalid_byte() {
        // A complete prefix is emitted; a lone invalid byte is consumed lossily
        // so it can't stall the stream.
        let mut buf = b"ab".to_vec();
        let mut cursor = 0usize;
        assert_eq!(drain_utf8(&buf, &mut cursor), "ab");
        assert_eq!(cursor, 2);
        buf.push(0xFF);
        assert_eq!(drain_utf8(&buf, &mut cursor), "\u{FFFD}");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn append_tail_capped_retains_recent_bytes_and_counts_dropped() {
        let mut buf = Vec::new();
        let mut dropped = 0usize;

        append_tail_capped(&mut buf, &mut dropped, b"abcdef", 4);
        append_tail_capped(&mut buf, &mut dropped, b"gh", 4);

        assert_eq!(buf, b"efgh");
        assert_eq!(dropped, 4);
    }
}

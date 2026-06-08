//! Shared filesystem helpers: sandbox path resolution, atomic writes,
//! and read-staleness snapshots. Split out of `fs`; logic unchanged.

use std::ffi::OsString;

use anyhow::{anyhow, Context, Result};
use base64::Engine;

pub(crate) fn rand_suffix() -> String {
    use rand::RngCore;
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub(crate) async fn atomic_write_preserving_permissions(
    path: &std::path::Path,
    tmp: &std::path::Path,
    bytes: &[u8],
) -> Result<()> {
    let permissions = tokio::fs::metadata(path)
        .await
        .ok()
        .map(|meta| meta.permissions());
    // Any failure after the temp file is created (a partial write, a failed
    // permission set, or a cross-device/EISDIR rename) must not leave a stray
    // `.tmp` sibling behind to accumulate on disk.
    let result = write_temp_then_swap(path, tmp, bytes, permissions).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(tmp).await;
    }
    result
}

async fn write_temp_then_swap(
    path: &std::path::Path,
    tmp: &std::path::Path,
    bytes: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> Result<()> {
    tokio::fs::write(tmp, bytes)
        .await
        .with_context(|| format!("write temp {}", tmp.display()))?;
    if let Some(permissions) = permissions {
        tokio::fs::set_permissions(tmp, permissions)
            .await
            .with_context(|| format!("set permissions on temp {}", tmp.display()))?;
    }
    tokio::fs::rename(tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Snapshots (mtime, size) used by every edit/write tool immediately after a
/// successful write, and by `UndoLastEdit` to detect post-edit modifications
/// before restoring. Both come from one `metadata()` call so they're
/// consistent. Comparing size as well as mtime catches same-second external
/// edits a coarse mtime alone would miss.
pub(crate) fn snapshot_meta(
    path: &std::path::Path,
) -> (Option<std::time::SystemTime>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.modified().ok(), Some(m.len())),
        Err(_) => (None, None),
    }
}

/// Refuse an edit/overwrite when the file changed on disk since the model last
/// read it this session, forcing a fresh `read_file` so the edit targets the
/// current bytes. The caller has already confirmed the path was read this
/// session; this adds the "still fresh?" half. Best-effort: only fires when a
/// read-time snapshot exists — a resumed session has none and falls back to the
/// plain read-once guard. A snapshot recorded after the model's own write/edit
/// keeps back-to-back edits from tripping the check.
pub(crate) fn ensure_not_stale(
    session: &crate::tools::SessionState,
    path: &std::path::Path,
    tool: &str,
) -> Result<()> {
    if let Some(recorded) = session.read_file_meta.get(path) {
        if snapshot_meta(path) != *recorded {
            return Err(anyhow!(
                "{tool}: {} changed on disk since you read it. Call read_file on it again so your edit matches the current bytes (it may contain changes you haven't seen).",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Resolve a model-supplied path against the sandbox `cwd`. Accepts either a
/// relative path or an absolute path that is lexically inside
/// `cwd`. Rejects absolute paths outside `cwd`, lexical `..` escapes, and
/// symlinks whose resolved target leaves `cwd`. Without this guard the LLM
/// could read `/etc/shadow`, write to `~/.ssh/authorized_keys`, or otherwise
/// escape the working tree.
pub(crate) fn resolve(cwd: &std::path::Path, p: &str) -> Result<std::path::PathBuf> {
    let raw_path = std::path::Path::new(p);
    let sandbox = cwd
        .canonicalize()
        .with_context(|| format!("resolve sandbox cwd {}", cwd.display()))?;
    let path = if raw_path.is_absolute() {
        let absolute = canonicalize_with_missing(raw_path)
            .with_context(|| format!("resolve {}", raw_path.display()))?;
        absolute
            .strip_prefix(&sandbox)
            .map_err(|_| {
                anyhow!(
                    "absolute path escapes the sandbox (cwd {}): {}",
                    sandbox.display(),
                    raw_path.display()
                )
            })?
            .to_path_buf()
    } else {
        raw_path.to_path_buf()
    };
    let mut normalized = std::path::PathBuf::new();
    for comp in path.components() {
        use std::path::Component;
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(anyhow!("path escapes the sandbox: {}", path.display()));
                }
            }
            Component::Normal(s) => normalized.push(s),
            Component::Prefix(_) | Component::RootDir => {
                return Err(anyhow!("invalid path component: {}", raw_path.display()));
            }
        }
    }

    let mut existing = sandbox.clone();
    let mut missing: Vec<OsString> = Vec::new();
    let mut found_missing = false;
    for comp in normalized.components() {
        let name = comp.as_os_str();
        if found_missing {
            missing.push(name.to_os_string());
            continue;
        }
        let next = existing.join(name);
        match std::fs::symlink_metadata(&next) {
            Ok(_) => existing = next,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                found_missing = true;
                missing.push(name.to_os_string());
            }
            Err(e) => return Err(e).with_context(|| format!("stat {}", next.display())),
        }
    }

    let resolved_existing = existing
        .canonicalize()
        .with_context(|| format!("resolve {}", existing.display()))?;
    if !resolved_existing.starts_with(&sandbox) {
        return Err(anyhow!("path escapes the sandbox: {}", path.display()));
    }

    let mut resolved = resolved_existing;
    for comp in missing {
        resolved.push(comp);
    }
    Ok(resolved)
}

fn canonicalize_with_missing(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();

    loop {
        match existing.canonicalize() {
            Ok(mut resolved) => {
                for comp in missing.iter().rev() {
                    resolved.push(comp);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| anyhow!("path has no existing parent: {}", path.display()))?;
                missing.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| anyhow!("path has no existing parent: {}", path.display()))?
                    .to_path_buf();
            }
            Err(e) => {
                return Err(e).with_context(|| format!("canonicalize {}", existing.display()))
            }
        }
    }
}

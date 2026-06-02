//! Conversation session persistence.
//!
//! Each chat session is stored as a single JSON file under
//! `~/.config/opencli/sessions/<cwd-slug>/<session-id>.json` so that
//! `opencli resume` and `/resume` can rehydrate prior conversations.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::openai::{InputItem, MessageContent};
use crate::tools::TodoItem;

/// Lightweight metadata describing a stored session — enough to render in a
/// picker without paying the cost of loading the full history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub cwd: PathBuf,
    pub model: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub message_count: usize,
    /// First user message, trimmed to a single line for the picker preview.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    #[serde(flatten)]
    pub meta: SessionMeta,
    /// Runtime state that is safe to resume across processes. Handles such as
    /// background shell processes and undo snapshots are intentionally not
    /// persisted because they cannot be reconstructed safely after restart.
    #[serde(default)]
    pub state: SessionSnapshot,
    pub history: Vec<InputItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub read_files: Vec<PathBuf>,
    #[serde(default)]
    pub active_goal: Option<SessionGoalSnapshot>,
    /// Cumulative billed token counts per model, so `/cost` survives a `/resume`.
    #[serde(default)]
    pub usage: Vec<ModelUsage>,
}

/// Cumulative billed token counts for one model within a session, split by
/// billing class. Cached reads and cache writes are priced very differently
/// from fresh input, so they are tracked separately for an accurate `/cost`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    /// Fresh (uncached) input tokens, billed at the model's input rate.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tokens served from the prompt cache, billed at the cheap cache-read rate.
    pub cache_read_tokens: u64,
    /// Tokens written into the prompt cache, billed at the cache-write rate.
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGoalSnapshot {
    pub objective: String,
    pub turns_completed: u32,
    #[serde(default)]
    pub waiting_for_user: bool,
    #[serde(default)]
    pub last_summary: Option<String>,
    #[serde(default)]
    pub started_at_ms: u64,
}

pub fn sessions_root() -> PathBuf {
    config_dir().join("sessions")
}

/// Per-cwd sub-directory so picking a session only shows ones started in this
/// project. The cwd is hashed to keep filenames sane (long absolute paths
/// would otherwise blow past common filesystem limits).
pub fn sessions_dir_for(cwd: &Path) -> PathBuf {
    sessions_root().join(slug_for(cwd))
}

fn slug_for(cwd: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    let h = hasher.finish();
    let base = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("root")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect::<String>();
    if base.is_empty() {
        format!("{h:x}")
    } else {
        format!("{base}-{h:x}")
    }
}

static SEQ: AtomicU64 = AtomicU64::new(0);
static SAVE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn new_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    // Mix the process id back in. The per-process SEQ alone collides across two
    // processes started in the same millisecond (each begins at seq 0), and a
    // collision lets one session's save() rename over the other's file, losing
    // history. pid in the high 32 bits + seq in the low 32 keeps ids unique both
    // within and across processes.
    let mix = ((std::process::id() as u64) << 32) | (seq & 0xFFFF_FFFF);
    format!("{now:x}-{mix:x}")
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Extract the first user message as a single-line preview.
pub fn derive_preview(history: &[InputItem]) -> String {
    for item in history {
        if let InputItem::Message { role, content } = item {
            if role == "user" {
                let mut s = String::new();
                for c in content {
                    if let MessageContent::InputText { text } = c {
                        s.push_str(text);
                        break;
                    }
                }
                let one = s.replace('\n', " ");
                let trimmed = one.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed.chars().count() > 80 {
                    return format!("{}…", trimmed.chars().take(79).collect::<String>());
                }
                return trimmed.to_string();
            }
        }
    }
    "(empty session)".to_string()
}

pub fn save(record: &SessionRecord) -> std::io::Result<()> {
    validate_session_id(&record.meta.id)?;
    let dir = sessions_dir_for(&record.meta.cwd);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", record.meta.id));
    let tmp = unique_tmp_path(&path);
    let text = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_session_file(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    // fsync the parent directory so the rename itself is durable. Without this,
    // a crash/power-loss right after rename returns can lose the new directory
    // entry and drop the just-saved session even though its bytes were flushed.
    fsync_dir(&dir);
    Ok(())
}

/// Best-effort fsync of a directory so a preceding rename is durable across a
/// crash. A no-op on platforms where directory fsync isn't supported.
fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(f) = std::fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

fn write_session_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    // Owner-only perms are Unix-specific; other platforms inherit the dir ACL.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    // fsync the staging file before save() renames it over the target, so a
    // crash can't leave a renamed-but-unflushed (empty/partial) session. Now
    // applied on every platform, not just Unix.
    f.sync_all()?;
    Ok(())
}

fn validate_session_id(id: &str) -> std::io::Result<()> {
    let is_valid = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if is_valid {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid session id",
        ))
    }
}

fn unique_tmp_path(path: &Path) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SAVE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp.{}.{}.{}", std::process::id(), now, seq))
}

pub fn load(cwd: &Path, id: &str) -> std::io::Result<SessionRecord> {
    validate_session_id(id)?;
    let path = sessions_dir_for(cwd).join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// List sessions for a given cwd, newest-first. Corrupt files are skipped.
pub fn list(cwd: &Path) -> Vec<SessionMeta> {
    let dir = sessions_dir_for(cwd);
    let mut out: Vec<SessionMeta> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(rec) = serde_json::from_str::<SessionRecord>(&text) {
            let file_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if validate_session_id(&rec.meta.id).is_err() || rec.meta.id != file_id {
                continue;
            }
            out.push(rec.meta);
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.updated_at_ms));
    out
}

#[cfg(test)]
mod tests {
    use super::unique_tmp_path;
    use std::path::PathBuf;

    #[test]
    fn save_temp_paths_are_unique() {
        let path = PathBuf::from("session.json");
        assert_ne!(unique_tmp_path(&path), unique_tmp_path(&path));
    }
}

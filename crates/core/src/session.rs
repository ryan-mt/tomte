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
    pub history: Vec<InputItem>,
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
    let dir = sessions_dir_for(&record.meta.cwd);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", record.meta.id));
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

pub fn load(cwd: &Path, id: &str) -> std::io::Result<SessionRecord> {
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
            out.push(rec.meta);
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.updated_at_ms));
    out
}

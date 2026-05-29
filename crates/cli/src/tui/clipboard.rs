//! Clipboard paste support for the TUI.
//!
//! Two outcomes when the user presses Ctrl+V:
//!   - Image on clipboard → encode PNG to a temp file, return the path
//!   - Text only → return the text
//!
//! Result is consumed by the App: image paths get attached as pending
//! image attachments and a `[Image #N]` marker is inserted into the input;
//! text is inserted at the cursor.
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;

static CLIPBOARD_IMAGE_SEQ: AtomicU64 = AtomicU64::new(0);

pub enum PasteResult {
    Image(PathBuf),
    Text(String),
    Empty,
}

/// Try to read whatever is on the system clipboard, preferring image data.
pub fn try_paste() -> Result<PasteResult> {
    let mut clip = Clipboard::new().map_err(|e| anyhow!("cannot access clipboard: {e}"))?;

    // Try image first.
    if let Ok(img) = clip.get_image() {
        let path = save_image(img).context("encoding clipboard image")?;
        return Ok(PasteResult::Image(path));
    }

    // Fall back to text.
    match clip.get_text() {
        Ok(t) if !t.is_empty() => Ok(PasteResult::Text(t)),
        _ => Ok(PasteResult::Empty),
    }
}

fn save_image(img: arboard::ImageData<'_>) -> Result<PathBuf> {
    let width = img.width as u32;
    let height = img.height as u32;
    // `arboard::ImageData::bytes` is RGBA8.
    let raw: Vec<u8> = img.bytes.into_owned();
    let buf = image::RgbaImage::from_raw(width, height, raw)
        .ok_or_else(|| anyhow!("clipboard image dimensions don't match buffer"))?;

    let mut png_bytes: Vec<u8> = Vec::new();
    {
        let mut cursor = Cursor::new(&mut png_bytes);
        buf.write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| anyhow!("encoding PNG: {e}"))?;
    }

    let dir = opencli_core::config::config_dir().join("clipboard");
    std::fs::create_dir_all(&dir).ok();
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f").to_string();
    let path = clipboard_image_path(&dir, &ts);
    std::fs::write(&path, &png_bytes).context("writing clipboard PNG")?;
    Ok(path)
}

fn clipboard_image_path(dir: &Path, timestamp: &str) -> PathBuf {
    let seq = CLIPBOARD_IMAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "paste-{timestamp}-{}-{seq}.png",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_image_paths_are_unique_within_same_millisecond() {
        let dir = Path::new("/tmp/opencli-clipboard-test");
        let timestamp = "20260101-000000.000";

        let first = clipboard_image_path(dir, timestamp);
        let second = clipboard_image_path(dir, timestamp);

        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(dir));
        assert_eq!(second.parent(), Some(dir));
    }
}

//! Clipboard paste support for the TUI.
//!
//! Two outcomes when the user pastes (Ctrl+V / Alt+V):
//!   - Image on clipboard → encode PNG to a temp file, return the path
//!   - Text only → return the text
//!
//! Result is consumed by the App: image paths get attached as pending
//! image attachments and a `[Image #N]` marker is inserted into the input;
//! text is inserted at the cursor.
//!
//! Image reading is per-OS, mirroring how Claude Code reads clipboard images
//! across platforms:
//!   - Windows: shell out to PowerShell's `System.Windows.Forms.Clipboard`.
//!     arboard's CF_DIB reader misses Snip & Sketch / screenshot bitmaps, so we
//!     use `GetImage()` → `Save(.., Png)` the way Claude Code does.
//!   - macOS / Linux: arboard's native `get_image()` (NSPasteboard /
//!     X11 + Wayland) already decodes clipboard images without an external
//!     binary, so we keep it there.
//!
//! Text is read through arboard on every platform.
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
    if let Some(path) = try_clipboard_image()? {
        return Ok(PasteResult::Image(path));
    }
    // Text fallback (arboard handles text on every platform).
    let mut clip = Clipboard::new().map_err(|e| anyhow!("cannot access clipboard: {e}"))?;
    match clip.get_text() {
        Ok(t) if !t.is_empty() => Ok(PasteResult::Text(t)),
        _ => Ok(PasteResult::Empty),
    }
}

/// Read a clipboard image to a temp PNG, returning its path (`None` when the
/// clipboard holds no image). Windows shells out to PowerShell because
/// arboard's CF_DIB reader misses screenshot bitmaps; other platforms use
/// arboard's native image support directly.
#[cfg(windows)]
fn try_clipboard_image() -> Result<Option<PathBuf>> {
    use std::process::Command;

    let path = new_clipboard_image_path();
    // System.Windows.Forms.Clipboard::GetImage() decodes the bitmap (incl.
    // Snip & Sketch / screenshots that arboard misses); `exit 1` = no image.
    // This is the mechanism Claude Code uses on Windows. The destination path
    // is passed via an env var so paths with quotes/backslashes need no
    // escaping inside the script.
    let script = "Add-Type -AssemblyName System.Windows.Forms; \
                  $img = [System.Windows.Forms.Clipboard]::GetImage(); \
                  if ($null -eq $img) { exit 1 }; \
                  $img.Save($env:TOMTE_CLIP_OUT, [System.Drawing.Imaging.ImageFormat]::Png)";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("TOMTE_CLIP_OUT", &path)
        .output()
        .context("running powershell to read the clipboard image")?;
    // exit 0 + file written → an image was pasted; exit 1 (no image) or any
    // failure → nothing to paste as an image (fall through to text).
    if output.status.success() && path.is_file() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

#[cfg(not(windows))]
fn try_clipboard_image() -> Result<Option<PathBuf>> {
    let mut clip = Clipboard::new().map_err(|e| anyhow!("cannot access clipboard: {e}"))?;
    match clip.get_image() {
        Ok(img) => Ok(Some(save_image(img)?)),
        // No image on the clipboard (or an unsupported format) → fall through
        // to text rather than surfacing an error.
        Err(_) => Ok(None),
    }
}

/// Copy text to the system clipboard (used by mouse text selection).
pub fn copy_text(text: &str) -> Result<()> {
    let mut clip = Clipboard::new().map_err(|e| anyhow!("cannot access clipboard: {e}"))?;
    clip.set_text(text.to_string())
        .map_err(|e| anyhow!("cannot set clipboard text: {e}"))
}

/// Encode an arboard RGBA image to a temp PNG, returning its path.
#[cfg(not(windows))]
fn save_image(img: arboard::ImageData<'_>) -> Result<PathBuf> {
    use std::io::Cursor;

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

    let path = new_clipboard_image_path();
    std::fs::write(&path, &png_bytes).context("writing clipboard PNG")?;
    Ok(path)
}

/// Build a unique temp path for a pasted clipboard image, creating the parent
/// `clipboard` dir under the config directory.
fn new_clipboard_image_path() -> PathBuf {
    let dir = tomte_core::config::config_dir().join("clipboard");
    std::fs::create_dir_all(&dir).ok();
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f").to_string();
    clipboard_image_path(&dir, &ts)
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
        let dir = Path::new("/tmp/tomte-clipboard-test");
        let timestamp = "20260101-000000.000";

        let first = clipboard_image_path(dir, timestamp);
        let second = clipboard_image_path(dir, timestamp);

        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(dir));
        assert_eq!(second.parent(), Some(dir));
    }
}

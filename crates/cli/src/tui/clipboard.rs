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
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use arboard::Clipboard;

pub enum PasteResult {
    Image(PathBuf),
    Text(String),
    Empty,
}

/// Try to read whatever is on the system clipboard, preferring image data.
pub fn try_paste() -> Result<PasteResult> {
    let mut clip =
        Clipboard::new().map_err(|e| anyhow!("cannot access clipboard: {e}"))?;

    // Try image first.
    match clip.get_image() {
        Ok(img) => {
            let path = save_image(img).context("encoding clipboard image")?;
            return Ok(PasteResult::Image(path));
        }
        Err(_) => {}
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
    let path = dir.join(format!("paste-{ts}.png"));
    std::fs::write(&path, &png_bytes).context("writing clipboard PNG")?;
    Ok(path)
}

use super::*;

/// One-line summary for a binary file that `read_file` can't show as text —
/// the kind (sniffed from `sniff`'s leading magic bytes) and `size`, plus how to
/// view an image. `size` is passed explicitly so the large-file path can report
/// the true file length even though it only sniffs a leading chunk.
pub(super) fn describe_binary(display_path: &str, sniff: &[u8], size: u64) -> String {
    let kind = sniff_binary_kind(sniff);
    let is_image = matches!(
        kind,
        "PNG image" | "JPEG image" | "GIF image" | "WebP image"
    );
    let hint = if is_image {
        " To have the model see it, attach it with /img."
    } else {
        ""
    };
    format!(
        "<system-reminder>`{}` is a {} ({} bytes); read_file shows text only, not its contents. \
         It is recorded as read, so write_file may overwrite it if you intend to replace it.{}</system-reminder>\n",
        display_path,
        kind,
        size,
        hint
    )
}

/// Best-effort binary type from leading magic bytes (more reliable than the
/// extension). Falls back to a generic label.
pub(super) fn sniff_binary_kind(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "PNG image"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "JPEG image"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "GIF image"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "WebP image"
    } else if bytes.starts_with(b"%PDF-") {
        "PDF document"
    } else {
        "binary file"
    }
}

/// True when a leading sniff window contains a genuinely invalid UTF-8 byte
/// (i.e. binary), as opposed to a multibyte char merely truncated at the window
/// boundary. `error_len() == Some(_)` means an invalid byte strictly inside the
/// window; `None` means the window ended mid-character, which a text file can do.
pub(super) fn leading_bytes_are_binary(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => false,
        Err(e) => e.error_len().is_some(),
    }
}

pub(super) fn bytes_to_line(bytes: &[u8], was_byte_truncated: bool) -> Result<String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) if was_byte_truncated && e.valid_up_to() > 0 => {
            std::str::from_utf8(&bytes[..e.valid_up_to()])?
        }
        Err(e) => return Err(anyhow!("file is not valid UTF-8: {e}")),
    };
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

use super::*;

pub(super) fn read_large_text_slice(
    path: &std::path::Path,
    display_path: &str,
    file_len: u64,
    start: usize,
    limit: usize,
    max_line_chars: usize,
) -> Result<(String, bool)> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);

    // Detect a binary/non-UTF-8 file up front and describe it (matching the
    // small-file path) instead of erroring deep in the line loop. `fill_buf`
    // peeks the leading chunk without consuming it, so the line loop below still
    // reads from the start. The true size is `file_len`, not the chunk length.
    let head = reader
        .fill_buf()
        .with_context(|| format!("read {}", path.display()))?;
    if leading_bytes_are_binary(head) {
        let head = head.to_vec();
        // UTF-16 text too large to decode in one piece: name the encoding and
        // keep it NON-overwritable — it has real contents the model never saw.
        if has_utf16_bom(&head) {
            return Ok((
                format!(
                    "<system-reminder>`{display_path}` is UTF-16 text ({file_len} bytes), too large to decode here; convert it to UTF-8 (or read a smaller copy) to see its contents.</system-reminder>\n"
                ),
                false,
            ));
        }
        // A binary file's whole content is "recorded as read".
        return Ok((describe_binary(display_path, &head, file_len), true));
    }

    let mut out = String::new();
    let mut line_no = 0usize;
    let mut printed = 0usize;
    let mut hit_limit = false;
    let mut any_line_truncated = false;
    let max_line_bytes = max_line_chars.saturating_mul(4);
    let cap = read_output_byte_cap();

    while let Some((bytes, was_byte_truncated)) =
        read_next_line_capped(&mut reader, max_line_bytes)?
    {
        if line_no >= start {
            if printed >= limit {
                hit_limit = true;
                out.push_str(&format!(
                    "<system-reminder>Showing a slice of large file `{display_path}`. More lines remain — call read_file again with offset={line_no} and an explicit limit to continue.</system-reminder>\n"
                ));
                break;
            }
            // A >5MB file whose leading sniff looked like text can still carry an
            // invalid UTF-8 byte deeper in (e.g. a binary blob appended to a
            // log). Render that line lossily instead of `?`-failing the entire
            // read — the small-file path degrades to a binary description, so a
            // single stray byte shouldn't wedge an otherwise-readable file.
            let text = bytes_to_line(&bytes, was_byte_truncated).unwrap_or_else(|_| {
                String::from_utf8_lossy(&bytes)
                    .trim_end_matches(['\r', '\n'])
                    .to_string()
            });
            if was_byte_truncated || text.chars().count() > max_line_chars {
                any_line_truncated = true;
            }
            out.push_str(&numbered_line(
                line_no + 1,
                &text,
                was_byte_truncated,
                max_line_chars,
            ));
            printed += 1;
            // Same token ceiling as the small-file path: stop once the rendered
            // output passes the cap so a slice of long lines can't flood context.
            if out.len() >= cap {
                hit_limit = true;
                out.push_str(&format!(
                    "<system-reminder>Showing a slice of large file `{display_path}` (stopped early to keep this read token-bounded). More lines remain — call read_file again with offset={} and an explicit limit to continue.</system-reminder>\n",
                    line_no + 1
                ));
                break;
            }
        }
        line_no = line_no.saturating_add(1);
    }
    // Full only if we read from the very top all the way to EOF and no line
    // was cut — a truncated line is content the model never saw, so the file
    // must not become overwritable on the strength of this read.
    let fully_read = start == 0 && !hit_limit && !any_line_truncated;
    Ok((out, fully_read))
}

pub(super) fn read_next_line_capped<R: std::io::BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    let mut out = Vec::new();
    let mut truncated = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return if out.is_empty() && !truncated {
                Ok(None)
            } else {
                Ok(Some((out, truncated)))
            };
        }
        let newline = buf.iter().position(|b| *b == b'\n');
        let take_len = newline.map(|i| i + 1).unwrap_or(buf.len());
        let chunk = &buf[..take_len];
        if !truncated {
            let remaining = max_bytes.saturating_sub(out.len());
            if chunk.len() <= remaining {
                out.extend_from_slice(chunk);
            } else {
                out.extend_from_slice(&chunk[..remaining]);
                truncated = true;
            }
        }
        reader.consume(take_len);
        if newline.is_some() {
            return Ok(Some((out, truncated)));
        }
    }
}

//! The `read_file` tool and its text/binary rendering helpers. Split out of
//! `fs`; logic unchanged.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::tools::{BuiltinTool, ToolContext};

use super::common::resolve;

pub struct ReadFile;

#[derive(Deserialize)]
struct ReadArgs {
    #[serde(alias = "file_path", alias = "filePath")]
    path: String,
    #[serde(default, deserialize_with = "crate::tools::deserialize_optional_usize")]
    offset: Option<usize>,
    #[serde(default, deserialize_with = "crate::tools::deserialize_optional_usize")]
    limit: Option<usize>,
}

#[async_trait]
impl BuiltinTool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a text file from the working directory. Returns the file contents with line numbers in the format `<lineno>\\t<content>` per line. Line numbers start at 1 and are right-padded so columns stay aligned.\n\
\n\
When to use:\n\
- ALWAYS call this before `edit_file` or `multi_edit` — those tools need the exact existing bytes, and guessing wastes a turn.\n\
- When you need to understand what a file does, cite a specific line, or verify the result of an edit.\n\
- Prefer reading the whole file when feasible; reach for `offset` + `limit` only on truly large files.\n\
\n\
When NOT to use:\n\
- Don't read a directory — use `list_dir` or `glob`.\n\
- Don't read to search across many files — use `grep`.\n\
- Don't shell out to `cat` instead of this tool; this tool returns structured output with line numbers.\n\
\n\
Common mistakes:\n\
- Skipping the read and going straight to `edit_file` — your `old_string` will not match.\n\
- Re-reading a file you just read this turn — the contents are already in context.\n\
\n\
Parameters:\n\
- `path`: Relative path inside the working directory. Absolute paths and `..` traversal are rejected.\n\
- `offset`: Zero-indexed line to start reading from, or `null` to start at the top.\n\
- `limit`: Maximum number of lines to return (1..=2000), or `null` to use the default cap.\n\
\n\
Output rules:\n\
- Default cap is 2000 lines per call when `limit` is null; the response includes a truncation notice telling you how to read the next slice with `offset` + `limit`.\n\
- Lines longer than 2000 characters are truncated and marked `… [line truncated]` so a minified file can't blow out your context window.\n\
- An empty file returns a `<system-reminder>` warning instead of a blank string, so you don't assume the read failed.\n\
- A Jupyter `.ipynb` read whole is rendered as cells (ids + text outputs; images omitted) instead of raw JSON, so you can cite a cell and edit it with `notebook_edit`; pass `offset`/`limit` to read the raw JSON slice instead.\n\
\n\
Constraints: files larger than 5 MB must be read with an explicit `limit`. Binary files are not supported by this tool — use `grep` or `run_shell` (e.g. `file`, `hexdump`) for non-text artefacts."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path inside the working directory."},
                "offset": {"type": ["integer", "null"], "description": "Zero-indexed starting line; null starts at the top."},
                "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 2000, "description": "Maximum number of lines to return; null uses the default cap."}
            },
            "required": ["path", "offset", "limit"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let a: ReadArgs = crate::tools::parse_args("read_file", args)?;
        let path = resolve(&ctx.cwd, &a.path)?;
        // Bound the read so the LLM can't request /dev/zero or a multi-GB log
        // and OOM the process.
        const MAX_BYTES: u64 = 5_000_000;
        // Default lines-per-call when caller does not pass `limit`. Matches
        // Claude Code's Read tool — keeps a single read from flooding the
        // context window with a large file.
        const DEFAULT_LINE_LIMIT: usize = 2000;
        const MAX_LINE_LIMIT: usize = DEFAULT_LINE_LIMIT;
        // Per-line truncation so a minified bundle (one giant line) can't
        // blow out the context. Mirrors Claude Code's 2000-char-per-line cap.
        const MAX_LINE_CHARS: usize = 2000;

        let meta = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta,
            // A clear "not found" beats leaking the `stat` syscall name, and
            // tells the model the path is wrong rather than the tool being broken.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!("file not found: {}", a.path));
            }
            Err(e) => return Err(e).with_context(|| format!("read {}", a.path)),
        };
        if a.limit == Some(0) {
            return Err(anyhow!("limit must be greater than 0"));
        }
        if a.limit.is_some_and(|limit| limit > MAX_LINE_LIMIT) {
            return Err(anyhow!("limit must be <= {MAX_LINE_LIMIT}"));
        }
        if meta.len() > MAX_BYTES && a.limit.is_none() {
            return Err(anyhow!(
                "file is too large ({} bytes > {} byte cap); pass `limit` to read a slice",
                meta.len(),
                MAX_BYTES
            ));
        }
        let start = a.offset.unwrap_or(0);
        let effective_limit = a.limit.unwrap_or(DEFAULT_LINE_LIMIT);
        let (out, fully_read) = if meta.len() > MAX_BYTES {
            read_large_text_slice(
                &path,
                &a.path,
                meta.len(),
                start,
                effective_limit,
                MAX_LINE_CHARS,
            )?
        } else {
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("read {}", path.display()))?;
            match String::from_utf8(bytes) {
                // A whole-file read of a Jupyter notebook is rendered as cells
                // (like Claude Code's Read) rather than dumped as raw JSON; a
                // sliced read (offset/limit) still returns the raw JSON. A parse
                // failure falls through to the plain-text reader.
                Ok(text)
                    if a.path.ends_with(".ipynb") && a.offset.is_none() && a.limit.is_none() =>
                {
                    match render_notebook(&a.path, &text) {
                        Some(rendered) => (rendered, true),
                        None => {
                            render_text_read(&a.path, &text, start, effective_limit, MAX_LINE_CHARS)
                        }
                    }
                }
                Ok(text) => {
                    render_text_read(&a.path, &text, start, effective_limit, MAX_LINE_CHARS)
                }
                Err(e) => {
                    let raw = e.as_bytes();
                    let size = raw.len() as u64;
                    // A binary file's whole content is "recorded as read": the
                    // model can't see it but may intend to replace it wholesale.
                    (describe_binary(&a.path, raw, size), true)
                }
            }
        };
        record_successful_read(ctx, &path, &meta, fully_read).await;
        Ok(out)
    }
}

async fn record_successful_read(
    ctx: &ToolContext,
    path: &std::path::Path,
    meta: &std::fs::Metadata,
    fully_read: bool,
) {
    let mut session = ctx.session.lock().await;
    session.read_files.insert(path.to_path_buf());
    // Only a full read lets write_file overwrite; a partial (offset/limit) read
    // drops the file back out so it can't discard content the model never saw.
    if fully_read {
        session.fully_read_files.insert(path.to_path_buf());
    } else {
        session.fully_read_files.remove(path);
    }
    session
        .read_file_meta
        .insert(path.to_path_buf(), (meta.modified().ok(), Some(meta.len())));
}

fn render_text_read(
    display_path: &str,
    text: &str,
    start: usize,
    limit: usize,
    max_line_chars: usize,
) -> (String, bool) {
    if text.is_empty() {
        let msg = format!(
            "<system-reminder>The file `{display_path}` exists but is empty.</system-reminder>\n"
        );
        return (msg, true);
    }
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = start.min(total);
    let end = start.saturating_add(limit).min(total);
    let mut out = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        out.push_str(&numbered_line(start + i + 1, line, false, max_line_chars));
    }
    if end < total {
        let remaining = total - end;
        out.push_str(&format!(
            "<system-reminder>Showing lines {}-{} of {}. {} more line(s) remain — call read_file again with offset={} and an explicit limit to continue.</system-reminder>\n",
            start + 1,
            end,
            total,
            remaining,
            end
        ));
    }
    // A full read starts at the top and reaches the last line untruncated.
    let fully_read = start == 0 && end >= total;
    (out, fully_read)
}

/// Render a Jupyter `.ipynb` (nbformat 4) as readable cells instead of raw
/// JSON. Returns `None` when the bytes don't parse as a notebook so the caller
/// falls back to the plain-text reader. Mirrors Claude Code's Read (notebooks as
/// cells with outputs) and pairs with `notebook_edit` — cell ids/indices are
/// shown so the model can target a cell. Binary outputs (images, etc.) become a
/// placeholder so a base64 PNG can't flood the context.
fn render_notebook(display_path: &str, text: &str) -> Option<String> {
    const MAX_OUTPUT_CHARS: usize = 2000;
    let nb: Value = serde_json::from_str(text).ok()?;
    let cells = nb.get("cells")?.as_array()?;
    let mut out = format!(
        "<system-reminder>`{}` is a Jupyter notebook ({} cells), rendered as cells below (not raw JSON). \
         Edit a cell with `notebook_edit`.</system-reminder>\n",
        display_path,
        cells.len()
    );
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let id_note = match cell.get("id").and_then(|v| v.as_str()) {
            Some(id) => format!(" id={id}"),
            None => String::new(),
        };
        out.push_str(&format!("\n[cell {i}{id_note}] {cell_type}\n"));
        let source = join_nb_text(cell.get("source"));
        if source.trim().is_empty() {
            out.push_str("(empty)\n");
        } else {
            out.push_str(&source);
            if !source.ends_with('\n') {
                out.push('\n');
            }
        }
        if cell_type == "code" {
            if let Some(outputs) = cell.get("outputs").and_then(|v| v.as_array()) {
                if let Some(rendered) = render_nb_outputs(outputs, MAX_OUTPUT_CHARS) {
                    out.push_str("--- output ---\n");
                    out.push_str(&rendered);
                    out.push('\n');
                }
            }
        }
    }
    Some(out)
}

/// nbformat stores text fields (`source`, stream `text`, `text/plain`) as either
/// a string or an array of line-strings; join either into one string.
fn join_nb_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(|x| x.as_str()).collect(),
        _ => String::new(),
    }
}

/// Render a code cell's `outputs`: text streams / `text/plain` results / errors
/// are shown (truncated to `max_chars`); rich or binary mimes become a
/// `[<mime> output omitted]` placeholder. `None` when there is nothing textual.
fn render_nb_outputs(outputs: &[Value], max_chars: usize) -> Option<String> {
    let mut buf = String::new();
    for o in outputs {
        let piece = match o.get("output_type").and_then(|v| v.as_str()).unwrap_or("") {
            "stream" => join_nb_text(o.get("text")),
            "execute_result" | "display_data" => match o.get("data") {
                Some(Value::Object(data)) => {
                    if let Some(t) = data.get("text/plain") {
                        join_nb_text(Some(t))
                    } else {
                        let mimes: Vec<&str> = data.keys().map(|k| k.as_str()).collect();
                        format!("[{} output omitted]", mimes.join(", "))
                    }
                }
                _ => String::new(),
            },
            "error" => {
                let ename = o.get("ename").and_then(|v| v.as_str()).unwrap_or("Error");
                let evalue = o.get("evalue").and_then(|v| v.as_str()).unwrap_or("");
                format!("{ename}: {evalue}")
            }
            _ => String::new(),
        };
        if piece.trim().is_empty() {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(piece.trim_end_matches('\n'));
    }
    if buf.is_empty() {
        return None;
    }
    if buf.chars().count() > max_chars {
        let truncated: String = buf.chars().take(max_chars).collect();
        buf = format!("{truncated}… [output truncated]");
    }
    Some(buf)
}

/// One-line summary for a binary file that `read_file` can't show as text —
/// the kind (sniffed from `sniff`'s leading magic bytes) and `size`, plus how to
/// view an image. `size` is passed explicitly so the large-file path can report
/// the true file length even though it only sniffs a leading chunk.
fn describe_binary(display_path: &str, sniff: &[u8], size: u64) -> String {
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
fn sniff_binary_kind(bytes: &[u8]) -> &'static str {
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

fn numbered_line(
    line_no: usize,
    line: &str,
    was_byte_truncated: bool,
    max_line_chars: usize,
) -> String {
    let printed: String = if was_byte_truncated || line.chars().count() > max_line_chars {
        let head: String = line.chars().take(max_line_chars).collect();
        format!("{head}… [line truncated]")
    } else {
        line.to_string()
    };
    format!("{line_no:>6}\t{printed}\n")
}

fn read_large_text_slice(
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
        // A binary file's whole content is "recorded as read".
        return Ok((describe_binary(display_path, &head, file_len), true));
    }

    let mut out = String::new();
    let mut line_no = 0usize;
    let mut printed = 0usize;
    let mut hit_limit = false;
    let max_line_bytes = max_line_chars.saturating_mul(4);

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
            out.push_str(&numbered_line(
                line_no + 1,
                &text,
                was_byte_truncated,
                max_line_chars,
            ));
            printed += 1;
        }
        line_no = line_no.saturating_add(1);
    }
    // Full only if we read from the very top all the way to EOF.
    let fully_read = start == 0 && !hit_limit;
    Ok((out, fully_read))
}

fn read_next_line_capped<R: std::io::BufRead>(
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

/// True when a leading sniff window contains a genuinely invalid UTF-8 byte
/// (i.e. binary), as opposed to a multibyte char merely truncated at the window
/// boundary. `error_len() == Some(_)` means an invalid byte strictly inside the
/// window; `None` means the window ended mid-character, which a text file can do.
fn leading_bytes_are_binary(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => false,
        Err(e) => e.error_len().is_some(),
    }
}

fn bytes_to_line(bytes: &[u8], was_byte_truncated: bool) -> Result<String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) if was_byte_truncated && e.valid_up_to() > 0 => {
            std::str::from_utf8(&bytes[..e.valid_up_to()])?
        }
        Err(e) => return Err(anyhow!("file is not valid UTF-8: {e}")),
    };
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

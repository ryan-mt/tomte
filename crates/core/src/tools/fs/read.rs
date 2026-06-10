//! The `read_file` tool and its text/binary rendering helpers. Split out of
//! `fs`; logic unchanged.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use crate::tools::{BuiltinTool, ToolContext, ToolOutput};

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
- A read is also capped by total size (~32k tokens): a file of long lines stops early — even under 2000 lines — with the same continue-with-`offset` notice, so one read can't flood your context. Reading only the slice you need (`offset` + `limit`, or `grep` first) is cheaper than a capped full read.\n\
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
        // Default lines-per-call when caller does not pass `limit`. Keeps a
        // single read from flooding the context window with a large file.
        const DEFAULT_LINE_LIMIT: usize = 2000;
        const MAX_LINE_LIMIT: usize = DEFAULT_LINE_LIMIT;
        // Per-line truncation so a minified bundle (one giant line) can't
        // blow out the context.
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
        // A directory read otherwise fails with a raw OS error ("Is a directory"
        // / "Access is denied"); name the mistake and the right tool instead.
        if meta.is_dir() {
            return Err(anyhow!(
                "{} is a directory, not a file — use list_dir or glob to see its contents",
                a.path
            ));
        }
        // Reject non-regular files (FIFO, socket, char/block device). A FIFO
        // reports len()==0, so it takes the eager-read path below and
        // `tokio::fs::read` blocks forever waiting for a writer — hanging the
        // tool. `metadata()` follows symlinks, so a symlink to a regular file is
        // still accepted.
        if !meta.is_file() {
            return Err(anyhow!(
                "{} is not a regular file (it is a FIFO, socket, or device) and cannot be read",
                a.path
            ));
        }
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
                // rather than dumped as raw JSON; a sliced read (offset/limit)
                // still returns the raw JSON. A parse failure falls through to
                // the plain-text reader.
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

    /// Whole-file read of an image (PNG/JPEG/GIF/WebP) or PDF attaches the bytes
    /// as media so a vision model can SEE it, instead of the text "binary file"
    /// note. A sliced read (offset/limit) or any other file defers to the
    /// text-only `execute`.
    async fn execute_rich(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let a: ReadArgs = crate::tools::parse_args("read_file", args.clone())?;
        if a.offset.is_none() && a.limit.is_none() {
            if let Ok(path) = resolve(&ctx.cwd, &a.path) {
                if let Some(media_type) = displayable_media_type(&path).await {
                    // Cap on the bytes we inline (base64 inflates ~33%); above
                    // it, fall through to the text path's `describe_binary`.
                    const MAX_MEDIA_BYTES: u64 = 5_000_000;
                    if let Ok(meta) = tokio::fs::metadata(&path).await {
                        let size = meta.len();
                        if size > 0 && size <= MAX_MEDIA_BYTES {
                            let bytes = tokio::fs::read(&path)
                                .await
                                .with_context(|| format!("read {}", path.display()))?;
                            let data_base64 =
                                base64::engine::general_purpose::STANDARD.encode(&bytes);
                            // Whole binary read: recorded as fully read so
                            // write_file may overwrite it (matches execute()).
                            record_successful_read(ctx, &path, &meta, true).await;
                            let kind = if media_type == "application/pdf" {
                                "a PDF"
                            } else {
                                "an image"
                            };
                            let text = format!(
                                "<system-reminder>`{}` is {} ({} bytes), attached as {} for vision-capable models to view.</system-reminder>",
                                a.path, kind, size, media_type
                            );
                            return Ok(ToolOutput {
                                text,
                                media: vec![crate::openai::ToolMedia {
                                    media_type: media_type.to_string(),
                                    data_base64,
                                }],
                            });
                        }
                    }
                }
            }
        }
        Ok(ToolOutput::text(self.execute(args, ctx).await?))
    }
}

/// MIME type for a file `read_file` can show to a vision model (images + PDF),
/// sniffed from leading magic bytes; `None` for anything else. Sniffing beats
/// the extension and avoids reading the whole file just to classify it.
async fn displayable_media_type(path: &std::path::Path) -> Option<&'static str> {
    let mut buf = [0u8; 16];
    let mut f = tokio::fs::File::open(path).await.ok()?;
    let n = f.read(&mut buf).await.ok()?;
    let head = &buf[..n];
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
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

/// Ceiling on a single read's rendered output, in bytes. The 2000-line and
/// per-line caps bound line COUNT and width but not total size, so a file of
/// long-but-under-2000-char lines could still dump hundreds of KB (≈hundreds of
/// thousands of tokens) in one call. This keeps every read token-bounded
/// regardless of line length, stopping early with a continue-with-offset notice.
///
/// Matches Claude Code's per-read output cap of 25000 tokens
/// (`CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS`): at tomte's ≈4-bytes/token
/// estimate that is ~100 KB. A `TOMTE_READ_MAX_TOKENS` env override mirrors
/// Claude Code's knob (see [`read_output_byte_cap`]).
mod binary;
mod large;
mod notebook;

use binary::*;
use large::*;
use notebook::*;

/// Ceiling on a single read's rendered output, in bytes. The 2000-line and
/// per-line caps bound line COUNT and width but not total size, so a file of
/// long-but-under-2000-char lines could still dump hundreds of KB (≈hundreds of
/// thousands of tokens) in one call. This keeps every read token-bounded
/// regardless of line length, stopping early with a continue-with-offset notice.
///
/// Matches Claude Code's per-read output cap of 25000 tokens
/// (`CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS`): at tomte's ≈4-bytes/token
/// estimate that is ~100 KB. A `TOMTE_READ_MAX_TOKENS` env override mirrors
/// Claude Code's knob (see [`read_output_byte_cap`]).
const READ_OUTPUT_TOKEN_CAP: usize = 25_000;

/// Bytes/token estimate tomte uses elsewhere (`context_report::est`).
const BYTES_PER_TOKEN: usize = 4;

/// The effective per-read byte ceiling, honoring a `TOMTE_READ_MAX_TOKENS`
/// override (clamped to a sane band) exactly as Claude Code honors
/// `CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS`. Pure given `raw`, so the override
/// parsing is unit-tested without touching process env.
fn read_output_byte_cap_from(raw: Option<&str>) -> usize {
    let tokens = raw
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|t| *t > 0)
        .map(|t| t.clamp(2_000, 1_000_000))
        .unwrap_or(READ_OUTPUT_TOKEN_CAP);
    tokens.saturating_mul(BYTES_PER_TOKEN)
}

fn read_output_byte_cap() -> usize {
    read_output_byte_cap_from(std::env::var("TOMTE_READ_MAX_TOKENS").ok().as_deref())
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
    let cap = read_output_byte_cap();
    let mut out = String::new();
    // Line index after the last one we actually rendered. Equals `end` for a
    // normal read; smaller when the byte cap stops us early.
    let mut shown_end = end;
    for (i, line) in lines[start..end].iter().enumerate() {
        let abs = start + i;
        out.push_str(&numbered_line(abs + 1, line, false, max_line_chars));
        // Stop once the rendered output passes the cap, so a file of long lines
        // can't flood the context. Always emits at least one line; only stops
        // when more lines remain in this window.
        if out.len() >= cap && abs + 1 < end {
            shown_end = abs + 1;
            break;
        }
    }
    if shown_end < total {
        let remaining = total - shown_end;
        let cap_note = if shown_end < end {
            " (stopped early to keep this read token-bounded)"
        } else {
            ""
        };
        out.push_str(&format!(
            "<system-reminder>Showing lines {}-{} of {}{}. {} more line(s) remain — call read_file again with offset={} and an explicit limit to continue.</system-reminder>\n",
            start + 1,
            shown_end,
            total,
            cap_note,
            remaining,
            shown_end
        ));
    }
    // A full read starts at the top and reaches the last line untruncated.
    let fully_read = start == 0 && shown_end >= total;
    (out, fully_read)
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

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn read_output_byte_cap_honors_and_clamps_override() {
        // Default = Claude Code's 25k-token cap, in bytes.
        assert_eq!(read_output_byte_cap_from(None), 25_000 * BYTES_PER_TOKEN);
        // A valid override scales tokens → bytes.
        assert_eq!(read_output_byte_cap_from(Some("10000")), 40_000);
        // Out-of-band values clamp, never apply verbatim.
        assert_eq!(
            read_output_byte_cap_from(Some("500")),
            2_000 * BYTES_PER_TOKEN
        );
        assert_eq!(
            read_output_byte_cap_from(Some("9999999")),
            1_000_000 * BYTES_PER_TOKEN
        );
        // Garbage / non-positive → the default, never a panic or zero.
        for bad in ["", "  ", "abc", "0", "-5", "1e6"] {
            assert_eq!(
                read_output_byte_cap_from(Some(bad)),
                25_000 * BYTES_PER_TOKEN,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn render_text_read_stops_at_the_byte_cap_before_the_line_cap() {
        // A file of many medium lines (well under 2000 lines and under 2000
        // chars/line) still exceeds the byte cap, so the read must stop early —
        // the token-bounding behavior that matches Claude Code's per-read cap.
        let line = "x".repeat(180);
        let text = std::iter::repeat_n(line.as_str(), 1500)
            .collect::<Vec<_>>()
            .join("\n");
        let (out, fully_read) = render_text_read("big.rs", &text, 0, 2000, 2000);
        assert!(!fully_read, "a capped read is not a full read");
        assert!(
            out.contains("stopped early to keep this read token-bounded"),
            "missing cap notice"
        );
        assert!(out.contains("more line(s) remain"));
        // Output stays near the cap, not the full ~270 KB the file would dump.
        assert!(
            out.len() < read_output_byte_cap_from(None) + 2_000,
            "output not bounded: {} bytes",
            out.len()
        );
    }

    #[test]
    fn render_text_read_small_file_is_unchanged() {
        // A normal small read completes fully with no cap notice (no regression).
        let text = "fn a() {}\nfn b() {}\nfn c() {}";
        let (out, fully_read) = render_text_read("small.rs", text, 0, 2000, 2000);
        assert!(fully_read);
        assert!(!out.contains("token-bounded"));
        assert!(!out.contains("more line(s) remain"));
        assert!(out.contains("fn c()"));
    }
}

use super::*;

/// Hard cap on a single JSON-RPC line from an MCP server. A server is untrusted
/// (an external subprocess), and `Lines::next_line`/`read_until` buffer until a
/// newline with no bound, so one newline-less line could exhaust memory before
/// the per-result `cap_tool_output` ever runs. 16 MiB is generous for any
/// legitimate tool result.
pub(super) const MAX_MCP_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Read one newline-delimited message, bounding its length so a malicious or
/// buggy MCP server can't OOM the process with a single unterminated line.
/// Returns `Ok(None)` at clean EOF; the trailing `\n` is consumed and stripped.
pub(super) async fn read_capped_line(
    reader: &mut BufReader<ChildStdout>,
) -> Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // EOF: surface any trailing unterminated bytes, else signal close.
            return Ok((!buf.is_empty()).then(|| String::from_utf8_lossy(&buf).into_owned()));
        }
        let chunk_len = available.len();
        if let Some(nl) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..nl]);
            reader.consume(nl + 1);
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        buf.extend_from_slice(available);
        reader.consume(chunk_len);
        if buf.len() > MAX_MCP_LINE_BYTES {
            return Err(anyhow!(
                "MCP server sent a line larger than {MAX_MCP_LINE_BYTES} bytes; \
                 aborting to avoid memory exhaustion"
            ));
        }
    }
}

/// Read the `resources` capability from an `initialize` result. The MCP spec
/// advertises a server's resource support as the presence of
/// `capabilities.resources` (an object, possibly empty). Pure for testing.
pub(super) fn server_supports_resources(init_result: &Value) -> bool {
    init_result
        .get("capabilities")
        .and_then(|c| c.get("resources"))
        .is_some()
}

/// Format a `resources/list` result into a compact index — one line per
/// resource (`uri — name (mimeType): description`), fenced as untrusted server
/// output. Pure, so the formatting/fencing is unit-tested without a live server.
pub(super) fn resource_list_result(server: &str, resp: &Value) -> String {
    let mut lines = String::new();
    if let Some(arr) = resp.get("resources").and_then(|v| v.as_array()) {
        for r in arr {
            let uri = r.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            if uri.is_empty() {
                continue;
            }
            let mut line = uri.to_string();
            if let Some(name) = r
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                line.push_str(" — ");
                line.push_str(name);
            }
            if let Some(mime) = r
                .get("mimeType")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                line.push_str(" (");
                line.push_str(mime);
                line.push(')');
            }
            if let Some(desc) = r
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                line.push_str(": ");
                line.push_str(desc);
            }
            if !lines.is_empty() {
                lines.push('\n');
            }
            lines.push_str(&line);
        }
    }
    let body = if lines.is_empty() {
        "(server exposes no resources)".to_string()
    } else {
        lines
    };
    fence_mcp_output(server, "resources/list", &body)
}

/// Build a `resources/read` result: concatenate the `contents` text blocks,
/// replacing any non-text (binary `blob`) block with a placeholder so a
/// binary-only resource doesn't deliver as an invisible empty string. Fenced.
/// Pure, so it's unit-testable off a synthetic body.
pub(super) fn resource_read_result(server: &str, uri: &str, resp: &Value) -> String {
    let mut buf = String::new();
    if let Some(arr) = resp.get("contents").and_then(|v| v.as_array()) {
        for item in arr {
            let piece = match item.get("text").and_then(|v| v.as_str()) {
                Some(text) => text.to_string(),
                None => {
                    let mime = item
                        .get("mimeType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("binary");
                    format!("[{mime} resource content omitted]")
                }
            };
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(&piece);
        }
    }
    if buf.is_empty() {
        buf = format!("(resource {uri} returned no content)");
    }
    fence_mcp_output(server, uri, &buf)
}

/// Build the `call_tool` result from a `tools/call` response body. The joined
/// content is fenced on BOTH the success and the `isError` path: a compromised
/// server can return its injection payload either way, and the error branch used
/// to surface the raw server text un-fenced — bypassing the very provenance fence
/// the success path applies (see [`fence_mcp_output`]). Pure, so the fencing is
/// unit-tested without a live server.
pub(super) fn call_result(server: &str, tool: &str, resp: &Value) -> Result<String> {
    let is_error = resp
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let buf = flatten_tool_content(resp.get("content"), is_error);
    let fenced = fence_mcp_output(server, tool, &buf);
    if is_error {
        Err(anyhow!(fenced))
    } else {
        Ok(fenced)
    }
}

/// Wrap an MCP tool's output in a labeled fence so the model treats it as data
/// from an external server, not instructions. A malicious or compromised server
/// can otherwise return text that reads as a directive (indirect prompt
/// injection); the fence (plus the `# Available tools` guidance) marks the
/// provenance. Framework block markers are neutralized, and the fence's own
/// closing tag is broken in the body so the server can't forge an early close.
/// Label values are stripped of structural characters so they can't break the
/// tag. Containment is downstream — the approval gate still vets any tool the
/// model is steered toward — but the fence removes the easy foothold.
pub(super) fn fence_mcp_output(server: &str, tool: &str, text: &str) -> String {
    let label = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, '"' | '<' | '>' | '\n' | '\r'))
            .take(64)
            .collect()
    };
    let safe = crate::memory::neutralize_block_markers(text)
        .replace("</untrusted-mcp-output", "</untrusted-mcp-\u{200b}output");
    format!(
        "<untrusted-mcp-output server=\"{}\" tool=\"{}\">\n{safe}\n</untrusted-mcp-output>",
        label(server),
        label(tool),
    )
}

/// Join an MCP `tools/call` result's `content` array into one string for the
/// model. Text blocks are concatenated; any non-text block (image, audio,
/// resource, …) becomes a `[<type> content omitted]` placeholder so a result
/// made only of non-text content is not delivered as an invisible empty string
/// the model can't act on. Falls back to a descriptive message when there is no
/// content at all, so an `isError` result never surfaces as a contentless error.
pub(super) fn flatten_tool_content(content: Option<&Value>, is_error: bool) -> String {
    let mut buf = String::new();
    if let Some(arr) = content.and_then(|v| v.as_array()) {
        for item in arr {
            let piece = match item.get("text").and_then(|v| v.as_str()) {
                Some(text) => text.to_string(),
                None => {
                    let kind = item
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("non-text");
                    format!("[{kind} content omitted]")
                }
            };
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(&piece);
        }
    }
    if buf.is_empty() {
        buf = if is_error {
            "MCP tool reported an error with no message".to_string()
        } else {
            "(MCP tool returned no content)".to_string()
        };
    }
    buf
}

/// Coerce an MCP-advertised `inputSchema` into something usable as function
/// `parameters`. Providers require a top-level JSON-Schema object; a server that
/// advertises a non-object schema (or omits `type`) would otherwise 400 the
/// whole request — taking down every tool in the turn, not just this one. Absent
/// or non-object schemas fall back to an empty object schema; the model then
/// gets per-arg errors it can self-correct instead of a request-level rejection.
pub(super) fn normalize_mcp_schema(schema: Option<Value>) -> Value {
    let Some(Value::Object(mut map)) = schema else {
        return json!({"type": "object", "properties": {}});
    };
    let is_object_type = matches!(map.get("type"), Some(Value::String(t)) if t == "object");
    if !is_object_type {
        map.insert("type".to_string(), Value::String("object".to_string()));
    }
    Value::Object(map)
}

pub(super) fn parse_tools(resp: &Value) -> Vec<McpToolInfo> {
    let Some(arr) = resp.get("tools").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| {
            let name = t.get("name").and_then(|v| v.as_str())?.to_string();
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = normalize_mcp_schema(t.get("inputSchema").cloned());
            Some(McpToolInfo {
                name,
                description,
                input_schema,
            })
        })
        .collect()
}

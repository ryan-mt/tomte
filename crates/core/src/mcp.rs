//! Minimal MCP (Model Context Protocol) stdio client.
//!
//! Spawns a child process configured in `~/.config/tomte/settings.json`
//! under `mcp_servers`, speaks JSON-RPC 2.0 over newline-delimited stdin/stdout,
//! and adapts each discovered MCP tool to tomte's `BuiltinTool` trait so the
//! agent can invoke them like any other built-in tool.
//!
//! Tools are exposed under the namespaced name `mcp__<server>__<tool>` so
//! collisions across servers are impossible.
//!
//! Settings format:
//! ```json
//! {
//!   "mcp_servers": {
//!     "filesystem": {
//!       "command": "npx",
//!       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
//!       "env": {}
//!     }
//!   }
//! }
//! ```

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

mod adapter;
pub use adapter::McpToolAdapter;

const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on a single JSON-RPC line from an MCP server. A server is untrusted
/// (an external subprocess), and `Lines::next_line`/`read_until` buffer until a
/// newline with no bound, so one newline-less line could exhaust memory before
/// the per-result `cap_tool_output` ever runs. 16 MiB is generous for any
/// legitimate tool result.
const MAX_MCP_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Read one newline-delimited message, bounding its length so a malicious or
/// buggy MCP server can't OOM the process with a single unterminated line.
/// Returns `Ok(None)` at clean EOF; the trailing `\n` is consumed and stripped.
async fn read_capped_line(reader: &mut BufReader<ChildStdout>) -> Result<Option<String>> {
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

#[cfg(unix)]
fn isolate_process_group(cmd: &mut Command) {
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_cmd: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    unsafe {
        let _ = kill(-pid, SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
struct SettingsFile {
    #[serde(default)]
    mcp_servers: HashMap<String, McpServerConfig>,
}

pub fn load_servers_config() -> HashMap<String, McpServerConfig> {
    let path = crate::config::config_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    match serde_json::from_str::<SettingsFile>(&text) {
        Ok(s) => s.mcp_servers,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse mcp_servers from settings.json");
            HashMap::new()
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A live, handshaked MCP server connection.
pub struct McpClient {
    pub name: String,
    pub tools: Vec<McpToolInfo>,
    state: Mutex<McpState>,
}

struct McpState {
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
    request_timeout: Duration,
    child_pid: Option<u32>,
    // Keep the child alive for the lifetime of the client; kill_on_drop
    // handles the direct child, while Drop below also kills descendants in
    // the process group.
    child: Child,
}

impl Drop for McpState {
    fn drop(&mut self) {
        kill_process_group(self.child_pid);
        let _ = self.child.start_kill();
    }
}

/// Resolve a bare program name (e.g. `npx`) to a concrete executable on Windows
/// by searching PATH × PATHEXT, the way a shell does. `CreateProcessW` (which
/// Rust's `Command` uses) only appends `.exe`, so `npx`/`pnpm`/`node`-style
/// shims that live as `.cmd`/`.bat` are otherwise unspawnable. Returns `None`
/// for a command that already carries a path or extension (used verbatim) or
/// that can't be found (caller falls back to the original name and its error).
#[cfg(windows)]
fn resolve_windows_program(command: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    resolve_program_in(command, &path, &pathext)
}

/// PATH×PATHEXT resolution as a pure function so it can be tested without
/// mutating the process environment.
#[cfg(windows)]
fn resolve_program_in(
    command: &str,
    path: &std::ffi::OsStr,
    pathext: &str,
) -> Option<std::path::PathBuf> {
    // A command that already names a path or an extension is used as-is.
    if command.contains(['/', '\\']) || std::path::Path::new(command).extension().is_some() {
        return None;
    }
    for dir in std::env::split_paths(path) {
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

impl McpClient {
    /// Spawn a new server, perform the `initialize` handshake, and list its
    /// tools. Returns a ready-to-use client.
    pub async fn spawn(name: String, config: McpServerConfig) -> Result<Self> {
        Self::spawn_with_timeout(name, config, REQUEST_TIMEOUT).await
    }

    async fn spawn_with_timeout(
        name: String,
        config: McpServerConfig,
        request_timeout: Duration,
    ) -> Result<Self> {
        // On Windows the canonical MCP config — `"command": "npx"` — names a
        // `.cmd` shim, but `CreateProcessW` only appends `.exe`, so a bare name
        // never resolves and every npx/node-wrapped server fails to spawn.
        // Resolve it against PATH+PATHEXT to the real `.cmd`/`.bat`/`.exe`;
        // already-qualified commands and all non-Windows targets are unchanged.
        #[cfg(windows)]
        let program: std::ffi::OsString = resolve_windows_program(&config.command)
            .map(Into::into)
            .unwrap_or_else(|| config.command.clone().into());
        #[cfg(not(windows))]
        let program: std::ffi::OsString = config.command.clone().into();
        let mut cmd = Command::new(&program);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Scrub inherited secret env (API keys, tokens, live agent sockets) the
        // server has no business seeing — like run_shell — then re-apply the
        // server's explicit `env` map so an operator can still pass it an
        // intended secret. A third-party `npx -y` server otherwise inherits
        // every credential in the agent's environment.
        crate::secret_env::scrub_secret_env(&mut cmd);
        cmd.envs(&config.env);
        isolate_process_group(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            anyhow!(
                "failed to spawn MCP server `{name}` ({}): {e}",
                config.command
            )
        })?;
        let child_pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server `{name}` has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server `{name}` has no stdout"))?;
        if let Some(mut stderr) = child.stderr.take() {
            let stderr_name = name.clone();
            let _stderr_task = tokio::spawn(async move {
                let mut buf = [0u8; 8192];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]);
                            tracing::debug!(server = %stderr_name, stderr = %chunk, "MCP server stderr");
                        }
                        Err(e) => {
                            tracing::debug!(server = %stderr_name, error = %e, "failed to read MCP stderr");
                            break;
                        }
                    }
                }
            });
        }
        let reader = BufReader::new(stdout);
        let mut state = McpState {
            stdin,
            reader,
            next_id: 1,
            request_timeout,
            child_pid,
            child,
        };

        // Handshake.
        let init_params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "tomte",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        state.request("initialize", init_params).await?;
        state.notify("notifications/initialized", json!({})).await?;

        // Tool discovery.
        let resp = state.request("tools/list", json!({})).await?;
        let tools = parse_tools(&resp);

        Ok(Self {
            name,
            tools,
            state: Mutex::new(state),
        })
    }

    /// Invoke a tool on the server. Returns the joined text content of the
    /// response, or an error containing the server-reported `isError` text.
    pub async fn call_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let params = json!({"name": tool_name, "arguments": args});
        let mut state = self.state.lock().await;
        let resp = state.request("tools/call", params).await?;
        drop(state);

        let is_error = resp
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let buf = flatten_tool_content(resp.get("content"), is_error);
        if is_error {
            Err(anyhow!(buf))
        } else {
            Ok(buf)
        }
    }
}

/// Join an MCP `tools/call` result's `content` array into one string for the
/// model. Text blocks are concatenated; any non-text block (image, audio,
/// resource, …) becomes a `[<type> content omitted]` placeholder so a result
/// made only of non-text content is not delivered as an invisible empty string
/// the model can't act on. Falls back to a descriptive message when there is no
/// content at all, so an `isError` result never surfaces as a contentless error.
fn flatten_tool_content(content: Option<&Value>, is_error: bool) -> String {
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
fn normalize_mcp_schema(schema: Option<Value>) -> Value {
    let Some(Value::Object(mut map)) = schema else {
        return json!({"type": "object", "properties": {}});
    };
    let is_object_type = matches!(map.get("type"), Some(Value::String(t)) if t == "object");
    if !is_object_type {
        map.insert("type".to_string(), Value::String("object".to_string()));
    }
    Value::Object(map)
}

fn parse_tools(resp: &Value) -> Vec<McpToolInfo> {
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

impl McpState {
    async fn write_message(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&req).await?;

        // Bound the WHOLE request, not each line read. A server that interleaves
        // a steady stream of unrelated notifications (more often than the
        // timeout) would otherwise reset a per-line timeout indefinitely and
        // stall the turn. Inside, read until we see a response with our id,
        // skipping unrelated notifications and other responses.
        let request_timeout = self.request_timeout;
        let reader = &mut self.reader;
        let read = async {
            loop {
                let Some(line) = read_capped_line(reader).await? else {
                    return Err(anyhow!(
                        "MCP server closed stdout while awaiting `{method}`"
                    ));
                };
                let Ok(v): std::result::Result<Value, _> = serde_json::from_str(&line) else {
                    tracing::debug!(line = %line, "non-JSON line on MCP stdout");
                    continue;
                };
                if v.get("id").is_some_and(|i| {
                    i.as_u64() == Some(id)
                        || i.as_str().and_then(|s| s.parse::<u64>().ok()) == Some(id)
                }) {
                    if let Some(err) = v.get("error") {
                        return Err(anyhow!("MCP `{method}` error: {err}"));
                    }
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
            }
        };
        tokio::time::timeout(request_timeout, read)
            .await
            .map_err(|_| {
                anyhow!(
                    "MCP `{method}` timed out after {}s",
                    request_timeout.as_secs()
                )
            })?
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let req = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&req).await
    }
}

/// Spawn every server configured in `settings.json`. Failures are logged but
/// do not abort — a misconfigured server should not prevent the agent from
/// running with the remaining servers (and the built-in tools).
pub async fn spawn_all() -> Vec<Arc<McpClient>> {
    let config = load_servers_config();
    let mut out = Vec::new();
    for (name, cfg) in config {
        match McpClient::spawn(name.clone(), cfg).await {
            Ok(c) => {
                tracing::info!(server = %name, tools = c.tools.len(), "MCP server ready");
                out.push(Arc::new(c));
            }
            Err(e) => {
                tracing::warn!(server = %name, error = %e, "failed to spawn MCP server");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;

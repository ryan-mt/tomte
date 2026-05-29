//! Minimal MCP (Model Context Protocol) stdio client.
//!
//! Spawns a child process configured in `~/.config/opencli/settings.json`
//! under `mcp_servers`, speaks JSON-RPC 2.0 over newline-delimited stdin/stdout,
//! and adapts each discovered MCP tool to opencli's `BuiltinTool` trait so the
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
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::openai::{Tool, ToolFunctionDef};
use crate::tools::{BuiltinTool, ToolContext};

const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    lines: Lines<BufReader<ChildStdout>>,
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
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
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
        let lines = BufReader::new(stdout).lines();
        let mut state = McpState {
            stdin,
            lines,
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
                "name": "opencli",
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
                    let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("non-text");
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
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
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

        // Read until we see a response with our id. Skip unrelated
        // notifications and other responses (the server may interleave).
        loop {
            let next = tokio::time::timeout(self.request_timeout, self.lines.next_line())
                .await
                .map_err(|_| {
                    anyhow!(
                        "MCP `{method}` timed out after {}s",
                        self.request_timeout.as_secs()
                    )
                })??;
            let Some(line) = next else {
                return Err(anyhow!(
                    "MCP server closed stdout while awaiting `{method}`"
                ));
            };
            let Ok(v): std::result::Result<Value, _> = serde_json::from_str(&line) else {
                tracing::debug!(line = %line, "non-JSON line on MCP stdout");
                continue;
            };
            if v.get("id").is_some_and(|i| {
                i.as_u64() == Some(id) || i.as_str().and_then(|s| s.parse::<u64>().ok()) == Some(id)
            }) {
                if let Some(err) = v.get("error") {
                    return Err(anyhow!("MCP `{method}` error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
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

/// Adapts one MCP tool to the `BuiltinTool` interface so the agent can call
/// it identically to its own built-ins. Tool name is namespaced as
/// `mcp__<server>__<tool>` to prevent cross-server collisions.
pub struct McpToolAdapter {
    client: Arc<McpClient>,
    qualified_name: &'static str,
    description: &'static str,
    schema: Value,
    /// The original (un-namespaced) tool name to send back to the server.
    original_name: String,
}

impl McpToolAdapter {
    pub fn new(client: Arc<McpClient>, info: McpToolInfo) -> Self {
        let qualified = portable_mcp_tool_name(&client.name, &info.name);
        // Leak the strings so they live for `'static`, satisfying the
        // BuiltinTool trait. We expect a small, bounded number of MCP tools
        // per process.
        let qualified_name: &'static str = Box::leak(qualified.into_boxed_str());
        let description: &'static str = Box::leak(info.description.into_boxed_str());
        Self {
            client,
            qualified_name,
            description,
            schema: info.input_schema,
            original_name: info.name,
        }
    }
}

fn portable_mcp_tool_name(server: &str, tool: &str) -> String {
    const MAX_NAME_LEN: usize = 64;
    let raw = format!("mcp__{server}__{tool}");
    let mut name = format!(
        "mcp__{}__{}",
        portable_name_part(server),
        portable_name_part(tool)
    );
    if name == raw && name.len() <= MAX_NAME_LEN {
        return name;
    }

    let suffix = format!("__{:08x}", fnv1a32(raw.as_bytes()));
    let max_base = MAX_NAME_LEN - suffix.len();
    if name.len() > max_base {
        name.truncate(max_base);
        while !name.is_char_boundary(name.len()) {
            name.pop();
        }
    }
    name.push_str(&suffix);
    name
}

fn portable_name_part(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "tool".to_string()
    } else {
        trimmed.to_string()
    }
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for b in bytes {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

#[async_trait]
impl BuiltinTool for McpToolAdapter {
    fn name(&self) -> &'static str {
        self.qualified_name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }
    fn definition(&self) -> Tool {
        // MCP-provided schemas often don't satisfy OpenAI's `strict: true`
        // requirements (missing `additionalProperties: false`, optional
        // fields not marked nullable, etc.). Fall back to non-strict so
        // discovery still works; the agent receives malformed args as tool
        // errors and can self-correct.
        Tool::Function(ToolFunctionDef {
            name: self.qualified_name.to_string(),
            description: self.description.to_string(),
            parameters: self.schema.clone(),
            strict: false,
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        self.client.call_tool(&self.original_name, args).await
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
mod tests {
    use super::*;

    fn is_portable_tool_name(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    }

    #[test]
    fn portable_mcp_tool_name_preserves_valid_names() {
        assert_eq!(
            portable_mcp_tool_name("filesystem", "read_file"),
            "mcp__filesystem__read_file"
        );
    }

    #[test]
    fn portable_mcp_tool_name_sanitizes_invalid_names() {
        let name = portable_mcp_tool_name("team server", "read/file 😬");
        assert!(is_portable_tool_name(&name), "{name}");
        assert!(name.starts_with("mcp__team_server__read_file"));
    }

    #[test]
    fn portable_mcp_tool_name_caps_long_names() {
        let name = portable_mcp_tool_name(&"s".repeat(80), &"t".repeat(80));
        assert!(is_portable_tool_name(&name), "{name}");
        assert_eq!(name.len(), 64);
    }

    #[cfg(unix)]
    fn sh_quote(path: &std::path::Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_timeout_kills_mcp_server_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("survived-mcp-timeout");
        let script = format!(
            "(sleep 0.5; printf survived > {}) & sleep 30",
            sh_quote(&marker)
        );
        let cfg = McpServerConfig {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), script],
            env: HashMap::new(),
        };

        let err = match McpClient::spawn_with_timeout(
            "leaky".to_string(),
            cfg,
            Duration::from_millis(80),
        )
        .await
        {
            Ok(_) => panic!("spawn should time out"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("timed out"), "got: {err}");
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !marker.exists(),
            "MCP timeout killed only the server process; a background descendant survived"
        );
    }

    #[test]
    fn flatten_tool_content_surfaces_non_text_and_empty() {
        // Text blocks join with newlines.
        let c = json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]);
        assert_eq!(flatten_tool_content(Some(&c), false), "a\nb");
        // A non-text block becomes a visible placeholder, never an empty string.
        let c = json!([{"type": "image", "data": "…"}]);
        assert_eq!(
            flatten_tool_content(Some(&c), false),
            "[image content omitted]"
        );
        // Text mixed with non-text keeps the text and flags the rest.
        let c = json!([{"type": "text", "text": "ok"}, {"type": "resource", "uri": "x"}]);
        assert_eq!(
            flatten_tool_content(Some(&c), false),
            "ok\n[resource content omitted]"
        );
        // No content at all is never an invisible empty success/error.
        assert_eq!(
            flatten_tool_content(None, false),
            "(MCP tool returned no content)"
        );
        assert_eq!(
            flatten_tool_content(None, true),
            "MCP tool reported an error with no message"
        );
    }
}

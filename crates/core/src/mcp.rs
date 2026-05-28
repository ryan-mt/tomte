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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::openai::{Tool, ToolFunctionDef};
use crate::tools::{BuiltinTool, ToolContext};

const PROTOCOL_VERSION: &str = "2024-11-05";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    // Keep the child alive for the lifetime of the client; kill_on_drop
    // ensures we don't leak processes if the client itself is dropped.
    _child: Child,
}

impl McpClient {
    /// Spawn a new server, perform the `initialize` handshake, and list its
    /// tools. Returns a ready-to-use client.
    pub async fn spawn(name: String, config: McpServerConfig) -> Result<Self> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                anyhow!(
                    "failed to spawn MCP server `{name}` ({}): {e}",
                    config.command
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server `{name}` has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server `{name}` has no stdout"))?;
        let lines = BufReader::new(stdout).lines();
        let mut state = McpState {
            stdin,
            lines,
            next_id: 1,
            _child: child,
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
        let mut buf = String::new();
        if let Some(arr) = resp.get("content").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
            }
        }
        if is_error {
            Err(anyhow!(buf))
        } else {
            Ok(buf)
        }
    }
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
            let next = tokio::time::timeout(REQUEST_TIMEOUT, self.lines.next_line())
                .await
                .map_err(|_| {
                    anyhow!(
                        "MCP `{method}` timed out after {}s",
                        REQUEST_TIMEOUT.as_secs()
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
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
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
        let qualified = format!("mcp__{}__{}", client.name, info.name);
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

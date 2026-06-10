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
mod resources;
pub use adapter::McpToolAdapter;
pub use resources::{ListMcpResources, ReadMcpResource};

mod process;
mod wire;

use process::*;
use wire::*;

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
    match serde_json::from_str::<SettingsFile>(crate::config::strip_bom(&text)) {
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
    /// Whether the server advertised the `resources` capability at handshake.
    /// Gates registration of the `list_mcp_resources`/`read_mcp_resource` tools
    /// so they only appear when at least one server can actually serve them.
    pub supports_resources: bool,
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
        let init = state.request("initialize", init_params).await?;
        let supports_resources = server_supports_resources(&init);
        state.notify("notifications/initialized", json!({})).await?;

        // Tool discovery.
        let resp = state.request("tools/list", json!({})).await?;
        let tools = parse_tools(&resp);

        Ok(Self {
            name,
            tools,
            supports_resources,
            state: Mutex::new(state),
        })
    }

    /// List the resources this server exposes (`resources/list`), as a fenced,
    /// model-readable index. Errors if the transport fails.
    pub async fn list_resources(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        let resp = state.request("resources/list", json!({})).await?;
        drop(state);
        Ok(resource_list_result(&self.name, &resp))
    }

    /// Read one resource by URI (`resources/read`), fenced as untrusted output.
    pub async fn read_resource(&self, uri: &str) -> Result<String> {
        let params = json!({ "uri": uri });
        let mut state = self.state.lock().await;
        let resp = state.request("resources/read", params).await?;
        drop(state);
        Ok(resource_read_result(&self.name, uri, &resp))
    }

    /// Invoke a tool on the server. Returns the joined text content of the
    /// response, or an error containing the server-reported `isError` text.
    pub async fn call_tool(&self, tool_name: &str, args: Value) -> Result<String> {
        let params = json!({"name": tool_name, "arguments": args});
        let mut state = self.state.lock().await;
        let resp = state.request("tools/call", params).await?;
        drop(state);
        call_result(&self.name, tool_name, &resp)
    }
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

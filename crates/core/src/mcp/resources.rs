//! Aggregate `list_mcp_resources` / `read_mcp_resource` tools spanning every
//! resource-capable MCP server, mirroring Claude Code's ListMcpResources /
//! ReadMcpResource. Registered by `Agent::load_mcp` only when at least one
//! connected server advertised the `resources` capability, so they never appear
//! when no server can serve them. Both are read-only (auto-approvable); the
//! server output is fenced as untrusted by `McpClient::{list,read}_resource`.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::McpClient;
use crate::tools::{BuiltinTool, ToolContext};

pub struct ListMcpResources {
    clients: Vec<Arc<McpClient>>,
}

pub struct ReadMcpResource {
    clients: Vec<Arc<McpClient>>,
}

impl ListMcpResources {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

impl ReadMcpResource {
    pub fn new(clients: Vec<Arc<McpClient>>) -> Self {
        Self { clients }
    }
}

#[derive(Deserialize)]
struct ListArgs {
    #[serde(default)]
    server: Option<String>,
}

#[derive(Deserialize)]
struct ReadArgs {
    #[serde(default)]
    server: Option<String>,
    #[serde(default, alias = "resource", alias = "url")]
    uri: Option<String>,
}

/// Resolve which server a `read_mcp_resource` call targets. `names` is every
/// connected resource-capable server in registration order. With an explicit
/// `requested` name, returns its index or an error listing the known servers.
/// With no name: the sole server when there's exactly one, else an error asking
/// the caller to name one. Pure, so the disambiguation is unit-tested without
/// constructing live clients.
pub(crate) fn resolve_server(names: &[&str], requested: Option<&str>) -> Result<usize> {
    if names.is_empty() {
        return Err(anyhow!("no connected MCP server exposes resources"));
    }
    match requested {
        Some(name) => names.iter().position(|n| *n == name).ok_or_else(|| {
            anyhow!(
                "unknown MCP server `{name}`; resource-capable servers: {}",
                names.join(", ")
            )
        }),
        None if names.len() == 1 => Ok(0),
        None => Err(anyhow!(
            "multiple MCP servers expose resources ({}); pass `server` to choose one",
            names.join(", ")
        )),
    }
}

#[async_trait]
impl BuiltinTool for ListMcpResources {
    fn name(&self) -> &'static str {
        "list_mcp_resources"
    }

    fn description(&self) -> &'static str {
        "List the resources exposed by connected MCP servers (files, configs, docs a server makes available by URI). With no `server`, lists every resource-capable server; with `server`, lists just that one. Read the contents of a listed entry with `read_mcp_resource`.\n\
\n\
Parameters:\n\
- `server` (optional): limit the listing to one MCP server by name."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Optional MCP server name to list resources from; omit to list all."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: ListArgs = crate::tools::parse_args("list_mcp_resources", args)?;
        let names: Vec<&str> = self.clients.iter().map(|c| c.name.as_str()).collect();
        if let Some(server) = a.server.as_deref() {
            let idx = resolve_server(&names, Some(server))?;
            return self.clients[idx].list_resources().await;
        }
        // No server named: aggregate every server's listing under a header.
        let mut out = String::new();
        for client in &self.clients {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&format!("## {}\n", client.name));
            match client.list_resources().await {
                Ok(listing) => out.push_str(&listing),
                Err(e) => out.push_str(&format!("(error listing resources: {e})")),
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl BuiltinTool for ReadMcpResource {
    fn name(&self) -> &'static str {
        "read_mcp_resource"
    }

    fn description(&self) -> &'static str {
        "Read one resource exposed by an MCP server, by URI. Find URIs with `list_mcp_resources` first. When more than one server exposes resources, pass `server` to disambiguate.\n\
\n\
Parameters:\n\
- `uri`: the resource URI to read (as shown by `list_mcp_resources`).\n\
- `server` (optional): the MCP server to read from; required only when several expose resources."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The resource URI to read (from list_mcp_resources)."
                },
                "server": {
                    "type": "string",
                    "description": "Optional MCP server name; required only when several expose resources."
                }
            },
            "required": ["uri"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: ReadArgs = crate::tools::parse_args("read_mcp_resource", args)?;
        let uri = a
            .uri
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("uri is required"))?;
        let names: Vec<&str> = self.clients.iter().map(|c| c.name.as_str()).collect();
        let idx = resolve_server(&names, a.server.as_deref())?;
        self.clients[idx].read_resource(uri).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_server_picks_by_name_or_sole_server() {
        // Exactly one server → used whether or not it's named.
        assert_eq!(resolve_server(&["fs"], None).unwrap(), 0);
        assert_eq!(resolve_server(&["fs"], Some("fs")).unwrap(), 0);
        // Several servers → name picks the index.
        assert_eq!(resolve_server(&["a", "b", "c"], Some("b")).unwrap(), 1);
    }

    #[test]
    fn resolve_server_errors_are_descriptive() {
        // No resource-capable servers at all.
        let err = resolve_server(&[], None).unwrap_err().to_string();
        assert!(err.contains("no connected MCP server"));
        // Unknown name lists the known ones.
        let err = resolve_server(&["a", "b"], Some("zzz"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown MCP server `zzz`"));
        assert!(err.contains("a, b"));
        // Ambiguous (several, none named) asks for a server.
        let err = resolve_server(&["a", "b"], None).unwrap_err().to_string();
        assert!(err.contains("pass `server`"));
        assert!(err.contains("a, b"));
    }

    #[tokio::test]
    async fn read_requires_a_uri() {
        let tool = ReadMcpResource::new(vec![]);
        let ctx = ToolContext::new(std::env::temp_dir(), crate::tools::ApprovalMode::OnRequest);
        let err = tool
            .execute(json!({"server": "fs"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("uri is required"));
        assert!(tool.is_read_only());
    }

    #[test]
    fn definitions_are_valid_strict_object_schemas() {
        // These tools are registered dynamically (not in `Registry::standard()`),
        // so the standard strict-schema sweep doesn't cover them. Guard that the
        // OpenAI strict wrapper still produces a well-formed object schema here.
        for def in [
            ListMcpResources::new(vec![]).definition(),
            ReadMcpResource::new(vec![]).definition(),
        ] {
            let crate::openai::Tool::Function(f) = def else {
                panic!("must be function tools");
            };
            assert_eq!(f.parameters["type"], "object");
            assert!(f.parameters.get("additionalProperties").is_some());
            assert!(f.parameters.get("properties").is_some());
        }
        // read_mcp_resource keeps `uri` in its required set.
        let crate::openai::Tool::Function(read) = ReadMcpResource::new(vec![]).definition() else {
            unreachable!()
        };
        let required = read.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "uri"));
    }

    #[tokio::test]
    async fn list_with_no_servers_lists_nothing_without_panicking() {
        let tool = ListMcpResources::new(vec![]);
        let ctx = ToolContext::new(std::env::temp_dir(), crate::tools::ApprovalMode::OnRequest);
        // No server named, no clients → empty aggregate, not an error.
        let out = tool.execute(json!({}), &ctx).await.unwrap();
        assert!(out.is_empty());
        assert!(tool.is_read_only());
        // A named-but-absent server still errors usefully.
        let err = tool
            .execute(json!({"server": "nope"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no connected MCP server"));
    }
}

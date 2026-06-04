//! Adapts a discovered MCP tool to tomte's [`BuiltinTool`] interface so the
//! agent can invoke it identically to its own built-ins.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::{McpClient, McpToolInfo};
use crate::openai::{Tool, ToolFunctionDef};
use crate::tools::{BuiltinTool, ToolContext};

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
}

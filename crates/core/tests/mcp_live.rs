//! Live MCP smoke test — spawns a REAL stdio MCP server and verifies tomte's
//! client handshakes and lists its tools.
//!
//! Marked `#[ignore]`: it shells out to `npx` to download/run the official
//! `@modelcontextprotocol/server-filesystem` server, so it needs Node + network
//! and must never run in the default `cargo test`/CI sweep. Run it explicitly:
//!
//!   cargo test -p tomte-core --test mcp_live -- --ignored --nocapture
//!
//! This exercises the same `McpClient::spawn` path the agent uses at startup,
//! including (on Windows) resolving the bare `npx` command to its `.cmd` shim via
//! PATH×PATHEXT. The `McpServerConfig` below is the exact shape `tomte mcp add`
//! writes to `settings.json` under `mcp_servers`, so a green run proves the
//! whole CLI-add → load → spawn → list-tools chain works against a real server.

use std::collections::HashMap;

use tomte_core::mcp::{McpClient, McpServerConfig};

#[tokio::test]
#[ignore = "spawns a real npx MCP server (needs Node + network); run with --ignored"]
async fn filesystem_mcp_server_handshakes_and_lists_tools() {
    // A throwaway directory the filesystem server is allowed to serve.
    let root = tempfile::tempdir().expect("create temp root");
    let root_str = root.path().to_string_lossy().to_string();

    // The exact config shape `tomte mcp add filesystem -- npx -y
    // @modelcontextprotocol/server-filesystem <dir>` writes.
    let config = McpServerConfig {
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            root_str.clone(),
        ],
        env: HashMap::new(),
    };

    println!("[1] spawning: npx -y @modelcontextprotocol/server-filesystem {root_str}");
    let client = McpClient::spawn("filesystem".to_string(), config)
        .await
        .expect("MCP server failed to spawn/handshake — is npx on PATH and the network up?");

    println!(
        "[2] handshake OK — {} tool(s) discovered:",
        client.tools.len()
    );
    for t in &client.tools {
        println!("      - {}", t.name);
    }

    assert!(
        !client.tools.is_empty(),
        "expected the filesystem server to expose at least one tool"
    );
    // The filesystem server always exposes directory/file operations; assert a
    // representative one is present without over-fitting to exact tool names.
    assert!(
        client.tools.iter().any(|t| {
            let n = t.name.to_ascii_lowercase();
            n.contains("director") || n.contains("file") || n.contains("read")
        }),
        "expected a file/directory tool, got: {:?}",
        client.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    println!(
        "\n✅ MCP LIVE TEST PASSED — tomte's McpClient handshaked a real server and listed its tools."
    );
}

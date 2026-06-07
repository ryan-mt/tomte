use super::*;

#[cfg(windows)]
#[test]
fn resolve_windows_program_finds_cmd_shim_via_pathext() {
    // The canonical MCP config uses a bare `npx`, which on Windows is a `.cmd`
    // shim CreateProcessW won't find. The resolver must locate it via PATHEXT.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mytool.cmd"), "@echo off\n").unwrap();
    let path = std::ffi::OsString::from(dir.path());
    let resolved =
        resolve_program_in("mytool", &path, ".COM;.EXE;.BAT;.CMD").expect("should resolve shim");
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .eq_ignore_ascii_case("mytool.cmd"),
        "got: {resolved:?}"
    );
    // A name that already carries an extension or a path is used verbatim.
    assert!(resolve_program_in("mytool.exe", &path, ".CMD").is_none());
    assert!(resolve_program_in(r"C:\abs\mytool", &path, ".CMD").is_none());
    // An unfindable bare name resolves to nothing (caller keeps the original).
    assert!(resolve_program_in("does-not-exist-xyz", &path, ".CMD").is_none());
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

    let err =
        match McpClient::spawn_with_timeout("leaky".to_string(), cfg, Duration::from_millis(80))
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

#[cfg(unix)]
#[tokio::test]
async fn request_timeout_bounds_whole_request_not_each_line() {
    // Regression: a server that streams unrelated notifications faster than
    // the timeout must not keep the request alive forever. The old per-line
    // timeout reset on every notification; the request now has one deadline.
    let cfg = McpServerConfig {
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "while true; do printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"noise\",\"params\":{}}'; sleep 0.02; done".to_string(),
        ],
        env: HashMap::new(),
    };
    // Outer guard so a regression (unbounded request) fails fast instead of
    // hanging the test: with the bug the inner spawn never resolves.
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        McpClient::spawn_with_timeout("chatty".to_string(), cfg, Duration::from_millis(150)),
    )
    .await;
    match result {
        Ok(Ok(_)) => panic!("handshake must not succeed against a noise-only server"),
        Ok(Err(err)) => assert!(err.to_string().contains("timed out"), "got: {err}"),
        Err(_) => panic!("request was not bounded by the timeout (per-line reset regression)"),
    }
}

#[test]
fn normalize_mcp_schema_coerces_unusable_shapes() {
    // A valid object schema is preserved untouched.
    let s = json!({"type": "object", "properties": {"x": {"type": "string"}}});
    assert_eq!(normalize_mcp_schema(Some(s.clone())), s);
    // An object missing `type` gets `type: object`.
    assert_eq!(
        normalize_mcp_schema(Some(json!({"properties": {}})))["type"],
        "object"
    );
    // Absent or non-object schemas fall back to an empty object schema.
    let fallback = json!({"type": "object", "properties": {}});
    assert_eq!(normalize_mcp_schema(None), fallback);
    assert_eq!(normalize_mcp_schema(Some(json!("nope"))), fallback);
    assert_eq!(normalize_mcp_schema(Some(json!([1, 2]))), fallback);
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

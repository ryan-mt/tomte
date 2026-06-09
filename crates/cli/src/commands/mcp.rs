//! `tomte mcp` — manage MCP (Model Context Protocol) servers without hand-editing
//! `settings.json`. Reads and writes the `mcp_servers` map in the config-dir
//! `settings.json`, preserving every other key (hooks, etc.). Each server's tools
//! are exposed to the agent as `mcp__<server>__<tool>`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};
use tomte_core::hooks::settings_path;
use tomte_core::mcp::{load_servers_config, McpServerConfig};

/// `tomte mcp <action>` — manage configured MCP servers.
#[derive(Debug, clap::Subcommand)]
pub enum McpAction {
    /// List configured MCP servers (the default action).
    List,
    /// Show one server's command, args, and env keys.
    Get {
        /// Server name (the key under `mcp_servers`).
        name: String,
    },
    /// Add or update a server. Put the command after `--`, e.g.
    /// `tomte mcp add fs -- npx -y @modelcontextprotocol/server-filesystem /tmp`.
    /// Repeat `--env KEY=VALUE` (before the command) to set environment variables.
    Add {
        /// Server name (the key under `mcp_servers`).
        name: String,
        /// Environment variable as KEY=VALUE (repeatable). Place before the command.
        #[arg(long = "env", short = 'e', value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// The command to launch, then its arguments.
        #[arg(trailing_var_arg = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Remove a server by name.
    #[command(visible_alias = "rm")]
    Remove {
        /// Server name to remove.
        name: String,
    },
}

pub async fn run(action: Option<McpAction>) -> Result<()> {
    match action.unwrap_or(McpAction::List) {
        McpAction::List => list(),
        McpAction::Get { name } => get(&name),
        McpAction::Add { name, env, command } => add(&name, &command, &env),
        McpAction::Remove { name } => remove(&name),
    }
}

fn list() -> Result<()> {
    let servers = load_servers_config();
    let path = settings_path();
    if servers.is_empty() {
        println!("No MCP servers configured.");
        println!("\nAdd one:  tomte mcp add <name> -- <command> [args…]");
        println!(
            "  e.g.    tomte mcp add filesystem -- npx -y @modelcontextprotocol/server-filesystem /tmp"
        );
        println!("\nStored in {}", path.display());
        return Ok(());
    }
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();
    println!("MCP servers — exposed to the agent as mcp__<server>__<tool>:\n");
    for name in &names {
        let cfg = &servers[*name];
        let args = if cfg.args.is_empty() {
            String::new()
        } else {
            format!(" {}", cfg.args.join(" "))
        };
        println!("  {name}");
        println!("    $ {}{args}", cfg.command);
        if !cfg.env.is_empty() {
            println!("    env: {}  (values hidden)", env_keys(cfg));
        }
    }
    println!(
        "\n{} server{} in {}",
        names.len(),
        if names.len() == 1 { "" } else { "s" },
        path.display()
    );
    Ok(())
}

fn get(name: &str) -> Result<()> {
    let servers = load_servers_config();
    let cfg = servers.get(name).ok_or_else(|| not_found(name, &servers))?;
    println!("{name}");
    println!("  command: {}", cfg.command);
    if cfg.args.is_empty() {
        println!("  args:    (none)");
    } else {
        println!("  args:    {}", cfg.args.join(" "));
    }
    if cfg.env.is_empty() {
        println!("  env:     (none)");
    } else {
        println!("  env:     {}  (values hidden)", env_keys(cfg));
    }
    Ok(())
}

fn add(name: &str, command: &[String], env: &[String]) -> Result<()> {
    let path = settings_path();
    let updated = add_in(&path, name, command, env)?;
    println!(
        "{} MCP server `{name}`",
        if updated { "✓ updated" } else { "✓ added" }
    );
    println!("  {}", path.display());
    println!("  takes effect next time you start tomte");
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let path = settings_path();
    if remove_in(&path, name)? {
        println!("✓ removed MCP server `{name}`");
        println!("  {}", path.display());
    } else {
        println!("· no MCP server named `{name}`");
    }
    Ok(())
}

/// Comma-joined, sorted env keys for display (values are never printed).
fn env_keys(cfg: &McpServerConfig) -> String {
    let mut keys: Vec<&String> = cfg.env.keys().collect();
    keys.sort();
    keys.iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn not_found(name: &str, servers: &HashMap<String, McpServerConfig>) -> anyhow::Error {
    if servers.is_empty() {
        return anyhow!("no MCP server named `{name}` — none are configured (see `tomte mcp add`)");
    }
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();
    let names = names
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow!("no MCP server named `{name}` — configured: {names}")
}

// --- path-parameterized core (so tests run hermetically, no env vars) ---

/// Insert or overwrite `name` in `mcp_servers`. Returns `true` if it replaced an
/// existing entry, `false` if it was newly added. Preserves every other key.
fn add_in(path: &Path, name: &str, command: &[String], env: &[String]) -> Result<bool> {
    if name.trim().is_empty() {
        return Err(anyhow!("server name must not be empty"));
    }
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow!("a command is required, e.g. `-- npx -y server`"))?;
    if program.trim().is_empty() {
        return Err(anyhow!("the command must not be empty"));
    }
    let env_map = parse_env(env)?;

    let mut entry = Map::new();
    entry.insert("command".into(), Value::String(program.clone()));
    if !args.is_empty() {
        entry.insert(
            "args".into(),
            Value::Array(args.iter().cloned().map(Value::String).collect()),
        );
    }
    if !env_map.is_empty() {
        entry.insert("env".into(), Value::Object(env_map));
    }

    let mut root = read_settings(path)?;
    let servers = root
        .entry("mcp_servers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("`mcp_servers` in settings.json is not an object"))?;
    let existed = servers.contains_key(name);
    servers.insert(name.to_string(), Value::Object(entry));
    write_settings(path, &Value::Object(root))?;
    Ok(existed)
}

/// Remove `name` from `mcp_servers`. Returns `true` if it was present and
/// removed, `false` if it was not found. Tidies away an emptied `mcp_servers`.
fn remove_in(path: &Path, name: &str) -> Result<bool> {
    let mut root = read_settings(path)?;
    let Some(servers) = root.get_mut("mcp_servers").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    if servers.remove(name).is_none() {
        return Ok(false);
    }
    if servers.is_empty() {
        root.remove("mcp_servers");
    }
    write_settings(path, &Value::Object(root))?;
    Ok(true)
}

/// Parse `KEY=VALUE` strings into a JSON object, splitting on the first `=` so a
/// value may itself contain `=`. Rejects an entry with no `=` or an empty key.
fn parse_env(entries: &[String]) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for e in entries {
        let (k, v) = e
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid --env `{e}` — expected KEY=VALUE"))?;
        if k.trim().is_empty() {
            return Err(anyhow!("invalid --env `{e}` — empty key"));
        }
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Ok(map)
}

/// Read settings.json into its top-level object. A missing or empty file is an
/// empty object (not an error); a present-but-malformed file errors so we never
/// silently drop the user's `mcp_servers`/hooks by overwriting garbage.
fn read_settings(path: &Path) -> Result<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(e).context("read settings.json"),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text).context("parse settings.json")? {
        Value::Object(map) => Ok(map),
        _ => Err(anyhow!("settings.json is not a JSON object")),
    }
}

/// Atomically rewrite settings.json (owner-only on Unix) via tmp-then-rename, so
/// a crash mid-write can't corrupt the user's config.
fn write_settings(path: &Path, root: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        tomte_core::config::create_dir_secure(dir)?;
    }
    let text = serde_json::to_string_pretty(root)?;
    let tmp = path.with_file_name(format!("settings.json.{}.tmp", std::process::id()));
    write_owner_only(&tmp, text.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).context("replace settings.json")?;
    Ok(())
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    drop(f);
    // settings.json can carry MCP `--env KEY=VALUE` secrets: give it the same
    // owner-only enforcement auth.json/config.json get. On Windows this is the
    // icacls ACL the Unix `mode(0o600)` above can't provide; tightened before
    // the caller's rename so the live file is never broader than the user.
    tomte_core::config::restrict_file_to_owner(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn add_writes_server_with_command_args_env() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let updated = add_in(
            &path,
            "fs",
            &["npx".into(), "-y".into(), "server".into(), "/tmp".into()],
            &["API_KEY=secret".into()],
        )
        .unwrap();
        assert!(!updated, "first add is new, not an update");
        let v = read(&path);
        assert_eq!(v["mcp_servers"]["fs"]["command"], "npx");
        assert_eq!(
            v["mcp_servers"]["fs"]["args"],
            serde_json::json!(["-y", "server", "/tmp"])
        );
        assert_eq!(v["mcp_servers"]["fs"]["env"]["API_KEY"], "secret");
    }

    #[test]
    fn add_preserves_other_keys_and_omits_empty_args_env() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"hooks":{"Stop":[]},"model":"gpt-5.5"}"#).unwrap();
        add_in(&path, "fs", &["npx".into()], &[]).unwrap();
        let v = read(&path);
        assert_eq!(v["model"], "gpt-5.5");
        assert!(v["hooks"]["Stop"].is_array());
        assert_eq!(v["mcp_servers"]["fs"]["command"], "npx");
        assert!(v["mcp_servers"]["fs"].get("args").is_none());
        assert!(v["mcp_servers"]["fs"].get("env").is_none());
    }

    #[test]
    fn add_existing_name_updates_in_place() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        add_in(&path, "fs", &["old".into()], &[]).unwrap();
        let updated = add_in(&path, "fs", &["new".into()], &[]).unwrap();
        assert!(updated, "re-adding the same name is an update");
        assert_eq!(read(&path)["mcp_servers"]["fs"]["command"], "new");
    }

    #[test]
    fn remove_deletes_and_tidies_empty_map() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        add_in(&path, "fs", &["npx".into()], &[]).unwrap();
        assert!(remove_in(&path, "fs").unwrap());
        let v = read(&path);
        assert!(
            v.get("mcp_servers").is_none(),
            "an emptied mcp_servers should be tidied away"
        );
    }

    #[test]
    fn remove_missing_is_false_and_keeps_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"model":"x"}"#).unwrap();
        assert!(!remove_in(&path, "nope").unwrap());
        assert_eq!(read(&path)["model"], "x");
    }

    #[test]
    fn parse_env_splits_on_first_equals_and_rejects_bare() {
        let m = parse_env(&["A=1".into(), "B=x=y".into()]).unwrap();
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "x=y");
        assert!(parse_env(&["NOEQ".into()]).is_err());
        assert!(parse_env(&["=v".into()]).is_err());
    }

    #[test]
    fn read_settings_missing_is_empty_malformed_errors() {
        let dir = tempdir().unwrap();
        assert!(read_settings(&dir.path().join("nope.json"))
            .unwrap()
            .is_empty());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(read_settings(&bad).is_err());
    }

    #[test]
    fn add_rejects_empty_command_or_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(add_in(&path, "fs", &[], &[]).is_err());
        assert!(add_in(&path, "", &["npx".into()], &[]).is_err());
    }
}

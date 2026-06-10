//! Built-in hook presets — one-line enable for the common "auto-format on edit"
//! workflows, so the agent self-triggers a tidy-up without the user hand-editing
//! `settings.json`.
//!
//! Each preset maps to a single entry in `settings.json` under its event.
//! Commands are deliberately written to run cross-platform: every one is a plain
//! `program + args` invocation that behaves the same under `sh -c` (Linux,
//! macOS, Git Bash) and `cmd /C` (stock Windows) — see
//! [`super::build_shell_command`]. They format the whole project rather than
//! parsing the changed path out of the hook's stdin, which would need `jq` on
//! Unix and a different incantation on Windows.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

/// The lifecycle events a preset can attach to. The string form is the exact
/// key used in `settings.json` (matching [`super::HooksConfig`]'s serde renames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    SessionStart,
    Stop,
}

impl HookEvent {
    pub fn key(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::SessionStart => "SessionStart",
            HookEvent::Stop => "Stop",
        }
    }
}

/// One installable preset: a stable `id`, a human description, and the single
/// hook entry (`event` + `matcher` + `command`) it writes.
#[derive(Debug, Clone, Copy)]
pub struct HookPreset {
    pub id: &'static str,
    pub description: &'static str,
    pub event: HookEvent,
    pub matcher: &'static str,
    pub command: &'static str,
}

/// The built-in preset catalog. Kept intentionally small and
/// cross-platform-safe: every `command` is a plain program invocation that runs
/// identically under `sh -c` and `cmd /C`. New formatters can be added here.
static PRESETS: &[HookPreset] = &[
    HookPreset {
        id: "rustfmt",
        description: "Run `cargo fmt` after tomte edits a .rs file",
        event: HookEvent::PostToolUse,
        matcher: "file:**/*.rs",
        command: "cargo fmt",
    },
    HookPreset {
        id: "gofmt",
        description: "Run `gofmt -w .` after tomte edits a .go file",
        event: HookEvent::PostToolUse,
        matcher: "file:**/*.go",
        command: "gofmt -w .",
    },
    HookPreset {
        id: "prettier",
        description: "Run `prettier --write` after tomte edits a JS/TS/JSON/CSS/Markdown file",
        event: HookEvent::PostToolUse,
        matcher: "file:**/*.{js,jsx,ts,tsx,json,jsonc,css,scss,md,mdx,yaml,yml,html}",
        command: "npx --no-install prettier --write .",
    },
];

/// All built-in presets, in catalog order.
pub fn all() -> &'static [HookPreset] {
    PRESETS
}

/// Look up a preset by id.
pub fn get(id: &str) -> Option<&'static HookPreset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Whether an enable/disable actually changed `settings.json`, so the caller can
/// print the right line ("enabled" vs "already enabled").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Applied,
    NoOp,
}

/// Enable a preset by id (idempotent). Returns [`Change::NoOp`] if it was
/// already present. Preserves every other key in `settings.json` (notably
/// `mcp_servers`) and any hooks the user added by hand.
pub fn enable(id: &str) -> Result<Change> {
    enable_in(&super::settings_path(), id)
}

/// Disable a preset by id. Returns [`Change::NoOp`] if it was not enabled.
pub fn disable(id: &str) -> Result<Change> {
    disable_in(&super::settings_path(), id)
}

/// Each preset paired with whether it is currently enabled, in catalog order.
/// Lenient: a missing or unparseable file reports everything disabled.
pub fn status() -> Vec<(&'static HookPreset, bool)> {
    let root = read_settings(&super::settings_path()).unwrap_or_default();
    PRESETS.iter().map(|p| (p, is_enabled(&root, p))).collect()
}

// --- path-parameterized core (so tests run hermetically, no env vars) ---

fn enable_in(path: &Path, id: &str) -> Result<Change> {
    let preset = get(id).ok_or_else(|| unknown(id))?;
    let mut root = read_settings(path)?;
    let arr = hooks_event_array_mut(&mut root, preset.event)?;
    if arr.iter().any(|e| entry_matches(e, preset)) {
        return Ok(Change::NoOp);
    }
    arr.push(serde_json::json!({
        "matcher": preset.matcher,
        "command": preset.command,
    }));
    write_settings(path, &root)?;
    Ok(Change::Applied)
}

fn disable_in(path: &Path, id: &str) -> Result<Change> {
    let preset = get(id).ok_or_else(|| unknown(id))?;
    let mut root = read_settings(path)?;
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(Change::NoOp);
    };
    let Some(arr) = hooks
        .get_mut(preset.event.key())
        .and_then(Value::as_array_mut)
    else {
        return Ok(Change::NoOp);
    };
    let before = arr.len();
    arr.retain(|e| !entry_matches(e, preset));
    if arr.len() == before {
        return Ok(Change::NoOp);
    }
    // Tidy up: drop an emptied event array, then an emptied `hooks` object, so
    // disabling the last preset leaves settings.json as clean as it started.
    if arr.is_empty() {
        hooks.remove(preset.event.key());
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    write_settings(path, &root)?;
    Ok(Change::Applied)
}

fn unknown(id: &str) -> anyhow::Error {
    let ids = PRESETS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ");
    anyhow!("unknown preset `{id}` — available: {ids}")
}

fn is_enabled(root: &Map<String, Value>, preset: &HookPreset) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .and_then(|h| h.get(preset.event.key()))
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|e| entry_matches(e, preset)))
}

fn entry_matches(entry: &Value, preset: &HookPreset) -> bool {
    let Some(obj) = entry.as_object() else {
        return false;
    };
    obj.get("matcher").and_then(Value::as_str) == Some(preset.matcher)
        && obj.get("command").and_then(Value::as_str) == Some(preset.command)
}

/// Get (creating if needed) the mutable hook array for `event`, erroring if an
/// existing `hooks`/event value has the wrong JSON shape — so a hand-edited file
/// we don't understand is reported, never clobbered.
fn hooks_event_array_mut(
    root: &mut Map<String, Value>,
    event: HookEvent,
) -> Result<&mut Vec<Value>> {
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow!("`hooks` in settings.json is not an object"))?;
    let arr = hooks
        .entry(event.key())
        .or_insert_with(|| Value::Array(Vec::new()));
    arr.as_array_mut()
        .ok_or_else(|| anyhow!("`hooks.{}` in settings.json is not an array", event.key()))
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
    let text = crate::config::strip_bom(&text);
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text).context("parse settings.json")? {
        Value::Object(map) => Ok(map),
        _ => Err(anyhow!("settings.json is not a JSON object")),
    }
}

/// Atomically rewrite settings.json (owner-only on Unix), reusing the same
/// secure tmp-then-rename helpers as config.json.
fn write_settings(path: &Path, root: &Map<String, Value>) -> Result<()> {
    if let Some(dir) = path.parent() {
        crate::config::create_dir_secure(dir)?;
    }
    let text = serde_json::to_string_pretty(&Value::Object(root.clone()))?;
    let tmp = crate::config::unique_tmp_path(path);
    crate::config::write_config_file(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path).context("replace settings.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_ids_are_unique_and_nonempty() {
        let mut seen = std::collections::HashSet::new();
        for p in all() {
            assert!(!p.id.is_empty(), "empty preset id");
            assert!(!p.command.is_empty(), "empty command for {}", p.id);
            assert!(!p.event.key().is_empty());
            assert!(seen.insert(p.id), "duplicate preset id {}", p.id);
        }
        assert!(get("rustfmt").is_some());
        assert!(get("nope").is_none());
    }

    #[test]
    fn read_settings_tolerates_a_utf8_bom() {
        // Same tolerance as the core loaders: a BOM'd settings.json must not
        // error the preset read-modify-write path.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "\u{feff}{\"hooks\":{}}").unwrap();
        let root = read_settings(&path).unwrap();
        assert!(root.contains_key("hooks"));
    }

    #[test]
    fn enable_is_idempotent_and_loads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");

        assert_eq!(enable_in(&path, "rustfmt").unwrap(), Change::Applied);
        assert_eq!(enable_in(&path, "rustfmt").unwrap(), Change::NoOp);

        let root = read_settings(&path).unwrap();
        let arr = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "idempotent enable must not duplicate");
        assert_eq!(arr[0]["matcher"], "file:**/*.rs");
        assert_eq!(arr[0]["command"], "cargo fmt");

        assert!(is_enabled(&root, get("rustfmt").unwrap()));
        assert!(!is_enabled(&root, get("gofmt").unwrap()));
    }

    #[test]
    fn enable_preserves_mcp_servers_and_user_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "mcp_servers": { "fs": { "command": "npx", "args": ["-y", "server"] } },
              "hooks": { "PostToolUse": [ { "matcher": "file:**/*.py", "command": "black ." } ] }
            }"#,
        )
        .unwrap();

        enable_in(&path, "rustfmt").unwrap();

        let root = read_settings(&path).unwrap();
        // mcp_servers must survive a hook rewrite (same file).
        assert_eq!(root["mcp_servers"]["fs"]["command"], "npx");
        // The user's python hook stays; our rust one is added alongside it.
        let arr = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|e| e["command"] == "black ."));
        assert!(arr.iter().any(|e| e["command"] == "cargo fmt"));
    }

    #[test]
    fn disable_removes_only_the_preset_and_tidies_up() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");

        enable_in(&path, "rustfmt").unwrap();
        assert_eq!(disable_in(&path, "rustfmt").unwrap(), Change::Applied);
        assert_eq!(disable_in(&path, "rustfmt").unwrap(), Change::NoOp);

        let root = read_settings(&path).unwrap();
        assert!(
            !root.contains_key("hooks"),
            "an emptied hooks object should be tidied away"
        );
    }

    #[test]
    fn disable_keeps_a_user_hook_with_a_different_command() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{ "hooks": { "PostToolUse": [ { "matcher": "file:**/*.rs", "command": "echo hi" } ] } }"#,
        )
        .unwrap();

        enable_in(&path, "rustfmt").unwrap(); // adds `cargo fmt` next to `echo hi`
        disable_in(&path, "rustfmt").unwrap(); // removes only `cargo fmt`

        let root = read_settings(&path).unwrap();
        let arr = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], "echo hi");
    }

    #[test]
    fn unknown_preset_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        assert!(enable_in(&path, "does-not-exist").is_err());
        assert!(disable_in(&path, "does-not-exist").is_err());
    }
}

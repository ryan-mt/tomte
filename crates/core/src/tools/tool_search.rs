//! The `tool_search` tool: progressive disclosure for MCP tools.
//!
//! When a workspace connects many MCP servers, injecting every tool's JSON
//! schema into each request burns context that mostly goes unused. So past a
//! threshold (`Registry::enable_tool_search`) tomte *defers* the MCP tools:
//! their schemas are withheld from the request, and the system prompt instead
//! lists them by `name: description` (one line each, like the skill manifest).
//!
//! This tool is the second half of that disclosure. The model calls it with a
//! query; matching tools are returned WITH their schemas and marked
//! `activated`. From the model's *next* message those tools appear in the
//! request's tool list and can be called directly — exactly the deferred-tool
//! flow Claude Code uses. Activation is shared (`Arc<Mutex<…>>`) so this
//! tool's `execute` can record it while `Registry::definitions` reads it.

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::{BuiltinTool, ToolContext};

/// One deferred tool's catalog entry: enough to search by and to hand back a
/// callable schema once a search matches it.
#[derive(Clone)]
pub struct DeferredToolInfo {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

pub struct ToolSearch {
    catalog: Vec<DeferredToolInfo>,
    activated: Arc<Mutex<HashSet<String>>>,
}

impl ToolSearch {
    pub fn new(catalog: Vec<DeferredToolInfo>, activated: Arc<Mutex<HashSet<String>>>) -> Self {
        Self { catalog, activated }
    }

    /// Resolve a query to deferred-tool matches. `select:a,b` selects by exact
    /// name; anything else is a keyword search scored by how many query words
    /// occur in the tool's `name + description`, best first.
    fn search(&self, query: &str, max: usize) -> Vec<DeferredToolInfo> {
        let q = query.trim();
        if let Some(rest) = q.strip_prefix("select:") {
            let names: HashSet<&str> = rest
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            return self
                .catalog
                .iter()
                .filter(|c| names.contains(c.name.as_str()))
                .cloned()
                .collect();
        }
        let tokens: Vec<String> = q
            .to_lowercase()
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect();
        if tokens.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &DeferredToolInfo)> = self
            .catalog
            .iter()
            .filter_map(|c| {
                let hay = format!("{} {}", c.name, c.description).to_lowercase();
                let score = tokens.iter().filter(|t| hay.contains(t.as_str())).count();
                (score > 0).then_some((score, c))
            })
            .collect();
        // Stable best-first: higher score wins; ties keep catalog (name) order.
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored
            .into_iter()
            .take(max)
            .map(|(_, c)| c.clone())
            .collect()
    }

    /// A few representative names to suggest when nothing matched.
    fn sample_names(&self, n: usize) -> String {
        self.catalog
            .iter()
            .take(n)
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Deserialize)]
struct Args {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

const DEFAULT_MAX_RESULTS: usize = 8;

#[async_trait]
impl BuiltinTool for ToolSearch {
    fn name(&self) -> &'static str {
        "tool_search"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Load the schemas of deferred MCP tools so you can call them.\n\
\n\
This workspace connects many MCP tools, so their schemas are withheld from the request to save context. They are listed by name + one-line description under \"# Searchable tools\" in your system prompt; until you load one here, calling it directly will fail.\n\
\n\
When to use:\n\
- A task matches one of the tools under \"# Searchable tools\" — load it, then call it on your NEXT message.\n\
\n\
When NOT to use:\n\
- For a built-in tool (read_file, grep, run_shell, …) or an MCP tool you already loaded this session; it is already callable.\n\
- Speculatively. Load only what the task needs.\n\
\n\
Parameters:\n\
- `query`: keywords to match against tool names/descriptions, OR `select:<name1>,<name2>` to load exact names from the manifest.\n\
- `max_results`: optional cap on how many tools to load (default 8).\n\
\n\
Behaviour:\n\
- Returns each matched tool's name, description, and parameter schema, and makes it callable from your next message. Loaded tools persist for the rest of the session.\n\
- If nothing matches, the error lists some available names so you can retry."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords to match against deferred tool names/descriptions, or `select:<name>,<name>` to load exact names."
                },
                "max_results": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Maximum tools to load; null uses the default (8)."
                }
            },
            "required": ["query", "max_results"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let a: Args = super::parse_args("tool_search", args)?;
        let max = a.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);
        let matches = self.search(&a.query, max);
        if matches.is_empty() {
            return Ok(format!(
                "No deferred tools matched `{}`. {} tool(s) are searchable; e.g. {}. \
                 Retry with different keywords, or `select:<exact-name>`.",
                a.query.trim(),
                self.catalog.len(),
                self.sample_names(8)
            ));
        }
        let mut activated = self.activated.lock().unwrap();
        let mut out = format!(
            "Loaded {} tool(s). They are callable from your next message:\n\n",
            matches.len()
        );
        for info in &matches {
            activated.insert(info.name.clone());
            let def = json!({
                "name": info.name,
                "description": info.description,
                "parameters": info.schema,
            });
            out.push_str("<function>");
            out.push_str(&serde_json::to_string(&def).unwrap_or_default());
            out.push_str("</function>\n");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<DeferredToolInfo> {
        vec![
            DeferredToolInfo {
                name: "mcp__github__create_issue".into(),
                description: "Open a new GitHub issue in a repository".into(),
                schema: json!({"type": "object", "properties": {"title": {"type": "string"}}}),
            },
            DeferredToolInfo {
                name: "mcp__github__list_pulls".into(),
                description: "List pull requests".into(),
                schema: json!({"type": "object"}),
            },
            DeferredToolInfo {
                name: "mcp__slack__post_message".into(),
                description: "Send a message to a Slack channel".into(),
                schema: json!({"type": "object"}),
            },
        ]
    }

    fn fresh() -> ToolSearch {
        ToolSearch::new(catalog(), Arc::new(Mutex::new(HashSet::new())))
    }

    #[test]
    fn keyword_search_scores_best_first() {
        let ts = fresh();
        let hits = ts.search("github issue", 8);
        assert_eq!(hits[0].name, "mcp__github__create_issue");
        // "list_pulls" still matches on "github" so it appears, lower-ranked.
        assert!(hits.iter().any(|h| h.name == "mcp__github__list_pulls"));
        assert!(!hits.iter().any(|h| h.name == "mcp__slack__post_message"));
    }

    #[test]
    fn select_loads_exact_names() {
        let ts = fresh();
        let hits = ts.search("select:mcp__slack__post_message", 8);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "mcp__slack__post_message");
    }

    #[test]
    fn max_results_caps_matches() {
        let ts = fresh();
        // every tool description+name shares no single word, but "message"/"list"
        // — use a query hitting two, capped to 1.
        let hits = ts.search("message list", 1);
        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn execute_marks_tools_activated() {
        let activated = Arc::new(Mutex::new(HashSet::new()));
        let ts = ToolSearch::new(catalog(), activated.clone());
        let ctx = ToolContext::new(std::env::temp_dir(), crate::tools::ApprovalMode::OnRequest);
        let out = ts
            .execute(json!({"query": "slack message"}), &ctx)
            .await
            .unwrap();
        assert!(out.contains("mcp__slack__post_message"));
        assert!(activated
            .lock()
            .unwrap()
            .contains("mcp__slack__post_message"));
    }

    #[tokio::test]
    async fn execute_reports_no_match() {
        let ts = fresh();
        let ctx = ToolContext::new(std::env::temp_dir(), crate::tools::ApprovalMode::OnRequest);
        let out = ts
            .execute(json!({"query": "nonexistent-xyzzy"}), &ctx)
            .await
            .unwrap();
        assert!(out.contains("No deferred tools matched"));
    }
}

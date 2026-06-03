//! Context-window usage breakdown for the `/context` command.
//!
//! Estimates how the model's context window is occupied, split into the same
//! categories Claude Code's `/context` surfaces: the base system prompt, the
//! built-in tool schemas, custom agents, inherited memory files, skills, MCP
//! tools, and the visible conversation.
//!
//! The **counts** (agents, skills, memory files, MCP servers) are exact. The
//! per-category **token** figures are `chars/4` estimates of the real source
//! text — the same heuristic the rest of the TUI uses — so the panel is labelled
//! "estimated": a faithful approximation, not the provider's exact tokenisation.
//! The one authoritative number is [`ContextReport::used_real`], the occupancy
//! the provider reported on the last turn.

use std::path::{Path, PathBuf};

use crate::config::Config;

/// Rough token count for a blob of text (≈ 4 chars/token).
fn est(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// A fully-computed context breakdown. Field order matches display order.
#[derive(Debug, Clone)]
pub struct ContextReport {
    pub model: String,
    /// Effective context window of the active model/endpoint, in tokens.
    pub limit: u64,
    /// Provider-reported occupancy from the last turn (0 before the first turn).
    pub used_real: u64,

    // Estimated per-category tokens.
    pub system_prompt: u64,
    pub system_tools: u64,
    pub custom_agents: u64,
    pub memory_files: u64,
    pub skills: u64,
    pub mcp_tools: u64,
    pub messages: u64,

    // Exact detail lists (each also gives the count for its section).
    pub agent_names: Vec<String>,
    pub memory_paths: Vec<PathBuf>,
    pub skill_names: Vec<String>,
    pub mcp_servers: Vec<String>,
}

impl ContextReport {
    /// Sum of the estimated category tokens — the headline "used" figure the
    /// grid and percentages are drawn against.
    pub fn estimated_used(&self) -> u64 {
        self.system_prompt
            + self.system_tools
            + self.custom_agents
            + self.memory_files
            + self.skills
            + self.mcp_tools
            + self.messages
    }

    /// Free space = window minus the estimated used total (floored at 0).
    pub fn free(&self) -> u64 {
        self.limit.saturating_sub(self.estimated_used())
    }
}

/// Build the report for `cwd` / `config`.
///
/// `messages_tokens` is the caller's estimate of the visible conversation (the
/// TUI computes it from its on-screen blocks); `used_real` is the
/// provider-reported occupancy (0 if no turn has run yet).
pub fn build(cwd: &Path, config: &Config, messages_tokens: u64, used_real: u64) -> ContextReport {
    let system_prompt = est(&crate::agent::default_system_prompt());

    // Built-in tool schemas as they go on the wire.
    let defs = crate::tools::Registry::standard().definitions();
    let system_tools = serde_json::to_string(&defs).map(|s| est(&s)).unwrap_or(0);

    // Custom agents: estimate the surface (name + description) the dispatch path
    // exposes; the count itself is exact.
    let agents = crate::subagent::load_all(cwd);
    let agent_manifest: String = agents
        .iter()
        .map(|a| format!("{}: {}\n", a.name, a.description))
        .collect();
    let custom_agents = est(&agent_manifest);
    let agent_names: Vec<String> = agents.into_iter().map(|a| a.name).collect();

    // Memory: estimate from the exact blocks that get injected — inherited
    // files (CLAUDE.md/AGENTS.md) plus the agent-written memory-store index.
    let mut memory_block = String::new();
    crate::memory::apply_to_system_prompt(&mut memory_block, cwd);
    crate::tools::memory::apply_store_to_prompt(&mut memory_block, cwd);
    let memory_files = est(&memory_block);
    let memory_paths = crate::memory::applied_files(cwd);

    // Skills: the manifest (one `name: description` line per skill) injected into
    // the prompt, riding the prompt cache.
    let skill_entries = crate::skill::discover(cwd);
    let skills = est(&crate::skill::manifest(&skill_entries));
    let skill_names: Vec<String> = skill_entries.into_iter().map(|s| s.name).collect();

    let mut mcp_servers: Vec<String> = crate::mcp::load_servers_config().into_keys().collect();
    mcp_servers.sort();

    ContextReport {
        model: config.model.clone(),
        limit: config.effective_context_limit(),
        used_real,
        system_prompt,
        system_tools,
        custom_agents,
        memory_files,
        skills,
        // MCP tool schemas are loaded on-demand, not in the base prompt, so they
        // occupy no window until a tool is actually pulled in.
        mcp_tools: 0,
        messages: messages_tokens,
        agent_names,
        memory_paths,
        skill_names,
        mcp_servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn est_is_chars_over_four_rounded_up() {
        assert_eq!(est(""), 0);
        assert_eq!(est("abcd"), 1);
        assert_eq!(est("abcde"), 2);
    }

    #[test]
    fn estimated_used_sums_categories_and_free_is_remainder() {
        let r = ContextReport {
            model: "m".into(),
            limit: 1000,
            used_real: 0,
            system_prompt: 100,
            system_tools: 200,
            custom_agents: 10,
            memory_files: 50,
            skills: 40,
            mcp_tools: 0,
            messages: 100,
            agent_names: vec![],
            memory_paths: vec![],
            skill_names: vec![],
            mcp_servers: vec![],
        };
        assert_eq!(r.estimated_used(), 500);
        assert_eq!(r.free(), 500);
    }

    #[test]
    fn free_floors_at_zero_when_over_budget() {
        let r = ContextReport {
            model: "m".into(),
            limit: 100,
            used_real: 0,
            system_prompt: 200,
            system_tools: 0,
            custom_agents: 0,
            memory_files: 0,
            skills: 0,
            mcp_tools: 0,
            messages: 0,
            agent_names: vec![],
            memory_paths: vec![],
            skill_names: vec![],
            mcp_servers: vec![],
        };
        assert_eq!(r.free(), 0);
    }

    #[test]
    fn build_populates_real_categories() {
        let cfg = crate::config::load();
        let r = build(Path::new("."), &cfg, 1234, 5000);
        // The base system prompt and tool schemas always cost something.
        assert!(r.system_prompt > 0, "system prompt should be non-empty");
        assert!(r.system_tools > 0, "tool schemas should be non-empty");
        assert_eq!(r.messages, 1234);
        assert_eq!(r.used_real, 5000);
        assert!(r.limit > 0);
    }
}

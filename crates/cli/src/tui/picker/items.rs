//! Predefined picker item builders: the static command / model / reasoning /
//! session / verbosity catalogues rendered by the generic [`super::Picker`].

use super::PickerItem;
use std::path::Path;

pub fn slash_commands(cwd: &Path) -> Vec<PickerItem> {
    fn item(key: &str, title: &str, desc: &str) -> PickerItem {
        PickerItem {
            key: key.into(),
            title: title.into(),
            description: desc.into(),
        }
    }
    let mut items = vec![
        item("help", "/help", "list all commands"),
        item("model", "/model", "change the model"),
        item("thinking", "/thinking", "change reasoning effort"),
        item("effort", "/effort", "alias for /thinking"),
        item("verbosity", "/verbosity", "change output verbosity"),
        item("cost", "/cost", "show token usage and estimated cost"),
        item(
            "usage",
            "/usage",
            "show the provider's real quota / rate-limit status",
        ),
        item(
            "context",
            "/context",
            "show context-window usage + composition",
        ),
        item("buddy", "/buddy", "meet your account's pixel companion"),
        item("config", "/config", "show current configuration"),
        item("hooks", "/hooks", "list configured PreToolUse hooks"),
        item("mcp", "/mcp", "list configured MCP servers"),
        item("init", "/init", "create CLAUDE.md for this project"),
        item("memory", "/memory", "show CLAUDE.md"),
        item("diff", "/diff", "show `git diff` for the working tree"),
        item(
            "why",
            "/why",
            "decision trail: why changes were made (--all, --reconcile)",
        ),
        item(
            "blame",
            "/blame",
            "the decision trail for one file (greppable)",
        ),
        item(
            "twin",
            "/twin",
            "the Repo Twin: five verifiable indexes of this repo (--rebuild)",
        ),
        item(
            "pulse",
            "/pulse",
            "repo pulse: the files most likely to break next, scored from the twin",
        ),
        item(
            "handoff",
            "/handoff",
            "the shift report: git state + decisions + map, paste-ready for the next session",
        ),
        item(
            "why-context",
            "/why-context",
            "context X-ray: which files belong in context for a file/symbol, and why",
        ),
        item(
            "review",
            "/review",
            "ask the agent to review uncommitted changes",
        ),
        item(
            "prove",
            "/prove",
            "verify the work: run test/typecheck/lint/build, show a proof capsule",
        ),
        item(
            "commit",
            "/commit",
            "stage & commit with a generated message",
        ),
        item(
            "commit-push-pr",
            "/commit-push-pr",
            "commit, push a branch, and open a PR",
        ),
        item("export", "/export", "save conversation as markdown"),
        item(
            "compact",
            "/compact",
            "compact the conversation (add a focus: /compact <what to keep>)",
        ),
        item("todos", "/todos", "show the session todo list"),
        item(
            "thoughts",
            "/thoughts",
            "show or hide the model's live reasoning text (/thoughts on|off)",
        ),
        item("about", "/about", "show tomte version + build info"),
        item("login", "/login", "sign in with ChatGPT"),
        item("apikey", "/apikey", "save an OpenAI API key"),
        item("logout", "/logout", "clear credentials"),
        item("status", "/status", "show auth status"),
        item(
            "doctor",
            "/doctor",
            "run setup diagnostics (auth, config, MCP, tools)",
        ),
        item("img", "/img", "attach an image to next message"),
        item("cwd", "/cwd", "show / set working directory"),
        item(
            "worktree",
            "/worktree",
            "create or exit an isolated git worktree",
        ),
        item("goal", "/goal", "work until an objective is complete"),
        item("clear", "/clear", "clear the conversation"),
        item("resume", "/resume", "pick a previous session to continue"),
        item("agents", "/agents", "list installed subagents"),
        item("skills", "/skills", "list installed skills"),
        item(
            "commands",
            "/commands",
            "list installed custom slash commands",
        ),
        item("plan", "/plan", "enter plan mode (read-only tools)"),
        item("normal", "/normal", "leave plan mode"),
        item(
            "perms",
            "/perms",
            "toggle the approval modal for writes/shell",
        ),
        item("undo", "/undo", "revert the most recent file edit"),
        item(
            "rewind",
            "/rewind",
            "restore an earlier turn (undo its file edits)",
        ),
        item("quit", "/quit", "exit tomte"),
    ];
    let mut seen: std::collections::HashSet<String> = items.iter().map(|i| i.key.clone()).collect();
    // Custom commands (commands/*.md) — user-defined, manually triggerable.
    for c in tomte_core::command::load_all(cwd) {
        if !seen.insert(c.name.clone()) {
            continue;
        }
        let desc = first_line(&c.description);
        items.push(PickerItem {
            key: c.name.clone(),
            title: format!("/{}", c.name),
            description: if desc.is_empty() {
                "custom command".into()
            } else {
                format!("command · {desc}")
            },
        });
    }
    // Project-local skills (.tomte/.claude/.codex skills) — surfaced so the user
    // can manually trigger them with `/<name>`, like a custom command. Global
    // skills are left out so the quick `/` menu stays uncluttered (the model
    // still loads any skill on demand via the `skill` tool).
    for s in tomte_core::skill::discover(cwd) {
        let Some(scope) = project_skill_scope(&s.path, cwd) else {
            continue;
        };
        if !seen.insert(s.name.clone()) {
            continue;
        }
        let desc = first_line(&s.description);
        items.push(PickerItem {
            key: s.name.clone(),
            title: format!("/{}", s.name),
            description: if desc.is_empty() {
                format!("skill ({scope})")
            } else {
                format!("skill ({scope}) · {desc}")
            },
        });
    }
    items
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// `Some(".tomte"|".claude"|".codex")` when the skill's `SKILL.md` lives under
/// that project skills dir of `cwd`; `None` for a global skill.
fn project_skill_scope(path: &Path, cwd: &Path) -> Option<&'static str> {
    [".tomte", ".claude", ".codex"]
        .into_iter()
        .find(|sub| path.starts_with(cwd.join(sub).join("skills")))
}

/// Build the model picker dynamically from the providers the user is
/// currently signed in to. When only OpenAI creds exist the user sees the
/// GPT catalogue; after `tomte login --provider anthropic` the Claude
/// models appear alongside (or instead of) the GPT ones. After `logout`
/// nothing is signed in and the picker shows a single offline placeholder.
pub fn models() -> Vec<PickerItem> {
    use tomte_core::auth::signed_in_model_catalogs;
    use tomte_core::provider::Provider;

    let mut items = Vec::new();
    for catalog in signed_in_model_catalogs() {
        for model in catalog.models {
            let description = match (catalog.provider, *model) {
                (Provider::OpenAi, "gpt-5.5") => "frontier · default",
                (Provider::OpenAi, "gpt-5.5-pro") => "more compute for hard problems",
                (Provider::OpenAi, "gpt-5.4") => "previous frontier · stable",
                (Provider::OpenAi, "gpt-5.4-mini") => "fast · cheaper",
                (Provider::OpenAi, "gpt-5.4-nano") => "latency-sensitive",
                (Provider::OpenAi, "gpt-5.2") => "older frontier",
                (Provider::OpenAi, "gpt-5") => "oldest GPT-5-class",
                (Provider::Anthropic, "claude-fable-5") => "most capable · top tier",
                (Provider::Anthropic, "claude-opus-4-8") => "frontier · most capable opus",
                (Provider::Anthropic, "claude-opus-4-7") => {
                    "previous frontier · long-running agents"
                }
                (Provider::Anthropic, "claude-opus-4-6") => "frontier · previous opus",
                (Provider::Anthropic, "claude-opus-4-5") => "max intelligence · practical",
                (Provider::Anthropic, "claude-sonnet-4-6") => "best speed/intelligence balance",
                (Provider::Anthropic, "claude-sonnet-4-5") => "high-perf agents · coding",
                (Provider::Anthropic, "claude-haiku-4-5") => "fastest · near-frontier",
                _ => "available",
            };
            items.push(PickerItem {
                key: (*model).into(),
                title: (*model).into(),
                description: description.into(),
            });
        }
    }
    // Tag every model with its context window so 1M vs 200K is visible at a
    // glance in the picker (mirrors the textual catalogue). Done before the
    // not-signed-in placeholder below so that placeholder stays untagged.
    for it in &mut items {
        let win = tomte_core::agent::context_window_label(&it.key);
        it.description = format!("{win} ctx · {}", it.description);
    }
    if items.is_empty() {
        items.push(PickerItem {
            key: "gpt-5.5".into(),
            title: "(not signed in)".into(),
            description: "run `/login` to choose a provider".into(),
        });
    }
    items
}

/// Build the logout picker from the credentials actually stored in auth.json.
/// Env-var credentials are intentionally omitted — they aren't stored here and
/// can't be cleared by logging out. An "all" entry appears only when more than
/// one credential is stored.
pub fn logout_targets() -> Vec<PickerItem> {
    use tomte_core::auth::{load_auth, LogoutTarget};
    let r = load_auth().unwrap_or_default();
    let mut items = Vec::new();
    let item = |t: LogoutTarget, title: &str, desc: &str| PickerItem {
        key: t.key().into(),
        title: title.into(),
        description: desc.into(),
    };
    if r.tokens
        .as_ref()
        .is_some_and(|t| !t.access_token.is_empty())
    {
        items.push(item(
            LogoutTarget::OpenAiOauth,
            "OpenAI — ChatGPT OAuth",
            "sign out of the ChatGPT subscription token",
        ));
    }
    if r.api_key.as_ref().is_some_and(|k| !k.is_empty()) {
        items.push(item(
            LogoutTarget::OpenAiApiKey,
            "OpenAI — API key",
            "remove the stored OpenAI API key",
        ));
    }
    if r.anthropic_tokens
        .as_ref()
        .is_some_and(|t| !t.access_token.is_empty())
    {
        items.push(item(
            LogoutTarget::AnthropicOauth,
            "Anthropic — Claude Pro/Max OAuth",
            "sign out of the Claude subscription token",
        ));
    }
    if r.anthropic_api_key.as_ref().is_some_and(|k| !k.is_empty()) {
        items.push(item(
            LogoutTarget::AnthropicApiKey,
            "Anthropic — API key",
            "remove the stored Anthropic API key",
        ));
    }
    if items.len() > 1 {
        items.push(item(
            LogoutTarget::All,
            "All credentials",
            "clear every stored credential",
        ));
    }
    items
}

/// Reasoning-effort options for the given model's provider. OpenAI and
/// Anthropic expose different tiers, so the picker shows only the levels the
/// current model actually honours — `xhigh` only on Opus 4.7+, and
/// `max`/`ultracode` only on Anthropic. A single shared list for both providers
/// left users unsure which levels existed where.
pub fn efforts(model: &str) -> Vec<PickerItem> {
    use tomte_core::catalog;
    use tomte_core::provider::Provider;

    let item = |key: &str, description: &str| PickerItem {
        key: key.into(),
        title: key.into(),
        description: description.into(),
    };

    match Provider::from_model(model) {
        // OpenAI ChatGPT/Codex OAuth rejects `minimal` on current GPT-5.4/5.5
        // models but accepts `none`; keep the picker aligned with the runtime
        // path so selecting the fastest tier does not 400 the first request.
        Provider::OpenAi => vec![
            item("none", "fastest · no reasoning"),
            item("low", "light reasoning · latency-sensitive"),
            item("medium", "balanced · default"),
            item("high", "deep reasoning for hard tasks"),
            item("xhigh", "maximum reasoning depth"),
        ],
        // Anthropic thinking tiers. `minimal` is omitted (same as `none` here);
        // `xhigh` only appears on models that honour it (Opus 4.7+).
        Provider::Anthropic => {
            let mut items = vec![
                item("none", "no extended thinking · fastest"),
                item("low", "light thinking"),
                item("medium", "balanced · default"),
                item("high", "deep thinking for hard tasks"),
            ];
            if catalog::supports_xhigh(model) {
                items.push(item("xhigh", "Opus 4.7+ · between high and max"));
            }
            items.push(item("max", "adaptive max · top thinking tier"));
            items.push(item("ultracode", "ultra tier · xhigh + multi-agent"));
            items
        }
    }
}

/// Build picker items from a snapshot of stored sessions for the current cwd.
/// Newest first, with a single-line preview shaped like the slash command rows.
pub fn sessions(metas: &[tomte_core::session::SessionMeta]) -> Vec<PickerItem> {
    metas
        .iter()
        .map(|m| PickerItem {
            key: m.id.clone(),
            title: m.preview.clone(),
            description: format!(
                "{}  ·  {} msgs  ·  {}",
                ago(m.updated_at_ms),
                m.message_count,
                m.model
            ),
        })
        .collect()
}

/// Build picker rows from the session's rewind points, newest turn first so the
/// most recent is the default selection. Each row shows its blast radius — how
/// many later turns it drops and how many files it would revert — so the scope is
/// legible before committing (Pillar 1). The `key` is the point's ordinal (its
/// index in the preview) — exactly what `Agent::rewind_to` takes.
pub fn rewind_points(points: &[tomte_core::tools::RewindPointView]) -> Vec<PickerItem> {
    let total = points.len();
    points
        .iter()
        .enumerate()
        .rev()
        .map(|(ordinal, p)| {
            let later = total - 1 - ordinal;
            let turns = if later == 0 {
                "the latest turn".to_string()
            } else {
                format!(
                    "drops {later} later turn{}",
                    if later == 1 { "" } else { "s" }
                )
            };
            PickerItem {
                key: ordinal.to_string(),
                title: p.label.clone(),
                description: format!(
                    "{}  ·  {}  ·  reverts {} file{}",
                    ago(p.created_at_ms),
                    turns,
                    p.files_to_revert,
                    if p.files_to_revert == 1 { "" } else { "s" }
                ),
            }
        })
        .collect()
}

fn ago(ms: u64) -> String {
    let now = tomte_core::session::now_ms();
    let diff = now.saturating_sub(ms);
    let secs = diff / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

pub fn verbosities() -> Vec<PickerItem> {
    vec![
        PickerItem {
            key: "low".into(),
            title: "low".into(),
            description: "concise output".into(),
        },
        PickerItem {
            key: "medium".into(),
            title: "medium".into(),
            description: "default".into(),
        },
        PickerItem {
            key: "high".into(),
            title: "high".into(),
            description: "verbose output".into(),
        },
    ]
}

#[cfg(test)]
mod effort_tests {
    use super::efforts;

    fn keys(model: &str) -> Vec<String> {
        efforts(model).into_iter().map(|it| it.key).collect()
    }

    #[test]
    fn openai_shows_only_openai_tiers() {
        let k = keys("gpt-5.5");
        assert_eq!(k, ["none", "low", "medium", "high", "xhigh"]);
        // minimal/max/ultracode are not offered for OpenAI OAuth models.
        for absent in ["minimal", "max", "ultracode"] {
            assert!(
                !k.iter().any(|s| s == absent),
                "{absent} should be hidden for OpenAI"
            );
        }
    }

    #[test]
    fn anthropic_gates_xhigh_by_model() {
        // Opus 4.8 honours xhigh and the Anthropic-only tiers.
        let opus = keys("claude-opus-4-8");
        for present in ["none", "xhigh", "max", "ultracode"] {
            assert!(
                opus.iter().any(|s| s == present),
                "{present} missing for Opus"
            );
        }
        // Fable 5 honours xhigh like Opus 4.7+.
        let fable = keys("claude-fable-5");
        assert!(
            fable.iter().any(|s| s == "xhigh"),
            "xhigh missing for Fable"
        );
        // Sonnet 4.6 clamps xhigh to high, so the picker hides xhigh but keeps max.
        let sonnet = keys("claude-sonnet-4-6");
        assert!(!sonnet.iter().any(|s| s == "xhigh"));
        assert!(sonnet.iter().any(|s| s == "max"));
    }
}

#[cfg(test)]
mod slash_menu_tests {
    use super::{rewind_points, slash_commands};
    use std::fs;

    #[test]
    fn project_skills_and_commands_appear_with_builtins() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // A project-local skill under .tomte/skills.
        let skill_dir = cwd.join(".tomte").join("skills").join("zzz-demo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: zzz-demo-skill\ndescription: a demo project skill\n---\nbody\n",
        )
        .unwrap();
        // A project-local custom command under .tomte/commands.
        let cmd_dir = cwd.join(".tomte").join("commands");
        fs::create_dir_all(&cmd_dir).unwrap();
        fs::write(cmd_dir.join("zzz-demo-cmd.md"), "do the thing\n").unwrap();

        let items = slash_commands(cwd);
        // The project skill shows with a clear scope tag and is triggerable.
        assert!(
            items
                .iter()
                .any(|i| i.title == "/zzz-demo-skill" && i.description.contains("skill (.tomte)")),
            "project skill should appear in the slash menu with a scope tag"
        );
        // The project custom command shows too.
        assert!(
            items.iter().any(|i| i.title == "/zzz-demo-cmd"),
            "project custom command should appear in the slash menu"
        );
        // Built-ins are still present and come first.
        assert!(items.iter().any(|i| i.key == "model"));
        assert!(items.first().is_some_and(|i| i.key == "help"));
    }

    #[test]
    fn rewind_is_in_the_slash_menu() {
        let tmp = tempfile::tempdir().unwrap();
        let items = slash_commands(tmp.path());
        assert!(
            items
                .iter()
                .any(|i| i.key == "rewind" && i.title == "/rewind"),
            "/rewind should be offered in the slash menu"
        );
    }

    #[test]
    fn rewind_points_are_newest_first_keyed_by_ordinal_with_blast_radius() {
        let p = |label: &str, files: usize| tomte_core::tools::RewindPointView {
            label: label.into(),
            created_at_ms: 0,
            files_to_revert: files,
        };
        let points = vec![p("first turn", 2), p("second turn", 0), p("third turn", 1)];
        let items = rewind_points(&points);
        assert_eq!(items.len(), 3);
        // Newest turn first; the key is its ordinal in the original list, which is
        // exactly what `Agent::rewind_to` takes.
        assert_eq!(items[0].title, "third turn");
        assert_eq!(items[0].key, "2");
        assert!(items[0].description.contains("the latest turn"));
        assert!(items[0].description.contains("reverts 1 file"));
        assert_eq!(items[2].title, "first turn");
        assert_eq!(items[2].key, "0");
        assert!(items[2].description.contains("drops 2 later turns"));
        assert!(items[2].description.contains("reverts 2 files"));
    }
}

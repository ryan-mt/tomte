use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use super::{BuiltinTool, ToolContext, WorktreeState};

pub struct EnterWorktree;
pub struct ExitWorktree;

#[derive(Debug, Deserialize)]
struct EnterArgs {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExitArgs {
    action: String,
    #[serde(
        default,
        alias = "discardChanges",
        deserialize_with = "super::deserialize_optional_bool"
    )]
    discard_changes: Option<bool>,
}

#[async_trait]
impl BuiltinTool for EnterWorktree {
    fn name(&self) -> &'static str {
        "enter_worktree"
    }

    fn description(&self) -> &'static str {
        "Create an isolated git worktree for this session and switch the session cwd into it. Use ONLY when the user explicitly asks to work in a worktree."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": ["string", "null"], "description": "Optional worktree name/slug. Letters, digits, dots, underscores, and dashes only; generated if null."}
            },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let args: EnterArgs = super::parse_args("enter_worktree", args)?;
        enter_worktree(ctx, args.name.as_deref()).await
    }
}

#[async_trait]
impl BuiltinTool for ExitWorktree {
    fn name(&self) -> &'static str {
        "exit_worktree"
    }

    fn description(&self) -> &'static str {
        "Exit a worktree created by enter_worktree in this session. Can keep it on disk or remove it after safety checks."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["keep", "remove"], "description": "keep leaves the worktree and branch on disk; remove deletes them after safety checks."},
                "discard_changes": {"type": ["boolean", "null"], "description": "Required true to remove a dirty/ahead worktree. Ask the user before setting true."}
            },
            "required": ["action", "discard_changes"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let args: ExitArgs = super::parse_args("exit_worktree", args)?;
        exit_worktree(ctx, &args.action, args.discard_changes.unwrap_or(false)).await
    }
}

pub async fn enter_worktree(ctx: &ToolContext, name: Option<&str>) -> Result<String> {
    {
        let session = ctx.session.lock().await;
        if let Some(active) = &session.worktree {
            return Err(anyhow!(
                "already in a worktree for this session: {}",
                active.worktree_path.display()
            ));
        }
    }

    let original_cwd = canonicalize_dir(&ctx.cwd)?;
    let repo_root = git_stdout(&original_cwd, ["rev-parse", "--show-toplevel"])
        .await
        .context("not in a git repository")?;
    let repo_root = PathBuf::from(repo_root.trim()).canonicalize()?;
    let base_head = git_stdout(&repo_root, ["rev-parse", "HEAD"])
        .await
        .context("failed to resolve HEAD")?
        .trim()
        .to_string();
    let slug = match name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => validate_slug(name)?.to_string(),
        None => generated_slug(),
    };
    let branch = unique_branch(&repo_root, &slug).await?;
    let worktrees_root = repo_root.join(".opencli").join("worktrees");
    tokio::fs::create_dir_all(&worktrees_root)
        .await
        .with_context(|| format!("create {}", worktrees_root.display()))?;
    let worktree_path = unique_worktree_path(&worktrees_root, &slug);

    let out = Command::new("git")
        .args(["worktree", "add", "-b"])
        .arg(&branch)
        .arg(&worktree_path)
        .arg(&base_head)
        .current_dir(&repo_root)
        .output()
        .await
        .context("run git worktree add")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let worktree_path = worktree_path.canonicalize()?;

    {
        let mut session = ctx.session.lock().await;
        session.worktree = Some(WorktreeState {
            original_cwd: original_cwd.clone(),
            repo_root: repo_root.clone(),
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            base_head: base_head.clone(),
        });
    }
    set_cwd(ctx, worktree_path.clone()).await;

    Ok(format!(
        "Created worktree at {} on branch `{}`. The session cwd is now {}. Use exit_worktree with action=keep or action=remove to leave it.",
        worktree_path.display(),
        branch,
        worktree_path.display()
    ))
}

pub async fn exit_worktree(
    ctx: &ToolContext,
    action: &str,
    discard_changes: bool,
) -> Result<String> {
    let state = {
        let session = ctx.session.lock().await;
        session
            .worktree
            .clone()
            .ok_or_else(|| anyhow!("no worktree is active for this session"))?
    };

    match action {
        "keep" => {
            clear_worktree(ctx).await;
            set_cwd(ctx, state.original_cwd.clone()).await;
            Ok(format!(
                "Left worktree {} and kept it on disk on branch `{}`. Session cwd restored to {}.",
                state.worktree_path.display(),
                state.branch,
                state.original_cwd.display()
            ))
        }
        "remove" => {
            let dirty = changed_file_count(&state.worktree_path).await?;
            let ahead = ahead_commit_count(&state.worktree_path, &state.base_head).await?;
            if (dirty > 0 || ahead > 0) && !discard_changes {
                return Err(anyhow!(
                    "worktree has {dirty} changed file(s) and {ahead} commit(s) after the base; refusing to remove without discard_changes=true"
                ));
            }
            let remove = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&state.worktree_path)
                .current_dir(&state.repo_root)
                .output()
                .await
                .context("run git worktree remove")?;
            if !remove.status.success() {
                return Err(anyhow!(
                    "git worktree remove failed: {}",
                    String::from_utf8_lossy(&remove.stderr).trim()
                ));
            }
            let delete_branch = Command::new("git")
                .args(["branch", "-D", &state.branch])
                .current_dir(&state.repo_root)
                .output()
                .await
                .context("run git branch -D")?;
            if !delete_branch.status.success() {
                return Err(anyhow!(
                    "removed worktree but failed to delete branch `{}`: {}",
                    state.branch,
                    String::from_utf8_lossy(&delete_branch.stderr).trim()
                ));
            }
            clear_worktree(ctx).await;
            set_cwd(ctx, state.original_cwd.clone()).await;
            Ok(format!(
                "Removed worktree {} and branch `{}`. Session cwd restored to {}. Discarded {dirty} changed file(s) and {ahead} commit(s).",
                state.worktree_path.display(),
                state.branch,
                state.original_cwd.display()
            ))
        }
        other => Err(anyhow!("action must be `keep` or `remove` (got `{other}`)")),
    }
}

pub async fn worktree_status(ctx: &ToolContext) -> String {
    let session = ctx.session.lock().await;
    match &session.worktree {
        Some(state) => format!(
            "active worktree:\n  path: {}\n  branch: {}\n  original cwd: {}\n  base: {}",
            state.worktree_path.display(),
            state.branch,
            state.original_cwd.display(),
            state.base_head
        ),
        None => "no active worktree for this session".to_string(),
    }
}

async fn clear_worktree(ctx: &ToolContext) {
    let mut session = ctx.session.lock().await;
    session.worktree = None;
}

async fn set_cwd(ctx: &ToolContext, cwd: PathBuf) {
    *ctx.cwd_override.lock().await = Some(cwd.clone());
    if let Some(tx) = &ctx.events {
        let _ = tx
            .send(crate::agent::AgentEvent::CwdChanged {
                cwd: cwd.to_string_lossy().to_string(),
            })
            .await;
    }
}

fn canonicalize_dir(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolve cwd {}", path.display()))?;
    if !path.is_dir() {
        return Err(anyhow!("cwd is not a directory: {}", path.display()));
    }
    Ok(path)
}

fn validate_slug(name: &str) -> Result<&str> {
    if name.len() > 64 {
        return Err(anyhow!("worktree name must be <= 64 characters"));
    }
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid || name.starts_with('.') || name.ends_with('.') {
        return Err(anyhow!(
            "worktree name may contain only letters, digits, dots, underscores, and dashes, and may not start/end with a dot"
        ));
    }
    Ok(name)
}

fn generated_slug() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "work-{}",
        base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
    )
}

async fn unique_branch(repo_root: &Path, slug: &str) -> Result<String> {
    for idx in 0..100u32 {
        let candidate = if idx == 0 {
            format!("opencli/{slug}")
        } else {
            format!("opencli/{slug}-{idx}")
        };
        let status = Command::new("git")
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{candidate}"))
            .current_dir(repo_root)
            .status()
            .await
            .context("run git show-ref")?;
        if !status.success() {
            return Ok(candidate);
        }
    }
    Err(anyhow!("could not find an unused branch name for `{slug}`"))
}

fn unique_worktree_path(root: &Path, slug: &str) -> PathBuf {
    for idx in 0..100u32 {
        let candidate = if idx == 0 {
            root.join(slug)
        } else {
            root.join(format!("{slug}-{idx}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    root.join(format!("{slug}-{}", std::process::id()))
}

async fn changed_file_count(worktree_path: &Path) -> Result<usize> {
    let out = git_output(worktree_path, ["status", "--porcelain"]).await?;
    if !out.status.success() {
        return Err(anyhow!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

async fn ahead_commit_count(worktree_path: &Path, base_head: &str) -> Result<usize> {
    let range = format!("{base_head}..HEAD");
    let out = git_output(worktree_path, ["rev-list", "--count", &range]).await?;
    if !out.status.success() {
        return Err(anyhow!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<usize>()
        .unwrap_or(0))
}

async fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let out = git_output(cwd, args).await?;
    if !out.status.success() {
        return Err(anyhow!(String::from_utf8_lossy(&out.stderr)
            .trim()
            .to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .context("run git")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_slug() {
        assert!(validate_slug("feature_1.ok").is_ok());
        assert!(validate_slug("bad/name").is_err());
        assert!(validate_slug(".hidden").is_err());
    }
}

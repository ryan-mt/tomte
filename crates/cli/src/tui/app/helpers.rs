//! Free helpers: env keys, screen selection, permission mode, session save. Split out of `app`; logic unchanged.

use super::*;

pub fn has_supported_env_key() -> bool {
    auth_mode_from_env().is_some()
}

pub fn auth_mode_from_env() -> Option<AuthMode> {
    ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"]
        .iter()
        .find_map(|name| match *name {
            "OPENAI_API_KEY" if std::env::var(name).is_ok_and(|v| !v.is_empty()) => {
                Some(AuthMode::OpenaiApiKey)
            }
            "ANTHROPIC_API_KEY" if std::env::var(name).is_ok_and(|v| !v.is_empty()) => {
                Some(AuthMode::AnthropicApiKey)
            }
            _ => None,
        })
}

pub fn initial_screen(auth_mode: AuthMode, has_env_key: bool) -> Screen {
    if auth_mode == AuthMode::None && !has_env_key {
        Screen::Login
    } else {
        Screen::Chat
    }
}

pub fn resolve_cwd_arg(current: &std::path::Path, arg: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(arg);
    let candidate = if path.is_absolute() {
        path
    } else {
        current.join(path)
    };
    if !candidate.is_dir() {
        return None;
    }
    Some(candidate.canonicalize().unwrap_or(candidate))
}

pub fn apply_host_state_to_session_record(app: &App, record: &mut SessionRecord) {
    record.state.active_goal = app
        .active_goal
        .as_ref()
        .map(ActiveGoal::to_session_snapshot);
}

pub fn set_permission_mode_and_save(app: &mut App, mode: PermissionMode) {
    app.set_permission_mode(mode);
    app.config.default_permission_mode = mode.config_str().to_string();
    if let Err(e) = config::save(&app.config) {
        app.blocks
            .push(Block::System(format!("config save failed: {e}")));
    }
}

pub fn permission_mode_after_plan_approval(app: &App) -> PermissionMode {
    match PermissionMode::from_config_str(&app.config.default_permission_mode) {
        PermissionMode::Plan => PermissionMode::Default,
        mode => mode,
    }
}

pub fn apply_plan_mode_required(app: &mut App) {
    app.set_permission_mode(PermissionMode::Plan);
    app.pending_plan_exit = None;
    app.blocks.push(Block::System(
        "plan mode required → on (read-only until a plan is approved)".into(),
    ));
    app.auto_scroll = true;
}

pub async fn save_current_session_record(
    app: &mut App,
    agent: &std::sync::Arc<tokio::sync::Mutex<Option<Agent>>>,
) {
    let mut record = {
        let guard = agent.lock().await;
        let Some(a) = guard.as_ref() else {
            app.pending_session_save = false;
            return;
        };
        a.to_session_record().await
    };
    apply_host_state_to_session_record(app, &mut record);
    if let Err(e) = tomte_core::session::save(&record) {
        tracing::debug!(error = %e, "session save with host state failed");
    }
    app.pending_session_save = false;
}

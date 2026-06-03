//! Shared test helpers for the app test modules.

use super::*;

pub(super) fn temp_dir(name: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("opencli-tui-app-{name}-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

pub(super) fn todo(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.to_string(),
        status,
        active_form: format!("Doing {content}"),
        id: None,
        blocked_by: Vec::new(),
    }
}

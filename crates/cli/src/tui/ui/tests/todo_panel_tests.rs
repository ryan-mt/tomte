
use super::super::{
    hidden_todos_summary, todo_label, todos_height_for_count, truncate_chars, visible_todo_indices,
    TODO_VISIBLE_ROWS,
};
use std::collections::HashSet;
use tomte_core::tools::{TodoItem, TodoStatus};

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.to_string(),
        status,
        active_form: format!("Doing {content}"),
        id: None,
        blocked_by: Vec::new(),
    }
}

#[test]
fn todo_panel_height_caps_and_reserves_overflow_row() {
    assert_eq!(todos_height_for_count(0), 0);
    assert_eq!(todos_height_for_count(1), 2);
    assert_eq!(todos_height_for_count(TODO_VISIBLE_ROWS), 7);
    assert_eq!(todos_height_for_count(TODO_VISIBLE_ROWS + 2), 8);
}

#[test]
fn truncated_todos_prioritize_active_and_pending_items() {
    let todos = vec![
        item("completed one", TodoStatus::Completed),
        item("pending one", TodoStatus::Pending),
        item("completed two", TodoStatus::Completed),
        item("active one", TodoStatus::InProgress),
        item("pending two", TodoStatus::Pending),
        item("completed three", TodoStatus::Completed),
        item("pending three", TodoStatus::Pending),
        item("completed four", TodoStatus::Completed),
    ];

    let visible = visible_todo_indices(&todos, &HashSet::new(), TODO_VISIBLE_ROWS);

    assert_eq!(visible, vec![3, 1, 4, 6, 0, 2]);
    assert_eq!(
        hidden_todos_summary(&todos, &visible),
        Some("… +2 completed".to_string())
    );
}

#[test]
fn truncated_todos_prioritize_recently_completed_items() {
    let todos = vec![
        item("pending one", TodoStatus::Pending),
        item("pending two", TodoStatus::Pending),
        item("active one", TodoStatus::InProgress),
        item("pending three", TodoStatus::Pending),
        item("pending four", TodoStatus::Pending),
        item("completed old", TodoStatus::Completed),
        item("completed recent", TodoStatus::Completed),
        item("pending five", TodoStatus::Pending),
    ];
    let recent_completed = HashSet::from([6usize]);

    let visible = visible_todo_indices(&todos, &recent_completed, TODO_VISIBLE_ROWS);

    assert_eq!(visible, vec![6, 2, 0, 1, 3, 4]);
    assert_eq!(
        hidden_todos_summary(&todos, &visible),
        Some("… +1 pending, 1 completed".to_string())
    );
}

#[test]
fn truncated_recent_completed_todos_are_deterministic() {
    let todos = (0..TODO_VISIBLE_ROWS + 2)
        .map(|i| item(&format!("completed {i}"), TodoStatus::Completed))
        .collect::<Vec<_>>();
    let recent_completed = HashSet::from([5usize, 2usize, 4usize, 1usize, 3usize, 0usize]);

    let visible = visible_todo_indices(&todos, &recent_completed, TODO_VISIBLE_ROWS);

    assert_eq!(visible, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn truncated_todos_respect_a_smaller_row_grant() {
    // The viewport budget can grant the panel fewer than the 6-row ideal
    // (see `split_frame`); the visible set must shrink to the grant so the
    // hidden-summary row still fits on screen.
    let todos = vec![
        item("completed one", TodoStatus::Completed),
        item("pending one", TodoStatus::Pending),
        item("active one", TodoStatus::InProgress),
        item("pending two", TodoStatus::Pending),
        item("pending three", TodoStatus::Pending),
    ];

    let visible = visible_todo_indices(&todos, &HashSet::new(), 2);
    assert_eq!(visible, vec![2, 1]);
    assert_eq!(
        hidden_todos_summary(&todos, &visible),
        Some("… +2 pending, 1 completed".to_string())
    );

    assert!(visible_todo_indices(&todos, &HashSet::new(), 0).is_empty());
}

#[test]
fn todo_label_uses_active_form_only_for_active_item() {
    let active = item("write tests", TodoStatus::InProgress);
    let done = item("read code", TodoStatus::Completed);

    assert_eq!(todo_label(&active), "Doing write tests");
    assert_eq!(todo_label(&done), "read code");
}

#[test]
fn truncation_handles_narrow_width_without_splitting_utf8() {
    assert_eq!(truncate_chars("abcdef", 0), "");
    assert_eq!(truncate_chars("éclair", 2), "é…");
}

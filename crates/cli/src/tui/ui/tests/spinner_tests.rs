
use super::super::render_spinner;
use crate::tui::app::App;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

// Cancellation must be discoverable at the moment it's needed: the busy
// spinner line always carries the esc affordance.
#[test]
fn spinner_advertises_esc_to_interrupt() {
    let mut app = App::new();
    app.busy = true;
    let mut t = Terminal::new(TestBackend::new(100, 1)).unwrap();
    t.draw(|f| render_spinner(f, f.area(), &app)).unwrap();
    let buf = t.backend().buffer();
    let row: String = (0..buf.area.width)
        .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol()))
        .collect();
    assert!(row.contains("esc to interrupt"), "got: {row}");
}

fn spinner_row(app: &App) -> String {
    let mut t = Terminal::new(TestBackend::new(120, 1)).unwrap();
    t.draw(|f| render_spinner(f, f.area(), app)).unwrap();
    let buf = t.backend().buffer();
    (0..buf.area.width)
        .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
        .collect()
}

fn running_tool(name: &str) -> crate::tui::app::Block {
    crate::tui::app::Block::Tool {
        call_id: "c1".into(),
        name: name.into(),
        args: "{}".into(),
        output: None,
        error: false,
        preflight: None,
    }
}

#[test]
fn tool_action_verb_maps_real_tools_and_skips_meta() {
    use crate::tui::app::tool_action_verb;
    assert_eq!(tool_action_verb("read_file"), Some("Reading"));
    assert_eq!(tool_action_verb("run_shell"), Some("Running"));
    assert_eq!(tool_action_verb("grep"), Some("Searching"));
    assert_eq!(tool_action_verb("dispatch_agent"), Some("Delegating"));
    // Meta tools, MCP tools, and unknown names fall through to the pool.
    assert_eq!(tool_action_verb("todo_write"), None);
    assert_eq!(tool_action_verb("goal_update"), None);
    assert_eq!(tool_action_verb("wait"), None);
    assert_eq!(tool_action_verb("mcp__gh__create_issue"), None);
    assert_eq!(tool_action_verb("nonsense"), None);
}

#[test]
fn spinner_narrates_the_running_tool_when_no_todo() {
    // With no in-progress todo, the spinner says what the running tool is
    // doing instead of a stock pool word.
    let mut app = App::new();
    app.busy = true;
    app.blocks.push(running_tool("run_shell"));
    let row = spinner_row(&app);
    assert!(row.contains("Running…"), "got: {row}");
}

#[test]
fn spinner_prefers_todo_active_form_over_the_running_tool() {
    let mut app = App::new();
    app.busy = true;
    app.blocks.push(running_tool("run_shell"));
    app.session_todos = vec![tomte_core::tools::TodoItem {
        content: "run the suite".into(),
        status: tomte_core::tools::TodoStatus::InProgress,
        active_form: "Verifying the build".into(),
        id: None,
        blocked_by: Vec::new(),
    }];
    let row = spinner_row(&app);
    assert!(row.contains("Verifying the build…"), "got: {row}");
    assert!(!row.contains("Running…"), "todo verb must win: {row}");
}

#[test]
fn spinner_finished_tool_falls_back_to_the_pool() {
    // A tool whose result has arrived is no longer "running" → pool word.
    let mut app = App::new();
    app.busy = true;
    app.blocks.push(crate::tui::app::Block::Tool {
        call_id: "c1".into(),
        name: "run_shell".into(),
        args: "{}".into(),
        output: Some("done".into()),
        error: false,
        preflight: None,
    });
    assert!(!spinner_row(&app).contains("Running…"));
}

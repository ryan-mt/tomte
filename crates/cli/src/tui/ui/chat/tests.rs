
use super::{read_group_crosses, resolve_scroll, stable_boundary, visible_thought_rows};
use crate::tui::app::Block;

fn collapsed_thought(reasoning: &str, expanded: bool) -> Block {
    Block::Assistant {
        text: "the answer".into(),
        reasoning: reasoning.into(),
        done: true,
        thought_for_secs: Some(4),
        reasoning_started_at: None,
        thinking_expanded: expanded,
    }
}

fn lines_text(lines: &[ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_tool(id: &str) -> Block {
    Block::Tool {
        call_id: id.to_string(),
        name: "read_file".to_string(),
        args: String::new(),
        output: None,
        error: false,
        preflight: None,
    }
}

#[test]
fn stable_boundary_covers_everything_when_idle() {
    let blocks = vec![Block::Welcome, Block::User("hi".into()), read_tool("c1")];
    assert_eq!(stable_boundary(&blocks, false), 3);
}

#[test]
fn stable_boundary_starts_the_live_turn_after_its_prompt() {
    // Busy: the live turn begins right AFTER the last User block (the
    // prompt itself never mutates, so it stays cached).
    let blocks = vec![
        Block::Welcome,
        Block::User("first".into()),
        Block::System("done".into()),
        Block::User("second".into()),
        read_tool("c1"),
    ];
    assert_eq!(stable_boundary(&blocks, true), 4);
}

#[test]
fn stable_boundary_without_a_prompt_keeps_everything_live() {
    let blocks = vec![Block::Welcome, read_tool("c1")];
    assert_eq!(stable_boundary(&blocks, true), 0);
}

#[test]
fn read_group_crossing_is_detected_only_inside_a_run() {
    let blocks = vec![
        Block::User("hi".into()),
        read_tool("c1"),
        read_tool("c2"),
        Block::System("done".into()),
    ];
    // Boundary inside the read_file run → crossing.
    assert!(read_group_crosses(&blocks, 2));
    // Boundaries at the run's edges, the ends, or out of range → no crossing.
    assert!(!read_group_crosses(&blocks, 0));
    assert!(!read_group_crosses(&blocks, 1));
    assert!(!read_group_crosses(&blocks, 3));
    assert!(!read_group_crosses(&blocks, 4));
}

#[test]
fn auto_follow_pins_to_the_bottom_gap() {
    // 100 lines, 22-row viewport → inner_height 20 → max_scroll 80.
    // Auto-following shows the last 20 lines with the 2-row bottom gap.
    assert_eq!(resolve_scroll(100, 22, true, 0), (80, true));
}

#[test]
fn parked_above_the_tail_keeps_the_users_scroll() {
    // Not auto-following and parked below max → scroll is held, auto stays off.
    assert_eq!(resolve_scroll(100, 22, false, 30), (30, false));
}

#[test]
fn scrolling_to_the_bottom_resumes_auto_follow() {
    // cur_scroll at/over max_scroll re-arms auto-follow (sticky bottom).
    assert_eq!(resolve_scroll(100, 22, false, 80), (80, true));
    assert_eq!(resolve_scroll(100, 22, false, 999), (80, true));
}

#[test]
fn short_transcript_never_scrolls() {
    // Fewer lines than the viewport → max_scroll 0, always top-anchored.
    assert_eq!(resolve_scroll(5, 22, true, 0), (0, true));
    assert_eq!(resolve_scroll(5, 22, false, 3), (0, true));
}

#[test]
fn a_parked_scroll_is_clamped_to_max() {
    // A stale cur_scroll past the new max clamps down (no overscroll).
    assert_eq!(resolve_scroll(50, 22, false, 40), (30, true));
}

#[test]
fn visible_thought_rows_maps_only_on_screen_marks() {
    use ratatui::layout::Rect;
    // Chat area at y=2, height 5 → flat lines [scroll, scroll+5) are visible.
    let area = Rect::new(0, 2, 40, 5);
    let marks = vec![(0usize, 10usize), (3, 11), (7, 12), (100, 13)];
    // scroll=3 → visible flat in [3, 8): mark 3 (block 11) and 7 (block 12).
    let rows = visible_thought_rows(&marks, 3, area);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, 11);
    assert_eq!(rows[0].0.y, 2); // flat 3 at top of area
    assert_eq!(rows[1].1, 12);
    assert_eq!(rows[1].0.y, 6); // flat 7 → 2 + (7-3)
                                // The whole row is the click target.
    assert_eq!((rows[0].0.x, rows[0].0.width, rows[0].0.height), (0, 40, 1));
}

#[test]
fn collapsed_thought_hides_reasoning_until_expanded_and_marks_the_line() {
    let app = crate::tui::app::App::new();
    // Collapsed: the reasoning is retained but not shown; one click-mark at
    // the block's first line (offset 0, block 0).
    let (lines, marks) = super::super::inline_blocks_to_lines_marked(
        std::slice::from_ref(&collapsed_thought("a deep thought", false)),
        40,
        false,
        &app,
    );
    assert_eq!(marks, vec![(0, 0)]);
    let text = lines_text(&lines);
    assert!(text.contains("Thought for 4s"), "got: {text}");
    assert!(
        !text.contains("a deep thought"),
        "collapsed must hide reasoning: {text}"
    );

    // Expanded: the same line stays the click target (offset 0), and the
    // reasoning now renders below it.
    let (lines2, marks2) = super::super::inline_blocks_to_lines_marked(
        std::slice::from_ref(&collapsed_thought("a deep thought", true)),
        40,
        false,
        &app,
    );
    assert_eq!(marks2, vec![(0, 0)]);
    let text2 = lines_text(&lines2);
    assert!(text2.contains("Thought for 4s"), "got: {text2}");
    assert!(
        text2.contains("a deep thought"),
        "expanded must show reasoning: {text2}"
    );
}

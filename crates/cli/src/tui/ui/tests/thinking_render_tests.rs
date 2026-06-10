
use super::super::{push_assistant_lines, Block};
use ratatui::text::Line;

fn assistant(reasoning: &str, text: &str) -> Block {
    Block::Assistant {
        text: text.to_string(),
        reasoning: reasoning.to_string(),
        done: false,
        thought_for_secs: None,
        reasoning_started_at: None,
        thinking_expanded: false,
    }
}

fn flatten(lines: &[Line<'static>]) -> String {
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

#[test]
fn live_reasoning_shows_when_thinking_on() {
    let block = assistant("weighing the two options", "the final answer");
    let mut lines = Vec::new();
    push_assistant_lines(&mut lines, &block, 60, true);
    let out = flatten(&lines);
    assert!(
        out.contains("weighing the two options"),
        "thinking shown: {out:?}"
    );
    assert!(out.contains("the final answer"), "answer shown: {out:?}");
}

#[test]
fn live_reasoning_hidden_when_thinking_off() {
    let block = assistant("weighing the two options", "the final answer");
    let mut lines = Vec::new();
    push_assistant_lines(&mut lines, &block, 60, false);
    let out = flatten(&lines);
    assert!(
        !out.contains("weighing the two options"),
        "thinking hidden: {out:?}"
    );
    assert!(
        out.contains("the final answer"),
        "answer still shown: {out:?}"
    );
}

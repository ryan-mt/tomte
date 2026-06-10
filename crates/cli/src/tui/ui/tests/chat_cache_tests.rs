use super::super::render_chat;
use crate::tui::app::{App, Block};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

fn assistant(text: &str, done: bool) -> Block {
    Block::Assistant {
        text: text.to_string(),
        reasoning: String::new(),
        done,
        thought_for_secs: None,
        reasoning_started_at: None,
        thinking_expanded: false,
    }
}

fn read_tool(id: &str, path: &str, output: Option<&str>) -> Block {
    Block::Tool {
        call_id: id.to_string(),
        name: "read_file".to_string(),
        args: format!("{{\"path\":\"{path}\"}}"),
        output: output.map(str::to_string),
        error: false,
        preflight: None,
    }
}

fn draw(app: &mut App, width: u16) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(width, 16)).unwrap();
    t.draw(|f| {
        let area = f.area();
        render_chat(f, area, app);
    })
    .unwrap();
    t.backend().buffer().clone()
}

/// Render once through the cache as it evolved across the scripted steps,
/// once from a cold cache, and demand identical frames. This is the
/// correctness contract that replaced per-event invalidation: whatever the
/// stable-prefix cache reuses must be indistinguishable from a fresh build.
fn assert_step(app: &mut App, width: u16, label: &str) {
    let evolved = draw(app, width);
    let evolved_cache = app.chat_render_cache.take();
    let fresh = draw(app, width);
    assert_eq!(
        evolved, fresh,
        "cache-evolved frame diverged from fresh build at step: {label}"
    );
    app.chat_render_cache = evolved_cache;
}

#[test]
fn cache_evolved_rendering_matches_fresh_build_across_turns() {
    let mut app = App::new();
    assert_step(&mut app, 60, "welcome, idle");

    // A turn starts: prompt + open assistant block, busy.
    app.blocks.push(Block::User("please do the thing".into()));
    app.blocks.push(assistant("", false));
    app.busy = true;
    assert_step(&mut app, 60, "turn start");

    // Streaming deltas grow the open assistant block.
    for chunk in ["Working on", " it — reading", " the files now."] {
        if let Some(Block::Assistant { text, .. }) = app.blocks.last_mut() {
            text.push_str(chunk);
        }
        assert_step(&mut app, 60, "stream delta");
    }

    // Tool lifecycle inside the live tail: started → args → a sibling that
    // groups with it → result landing on a NON-last block.
    app.blocks.push(read_tool("c1", "src/main.rs", None));
    assert_step(&mut app, 60, "tool started");
    app.blocks.push(read_tool("c2", "src/lib.rs", None));
    assert_step(&mut app, 60, "grouped second read");
    if let Some(Block::Tool { output, .. }) = app
        .blocks
        .iter_mut()
        .find(|b| matches!(b, Block::Tool { call_id, .. } if call_id == "c1"))
    {
        *output = Some("fn main() {}".into());
    }
    assert_step(&mut app, 60, "result on non-last block");

    // The turn settles: boundary moves to the end (append path).
    if let Some(Block::Assistant { done, .. }) = app
        .blocks
        .iter_mut()
        .find(|b| matches!(b, Block::Assistant { .. }))
    {
        *done = true;
    }
    app.busy = false;
    assert_step(&mut app, 60, "turn complete");

    // A second turn exercises append-after-idle.
    app.blocks.push(Block::User("and again".into()));
    app.blocks.push(assistant("Sure — done.", false));
    app.busy = true;
    assert_step(&mut app, 60, "second turn streaming");
    app.busy = false;
    assert_step(&mut app, 60, "second turn settled");

    // A width change must rebuild for the new wrap.
    assert_step(&mut app, 44, "narrower terminal");

    // A wholesale replacement (the `/clear` shape) must drop the cache.
    app.blocks.clear();
    app.blocks.push(Block::Welcome);
    assert_step(&mut app, 44, "cleared transcript");
}

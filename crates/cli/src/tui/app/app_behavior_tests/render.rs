use super::super::*;

#[test]
fn rich_block_renders_to_terminal_buffer() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.blocks.push(Block::Rich(vec![ratatui::text::Line::from(
        "CTXMARKER-line",
    )]));
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render(f, &mut app))
        .unwrap();
    let dump: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        dump.contains("CTXMARKER-line"),
        "Block::Rich content must reach the terminal buffer"
    );
}

#[tokio::test]
async fn buddy_starts_hatch_then_locks() {
    let mut app = App::new();
    handle_slash(&mut app, "buddy").await;
    assert!(
        app.hatch.is_some(),
        "/buddy should start the hatch animation"
    );

    finish_hatch(&mut app);
    assert!(
        app.buddy_pet.is_some(),
        "finishing the hatch adopts the pet"
    );
    assert!(app.hatch.is_none());

    // Locked: a second /buddy must NOT re-hatch or spawn another pet.
    handle_slash(&mut app, "buddy").await;
    assert!(app.hatch.is_none(), "must not re-hatch once adopted");
    assert!(
        matches!(app.blocks.last(), Some(Block::System(t)) if t.contains("already")),
        "expected the locked note"
    );
}

// --- Pillar 4: inline viewport ---

fn streaming_assistant(text: &str, done: bool) -> Block {
    Block::Assistant {
        text: text.into(),
        reasoning: String::new(),
        done,
        thought_for_secs: None,
        reasoning_started_at: None,
        thinking_expanded: false,
    }
}

#[test]
fn render_mode_resolves_env_and_config() {
    use RenderMode::{AltScreen, Inline};
    // Inline (Pillar 4) is the default. An explicit truthy/falsy TOMTE_INLINE
    // wins over config; otherwise `render_mode: "alt"` opts into the alternate
    // screen and anything else (including garbage) stays inline.
    let cases = [
        // env unset → config decides, inline default
        (None, "inline", Inline),
        (None, "", Inline),
        (None, "garbage", Inline),
        (None, "alt", AltScreen),
        (None, " ALT ", AltScreen),
        (None, "altscreen", AltScreen),
        (None, "alt-screen", AltScreen),
        (None, "alt_screen", AltScreen),
        (None, "fullscreen", AltScreen),
        // truthy env forces inline, even over config "alt"
        (Some("1"), "alt", Inline),
        (Some(" on "), "alt", Inline),
        (Some("true"), "alt", Inline),
        (Some("yes"), "alt", Inline),
        // falsy env forces alt-screen, even over config "inline"
        (Some("0"), "inline", AltScreen),
        (Some(" off "), "inline", AltScreen),
        (Some("false"), "inline", AltScreen),
        (Some("no"), "inline", AltScreen),
        // unrecognized env value falls through to config
        (Some("nope"), "inline", Inline),
        (Some("nope"), "alt", AltScreen),
    ];
    for (env, config_mode, want) in cases {
        assert_eq!(
            RenderMode::resolve_values(env, config_mode),
            want,
            "env={env:?} config={config_mode:?}"
        );
    }
}

#[test]
fn committed_end_keeps_streaming_block_while_busy() {
    let tool = |output: Option<&str>| Block::Tool {
        call_id: "c".into(),
        name: "run_shell".into(),
        args: String::new(),
        output: output.map(str::to_string),
        error: false,
        preflight: None,
    };
    let finished = vec![
        Block::User("q".into()),
        tool(Some("ok")),
        tool(Some("ok")),
        tool(Some("ok")),
        streaming_assistant("live", false),
    ];
    assert_eq!(committed_end(&finished, true), 4);
    assert_eq!(committed_end(&finished, false), 5);
    assert_eq!(committed_end(&[], true), 0);
    assert_eq!(committed_end(&[], false), 0);
}

#[test]
fn committed_end_holds_at_first_inflight_tool() {
    // Parallel tool calls: an in-flight tool (output: None) sits BEFORE later
    // finished blocks. Committing it would freeze "working…" into native
    // scrollback forever, so the boundary must stop there while busy.
    let tool = |output: Option<&str>| Block::Tool {
        call_id: "c".into(),
        name: "dispatch_agent".into(),
        args: String::new(),
        output: output.map(str::to_string),
        error: false,
        preflight: None,
    };
    let blocks = vec![
        Block::User("q".into()),
        tool(Some("done")),
        tool(None), // still running
        tool(Some("done")),
        streaming_assistant("live", false),
    ];
    assert_eq!(
        committed_end(&blocks, true),
        2,
        "while busy, nothing past the first in-flight tool may commit"
    );
    // Idle: turn end settles every tool (see settle_inflight_tools), so a
    // lingering None should not exist — but if one does, the boundary still
    // holds there rather than freeze "working…" into scrollback.
    assert_eq!(committed_end(&blocks, false), 2);
}

#[test]
fn turn_end_settles_inflight_tool_blocks() {
    // A turn that ends (complete / error / esc-interrupt) delivers no further
    // tool results; an open tool block left behind renders "working…" forever.
    let mut app = App::new();
    apply_agent_event(
        &mut app,
        AgentEvent::ToolCallStarted {
            name: "run_shell".to_string(),
            call_id: "c1".to_string(),
        },
    );
    apply_agent_event(&mut app, AgentEvent::TurnComplete);
    let settled = app.blocks.iter().any(|b| {
        matches!(b, Block::Tool { output: Some(o), error: true, .. } if o.contains("interrupted"))
    });
    assert!(
        settled,
        "TurnComplete must settle the open tool block instead of leaving 'working…'"
    );
}

#[test]
fn render_inline_shows_live_tail_not_committed_history() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.blocks = vec![
        Block::User("OLDUSERMARK".into()),
        streaming_assistant("OLDANSWERMARK", true),
        streaming_assistant("LIVEANSWERMARK", false),
    ];
    // First two blocks have already been pushed to native scrollback.
    app.committed_blocks = 2;
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render_inline(f, &mut app))
        .unwrap();
    let dump: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        dump.contains("LIVEANSWERMARK"),
        "the live tail must render in the inline viewport"
    );
    assert!(
        !dump.contains("OLDANSWERMARK") && !dump.contains("OLDUSERMARK"),
        "committed history must NOT be redrawn in the viewport (it lives in scrollback)"
    );
}

#[test]
fn inline_busy_panels_never_clip_input_or_status() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tomte_core::tools::TodoStatus;
    // The worst-case live region from a real multi-agent turn: spinner (1) +
    // queue (2) + fleet (4) + todos (7) + input (3) + status (1) = 18 rows,
    // more than the whole 15-row inline viewport. The budget in `split_frame`
    // must clip the PANELS, never the input box or status line — clipping
    // those made the app look frozen (typing echoed nowhere).
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.busy = true;
    app.turn_started_at = Some(std::time::Instant::now());
    app.blocks = vec![streaming_assistant("live", false)];
    app.message_queue = vec!["queued message".into()];
    for i in 0..3 {
        app.subagents.push(SubagentView {
            id: format!("a{i}"),
            kind: "Explore".into(),
            prompt: format!("review area {i}"),
            activity: "reading files".into(),
            tokens: 1000,
            started_at: std::time::Instant::now(),
            done: None,
            expanded: false,
        });
    }
    app.session_todos = (0..7)
        .map(|i| super::super::app_test_support::todo(&format!("task {i}"), TodoStatus::Pending))
        .collect();

    let mut terminal = Terminal::new(TestBackend::new(80, 15)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render_inline(f, &mut app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y: u16| -> String {
        (0..80)
            .map(|x| buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect()
    };
    let dump: String = (0..15).map(|y| row(y) + "\n").collect();

    assert!(
        dump.contains("what shall we build today?"),
        "the input row must survive the panel squeeze:\n{dump}"
    );
    assert!(
        row(14).contains(&app.config.model),
        "the status line must survive as the last row:\n{dump}"
    );
    assert!(
        dump.contains("Sub-agents") && dump.contains("tasks (") && dump.contains("queued"),
        "the panels still render (clipped, not dropped):\n{dump}"
    );
}

#[test]
fn commit_advances_cursor_and_leaves_streaming_block() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.committed_blocks = 0;
    app.blocks = vec![
        Block::User("q".into()),
        streaming_assistant("streaming…", false),
    ];
    app.busy = true;
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 12),
        TerminalOptions {
            viewport: Viewport::Inline(6),
        },
    )
    .unwrap();
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 1,
        "while busy, only the finished User block commits; the streaming block stays live"
    );
    // Turn finishes: the streaming block is now finished and commits too.
    app.busy = false;
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(app.committed_blocks, 2);
}

#[test]
fn commit_resets_after_transcript_shrinks() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.blocks = vec![Block::System("x".into())];
    app.committed_blocks = 9; // stale (simulates a /clear that shrank the transcript)
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 12),
        TerminalOptions {
            viewport: Viewport::Inline(6),
        },
    )
    .unwrap();
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 1,
        "a stale cursor past the end resets to 0, then commits the surviving block"
    );
}

#[test]
fn commit_keeps_lone_welcome_live_until_conversation_starts() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    // Fresh app: the transcript is just the welcome card, nothing committed.
    assert!(matches!(app.blocks.as_slice(), [Block::Welcome]));
    app.committed_blocks = 0;
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 16),
        TerminalOptions {
            viewport: Viewport::Inline(13),
        },
    )
    .unwrap();
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 0,
        "the lone welcome stays live on the first screen (no scrollback gap above the input)"
    );

    // Once a turn begins, the welcome commits to scrollback like any finished block.
    app.blocks.push(Block::User("hi".into()));
    app.busy = true;
    commit_finished_blocks(&mut app, &mut terminal);
    assert_eq!(
        app.committed_blocks, 1,
        "the welcome commits the moment the conversation grows past it"
    );
}

#[test]
fn first_screen_renders_welcome_and_cute_placeholder() {
    use ratatui::backend::TestBackend;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    let mut terminal = Terminal::with_options(
        TestBackend::new(80, 16),
        TerminalOptions {
            viewport: Viewport::Inline(13),
        },
    )
    .unwrap();
    terminal
        .draw(|f| crate::tui::ui::render_inline(f, &mut app))
        .unwrap();
    let dump: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    // The lone welcome must render live in the viewport (it is bottom-anchored
    // right above the input), and the input shows the new playful placeholder.
    assert!(
        dump.contains("hi, i'm tomte!"),
        "the welcome greeting renders in the live viewport, not just scrollback"
    );
    assert!(
        dump.contains("what shall we build today?"),
        "the cute input placeholder renders"
    );
}

#[test]
fn welcome_panel_is_complete_with_setup_and_getting_started() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.last_width = 100;
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render(f, &mut app))
        .unwrap();
    let dump: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    // The welcome is a complete panel, not just a greeting: it carries the
    // tagline, the active setup (model/workspace), and a live getting-started
    // signal — all welcome-only strings, so they can't match the status line.
    for needle in ["keep your codebase tidy", "workspace", "house rules"] {
        assert!(dump.contains(needle), "welcome panel missing {needle:?}");
    }
}

#[test]
fn altscreen_pins_input_to_the_bottom() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = App::new();
    app.render_mode = RenderMode::AltScreen;
    let h = 24u16;
    let mut terminal = Terminal::new(TestBackend::new(80, h)).unwrap();
    terminal
        .draw(|f| crate::tui::ui::render(f, &mut app))
        .unwrap();
    let buf = terminal.backend().buffer();
    let w = buf.area().width as usize;
    let rows: Vec<String> = buf
        .content()
        .chunks(w)
        .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
        .collect();
    // The default full-screen layout: welcome at the top, input pinned to the
    // bottom edge of the terminal.
    let greet = rows
        .iter()
        .position(|r| r.contains("hi, i'm tomte!"))
        .expect("welcome greeting renders");
    let input = rows
        .iter()
        .position(|r| r.contains("what shall we build today?"))
        .expect("input placeholder renders");
    assert!(greet < 8, "welcome sits at the top (row {greet})");
    assert!(
        input >= h as usize - 4,
        "input is pinned to the bottom (row {input} of {h})"
    );
}

// The expanded flag is a per-turn live-detail view in inline mode: left on
// across turns, the next turn's scrollback commits would bake expanded detail
// in forever with no way to collapse them (Ctrl+O at rest opens the pager,
// not the toggle). Alt-screen keeps the flag — there it stays collapsible.
#[test]
fn turn_end_resets_expanded_tools_only_in_inline_mode() {
    let mut app = App::new();
    app.render_mode = RenderMode::Inline;
    app.busy = true;
    app.expanded_tools = true;
    apply_agent_event(&mut app, AgentEvent::TurnComplete);
    assert!(
        !app.expanded_tools,
        "inline: the per-turn flag ends with the turn"
    );

    let mut app = App::new();
    app.render_mode = RenderMode::AltScreen;
    app.busy = true;
    app.expanded_tools = true;
    apply_agent_event(&mut app, AgentEvent::TurnComplete);
    assert!(
        app.expanded_tools,
        "alt-screen: the toggle persists across turns"
    );
}

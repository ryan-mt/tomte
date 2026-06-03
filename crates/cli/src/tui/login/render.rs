//! Rendering of the login/onboarding screen: the logo, tagline, the per-stage
//! body (mode picker, browser wait, ToS gate, code paste, API-key entry), and
//! the footer. Pure draw code — all state lives in [`super::LoginScreen`].

use opencli_core::auth::anthropic as anth;
use opencli_core::provider::Provider;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{LoginScreen, Option_, Stage};
use crate::tui::input::TextInput;

const ASCII_LOGO: &str = "
  ██████╗ ██████╗ ███████╗███╗   ██╗ ██████╗██╗     ██╗
 ██╔═══██╗██╔══██╗██╔════╝████╗  ██║██╔════╝██║     ██║
 ██║   ██║██████╔╝█████╗  ██╔██╗ ██║██║     ██║     ██║
 ██║   ██║██╔═══╝ ██╔══╝  ██║╚██╗██║██║     ██║     ██║
 ╚██████╔╝██║     ███████╗██║ ╚████║╚██████╗███████╗██║
  ╚═════╝ ╚═╝     ╚══════╝╚═╝  ╚═══╝ ╚═════╝╚══════╝╚═╝
";

pub fn render(f: &mut Frame, area: Rect, screen: &LoginScreen, stage: &Stage, err: Option<&str>) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // logo
            Constraint::Length(3), // tagline
            Constraint::Min(8),    // body
            Constraint::Length(1), // footer
        ])
        .split(centered(area));

    render_logo(f, layout[0]);
    render_tagline(f, layout[1]);
    match stage {
        Stage::PickMode => render_pick(f, layout[2], screen.selected, err),
        Stage::WaitingForBrowser { url } => render_browser(f, layout[2], url, err),
        Stage::ApiKeyEntry { provider } => {
            render_api_key(f, layout[2], *provider, &screen.api_input, err)
        }
        Stage::AnthropicTos => render_tos(f, layout[2], err),
        Stage::AnthropicPaste { url } => render_paste(f, layout[2], url, &screen.paste_input, err),
        Stage::Success(_) => {}
        Stage::Cancelled => {}
    }
    render_footer(f, layout[3]);
}

fn centered(area: Rect) -> Rect {
    let max_w = 76u16;
    if area.width <= max_w {
        return area;
    }
    let x = area.x + (area.width - max_w) / 2;
    Rect {
        x,
        y: area.y,
        width: max_w,
        height: area.height,
    }
}

fn render_logo(f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for raw in ASCII_LOGO.lines() {
        lines.push(Line::from(Span::styled(
            raw.to_string(),
            Style::default()
                .fg(Color::Rgb(16, 163, 127))
                .add_modifier(Modifier::BOLD),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_tagline(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            " opencli — a coding agent for your terminal",
            Style::default()
                .fg(Color::Rgb(240, 240, 240))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " powered by OpenAI & Anthropic models",
            Style::default().fg(Color::Rgb(180, 180, 180)),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_pick(f: &mut Frame, area: Rect, selected: Option_, err: Option<&str>) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Choose how to sign in to opencli",
        Style::default().fg(Color::Rgb(240, 240, 240)),
    )));
    lines.push(Line::raw(""));

    let item = |opt: Option_, idx: usize, title: &str, desc: &str| -> Vec<Line<'static>> {
        let is_sel = selected == opt;
        let caret = if is_sel { ">" } else { " " };
        let title_style = if is_sel {
            Style::default()
                .fg(Color::Rgb(25, 195, 154))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(230, 230, 230))
        };
        let head = Line::from(vec![
            Span::styled(
                format!(" {caret} {idx}. "),
                Style::default().fg(if is_sel {
                    Color::Rgb(25, 195, 154)
                } else {
                    Color::Rgb(180, 180, 180)
                }),
            ),
            Span::styled(title.to_string(), title_style),
        ]);
        let sub = Line::from(Span::styled(
            format!("     {desc}"),
            Style::default().fg(Color::Rgb(180, 180, 180)),
        ));
        vec![head, sub]
    };
    lines.extend(item(
        Option_::OpenAiChatGpt,
        1,
        "OpenAI — Sign in with ChatGPT",
        "Included with Plus, Pro, Business, and Enterprise plans",
    ));
    lines.extend(item(
        Option_::OpenAiApiKey,
        2,
        "OpenAI — API key",
        "Pay-as-you-go with an sk-… key",
    ));
    lines.extend(item(
        Option_::AnthropicOauth,
        3,
        "Anthropic — Claude Pro/Max (OAuth)",
        "Uses your claude.ai subscription — MAY violate Anthropic ToS",
    ));
    lines.extend(item(
        Option_::AnthropicApiKey,
        4,
        "Anthropic — Console API key",
        "Pay-as-you-go with an sk-ant-… key",
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " ↑↓ to select · Enter to continue · Ctrl+C to exit",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_browser(f: &mut Frame, area: Rect, url: &str, err: Option<&str>) {
    use ratatui::widgets::Wrap;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Finish signing in via your browser…",
        Style::default()
            .fg(Color::Rgb(25, 195, 154))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " If the page didn't open automatically, copy this URL:",
        Style::default().fg(Color::Rgb(190, 190, 190)),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!(" {url}"),
        Style::default()
            .fg(Color::Rgb(120, 200, 255))
            .add_modifier(Modifier::UNDERLINED),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Press Esc to cancel and pick a different sign-in method.",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        for chunk in textwrap::wrap(e, area.width.saturating_sub(2) as usize) {
            lines.push(Line::from(Span::styled(
                format!(" {chunk}"),
                Style::default().fg(Color::Rgb(255, 120, 120)),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_tos(f: &mut Frame, area: Rect, err: Option<&str>) {
    use ratatui::widgets::Wrap;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Claude Pro/Max sign-in — read this first",
        Style::default()
            .fg(Color::Rgb(255, 196, 0))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    for raw in anth::TOS_WARNING.lines() {
        lines.push(Line::from(Span::styled(
            format!(" {raw}"),
            Style::default().fg(Color::Rgb(220, 200, 160)),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Press Enter to accept and continue · Esc to cancel",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_paste(f: &mut Frame, area: Rect, url: &str, input: &TextInput, err: Option<&str>) {
    use ratatui::widgets::Wrap;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        " Sign in with Claude in your browser…",
        Style::default()
            .fg(Color::Rgb(25, 195, 154))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " If the page didn't open automatically, copy this URL:",
        Style::default().fg(Color::Rgb(190, 190, 190)),
    )));
    lines.push(Line::from(Span::styled(
        format!(" {url}"),
        Style::default()
            .fg(Color::Rgb(120, 200, 255))
            .add_modifier(Modifier::UNDERLINED),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " After approving, paste the authorization code shown by claude.ai:",
        Style::default().fg(Color::Rgb(230, 230, 230)),
    )));
    lines.push(Line::raw(""));
    let body = if input.is_empty() {
        Span::styled(
            "paste code here…",
            Style::default().fg(Color::Rgb(120, 120, 120)),
        )
    } else {
        Span::styled(input.buffer.clone(), Style::default().fg(Color::White))
    };
    lines.push(Line::from(vec![
        Span::styled(
            " > ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        body,
        Span::styled("█", Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Enter to finish · Esc to cancel · Ctrl+U to clear",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        for chunk in textwrap::wrap(e, area.width.saturating_sub(2) as usize) {
            lines.push(Line::from(Span::styled(
                format!(" {chunk}"),
                Style::default().fg(Color::Rgb(255, 120, 120)),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_api_key(
    f: &mut Frame,
    area: Rect,
    provider: Provider,
    input: &TextInput,
    err: Option<&str>,
) {
    let (label, hint, placeholder) = match provider {
        Provider::OpenAi => (" Paste your OpenAI API key", " starts with sk-…", "sk-…"),
        Provider::Anthropic => (
            " Paste your Anthropic API key",
            " starts with sk-ant-…",
            "sk-ant-…",
        ),
    };
    let masked: String = "•".repeat(input.buffer.chars().count());
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        label,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    lines.push(Line::raw(""));
    let body = if input.is_empty() {
        Span::styled(placeholder, Style::default().fg(Color::Rgb(170, 170, 170)))
    } else {
        Span::styled(masked, Style::default().fg(Color::White))
    };
    lines.push(Line::from(vec![
        Span::styled(
            " > ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        body,
        Span::styled("█", Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        " Enter to save · Esc to go back · Ctrl+U to clear",
        Style::default().fg(Color::Rgb(170, 170, 170)),
    )));
    if let Some(e) = err {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_footer(f: &mut Frame, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " opencli · Rust · MIT",
            Style::default().fg(Color::Rgb(170, 170, 170)),
        ))),
        area,
    );
}

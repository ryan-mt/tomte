//! Colored renderer for the `/context` report.
//!
//! Turns a [`tomte_core::context_report::ContextReport`] into styled
//! scrollback lines that mirror Claude Code's `/context`: a proportional grid on
//! the left, the model headline plus a per-category legend on the right, and
//! detail sections (MCP servers, custom agents, memory files, skills) underneath
//! with their slash-command shortcuts. `/context all` expands the detail
//! sections to list the actual items.

use crate::tui::palette;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tomte_core::context_report::ContextReport;

// Grid dimensions. 12 rows lines the grid up with the 12-line right column
// (title, model, tokens, blank, legend header, 6 categories, free space).
const GRID_W: usize = 24;
const GRID_H: usize = 12;
const CELLS: usize = GRID_W * GRID_H;

const USED_CELL: char = '█';
const FREE_CELL: char = '░';
const INDENT: &str = "  ";

// One stable color per category, reused by the grid, the legend marker, and the
// matching detail-section header.
const C_SYSTEM_PROMPT: Color = Color::Rgb(120, 170, 255);
const C_SYSTEM_TOOLS: Color = Color::Rgb(95, 205, 205);
const C_CUSTOM_AGENTS: Color = Color::Rgb(190, 150, 255);
const C_MEMORY: Color = Color::Rgb(245, 170, 95);
const C_SKILLS: Color = Color::Rgb(130, 210, 130);
const C_MESSAGES: Color = Color::Rgb(225, 225, 235);
const C_MCP: Color = Color::Rgb(240, 220, 110);
const C_FREE: Color = Color::Rgb(70, 75, 88);

fn dim() -> Style {
    Style::default().fg(palette::TEXT_MUTED)
}
fn bright() -> Style {
    Style::default().fg(Color::Rgb(230, 230, 235))
}

/// Render the report into colored scrollback lines (indent already baked in, so
/// the caller pushes them verbatim).
pub fn render(report: &ContextReport, expanded: bool) -> Vec<Line<'static>> {
    let limit = report.limit.max(1);
    let used = report.estimated_used();

    // Categories that occupy the window, in grid/legend order. MCP tools are
    // loaded on-demand (0 tokens), so they sit in the detail section only.
    let cats: [(&str, u64, Color); 6] = [
        ("System prompt", report.system_prompt, C_SYSTEM_PROMPT),
        ("System tools", report.system_tools, C_SYSTEM_TOOLS),
        ("Custom agents", report.custom_agents, C_CUSTOM_AGENTS),
        ("Memory files", report.memory_files, C_MEMORY),
        ("Skills", report.skills, C_SKILLS),
        ("Messages", report.messages, C_MESSAGES),
    ];

    let grid = build_grid(&cats, limit);
    let right = build_right_column(report, &cats, used, limit);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for row in 0..GRID_H {
        let mut spans = vec![Span::raw(INDENT)];
        for col in 0..GRID_W {
            let (ch, color) = grid[row * GRID_W + col];
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        }
        spans.push(Span::raw("   "));
        if let Some(extra) = right.get(row) {
            spans.extend(extra.iter().cloned());
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));
    push_detail_sections(&mut lines, report, expanded);

    if !expanded {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("{INDENT}/context all to expand"),
            dim().add_modifier(Modifier::ITALIC),
        )));
    }
    if report.used_real > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "{INDENT}Provider-reported occupancy last turn: {} tokens",
                human(report.used_real)
            ),
            dim().add_modifier(Modifier::ITALIC),
        )));
    }

    lines
}

/// Allocate `CELLS` grid cells across the categories (rounded to nearest cell),
/// the remainder being free space.
fn build_grid(cats: &[(&str, u64, Color); 6], limit: u64) -> Vec<(char, Color)> {
    let mut cells: Vec<(char, Color)> = Vec::with_capacity(CELLS);
    for (_, tokens, color) in cats {
        let n = (((*tokens as u128) * (CELLS as u128) + (limit as u128) / 2) / (limit as u128))
            as usize;
        for _ in 0..n {
            if cells.len() < CELLS {
                cells.push((USED_CELL, *color));
            }
        }
    }
    while cells.len() < CELLS {
        cells.push((FREE_CELL, C_FREE));
    }
    cells.truncate(CELLS);
    cells
}

/// The right-hand column: title, model headline, token headline, blank, legend
/// header, one row per category, then free space. Exactly `GRID_H` entries so it
/// aligns with the grid.
fn build_right_column(
    report: &ContextReport,
    cats: &[(&str, u64, Color); 6],
    used: u64,
    limit: u64,
) -> Vec<Vec<Span<'static>>> {
    let mut col: Vec<Vec<Span<'static>>> = Vec::with_capacity(GRID_H);
    col.push(vec![Span::styled(
        "Context Usage",
        bright().add_modifier(Modifier::BOLD),
    )]);
    col.push(vec![
        Span::styled(report.model.clone(), bright()),
        Span::styled(format!(" ({} context)", human(limit)), dim()),
    ]);
    col.push(vec![Span::styled(
        format!(
            "{}/{} tokens ({}%)",
            human(used),
            human(limit),
            pct0(used, limit)
        ),
        dim(),
    )]);
    col.push(vec![]);
    col.push(vec![Span::styled(
        "Estimated usage by category",
        dim().add_modifier(Modifier::ITALIC),
    )]);
    for (label, tokens, color) in cats {
        col.push(legend_row(label, *tokens, *color, limit, true));
    }
    col.push(legend_row(
        "Free space",
        report.free(),
        C_FREE,
        limit,
        false,
    ));
    col
}

/// `█ Label: 2.5k tokens (0.3%)` — the trailing "tokens" word is dropped for the
/// free-space row to match the reference.
fn legend_row(
    label: &str,
    tokens: u64,
    color: Color,
    limit: u64,
    with_word: bool,
) -> Vec<Span<'static>> {
    let value = if with_word {
        format!("{} tokens ({}%)", human(tokens), pct1(tokens, limit))
    } else {
        format!("{} ({}%)", human(tokens), pct1(tokens, limit))
    };
    vec![
        Span::styled("█ ", Style::default().fg(color)),
        Span::styled(format!("{label}: "), bright()),
        Span::styled(value, dim()),
    ]
}

fn push_detail_sections(lines: &mut Vec<Line<'static>>, report: &ContextReport, expanded: bool) {
    let memory_items: Vec<String> = report
        .memory_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    detail_section(
        lines,
        C_MCP,
        "MCP tools",
        "/mcp",
        Some("loaded on-demand"),
        report.mcp_servers.len(),
        "server",
        0,
        expanded,
        &report.mcp_servers,
    );
    detail_section(
        lines,
        C_CUSTOM_AGENTS,
        "Custom agents",
        "/agents",
        None,
        report.agent_names.len(),
        "agent",
        report.custom_agents,
        expanded,
        &report.agent_names,
    );
    detail_section(
        lines,
        C_MEMORY,
        "Memory files",
        "/memory",
        None,
        memory_items.len(),
        "file",
        report.memory_files,
        expanded,
        &memory_items,
    );
    detail_section(
        lines,
        C_SKILLS,
        "Skills",
        "/skills",
        None,
        report.skill_names.len(),
        "skill",
        report.skills,
        expanded,
        &report.skill_names,
    );
}

#[allow(clippy::too_many_arguments)]
fn detail_section(
    lines: &mut Vec<Line<'static>>,
    color: Color,
    title: &str,
    slash: &str,
    note: Option<&str>,
    count: usize,
    noun: &str,
    tokens: u64,
    expanded: bool,
    items: &[String],
) {
    let mut header = vec![
        Span::styled(
            format!("{INDENT}{title}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", dim()),
        Span::styled(slash.to_string(), Style::default().fg(palette::INFO)),
    ];
    if let Some(n) = note {
        header.push(Span::styled(format!(" ({n})"), dim()));
    }
    lines.push(Line::from(header));

    let summary = format!(
        "{INDENT}  └ {count} {noun}{} · {} tokens",
        if count == 1 { "" } else { "s" },
        human(tokens)
    );
    lines.push(Line::from(Span::styled(summary, dim())));

    if expanded && !items.is_empty() {
        const CAP: usize = 30;
        for item in items.iter().take(CAP) {
            lines.push(Line::from(Span::styled(
                format!("{INDENT}    • {item}"),
                dim(),
            )));
        }
        if items.len() > CAP {
            lines.push(Line::from(Span::styled(
                format!("{INDENT}    … and {} more", items.len() - CAP),
                dim().add_modifier(Modifier::ITALIC),
            )));
        }
    }
    lines.push(Line::raw(""));
}

/// `2500 → "2.5k"`, `975100 → "975.1k"`, `1_000_000 → "1M"`, `8 → "8"`.
fn human(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        return trim_zero(format!("{:.1}", n as f64 / 1000.0)) + "k";
    }
    trim_zero(format!("{:.1}", n as f64 / 1_000_000.0)) + "M"
}

fn trim_zero(s: String) -> String {
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

fn pct1(n: u64, limit: u64) -> String {
    if limit == 0 {
        return "0.0".to_string();
    }
    format!("{:.1}", n as f64 * 100.0 / limit as f64)
}

fn pct0(n: u64, limit: u64) -> String {
    if limit == 0 {
        return "0".to_string();
    }
    format!("{}", (n as f64 * 100.0 / limit as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ContextReport {
        ContextReport {
            model: "claude-opus-4-8".to_string(),
            limit: 1_000_000,
            used_real: 24_900,
            system_prompt: 2_500,
            system_tools: 6_900,
            custom_agents: 314,
            memory_files: 6_900,
            skills: 8_200,
            mcp_tools: 0,
            messages: 8,
            agent_names: vec!["code-explorer".into(), "code-editor".into()],
            memory_paths: vec!["/home/u/project/CLAUDE.md".into()],
            skill_names: vec!["api-design".into(), "tdd".into()],
            mcp_servers: vec![],
        }
    }

    #[test]
    fn human_formats_like_the_reference() {
        assert_eq!(human(8), "8");
        assert_eq!(human(2_500), "2.5k");
        assert_eq!(human(6_900), "6.9k");
        assert_eq!(human(975_100), "975.1k");
        assert_eq!(human(1_000_000), "1M");
        assert_eq!(human(1_500_000), "1.5M");
    }

    #[test]
    fn grid_is_full_size_and_mostly_free_when_lightly_used() {
        let cells = build_grid(
            &[
                ("System prompt", 2_500, C_SYSTEM_PROMPT),
                ("System tools", 6_900, C_SYSTEM_TOOLS),
                ("Custom agents", 314, C_CUSTOM_AGENTS),
                ("Memory files", 6_900, C_MEMORY),
                ("Skills", 8_200, C_SKILLS),
                ("Messages", 8, C_MESSAGES),
            ],
            1_000_000,
        );
        assert_eq!(cells.len(), CELLS);
        let free = cells.iter().filter(|(c, _)| *c == FREE_CELL).count();
        // ~2.5% used → the vast majority of cells are free.
        assert!(
            free > CELLS * 9 / 10,
            "expected mostly-free grid, got {free}"
        );
    }

    #[test]
    fn render_aligns_grid_with_right_column_and_lists_sections() {
        let lines = render(&sample(), false);
        // First GRID_H lines carry both grid and right-column content.
        assert!(lines.len() > GRID_H);
        let flat = |l: &Line| -> String { l.spans.iter().map(|s| s.content.to_string()).collect() };
        assert!(flat(&lines[0]).contains("Context Usage"));
        assert!(flat(&lines[1]).contains("claude-opus-4-8"));
        let all: String = lines.iter().map(flat).collect::<Vec<_>>().join("\n");
        assert!(all.contains("System prompt:"));
        assert!(all.contains("Free space:"));
        assert!(all.contains("Custom agents · /agents"));
        assert!(all.contains("Skills · /skills"));
        assert!(all.contains("/context all to expand"));
    }

    #[test]
    fn expanded_lists_items_and_hides_expand_hint() {
        let lines = render(&sample(), true);
        let flat = |l: &Line| -> String { l.spans.iter().map(|s| s.content.to_string()).collect() };
        let all: String = lines.iter().map(flat).collect::<Vec<_>>().join("\n");
        assert!(all.contains("code-explorer"));
        assert!(all.contains("api-design"));
        assert!(!all.contains("/context all to expand"));
    }

    /// End-to-end preview against THIS repo's real config/skills/agents/memory:
    /// `cargo test -p tomte context_view_preview_real -- --ignored --nocapture`.
    /// Exercises the same `build` → `render` path the `/context` command uses.
    #[test]
    #[ignore]
    fn context_view_preview_real() {
        let cfg = tomte_core::config::load();
        let report = tomte_core::context_report::build(std::path::Path::new("."), &cfg, 4096, 0);
        let lines = render(&report, false);
        println!();
        for line in &lines {
            let mut s = String::new();
            for span in &line.spans {
                if let Color::Rgb(r, g, b) = span.style.fg.unwrap_or(Color::Reset) {
                    s.push_str(&format!("\x1b[38;2;{r};{g};{b}m{}\x1b[0m", span.content));
                } else {
                    s.push_str(&span.content);
                }
            }
            println!("{s}");
        }
    }

    /// Truecolor preview: `cargo test -p tomte context_view_preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn context_view_preview() {
        let lines = render(&sample(), false);
        println!();
        for line in &lines {
            let mut s = String::new();
            for span in &line.spans {
                if let Color::Rgb(r, g, b) = span.style.fg.unwrap_or(Color::Reset) {
                    s.push_str(&format!("\x1b[38;2;{r};{g};{b}m{}\x1b[0m", span.content));
                } else {
                    s.push_str(&span.content);
                }
            }
            println!("{s}");
        }
    }
}

//! Rendering the companion: the full sprite + speech bubble, the compact corner
//! view, and the hatch animation. Pure — a `(pet index, tick/elapsed)` pair maps
//! deterministically to a list of ratatui lines, so the render cache stays
//! correct and the output is testable. Each terminal cell packs two vertical
//! pixels using the upper/lower half blocks (`▀`/`▄`).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Rgb, PETS};

const INDENT: &str = "   ";

/// Render the companion at `index` for this `tick` (selects the speech line).
pub fn render(index: usize, tick: usize) -> Vec<Line<'static>> {
    let pet = &PETS[index % PETS.len()];
    let mut lines = Vec::new();
    let rarity_style = Style::default()
        .fg(Color::Rgb(
            pet.rarity.color().0,
            pet.rarity.color().1,
            pet.rarity.color().2,
        ))
        .add_modifier(Modifier::BOLD);

    // Header: rarity tag + name.
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled(pet.rarity.label(), rarity_style),
    ]));
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled(
            pet.name,
            Style::default()
                .fg(Color::Rgb(235, 235, 240))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::raw(""));

    // Sprite.
    for pair in pet.rows.chunks(2) {
        lines.push(half_block_line(
            INDENT,
            pair[0],
            pair.get(1).copied().unwrap_or(""),
            |c| pet.color_of(c),
        ));
    }

    // Speech bubble below the pet, bordered in the rarity colour.
    let text = pet.speech[tick % pet.speech.len()];
    let inner = text.chars().count();
    let border = Style::default().fg(Color::Rgb(
        pet.rarity.color().0,
        pet.rarity.color().1,
        pet.rarity.color().2,
    ));
    let body = Style::default().fg(Color::Rgb(225, 225, 230));
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled("  ╱", border),
    ]));
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled(format!("╭{}╮", "─".repeat(inner + 2)), border),
    ]));
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled("│ ", border),
        Span::styled(text.to_string(), body),
        Span::styled(" │", border),
    ]));
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled(format!("╰{}╯", "─".repeat(inner + 2)), border),
    ]));
    lines.push(Line::raw(""));
    lines
}

/// Render one pair of pixel rows into a single terminal line of half blocks,
/// mapping each palette char through `color` (`None` = transparent). The top
/// pixel becomes the cell foreground (`▀`), the bottom the background.
fn half_block_line(
    indent: &str,
    top: &str,
    bottom: &str,
    color: impl Fn(char) -> Option<Rgb>,
) -> Line<'static> {
    let top: Vec<char> = top.chars().collect();
    let bottom: Vec<char> = bottom.chars().collect();
    let width = top.len().max(bottom.len());
    let mut spans = vec![Span::raw(indent.to_string())];
    for x in 0..width {
        let t = top.get(x).copied().and_then(&color);
        let b = bottom.get(x).copied().and_then(&color);
        let span = match (t, b) {
            (Some(t), Some(b)) => Span::styled(
                "▀",
                Style::default()
                    .fg(Color::Rgb(t.0, t.1, t.2))
                    .bg(Color::Rgb(b.0, b.1, b.2)),
            ),
            (Some(t), None) => Span::styled("▀", Style::default().fg(Color::Rgb(t.0, t.1, t.2))),
            (None, Some(b)) => Span::styled("▄", Style::default().fg(Color::Rgb(b.0, b.1, b.2))),
            (None, None) => Span::raw(" "),
        };
        spans.push(span);
    }
    Line::from(spans)
}

/// Name of the pet at `index` (for lock/status messages).
pub fn pet_name(index: usize) -> &'static str {
    PETS[index % PETS.len()].name
}

/// The compact corner companion: the whole pet sprite (no header, bubble, or
/// indent) so the full creature tucks small into the bottom-right of the chat.
pub fn mini_lines(index: usize) -> Vec<Line<'static>> {
    let pet = &PETS[index % PETS.len()];
    pet.rows
        .chunks(2)
        .map(|p| {
            half_block_line("", p[0], p.get(1).copied().unwrap_or(""), |c| {
                pet.color_of(c)
            })
        })
        .collect()
}

// ---- hatching animation ---------------------------------------------------

/// Total hatch animation length. After this the caller adopts the pet and the
/// big overlay gives way to the small corner companion.
pub const HATCH_MS: u64 = 2800;

const EGG_ROWS: [&str; 14] = [
    "....SSSS....",
    "...SSSSSS...",
    "..SSSSSSSS..",
    ".SSSSoSSSSS.",
    ".SSSSSSSSSS.",
    "SSSSSSSSSSSS",
    "SSSoSSSSSSSS",
    "SSSSSSSSSSSS",
    "SSSSSSSoSSSS",
    "SSSSSSSSSSSS",
    ".SSSSSSSSSS.",
    ".SSSSoSSSSS.",
    "..SSSSSSSS..",
    "...SSSSSS...",
];

const EGG_CRACKED: [&str; 14] = [
    "....SSSS....",
    "...SSSSSS...",
    "..SSSSxSSS..",
    ".SSSSxSSSSS.",
    ".SSSSSxSSSS.",
    "SSSSSxSSSSSS",
    "SSSSSSxSSSSS",
    "SSSSSxSSSSSS",
    "SSSSSSxSSSSS",
    "SSSSSxSSSSSS",
    ".SSSSSxSSSS.",
    ".SSSSxSSSSS.",
    "..SSSSSSSS..",
    "...SSSSSS...",
];

fn egg_color(c: char) -> Option<Rgb> {
    Some(match c {
        'S' => (240, 228, 205),
        'o' => (205, 175, 135),
        'x' => (70, 55, 48),
        _ => return None,
    })
}

/// One frame of the hatch animation for `pet` at `elapsed_ms`. The egg rocks
/// side to side, cracks near the end, then the companion is revealed.
pub fn hatch_lines(pet: usize, elapsed_ms: u64) -> Vec<Line<'static>> {
    let accent = PETS[pet % PETS.len()].rarity.color();
    let accent_style = Style::default()
        .fg(Color::Rgb(accent.0, accent.1, accent.2))
        .add_modifier(Modifier::BOLD);

    // Final beat: reveal the full companion.
    if elapsed_ms + 250 >= HATCH_MS {
        let mut out = vec![
            Line::from(Span::styled("   ✦ it hatched! ✦", accent_style)),
            Line::raw(""),
        ];
        out.extend(render(pet, 0));
        return out;
    }

    let cracking = elapsed_ms + 750 >= HATCH_MS;
    let rows = if cracking { &EGG_CRACKED } else { &EGG_ROWS };
    // A gentle, slow rock while incubating; a faster, harder tremble once it
    // starts cracking. `step_ms` is how long each tilt is held.
    let (step_ms, amp) = if cracking { (110, 2) } else { (300, 1) };
    let offset = [0i32, 1, 0, -1][((elapsed_ms / step_ms) % 4) as usize] * amp;
    let indent = " ".repeat((4 + offset).max(0) as usize);

    let caption = if cracking {
        "   crack… crack…"
    } else {
        "   hatching your companion…"
    };
    let mut out = vec![
        Line::from(Span::styled(
            caption,
            Style::default().fg(Color::Rgb(200, 200, 205)),
        )),
        Line::raw(""),
    ];
    for pair in rows.chunks(2) {
        out.push(half_block_line(
            &indent,
            pair[0],
            pair.get(1).copied().unwrap_or(""),
            egg_color,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egg_grids_are_well_formed() {
        for grid in [EGG_ROWS, EGG_CRACKED] {
            assert_eq!(grid.len() % 2, 0, "egg needs even rows");
            let w = grid[0].chars().count();
            for row in grid {
                assert_eq!(row.chars().count(), w, "egg row {row:?}");
            }
        }
    }

    #[test]
    fn hatch_lines_render_through_all_phases() {
        for ms in [0u64, HATCH_MS / 2, HATCH_MS - 400, HATCH_MS - 100] {
            assert!(!hatch_lines(0, ms).is_empty(), "ms={ms}");
        }
    }

    #[test]
    fn mini_lines_show_the_whole_pet() {
        for (i, pet) in PETS.iter().enumerate() {
            assert_eq!(
                mini_lines(i).len(),
                pet.rows.len() / 2,
                "mini for {}",
                pet.name
            );
        }
    }

    #[test]
    fn render_produces_lines_for_every_pet() {
        for (i, pet) in PETS.iter().enumerate() {
            // header(2) + blank(1) + sprite(6) + tail(1) + bubble(3) + blank(1) = 14
            assert_eq!(render(i, 0).len(), 14, "pet {}", pet.name);
        }
    }
}

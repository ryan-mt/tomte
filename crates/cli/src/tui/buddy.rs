//! The `/buddy` companion — a deterministic, rarity-weighted pixel pet, in the
//! spirit of Claude Code's buddy system.
//!
//! The pet is **deterministic from the signed-in AI account**: a hash of the
//! account identity seeds a Mulberry32 PRNG (the same generator Claude Code
//! uses), so an account always hatches the same companion ("stable") and it
//! only re-rolls when the user switches accounts. Because it is derived purely
//! from the account — nothing is persisted — deleting local state can't re-roll
//! it. Rarity is rolled with weighted odds: one legendary at the lowest rate.
//!
//! Rendering is pure: a `(pet index, tick)` pair maps deterministically to a
//! list of ratatui lines, so the render cache stays correct and the output is
//! testable. Each terminal cell packs two vertical pixels using the upper/lower
//! half blocks (`▀`/`▄`) — the top pixel is the foreground colour, the bottom
//! the background — the standard way to draw colour pixel art in a terminal.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
}

impl Rarity {
    /// Relative drop weight; the five weights sum to 100.
    fn weight(self) -> f64 {
        match self {
            Self::Common => 60.0,
            Self::Uncommon => 25.0,
            Self::Rare => 10.0,
            Self::Epic => 4.0,
            Self::Legendary => 1.0,
        }
    }

    fn color(self) -> Rgb {
        match self {
            Self::Common => (170, 172, 180),
            Self::Uncommon => (120, 205, 140),
            Self::Rare => (110, 165, 255),
            Self::Epic => (190, 130, 255),
            Self::Legendary => (255, 205, 90),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Common => "COMMON",
            Self::Uncommon => "UNCOMMON",
            Self::Rare => "RARE",
            Self::Epic => "EPIC",
            Self::Legendary => "✦ LEGENDARY ✦",
        }
    }
}

/// One companion species: name, rarity tier, sprite grid, its colour palette,
/// and a few speech lines. The grid is an even number of equal-width rows; each
/// char keys into `colors` (any char not listed is transparent).
struct Pet {
    name: &'static str,
    rarity: Rarity,
    rows: &'static [&'static str],
    colors: &'static [(char, Rgb)],
    speech: &'static [&'static str],
}

impl Pet {
    fn color_of(&self, c: char) -> Option<Rgb> {
        self.colors
            .iter()
            .find_map(|&(k, rgb)| (k == c).then_some(rgb))
    }
}

// ---- the roster -----------------------------------------------------------
// Sprites are 14 wide × 12 tall (→ 6 terminal lines). `.` is transparent.

const PETS: &[Pet] = &[
    Pet {
        name: "Mochi the Cat",
        rarity: Rarity::Common,
        rows: &[
            "..CC......CC..",
            "..CC......CC..",
            ".CCCCCCCCCCCC.",
            "CCCCCCCCCCCCCC",
            "CCEECCCCCCEECC",
            "CCCCCCCCCCCCCC",
            "CCCCCMMMMCCCCC",
            "CCCCCCCCCCCCCC",
            ".CCWWWWWWWWCC.",
            ".CCWWWWWWWWCC.",
            ".CCCCCCCCCCCC.",
            "..CC......CC..",
        ],
        colors: &[
            ('C', (235, 150, 80)),
            ('E', (45, 40, 40)),
            ('M', (235, 150, 160)),
            ('W', (250, 228, 200)),
        ],
        speech: &[
            "all systems go!",
            "ready when you are",
            "let's build something",
        ],
    },
    Pet {
        name: "Goo the Slime",
        rarity: Rarity::Common,
        rows: &[
            ".....SSSS.....",
            "...SSSSSSSS...",
            "..SSSSSSSSSS..",
            ".SSSSSSSSSSSS.",
            ".SSEESSSSEESS.",
            ".SSSSSSSSSSSS.",
            ".SSSSWWWWSSSS.",
            ".SSSSSSSSSSSS.",
            "..SSSSSSSSSS..",
            ".SSSSSSSSSSSS.",
            "SSSSSSSSSSSSSS",
            "SSSSSSSSSSSSSS",
        ],
        colors: &[
            ('S', (90, 200, 190)),
            ('E', (40, 50, 55)),
            ('W', (240, 245, 245)),
        ],
        speech: &["blub!", "squish squish", "i go where you go"],
    },
    Pet {
        name: "Quackers the Duck",
        rarity: Rarity::Uncommon,
        rows: &[
            "....DDDDDD....",
            "...DDDDDDDD...",
            "..DDDDDDDDDD..",
            "..DDEEDDEEDD..",
            "..DDDDDDDDDD..",
            "....OOOOOO....",
            ".DDDDDDDDDDDD.",
            "DDDDDDDDDDDDDD",
            "DDDDDDDDDDDDDD",
            "DDDDDDDDDDDDDD",
            ".DDDDDDDDDDDD.",
            "...OO....OO...",
        ],
        colors: &[
            ('D', (245, 215, 90)),
            ('E', (40, 40, 45)),
            ('O', (240, 140, 60)),
        ],
        speech: &[
            "quack!",
            "swimming through the diff",
            "found a bug, fixed it",
        ],
    },
    Pet {
        name: "Rusty the Fox",
        rarity: Rarity::Uncommon,
        rows: &[
            ".FF........FF.",
            ".FFF......FFF.",
            "..FFFFFFFFFF..",
            "..FFFFFFFFFF..",
            "..FEEFFFFEEF..",
            "..FFFFFFFFFF..",
            "...FWWWWWWF...",
            "...FWWMMWWF...",
            "...FFWWWWFF...",
            "....FFFFFF....",
            "...WFFFFFFW...",
            "...WW....WW...",
        ],
        colors: &[
            ('F', (225, 120, 55)),
            ('E', (40, 35, 35)),
            ('W', (248, 240, 230)),
            ('M', (35, 30, 30)),
        ],
        speech: &["sly and ready", "let's outsmart this", "tail wag"],
    },
    Pet {
        name: "Professor Owl",
        rarity: Rarity::Rare,
        rows: &[
            "..TT......TT..",
            ".TTTTTTTTTTTT.",
            "TTTTTTTTTTTTTT",
            "TTWWWTTTTWWWTT",
            "TTWEWTTTTWEWTT",
            "TTWWWTTTTWWWTT",
            "TTTTTTOOTTTTTT",
            "TGGTTTTTTTTGGT",
            "TGGTTTTTTTTGGT",
            ".TGGGGGGGGGGT.",
            "..TTTTTTTTTT..",
            "...YY....YY...",
        ],
        colors: &[
            ('T', (150, 110, 70)),
            ('G', (120, 88, 56)),
            ('W', (245, 238, 225)),
            ('E', (40, 35, 35)),
            ('O', (245, 175, 70)),
            ('Y', (245, 175, 70)),
        ],
        speech: &[
            "hoo's debugging? you are",
            "wisdom: read the error first",
            "patience…",
        ],
    },
    Pet {
        name: "Smolder the Dragon",
        rarity: Rarity::Epic,
        rows: &[
            "GG..........GG",
            "GGG........GGG",
            ".GGGGGGGGGGGG.",
            "..GGGGGGGGGG..",
            "..GGEEGGEEGG..",
            "..GGGGGGGGGG..",
            ".GGGGRRRRGGGG.",
            "GGGGGGGGGGGGGG",
            "GBGGGGGGGGGGBG",
            "GBBGGGGGGGGBBG",
            ".GGGGGGGGGGGG.",
            "..RR......RR..",
        ],
        colors: &[
            ('G', (90, 185, 110)),
            ('E', (35, 40, 35)),
            ('R', (235, 95, 80)),
            ('B', (190, 230, 140)),
        ],
        speech: &[
            "rawr — i mean, hi",
            "i hoard clean code",
            "breathing fire on bugs",
        ],
    },
    Pet {
        name: "Blaze the Phoenix",
        rarity: Rarity::Legendary,
        rows: &[
            "...A......A...",
            "..AAA....AAA..",
            ".AAAAAAAAAAAA.",
            "AAAAAAAAAAAAAA",
            "AAAAEEAAEEAAAA",
            "AAAAAAAAAAAAAA",
            "RAAAAAOOAAAAAR",
            "RRAAAAAAAAAARR",
            "RRRAAAAAAAARRR",
            ".RRRRAAAARRRR.",
            "..RRRRRRRRRR..",
            "...RR....RR...",
        ],
        colors: &[
            ('A', (255, 180, 60)),
            ('R', (235, 80, 60)),
            ('O', (255, 225, 120)),
            ('E', (60, 35, 30)),
        ],
        speech: &[
            "rise and shine",
            "from the ashes, a green build",
            "you found me — lucky!",
        ],
    },
];

// ---- PRNG (Mulberry32, matching Claude Code) ------------------------------

struct Rng {
    state: u32,
}

impl Rng {
    fn new(seed: u32) -> Self {
        Rng { state: seed }
    }

    fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6D2B_79F5);
        let a = self.state;
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        (t ^ (t >> 14)) as f64 / 4_294_967_296.0
    }
}

/// FNV-1a hash of the account identity → a stable 32-bit seed. Platform-stable
/// (unlike `DefaultHasher`), so the same account always hatches the same pet.
fn seed_from(identity: &str) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for b in identity.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Mix in the length so a short identity still seeds a non-trivial value.
    hash ^ (identity.len() as u32).wrapping_mul(0x0100_0193)
}

const RARITIES: [Rarity; 5] = [
    Rarity::Common,
    Rarity::Uncommon,
    Rarity::Rare,
    Rarity::Epic,
    Rarity::Legendary,
];

fn roll_rarity(rng: &mut Rng) -> Rarity {
    let r = rng.next_f64() * 100.0;
    let mut acc = 0.0;
    for rarity in RARITIES {
        acc += rarity.weight();
        if r < acc {
            return rarity;
        }
    }
    Rarity::Legendary
}

/// Account identities that always hatch the legendary companion — the
/// dev/founder. Also forced by the `OPENCLI_BUDDY_DEV` env var, so the dev
/// gets the legendary on any account. (Mirrors how Claude Code hands its
/// internal builds special treatment.)
const FOUNDER_IDS: &[&str] = &[];

/// Roll the companion for an account identity, returning an index into
/// [`PETS`]. The dev/founder always hatches the legendary; everyone else gets a
/// rarity-weighted roll with the lone legendary at its fixed ~1%.
pub fn roll(identity: &str) -> usize {
    if FOUNDER_IDS.contains(&identity)
        || dev_override(std::env::var_os("OPENCLI_BUDDY_DEV").as_deref())
    {
        return legendary_index();
    }
    roll_weighted(identity)
}

/// The pure, env-independent weighted roll. Kept separate from [`roll`] so the
/// distribution is tested deterministically, free of process-global env state.
fn roll_weighted(identity: &str) -> usize {
    let mut rng = Rng::new(seed_from(identity));
    let rarity = roll_rarity(&mut rng);
    let tier: Vec<usize> = (0..PETS.len())
        .filter(|&i| PETS[i].rarity == rarity)
        .collect();
    // Every rarity has at least one species by construction.
    let pick = (rng.next_f64() * tier.len() as f64) as usize;
    tier[pick.min(tier.len() - 1)]
}

fn legendary_index() -> usize {
    PETS.iter()
        .position(|p| p.rarity == Rarity::Legendary)
        .unwrap_or(0)
}

/// Whether the `OPENCLI_BUDDY_DEV` override is set to a truthy value.
fn dev_override(value: Option<&std::ffi::OsStr>) -> bool {
    match value {
        Some(v) => {
            let v = v.to_string_lossy();
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
        None => false,
    }
}

// ---- rendering ------------------------------------------------------------

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
    fn every_sprite_is_well_formed() {
        for pet in PETS {
            assert!(!pet.rows.is_empty(), "{} has no rows", pet.name);
            assert_eq!(pet.rows.len() % 2, 0, "{} needs even rows", pet.name);
            let w = pet.rows[0].chars().count();
            for row in pet.rows {
                assert_eq!(row.chars().count(), w, "{} row {row:?}", pet.name);
            }
            assert!(!pet.speech.is_empty(), "{} has no speech", pet.name);
        }
    }

    #[test]
    fn weights_sum_to_one_hundred_and_every_tier_has_a_pet() {
        let total: f64 = RARITIES.iter().map(|r| r.weight()).sum();
        assert!((total - 100.0).abs() < 1e-9);
        for rarity in RARITIES {
            assert!(
                PETS.iter().any(|p| p.rarity == rarity),
                "no pet for {rarity:?}"
            );
        }
        // Exactly one legendary, the rarest.
        assert_eq!(
            PETS.iter()
                .filter(|p| p.rarity == Rarity::Legendary)
                .count(),
            1
        );
    }

    #[test]
    fn rarity_weights_are_fixed() {
        // The drop rates are hardcoded and locked — changing any of these (or
        // their 100 total) is a deliberate act that must update this test.
        assert_eq!(Rarity::Common.weight(), 60.0);
        assert_eq!(Rarity::Uncommon.weight(), 25.0);
        assert_eq!(Rarity::Rare.weight(), 10.0);
        assert_eq!(Rarity::Epic.weight(), 4.0);
        assert_eq!(Rarity::Legendary.weight(), 1.0);
    }

    #[test]
    fn roll_is_deterministic_for_an_identity() {
        assert_eq!(
            roll_weighted("anthropic-oauth:acct-123"),
            roll_weighted("anthropic-oauth:acct-123")
        );
    }

    #[test]
    fn dev_override_parses_truthy_values() {
        use std::ffi::OsStr;
        assert!(dev_override(Some(OsStr::new("1"))));
        assert!(dev_override(Some(OsStr::new("yes"))));
        assert!(!dev_override(Some(OsStr::new("0"))));
        assert!(!dev_override(Some(OsStr::new("false"))));
        assert!(!dev_override(Some(OsStr::new("  "))));
        assert!(!dev_override(None));
    }

    #[test]
    fn legendary_index_points_at_the_legendary() {
        assert_eq!(PETS[legendary_index()].rarity, Rarity::Legendary);
    }

    #[test]
    fn roll_respects_weighted_rarity_distribution() {
        // Sample the seed space; common should dominate, legendary should be rare.
        let mut counts = [0usize; 5];
        for i in 0..20_000u32 {
            let idx = roll_weighted(&format!("session-{i}"));
            let rarity = PETS[idx].rarity;
            let slot = RARITIES.iter().position(|&r| r == rarity).unwrap();
            counts[slot] += 1;
        }
        // Common (60%) must be the most frequent; legendary (1%) the least.
        let common = counts[0];
        let legendary = counts[4];
        assert!(common > counts[1], "common should dominate: {counts:?}");
        assert!(legendary > 0, "legendary should still appear: {counts:?}");
        assert!(legendary < counts[3], "legendary rarest: {counts:?}");
    }

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

    /// Print each sprite as REAL 24-bit-colour half-block pixel art (the actual
    /// render technique, not text), so the quality can be judged in a truecolor
    /// terminal: `cargo test -p opencli buddy_colored_preview -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn buddy_colored_preview() {
        for pet in PETS {
            println!("\n  {} [{}]", pet.name, pet.rarity.label());
            for pair in pet.rows.chunks(2) {
                let top: Vec<char> = pair[0].chars().collect();
                let bottom: Vec<char> = pair.get(1).copied().unwrap_or("").chars().collect();
                let w = top.len().max(bottom.len());
                let mut line = String::from("  ");
                for x in 0..w {
                    let t = top.get(x).and_then(|&c| pet.color_of(c));
                    let b = bottom.get(x).and_then(|&c| pet.color_of(c));
                    match (t, b) {
                        (Some(t), Some(b)) => line.push_str(&format!(
                            "\x1b[38;2;{};{};{};48;2;{};{};{}m▀\x1b[0m",
                            t.0, t.1, t.2, b.0, b.1, b.2
                        )),
                        (Some(t), None) => {
                            line.push_str(&format!("\x1b[38;2;{};{};{}m▀\x1b[0m", t.0, t.1, t.2))
                        }
                        (None, Some(b)) => {
                            line.push_str(&format!("\x1b[38;2;{};{};{}m▄\x1b[0m", b.0, b.1, b.2))
                        }
                        (None, None) => line.push(' '),
                    }
                }
                println!("{line}");
            }
        }
    }

    /// Print each sprite's silhouette so the shapes can be eyeballed with
    /// `cargo test -p opencli buddy_silhouettes -- --nocapture --ignored`.
    #[test]
    #[ignore]
    fn buddy_silhouettes() {
        for pet in PETS {
            println!("\n=== {} ({:?}) ===", pet.name, pet.rarity);
            for row in pet.rows {
                let line: String = row
                    .chars()
                    .map(|c| {
                        if pet.color_of(c).is_some() {
                            '█'
                        } else {
                            ' '
                        }
                    })
                    .collect();
                println!("{line}");
            }
        }
    }
}

//! The `/buddy` companion — tomte's own deterministic, rarity-weighted pixel
//! pet: a small bit of character that hatches alongside the custodian.
//!
//! The pet is **deterministic from the signed-in AI account**: a hash of the
//! account identity seeds a Mulberry32 PRNG, so an account always hatches the
//! same companion ("stable") and it only re-rolls when the user switches
//! accounts. Because it is derived purely
//! from the account — nothing is persisted — deleting local state can't re-roll
//! it. Rarity is rolled with weighted odds: one legendary at the lowest rate.
//!
//! Rendering lives in [`render`]: a `(pet index, tick)` pair maps
//! deterministically to a list of ratatui lines, so the render cache stays
//! correct and the output is testable.

mod render;
pub use render::{hatch_lines, mini_lines, pet_name, HATCH_MS};

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

// ---- PRNG (Mulberry32) ----------------------------------------------------

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
/// dev/founder. Also forced by the `TOMTE_BUDDY_DEV` env var, so the dev
/// gets the legendary on any account.
const FOUNDER_IDS: &[&str] = &[];

/// Roll the companion for an account identity, returning an index into
/// [`PETS`]. The dev/founder always hatches the legendary; everyone else gets a
/// rarity-weighted roll with the lone legendary at its fixed ~1%.
pub fn roll(identity: &str) -> usize {
    if FOUNDER_IDS.contains(&identity)
        || dev_override(std::env::var_os("TOMTE_BUDDY_DEV").as_deref())
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
    pick_from_tier(&tier, rng.next_f64())
}

/// Pick a [`PETS`] index from `tier` using `r` in [0, 1). Every rarity has at
/// least one species by construction, but fall back to the first pet rather
/// than underflowing `len() - 1` into a panic if a rarity is ever added
/// without one. For any non-empty tier `r in [0, 1)` yields a valid index, so
/// the distribution is unchanged.
fn pick_from_tier(tier: &[usize], r: f64) -> usize {
    let pick = (r * tier.len() as f64) as usize;
    tier.get(pick).copied().unwrap_or(0)
}

fn legendary_index() -> usize {
    PETS.iter()
        .position(|p| p.rarity == Rarity::Legendary)
        .unwrap_or(0)
}

/// Whether the `TOMTE_BUDDY_DEV` override is set to a truthy value.
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

#[cfg(test)]
mod tests;

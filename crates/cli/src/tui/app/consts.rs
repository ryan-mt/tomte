//! Goal/plan command constants, spinner words, and small helpers. Split out of `app`; logic unchanged.

use super::*;

pub const GOAL_START_PREFIX: &str = "[tomte:/goal start]";
pub const GOAL_CONTINUATION_PREFIX: &str = "[tomte:/goal continuation]";
/// Max words allowed in a `/goal` objective. The objective is re-injected into
/// context on every autonomous continuation turn, so an overlong one crowds out
/// the actual work and degrades the model's reasoning ("mất thông minh"). ~100
/// words (≈150 tokens/turn) stays negligible over a long run while still fitting
/// a detailed, multi-step objective; longer detail belongs in a normal message.
pub const GOAL_MAX_WORDS: usize = 100;

/// Char ceiling that backstops [`GOAL_MAX_WORDS`] for scripts without spaces
/// (CJK, Thai, …), where word-counting collapses a whole objective to one
/// "word" and would let the limit be bypassed entirely. Sized so any objective
/// within the word cap (~100 words ≈ ~650 chars) stays under it, so it only
/// bites genuinely overlong non-whitespace-delimited input.
pub const GOAL_MAX_CHARS: usize = 800;

/// Word count used to bound a `/goal` objective (whitespace-separated).
pub fn goal_word_count(objective: &str) -> usize {
    objective.split_whitespace().count()
}

/// Whether a `/goal` objective is too long to re-inject every turn. Bounds by
/// word count (whitespace-delimited languages) AND raw char count, so a CJK/Thai
/// objective with no whitespace — which counts as a single word — is still
/// caught instead of bypassing the limit.
pub fn goal_exceeds_limit(objective: &str) -> bool {
    goal_word_count(objective) > GOAL_MAX_WORDS || objective.chars().count() > GOAL_MAX_CHARS
}
pub const PLAN_APPROVED_PREFIX: &str = "[tomte:/plan approved]";
pub const PLAN_REJECTED_PREFIX: &str = "[tomte:/plan rejected]";
pub const TODO_RECENT_COMPLETED_TTL: Duration = Duration::from_secs(30);
/// Cap on in-memory composer recall history so a multi-day session can't grow
/// it without bound. Oldest entries are dropped past this.
pub const MAX_INPUT_HISTORY: usize = 1000;

pub fn todo_completion_key(todo: &TodoItem) -> String {
    format!("{}\n{}", todo.content, todo.active_form)
}

pub fn format_goal_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs <= 60 {
        format!("{secs}s")
    } else {
        format!("{}m{}", secs / 60, secs % 60)
    }
}

// === Spinner word — tomte's own voice ========================================
//
// Studied from the real Claude Code CLI (reverse-read from its bundled binary):
// it keeps a few hundred whimsical gerunds (`Sgq`: "Reticulating",
// "Flibbertigibbeting", "Razzle-dazzling", "Clauding", "Moonwalking", …), picks
// ONE at random per spinner mount and holds it, prefers a running task's verb
// when there is one, and lets the user override the pool via config.
//
// We match that *scale* (hundreds of words, so a session rarely repeats) but not
// its *vocabulary*: every word here is tomte's own voice — a cozy, diligent
// Nordic house-spirit (a *tomte*) keeping house and tending the homestead:
// sweeping, mending, kneading, foraging, smithing, pottering by the hearth. One
// unmistakable flavour, Claude-scale variety, not a copy.
pub const SPINNER_WORDS: &[&str] = &[
    // Tidying & keeping house
    "Pottering",
    "Puttering",
    "Tidying",
    "Sweeping",
    "Dusting",
    "Scrubbing",
    "Scouring",
    "Wiping",
    "Straightening",
    "Decluttering",
    "Organizing",
    "Sorting",
    "Arranging",
    "Stacking",
    "Shelving",
    "Bundling",
    "Gathering",
    "Stowing",
    "Stashing",
    "Sprucing",
    "Neatening",
    "Squaring",
    "Airing",
    "Plumping",
    "Freshening",
    // Woodcraft & mending
    "Whittling",
    "Carving",
    "Cobbling",
    "Mending",
    "Fettling",
    "Sanding",
    "Planing",
    "Chiseling",
    "Hewing",
    "Joining",
    "Dovetailing",
    "Fastening",
    "Riveting",
    "Gluing",
    "Patching",
    "Splicing",
    "Soldering",
    "Fixing",
    "Repairing",
    "Restoring",
    "Reinforcing",
    "Bracing",
    "Shimming",
    "Notching",
    "Fitting",
    "Truing",
    "Turning",
    "Rasping",
    "Scraping",
    "Beveling",
    "Mortising",
    "Pegging",
    "Doweling",
    "Wedging",
    // Textiles & handiwork
    "Stitching",
    "Sewing",
    "Weaving",
    "Knitting",
    "Crocheting",
    "Carding",
    "Threading",
    "Embroidering",
    "Quilting",
    "Felting",
    "Hemming",
    "Tacking",
    "Plaiting",
    "Braiding",
    "Knotting",
    "Lacing",
    "Darning",
    "Looming",
    "Spinning",
    "Winding",
    "Skeining",
    "Twining",
    "Basting",
    "Spooling",
    "Tatting",
    // Hearth & kitchen
    "Whisking",
    "Kneading",
    "Steeping",
    "Brewing",
    "Simmering",
    "Stewing",
    "Folding",
    "Stirring",
    "Proofing",
    "Buttering",
    "Ladling",
    "Mashing",
    "Grinding",
    "Milling",
    "Pressing",
    "Straining",
    "Bottling",
    "Preserving",
    "Pickling",
    "Toasting",
    "Glazing",
    "Whipping",
    "Churning",
    "Roasting",
    "Baking",
    "Skimming",
    "Frothing",
    "Reducing",
    "Rendering",
    "Sieving",
    "Shelling",
    // Garden & growing
    "Planting",
    "Sowing",
    "Weeding",
    "Pruning",
    "Grafting",
    "Potting",
    "Watering",
    "Mulching",
    "Harvesting",
    "Composting",
    "Digging",
    "Raking",
    "Hoeing",
    "Transplanting",
    "Trellising",
    "Deadheading",
    "Sprigging",
    "Tending",
    "Thinning",
    "Staking",
    "Edging",
    "Tilling",
    "Furrowing",
    "Scything",
    "Layering",
    // Woodland & roaming
    "Foraging",
    "Gleaning",
    "Rummaging",
    "Fossicking",
    "Nestling",
    "Roaming",
    "Rambling",
    "Ambling",
    "Wending",
    "Trundling",
    "Snuffling",
    "Rooting",
    "Scouting",
    "Tracking",
    "Trailing",
    "Pootling",
    "Padding",
    "Prowling",
    "Loping",
    "Sauntering",
    "Plodding",
    "Toddling",
    // By the hearthfire
    "Kindling",
    "Stoking",
    "Banking",
    "Smouldering",
    "Crackling",
    "Warming",
    "Glowing",
    "Flickering",
    "Blazing",
    "Fanning",
    // Quiet thought
    "Pondering",
    "Musing",
    "Wondering",
    "Puzzling",
    "Reckoning",
    "Daydreaming",
    "Woolgathering",
    "Reflecting",
    "Brooding",
    "Dreaming",
    "Scheming",
    "Plotting",
    "Devising",
    "Figuring",
    "Weighing",
    "Sifting",
    "Untangling",
    "Unknotting",
    "Unraveling",
    "Divining",
    "Mulling",
    "Mapping",
    "Charting",
    "Plumbing",
    "Surmising",
    "Tallying",
    "Noting",
    // Forge & metal
    "Hammering",
    "Smithing",
    "Annealing",
    "Quenching",
    "Beating",
    "Bending",
    "Filing",
    "Honing",
    "Whetting",
    "Sharpening",
    "Polishing",
    "Burnishing",
    "Forging",
    "Casting",
    "Welding",
    "Stropping",
    "Swaging",
    // Cozy busyness
    "Humming",
    "Whistling",
    "Bustling",
    "Beavering",
    "Fussing",
    "Tinkering",
    "Faffing",
    "Fiddling",
    "Twiddling",
    "Niggling",
    "Bumbling",
    "Scuttling",
    "Bobbing",
    "Nattering",
    "Ferreting",
    "Toiling",
    "Plying",
    // Truing & fine-tuning
    "Drifting",
    "Settling",
    "Steadying",
    "Smoothing",
    "Rounding",
    "Leveling",
    "Aligning",
    "Balancing",
    "Calibrating",
    "Adjusting",
    "Tuning",
    "Tweaking",
];

/// The turn spinner's animated glyph: a flickering hearthfire rendered with
/// rising block heights, not the usual braille dots. One column wide and of
/// even width so it never shifts the layout, and — like the word drift — driven
/// purely by wall-clock elapsed (see `render_spinner`), so it animates smoothly
/// and never flickers under a heavy event stream. The tomte's fire, tended.
pub const SPINNER_FRAMES: &[&str] = &["▁", "▂", "▄", "▃", "▅", "▇", "█", "▆", "▇", "▅", "▃", "▂"];

/// Calm drift cadence: the spinner word advances to the next one every this many
/// seconds. Long enough that a typical short turn shows a single steady word
/// (Claude Code never drifts at all), yet a long wait gently cycles through the
/// pool so the line never looks frozen. Driven by wall-clock elapsed, so it stays
/// deterministic and flicker-free — the same trick the braille glyph already uses.
pub const SPINNER_WORD_SECS: u64 = 8;

/// Index into the (possibly user-customized) word pool for a turn at `elapsed`,
/// derived purely from the per-turn `seed`, the elapsed time, and the pool
/// length. Pure + deterministic: identical between every draw within a drift
/// window (so it can't flicker under a heavy event stream), it holds for
/// `SPINNER_WORD_SECS`, then steps to the next word.
pub fn spinner_word_index(seed: u32, elapsed: Duration, len: usize) -> usize {
    let len = len.max(1);
    let step = (elapsed.as_secs() / SPINNER_WORD_SECS) as usize;
    (seed as usize).wrapping_add(step) % len
}

/// Resolve the effective spinner word pool from config, mirroring the real
/// Claude Code `spinnerVerbs` logic (reverse-read from its binary): when
/// `exclude_default` is set and the user gave at least one word, show ONLY their
/// words; otherwise append their words to tomte's built-in pool. Resolved once
/// per session (see `App::new`), never per frame.
pub fn resolve_spinner_words(cfg: &Config) -> Vec<String> {
    let default = || SPINNER_WORDS.iter().map(|s| s.to_string());
    match &cfg.spinner_verbs {
        Some(sv) if sv.exclude_default && !sv.verbs.is_empty() => sv.verbs.clone(),
        Some(sv) => default().chain(sv.verbs.iter().cloned()).collect(),
        None => default().collect(),
    }
}

/// A fresh per-turn seed so different turns open on a different word. This is the
/// only randomness — chosen once per turn, never per frame — so the drift stays
/// a pure, stable function of the seed and elapsed time.
pub fn pick_spinner_seed() -> u32 {
    rand::random::<u32>()
}

/// Past-tense companion verbs for a *finished* sub-agent in the fleet view — the
/// settled counterpart to the present-tense [`SPINNER_WORDS`]. Claude Code reads
/// an idle teammate as "Baked · 1m 12s"; tomte keeps its own hearth-and-workshop
/// voice and its own list. A done agent's label must not drift, so each agent
/// gets one stable verb from [`fleet_idle_verb`].
pub const FLEET_IDLE_VERBS: &[&str] = &[
    "Tended", "Mended", "Whittled", "Wove", "Kindled", "Forged", "Gathered", "Tidied", "Polished",
    "Cobbled", "Stewed", "Pottered",
];

/// A settled past-tense verb for a finished sub-agent, chosen deterministically
/// from its id (a hash folded into a seed) so the same agent always reads the
/// same way once done. Reuses [`spinner_word_index`] with zero elapsed, i.e. no
/// drift — the idle analogue of the live spinner's drifting pick.
pub fn fleet_idle_verb(agent_id: &str) -> &'static str {
    let seed = agent_id
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let idx = spinner_word_index(seed, Duration::ZERO, FLEET_IDLE_VERBS.len());
    FLEET_IDLE_VERBS[idx]
}

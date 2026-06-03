//! Goal/plan command constants, spinner words, and small helpers. Split out of `app`; logic unchanged.

use super::*;

pub const GOAL_START_PREFIX: &str = "[opencli:/goal start]";
pub const GOAL_CONTINUATION_PREFIX: &str = "[opencli:/goal continuation]";
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
pub const PLAN_APPROVED_PREFIX: &str = "[opencli:/plan approved]";
pub const PLAN_REJECTED_PREFIX: &str = "[opencli:/plan rejected]";
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

pub const SPINNER_WORDS: &[&str] = &[
    // Cognitive
    "Thinking",
    "Pondering",
    "Cogitating",
    "Mulling",
    "Reasoning",
    "Reflecting",
    "Deliberating",
    "Ruminating",
    "Contemplating",
    "Musing",
    "Considering",
    "Reckoning",
    "Surmising",
    "Inferring",
    "Speculating",
    // Creative
    "Composing",
    "Crafting",
    "Forging",
    "Brewing",
    "Hatching",
    "Cooking",
    "Conjuring",
    "Sketching",
    "Imagining",
    "Drafting",
    "Painting",
    "Concocting",
    "Designing",
    "Improvising",
    "Inventing",
    // Mechanical / process
    "Computing",
    "Processing",
    "Whirring",
    "Calibrating",
    "Tuning",
    "Spinning",
    "Threading",
    "Weaving",
    "Hammering",
    "Tinkering",
    "Welding",
    "Wiring",
    "Polishing",
    "Refining",
    "Sharpening",
    "Aligning",
    "Recalibrating",
    "Hashing",
    "Crunching",
    "Buffing",
    // Search / discovery
    "Probing",
    "Investigating",
    "Surveying",
    "Mapping",
    "Scanning",
    "Exploring",
    "Hunting",
    "Sleuthing",
    "Decoding",
    "Unraveling",
    "Untangling",
    "Decrypting",
    "Foraging",
    "Excavating",
    "Quarrying",
    "Sifting",
    "Tracing",
    "Reading",
    "Parsing",
    "Combing",
    // Action / strategy
    "Plotting",
    "Scheming",
    "Charting",
    "Synthesizing",
    "Distilling",
    "Brainstorming",
    "Wrangling",
    "Marshaling",
    "Orchestrating",
    "Solving",
    "Sculpting",
    "Carving",
    "Molding",
    "Shaping",
    "Architecting",
    "Engineering",
    "Bootstrapping",
    "Stitching",
    "Coaxing",
    "Steering",
    // Whimsical
    "Noodling",
    "Doodling",
    "Stargazing",
    "Daydreaming",
    "Percolating",
    "Bubbling",
    "Fermenting",
    "Marinating",
    "Stewing",
    "Simmering",
    "Frothing",
    "Steeping",
    "Buzzing",
    "Humming",
    "Rumbling",
    "Whisking",
    "Kneading",
    "Folding",
    "Layering",
    "Garnishing",
    // More flavor — the spinner word is picked per turn, so a longer list means
    // less repetition across a session.
    "Conjuring",
    "Summoning",
    "Channeling",
    "Manifesting",
    "Tinkering",
    "Wrangling",
    "Untangling",
    "Noodling",
    "Percolating",
    "Marinating",
    "Simmering",
    "Brewing",
    "Distilling",
    "Fermenting",
    "Crystallizing",
    "Synthesizing",
    "Assembling",
    "Engineering",
    "Architecting",
    "Sculpting",
    "Chiseling",
    "Whittling",
    "Polishing",
    "Buffing",
    "Calibrating",
    "Tuning",
    "Orchestrating",
    "Choreographing",
    "Weaving",
    "Spinning",
    "Knitting",
    "Stitching",
    "Threading",
    "Plotting",
    "Scheming",
    "Devising",
    "Hatching",
    "Concocting",
    "Brainstorming",
    "Daydreaming",
    "Wondering",
    "Speculating",
    "Theorizing",
    "Hypothesizing",
    "Extrapolating",
    "Computing",
    "Crunching",
    "Number-crunching",
    "Processing",
    "Parsing",
    "Compiling",
    "Optimizing",
    "Refactoring",
    "Debugging",
    "Untangling spaghetti",
    "Herding bits",
    "Wrangling tokens",
    "Chasing pointers",
    "Greasing gears",
    "Stoking the furnace",
    "Charging flux",
    "Spooling up",
    "Warming up",
    "Limbering up",
    "Cranking",
    "Whirring",
    "Vibing",
    "Grooving",
    "Riffing",
    "Jamming",
    "Improvising",
    "Freestyling",
    "Doodling",
    "Sketching",
    "Drafting",
    "Outlining",
    "Storyboarding",
];

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn pick_spinner_word() -> String {
    use rand::seq::SliceRandom;
    let mut rng = rand::thread_rng();
    SPINNER_WORDS
        .choose(&mut rng)
        .copied()
        .unwrap_or("Thinking")
        .to_string()
}

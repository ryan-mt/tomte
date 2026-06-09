//! The calm palette — one disciplined source of color for the whole TUI.
//!
//! SOUL.md Pillar 4: "a disciplined, calm palette (achromatic base + one muted
//! accent)". The base carries **no hue** — all character lives in a single muted
//! sage-teal accent plus a small set of *muted* semantic colors for diff and
//! state. This replaces ~70 scattered RGB literals (the "gradient soup" — a dozen
//! near-identical grays, three teals, neon greens/reds) that read as generic
//! AI-tool slop. One accent, used for one or two things, is the single biggest
//! "craft, not slop" signal.
//!
//! Deliberately kept *out* of this calm flattening, because they encode meaning
//! or character rather than chrome: the per-provider auth dots (status line), the
//! context-usage category swatches (`context_view`), the `/buddy` pet sprite,
//! and the warm inline-code ink below (one hue, one purpose: "this is code").

use ratatui::style::Color;

// === Achromatic base (r == g == b) — text, borders, surfaces ===

/// Headers, emphasis, code identifiers — the brightest ink.
pub const TEXT_BRIGHT: Color = Color::Rgb(231, 231, 231);
/// Default body / assistant text.
pub const TEXT: Color = Color::Rgb(202, 202, 202);
/// Hints, metadata, the status line — present but quiet.
pub const TEXT_MUTED: Color = Color::Rgb(148, 148, 148);
/// Overflow notes ("…+N lines"), rules, the faintest markers.
pub const TEXT_FAINT: Color = Color::Rgb(106, 106, 106);

/// Idle borders and horizontal rules.
pub const BORDER: Color = Color::Rgb(62, 62, 62);
/// Focused / busy borders — a step brighter than [`BORDER`].
pub const BORDER_ACTIVE: Color = Color::Rgb(108, 108, 108);

/// Popup / modal background.
pub const SURFACE: Color = Color::Rgb(22, 22, 25);
/// Code-block background.
pub const SURFACE_CODE: Color = Color::Rgb(28, 29, 35);

// === The single accent — a calm, muted sage-teal ===

/// The one accent. Used sparingly: tool names, selected items, links, the prompt.
pub const ACCENT: Color = Color::Rgb(108, 173, 156);
/// Accent-tinted fill for the selected row of a picker/modal.
pub const ACCENT_DEEP: Color = Color::Rgb(46, 74, 68);

// === Semantic state — muted, never neon. Only for diff + status ===

/// Success, completion, added content.
pub const SUCCESS: Color = Color::Rgb(133, 176, 130);
/// Errors, failures, removed content.
pub const DANGER: Color = Color::Rgb(198, 116, 112);
/// Warnings, pending, approval gates.
pub const WARNING: Color = Color::Rgb(201, 167, 108);
/// Informational / in-progress / secondary highlight.
pub const INFO: Color = Color::Rgb(132, 158, 198);
/// A fifth muted hue, distinct from the semantic set — used only as the last
/// of the five per-provider auth status dots (a glanceable legend kept calm
/// but still mutually distinguishable).
pub const VIOLET: Color = Color::Rgb(160, 132, 190);

// === Diff — dark muted beds with legible muted ink ===

pub const DIFF_ADD_BG: Color = Color::Rgb(20, 44, 28);
pub const DIFF_ADD_FG: Color = Color::Rgb(150, 196, 150);
pub const DIFF_DEL_BG: Color = Color::Rgb(50, 24, 26);
pub const DIFF_DEL_FG: Color = Color::Rgb(206, 138, 134);

/// Left-drag text-selection highlight background.
pub const SELECTION_BG: Color = Color::Rgb(44, 56, 80);

// === Documented exception: inline `code` spans in chat prose ===
// A warm amber on its own dark bed — deliberately NOT the accent, so code
// fragments read as a different *material* than links/selection chrome.

/// Inline-code ink.
pub const INLINE_CODE: Color = Color::Rgb(255, 184, 108);
/// Inline-code bed.
pub const INLINE_CODE_BG: Color = Color::Rgb(40, 30, 18);

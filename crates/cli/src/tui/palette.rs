//! The Lantern palette — one disciplined source of color for the whole TUI.
//!
//! SOUL.md Pillar 4, second visual identity ("Lantern"): a cool night-tinted
//! base lit by a single warm lantern-amber accent — the tomte's lamp held up
//! against a dark Nordic night. The base inks carry a faint blue cast (night,
//! not gray); all warmth is reserved for the accent and the warning family,
//! which are deliberate siblings: amber means "the lantern is on this" —
//! attention, activity, a gate. Frost blue marks thought and information,
//! moss green completion, ember red failure.
//!
//! Deliberately kept *out* of this calm flattening, because they encode meaning
//! or character rather than chrome: the per-provider auth dots (status line), the
//! context-usage category swatches (`context_view`), the `/buddy` pet sprite,
//! and the glacier inline-code ink below (one hue, one purpose: "this is code").

use ratatui::style::Color;

// === Night base (cool-tinted inks) — text, borders, surfaces ===

/// Headers, emphasis, code identifiers — the brightest ink.
pub const TEXT_BRIGHT: Color = Color::Rgb(229, 232, 240);
/// Default body / assistant text.
pub const TEXT: Color = Color::Rgb(199, 204, 215);
/// Hints, metadata, the status line — present but quiet.
pub const TEXT_MUTED: Color = Color::Rgb(141, 148, 163);
/// Overflow notes ("…+N lines"), rules, the faintest markers.
pub const TEXT_FAINT: Color = Color::Rgb(99, 105, 121);

/// Idle borders and horizontal rules.
pub const BORDER: Color = Color::Rgb(55, 59, 72);
/// Focused / busy borders — a step brighter than [`BORDER`].
pub const BORDER_ACTIVE: Color = Color::Rgb(102, 110, 128);

/// Popup / modal background.
pub const SURFACE: Color = Color::Rgb(19, 21, 30);
/// Code-block background.
pub const SURFACE_CODE: Color = Color::Rgb(24, 26, 37);

// === The single accent — the lantern's amber ===

/// The one accent. Used sparingly: the prompt, tool activity, selected items,
/// links — wherever the lantern is currently raised.
pub const ACCENT: Color = Color::Rgb(224, 176, 102);
/// Accent-tinted fill for the selected row of a picker/modal.
pub const ACCENT_DEEP: Color = Color::Rgb(72, 56, 31);

// === Semantic state — muted, never neon. Only for diff + status ===

/// Success, completion, added content — moss.
pub const SUCCESS: Color = Color::Rgb(126, 178, 128);
/// Errors, failures, removed content — ember.
pub const DANGER: Color = Color::Rgb(205, 113, 108);
/// Warnings, pending, approval gates — a deeper amber, deliberately a sibling
/// of [`ACCENT`]: a gate is the lantern raised at the door.
pub const WARNING: Color = Color::Rgb(212, 158, 85);
/// Informational / in-progress thought / secondary highlight — frost blue.
pub const INFO: Color = Color::Rgb(124, 154, 206);
/// A fifth muted hue, distinct from the semantic set — used only as the last
/// of the five per-provider auth status dots (a glanceable legend kept calm
/// but still mutually distinguishable).
pub const VIOLET: Color = Color::Rgb(159, 134, 194);

// === Diff — dark night-tinted beds with legible muted ink ===

pub const DIFF_ADD_BG: Color = Color::Rgb(21, 42, 30);
pub const DIFF_ADD_FG: Color = Color::Rgb(146, 196, 152);
pub const DIFF_DEL_BG: Color = Color::Rgb(48, 25, 31);
pub const DIFF_DEL_FG: Color = Color::Rgb(209, 139, 137);

/// Left-drag text-selection highlight background.
pub const SELECTION_BG: Color = Color::Rgb(45, 58, 86);

// === Documented exception: inline `code` spans in chat prose ===
// A cool glacier blue on its own dark bed — deliberately NOT the accent, so
// code fragments read as a different *material* (cold, exact) than the warm
// lantern chrome around them.

/// Inline-code ink.
pub const INLINE_CODE: Color = Color::Rgb(136, 192, 208);
/// Inline-code bed.
pub const INLINE_CODE_BG: Color = Color::Rgb(21, 33, 42);

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Plan,
    Default,
    AcceptEdits,
    BypassPerms,
}

impl PermissionMode {
    pub fn next(self) -> Self {
        match self {
            Self::Plan => Self::Default,
            Self::Default => Self::AcceptEdits,
            Self::AcceptEdits => Self::BypassPerms,
            Self::BypassPerms => Self::Plan,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "⏸ plan mode",
            Self::Default => "▸ default (ask)",
            Self::AcceptEdits => "⏵⏵ accept edits",
            Self::BypassPerms => "⚠ bypass permissions",
        }
    }
    /// Stable string persisted to `config.default_permission_mode`.
    pub fn config_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPerms => "bypassPermissions",
        }
    }
    /// Inverse of `config_str`. Unknown values fall back to `Default` so a
    /// hand-edited or stale config can never wedge startup.
    pub fn from_config_str(s: &str) -> Self {
        match s {
            "plan" => Self::Plan,
            "acceptEdits" => Self::AcceptEdits,
            "bypassPermissions" => Self::BypassPerms,
            _ => Self::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    SlashMenu,
    FilePicker,
    ModelPicker,
    EffortPicker,
    VerbosityPicker,
    ResumePicker,
    RewindPicker,
    LogoutPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Chat,
}

/// How the TUI occupies the terminal.
///
/// - `Inline` (default): an inline viewport (SOUL.md Pillar 4 — the calm, tidy
///   terminal) that leaves finished turns in the terminal's own native
///   scrollback (via `Terminal::insert_before`) and never captures the mouse,
///   so native scrollback + selection/copy keep working.
/// - `AltScreen`: a full-screen alternate-buffer renderer with the input pinned
///   to the bottom edge and in-app scroll + drag-selection — the conventional
///   "transcript on top, prompt at the bottom" layout. Opt in with
///   `render_mode: "alt"` in config.json or `TOMTE_INLINE=0` (or
///   `false`/`no`/`off`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    AltScreen,
    Inline,
}

impl RenderMode {
    /// Resolve from config + environment. Inline (Pillar 4) is the default;
    /// `render_mode: "alt"` in config.json opts back into the full-screen
    /// alternate screen, and the `TOMTE_INLINE` env var overrides both ways
    /// (`1`/`true`/`yes`/`on` forces inline, `0`/`false`/`no`/`off` forces
    /// alt-screen).
    pub fn resolve(config: &config::Config) -> Self {
        Self::resolve_values(
            std::env::var("TOMTE_INLINE").ok().as_deref(),
            &config.render_mode,
        )
    }

    /// Pure resolution of env value + config value, split out so it can be
    /// tested without mutating process-global environment state. An explicit
    /// truthy/falsy env value wins; otherwise the config decides, with inline
    /// as the default for anything unrecognized.
    pub fn resolve_values(env: Option<&str>, config_mode: &str) -> Self {
        match env.map(str::trim) {
            Some("1" | "true" | "yes" | "on") => Self::Inline,
            Some("0" | "false" | "no" | "off") => Self::AltScreen,
            _ => match config_mode.trim().to_ascii_lowercase().as_str() {
                "alt" | "altscreen" | "alt-screen" | "alt_screen" | "fullscreen" => Self::AltScreen,
                _ => Self::Inline,
            },
        }
    }
}

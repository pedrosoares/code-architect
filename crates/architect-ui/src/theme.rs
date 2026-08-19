//! Design tokens and the application theme.
//!
//! The palette is ported from Code Architect v1: warm near-black surfaces with
//! a single amber accent.
//!
//! Two layers live here:
//!
//! * raw [`Color`] / metric tokens, used when styling `rect()` directly;
//! * [`dark`], the [`Theme`] handed to `use_init_theme`, which restyles every
//!   built-in Freya component (buttons, inputs, scrollbars, ...) to match.

use freya::components::{ColorsSheet, InputLayoutThemePreference, Preference, Theme, dark_theme};
use freya::prelude::{Color, CornerRadius, Gaps};

// -- Surfaces ---------------------------------------------------------------

/// Window background — the darkest surface, behind the transcript.
pub const BACKGROUND: Color = Color::from_rgb(13, 11, 10);
/// Panel background: sidebar, header, status bar, composer.
pub const SURFACE: Color = Color::from_rgb(22, 18, 14);
/// Raised elements: message cards, tool rows, chips, session rows.
pub const SURFACE_RAISED: Color = Color::from_rgb(36, 28, 20);
/// Hairlines between panels and around raised elements.
pub const BORDER: Color = Color::from_rgb(51, 41, 29);

// -- Text -------------------------------------------------------------------

/// Primary text.
pub const TEXT: Color = Color::from_rgb(237, 230, 220);
/// Secondary text: timestamps, captions, tool arguments, empty states.
pub const TEXT_DIM: Color = Color::from_rgb(160, 143, 121);

// -- Accents ----------------------------------------------------------------

/// The amber accent: filled buttons, active chips, selection.
pub const ACCENT: Color = Color::from_rgb(235, 182, 90);
/// Text and icons drawn on top of [`ACCENT`].
pub const ON_ACCENT: Color = Color::from_rgb(26, 20, 9);
/// Amber wash behind selected rows.
pub const ACCENT_SOFT: Color = Color::from_rgb(58, 45, 26);
/// Completed tool calls, applied edits, passing checks.
pub const SUCCESS: Color = Color::from_rgb(122, 184, 135);
/// Failed tool calls and surfaced errors.
pub const ERROR: Color = Color::from_rgb(224, 110, 105);
/// Green wash behind an inserted diff line — carries the same signal as
/// [`SUCCESS`] as a background instead of text/icon color, the same
/// relationship [`ACCENT_SOFT`] has to [`ACCENT`].
pub const SUCCESS_SOFT: Color = Color::from_rgb(30, 46, 34);
/// Red wash behind a deleted diff line — the [`ERROR`] equivalent of
/// [`SUCCESS_SOFT`].
pub const ERROR_SOFT: Color = Color::from_rgb(48, 28, 27);

// -- Metrics ----------------------------------------------------------------

/// Gap between related elements inside a group.
pub const SPACE_SM: f32 = 6.0;
/// Standard padding inside panels and rows.
pub const SPACE_MD: f32 = 12.0;
/// Height of the window header.
pub const HEADER_HEIGHT: f32 = 56.0;
/// Height of the status bar.
pub const STATUS_HEIGHT: f32 = 26.0;
/// Diameter of a transcript avatar.
pub const AVATAR_SIZE: f32 = 28.0;
/// Body text size.
pub const FONT_BODY: f32 = 14.0;
/// Caption / metadata text size.
pub const FONT_SMALL: f32 = 12.0;
/// Tool names and arguments.
pub const FONT_MONO: &str = "monospace";

/// The semantic palette every built-in Freya component resolves against.
///
/// Component themes reference these by name — `filled_button` takes its
/// background from `primary` and its text from `text_inverse`, `Input` borders
/// come from `border` / `border_focus`, and so on. Restyling the components is
/// therefore a matter of changing this sheet, not of touching call sites.
const COLORS: ColorsSheet = ColorsSheet {
    // Brand & accent
    primary: ACCENT,
    secondary: Color::from_rgb(245, 214, 160),
    tertiary: Color::from_rgb(198, 148, 66),

    // Status
    success: SUCCESS,
    warning: Color::from_rgb(230, 180, 80),
    error: ERROR,
    info: Color::from_rgb(140, 176, 214),

    // Surfaces
    background: BACKGROUND,
    surface_primary: Color::from_rgb(46, 36, 25),
    surface_secondary: SURFACE_RAISED,
    surface_tertiary: SURFACE,
    surface_inverse: Color::from_rgb(150, 137, 120),
    surface_inverse_secondary: Color::from_rgb(168, 155, 138),
    surface_inverse_tertiary: Color::from_rgb(186, 173, 156),

    // Borders
    border: BORDER,
    border_focus: Color::from_rgb(120, 95, 55),
    border_disabled: Color::from_rgb(60, 50, 38),

    // Text
    text_primary: TEXT,
    text_secondary: Color::from_rgb(200, 186, 168),
    text_placeholder: TEXT_DIM,
    text_inverse: ON_ACCENT,
    text_highlight: ACCENT,

    // States
    focus: Color::from_rgb(96, 78, 48),
    active: Color::from_rgb(64, 50, 32),
    disabled: Color::from_rgb(46, 38, 30),

    // Utility
    overlay: Color::from_af32rgb(0.5, 0, 0, 0),
    shadow: Color::from_af32rgb(0.6, 0, 0, 0),
};

/// The application theme.
///
/// Starts from Freya's [`dark_theme`] so every built-in component has a theme
/// registered, then swaps in [`COLORS`]. Overriding one component specifically
/// goes through `Theme::set` with that component's `*ThemePreference` — the
/// keys are `"button"`, `"filled_button"`, `"input"`, `"scroll_bar"`, and so
/// on. Do that here rather than restyling components at each call site.
pub fn dark() -> Theme {
    let mut theme = dark_theme();
    theme.colors = COLORS;

    // The composer input is a pill, as in v1.
    theme.set(
        "input_layout",
        InputLayoutThemePreference {
            corner_radius: Preference::Specific(CornerRadius::new_all(22.0)),
            inner_margin: Preference::Specific(Gaps::new(10.0, 16.0, 10.0, 16.0)),
        },
    );

    theme
}

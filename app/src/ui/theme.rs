//! Design tokens. The only module allowed to contain color literals
//! (enforced by `tests/no_color_literals.rs`).
//!
//! Values come from `figma-frontend_preview.fig` (1942 nodes decoded; see
//! `openspec/changes/bootstrap-gpui-shell/design.md`). Colors are plain
//! `0xRRGGBB` so this module — and its tests — need no GPUI types; convert at
//! the use site with `gpui::rgb(...)`.

// Backgrounds, darkest to card.
pub const BG_DEEPEST: u32 = 0x0B1016;
pub const BG_BASE: u32 = 0x0C131C;
pub const BG_SURFACE: u32 = 0x111A24;
pub const BG_CARD: u32 = 0x17222E;

pub const BORDER: u32 = 0x273443;

// Table rows (scanner-readability). Odd and even rows differ clearly (luminance ratio 1.21).
// HOVER is an indigo tint: muted text must stay >= 4.5:1 on it, which caps its luminance between
// the two row colors, so it is separated by hue (and a ~1.1 luminance ratio), not by brightness.
pub const TABLE_ROW: u32 = 0x0B1016;
pub const TABLE_STRIPE: u32 = 0x182430;
pub const TABLE_HOVER: u32 = 0x1A1838;

// Text, three levels.
pub const TEXT_PRIMARY: u32 = 0xE5EDF5;
pub const TEXT_SECONDARY: u32 = 0x91A2B4;
pub const TEXT_MUTED: u32 = 0x7A8CA2; // Figma has 0x586B80 (contrast 2.9-3.5:1, hard to read); lightened to >= 4.5:1 on every background

// Accent and status.
pub const ACCENT: u32 = 0x58D3C5;
pub const POSITIVE: u32 = 0x65D8A2;
pub const WARNING: u32 = 0xE9BE67;
pub const NEGATIVE: u32 = 0xF18B91;
pub const INFO: u32 = 0x789BDE;

// Funding rate colours (scanner-layout). A rate's sign is not good or bad (a negative rate is income
// for the long leg), so these are deliberately neither the green/red of POSITIVE/NEGATIVE: amber for
// above zero, cyan for below. Both keep >= 4.5:1 on every background. POSITIVE/NEGATIVE stay for
// Net Edge and profit/loss.
pub const RATE_POSITIVE: u32 = 0xF0A050;
pub const RATE_NEGATIVE: u32 = 0x4FC3F7;

// Risk-settings direction badges (spec design-tokens). Box fills only, text on them is TEXT_PRIMARY;
// deliberately not POSITIVE/NEGATIVE so they are not read as profit or funding sign.
pub const STRICT_HIGH: u32 = 0x1F5A3A;
pub const STRICT_LOW: u32 = 0x7A2D33;

// Font: Plex Mono for numbers/Latin, bundled. Chinese uses the macOS system CJK font (see design.md).
pub const FONT_MONO: &str = "IBM Plex Mono";

pub const FONT_SIZE_BODY: f32 = 11.0;
pub const FONT_SIZES: &[f32] = &[9.0, 10.0, 11.0, 12.0, 13.0, 14.0];
pub const FONT_SIZES_LARGE: &[f32] = &[21.0, 25.0, 28.0];

/// Semantic color role, so call sites say what a value *means* instead of picking a color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Success / profit (Net Edge, PnL).
    Positive,
    /// Danger / loss.
    Negative,
    /// Funding rate above zero: no good-or-bad meaning.
    RatePositive,
    /// Funding rate below zero: no good-or-bad meaning.
    RateNegative,
    Warning,
    Muted,
}

impl Tone {
    pub fn color(self) -> u32 {
        match self {
            Tone::Positive => POSITIVE,
            Tone::Negative => NEGATIVE,
            Tone::RatePositive => RATE_POSITIVE,
            Tone::RateNegative => RATE_NEGATIVE,
            Tone::Warning => WARNING,
            Tone::Muted => TEXT_MUTED,
        }
    }
}

/// Environment banner colours `(background, text)` by tone (live-banners). Live shares the loss
/// red on purpose; the words and the fixed place at the very top tell the two apart.
pub fn banner_colors(tone: crate::ui::env_label::BannerTone) -> (u32, u32) {
    use crate::ui::env_label::BannerTone;
    match tone {
        BannerTone::Live => (NEGATIVE, BG_DEEPEST),
        BannerTone::Demo => (WARNING, BG_DEEPEST),
        BannerTone::Simulation => (TEXT_MUTED, BG_DEEPEST),
    }
}

/// Funding rate color: > 0 rate-positive, < 0 rate-negative, 0 (or unordered, e.g. NaN) neutral.
/// Never the success/danger colours (those are for Net Edge and profit/loss, see [`pnl_tone`]).
pub fn funding_tone<T: PartialOrd + Default>(rate: T) -> Tone {
    let zero = T::default();
    if rate > zero {
        Tone::RatePositive
    } else if rate < zero {
        Tone::RateNegative
    } else {
        Tone::Muted
    }
}

/// Profit/loss color for an amount of money: > 0 success, < 0 danger, 0 (or unordered) neutral.
pub fn pnl_tone<T: PartialOrd + Default>(amount: T) -> Tone {
    let zero = T::default();
    if amount > zero {
        Tone::Positive
    } else if amount < zero {
        Tone::Negative
    } else {
        Tone::Muted
    }
}

/// Trend vs the previous observation: up → ('↑', RatePositive), down → ('↓', RateNegative),
/// equal or unordered → no arrow at all.
pub fn trend_indicator<T: PartialOrd>(current: T, previous: T) -> Option<(char, Tone)> {
    if current > previous {
        Some(('↑', Tone::RatePositive))
    } else if current < previous {
        Some(('↓', Tone::RateNegative))
    } else {
        None
    }
}

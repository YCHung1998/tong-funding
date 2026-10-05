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

// Font: Plex Mono for numbers/Latin, bundled. Chinese uses the macOS system CJK font (see design.md).
pub const FONT_MONO: &str = "IBM Plex Mono";

pub const FONT_SIZE_BODY: f32 = 11.0;
pub const FONT_SIZES: &[f32] = &[9.0, 10.0, 11.0, 12.0, 13.0, 14.0];
pub const FONT_SIZES_LARGE: &[f32] = &[21.0, 25.0, 28.0];

/// Semantic color role, so call sites say what a value *means* instead of picking a color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Positive,
    Negative,
    Warning,
    Muted,
}

impl Tone {
    pub fn color(self) -> u32 {
        match self {
            Tone::Positive => POSITIVE,
            Tone::Negative => NEGATIVE,
            Tone::Warning => WARNING,
            Tone::Muted => TEXT_MUTED,
        }
    }
}

/// Funding rate color: > 0 positive, < 0 negative, 0 (or unordered, e.g. NaN) neutral.
pub fn funding_tone<T: PartialOrd + Default>(rate: T) -> Tone {
    let zero = T::default();
    if rate > zero {
        Tone::Positive
    } else if rate < zero {
        Tone::Negative
    } else {
        Tone::Muted
    }
}

/// Trend vs the previous observation: up → ('↑', Positive), down → ('↓', Negative),
/// equal or unordered → no arrow at all.
pub fn trend_indicator<T: PartialOrd>(current: T, previous: T) -> Option<(char, Tone)> {
    if current > previous {
        Some(('↑', Tone::Positive))
    } else if current < previous {
        Some(('↓', Tone::Negative))
    } else {
        None
    }
}

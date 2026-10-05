//! Bundled fonts (see `tools/build_fonts.py` and `app/assets/fonts/README.md`).
//!
//! IBM Plex Mono is bundled and registered. Chinese is NOT bundled: a bundled Noto Sans TC
//! registered through `add_fonts` was measured to have zero effect as a *fallback*
//! (pixel-identical screenshots with and without it; see openspec/changes/bootstrap-gpui-shell/design.md),
//! so Chinese renders with the macOS system CJK font via normal CoreText cascading.

use std::borrow::Cow;

use gpui_kit::{App, Font, FontFeatures, FontStyle, FontWeight};

use super::theme::FONT_MONO;

const BUNDLED: &[&[u8]] = &[
    include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf"),
    include_bytes!("../../assets/fonts/IBMPlexMono-SemiBold.ttf"),
];

/// Registers the bundled fonts with GPUI. Call once at startup, before opening windows.
pub fn register(cx: &App) {
    let fonts: Vec<Cow<'static, [u8]>> = BUNDLED.iter().map(|b| Cow::Borrowed(*b)).collect();
    cx.text_system()
        .add_fonts(fonts)
        .expect("bundled fonts must load");
}

/// The app's standard font at the given weight (Plex Mono; Chinese cascades to the system font).
pub fn app_font(weight: FontWeight) -> Font {
    Font {
        family: FONT_MONO.into(),
        features: FontFeatures::default(),
        fallbacks: None,
        weight,
        style: FontStyle::Normal,
    }
}

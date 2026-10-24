//! Tests for design-tokens spec (changes/bootstrap-gpui-shell/specs/design-tokens/spec.md).
use super::theme::*;

#[test]
fn colors_match_figma_token_list() {
    // Values extracted from figma-frontend_preview.fig (see design.md).
    assert_eq!(BG_DEEPEST, 0x0B1016);
    assert_eq!(BG_BASE, 0x0C131C);
    assert_eq!(BG_SURFACE, 0x111A24);
    assert_eq!(BG_CARD, 0x17222E);
    assert_eq!(BORDER, 0x273443);
    assert_eq!(TEXT_PRIMARY, 0xE5EDF5);
    assert_eq!(TEXT_SECONDARY, 0x91A2B4);
    assert_eq!(TEXT_MUTED, 0x7A8CA2); // deliberate deviation from Figma 0x586B80 for legibility
    assert_eq!(ACCENT, 0x58D3C5);
    assert_eq!(POSITIVE, 0x65D8A2);
    assert_eq!(WARNING, 0xE9BE67);
    assert_eq!(NEGATIVE, 0xF18B91);
    assert_eq!(INFO, 0x789BDE);
}

#[test]
fn fonts_and_sizes_match_spec() {
    assert_eq!(FONT_MONO, "IBM Plex Mono");
    assert_eq!(FONT_SIZE_BODY, 11.0);
    assert_eq!(FONT_SIZES, &[9.0, 10.0, 11.0, 12.0, 13.0, 14.0]);
    assert_eq!(FONT_SIZES_LARGE, &[21.0, 25.0, 28.0]);
}

#[test]
fn funding_rate_tone_positive_negative_zero() {
    assert_eq!(funding_tone(0.01_f64), Tone::RatePositive);
    assert_eq!(funding_tone(-0.005_f64), Tone::RateNegative);
    assert_eq!(funding_tone(0.0_f64), Tone::Muted);
}

#[test]
fn trend_up_shows_up_arrow_positive() {
    assert_eq!(trend_indicator(0.02_f64, 0.01_f64), Some(('↑', Tone::RatePositive)));
}

#[test]
fn trend_down_shows_down_arrow_negative() {
    assert_eq!(trend_indicator(0.01_f64, 0.02_f64), Some(('↓', Tone::RateNegative)));
}

#[test]
fn trend_flat_shows_nothing() {
    assert_eq!(trend_indicator(0.01_f64, 0.01_f64), None);
}

#[test]
fn nan_is_treated_as_neutral_not_a_trend() {
    assert_eq!(funding_tone(f64::NAN), Tone::Muted);
    assert_eq!(trend_indicator(f64::NAN, 0.01_f64), None);
}

// ---- scanner-readability: table stripe colors (spec design-tokens) ----

fn lin(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
fn luminance(x: u32) -> f64 {
    0.2126 * lin((x >> 16) as u8) + 0.7152 * lin((x >> 8) as u8) + 0.0722 * lin(x as u8)
}
fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[test]
fn table_stripe_colors_are_the_decided_values() {
    assert_eq!(TABLE_ROW, 0x0B1016);
    assert_eq!(TABLE_STRIPE, 0x182430);
}

#[test]
fn table_row_and_stripe_are_distinguishable() {
    assert!(contrast(TABLE_ROW, TABLE_STRIPE) >= 1.2, "{}", contrast(TABLE_ROW, TABLE_STRIPE));
    assert!(contrast(TABLE_HOVER, TABLE_ROW) >= 1.1, "{}", contrast(TABLE_HOVER, TABLE_ROW));
    assert!(contrast(TABLE_HOVER, TABLE_STRIPE) >= 1.05, "{}", contrast(TABLE_HOVER, TABLE_STRIPE));
    assert!(TABLE_HOVER != TABLE_ROW && TABLE_HOVER != TABLE_STRIPE);
}

#[gpui_kit::test]
fn applied_component_theme_reaches_the_tokens_gpui_component_draws(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::theme::Theme;
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
        let tokens = &Theme::global(cx).tokens;
        let hsla = |x: u32| -> gpui_kit::Hsla { gpui_kit::rgb(x).into() };
        assert_eq!(tokens.table.color, hsla(TABLE_ROW));
        assert_eq!(tokens.table_even.color, hsla(TABLE_STRIPE));
        assert_eq!(tokens.table_hover.color, hsla(TABLE_HOVER));
    });
}

#[test]
fn text_has_at_least_4_5_contrast_on_every_table_background() {
    for bg in [TABLE_ROW, TABLE_STRIPE, TABLE_HOVER] {
        for fg in [TEXT_PRIMARY, TEXT_SECONDARY, TEXT_MUTED] {
            assert!(contrast(fg, bg) >= 4.5, "{fg:06X} on {bg:06X} = {}", contrast(fg, bg));
        }
    }
}

// ---- live-banners: environment banner colours ----

#[test]
fn banner_colours_are_distinct_legible_and_live_shares_the_loss_red() {
    use super::env_label::BannerTone;
    let (live, demo, sim) = (banner_colors(BannerTone::Live), banner_colors(BannerTone::Demo), banner_colors(BannerTone::Simulation));
    assert_eq!(live.0, NEGATIVE, "Live uses the red of a loss (open question in design.md: same red, told apart by text and place)");
    assert_eq!(demo.0, WARNING);
    assert!(live.0 != demo.0 && demo.0 != sim.0 && live.0 != sim.0);
    for (bg, fg) in [live, demo, sim] {
        assert!(contrast(fg, bg) >= 4.5, "{fg:06X} on {bg:06X} = {}", contrast(fg, bg));
    }
}

// ---- risk-settings-guidance: strictness badge colours ----

#[test]
fn strictness_badges_are_legible_and_distinguishable() {
    for bg in [STRICT_HIGH, STRICT_LOW] {
        assert!(contrast(TEXT_PRIMARY, bg) >= 4.5, "{bg:06X} = {}", contrast(TEXT_PRIMARY, bg));
    }
    assert_ne!(STRICT_HIGH, STRICT_LOW);
    let (hr, hg) = ((STRICT_HIGH >> 16) & 0xFF, (STRICT_HIGH >> 8) & 0xFF);
    let (lr, lg) = ((STRICT_LOW >> 16) & 0xFF, (STRICT_LOW >> 8) & 0xFF);
    assert!(hg > hr && lr > lg, "green-ish vs red-ish");
}

// ---- scanner-layout: funding rate colours carry no good/bad meaning ----

/// Hue in degrees (0 = red, 120 = green, 240 = blue).
fn hue(x: u32) -> f64 {
    let (r, g, b) = (((x >> 16) & 0xFF) as f64 / 255.0, ((x >> 8) & 0xFF) as f64 / 255.0, (x & 0xFF) as f64 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    if d == 0.0 {
        return 0.0;
    }
    let h = if max == r { ((g - b) / d).rem_euclid(6.0) } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    h * 60.0
}

/// Distance between two hues on the colour wheel, in degrees (0..=180).
fn hue_gap(a: u32, b: u32) -> f64 {
    let d = (hue(a) - hue(b)).abs();
    d.min(360.0 - d)
}

#[test]
fn a_rate_colour_is_never_mistaken_for_the_warning_colour() {
    // ui-clarity red-team E6: amber for a rate above zero sat next to the WARNING amber (hue gap ~10 degrees)
    for c in [RATE_POSITIVE, RATE_NEGATIVE] {
        assert!(hue_gap(c, WARNING) >= 40.0, "{c:06X} is only {} degrees from WARNING", hue_gap(c, WARNING));
    }
}

#[test]
fn rate_colours_are_the_decided_values() {
    assert_eq!(RATE_POSITIVE, 0xDE8FE6); // orchid, not amber: amber reads as the WARNING colour
    assert_eq!(RATE_NEGATIVE, 0x4FC3F7); // cyan
}

#[test]
fn rate_colours_are_neither_green_nor_red_and_differ_from_success_and_danger() {
    for c in [RATE_POSITIVE, RATE_NEGATIVE] {
        assert!(c != POSITIVE && c != NEGATIVE, "{c:06X} must not be the success or danger colour");
        let h = hue(c);
        assert!((190.0..=330.0).contains(&h), "hue {h} of {c:06X} is in a green, red or amber range");
        for good_bad in [POSITIVE, NEGATIVE] {
            assert!(hue_gap(c, good_bad) >= 40.0, "{c:06X} is too close in hue to {good_bad:06X}");
        }
    }
    assert!(hue_gap(RATE_POSITIVE, RATE_NEGATIVE) > 90.0, "the two rate colours must be easy to tell apart");
}

#[test]
fn rate_colours_have_at_least_4_5_contrast_on_every_table_background() {
    for bg in [BG_DEEPEST, BG_BASE, BG_SURFACE, BG_CARD, TABLE_ROW, TABLE_STRIPE, TABLE_HOVER] {
        for fg in [RATE_POSITIVE, RATE_NEGATIVE] {
            assert!(contrast(fg, bg) >= 4.5, "{fg:06X} on {bg:06X} = {}", contrast(fg, bg));
        }
    }
}

#[test]
fn rate_tones_use_the_rate_colours_and_never_the_success_or_danger_colours() {
    assert_eq!(funding_tone(0.01_f64).color(), RATE_POSITIVE);
    assert_eq!(funding_tone(-0.005_f64).color(), RATE_NEGATIVE);
    assert_eq!(funding_tone(0.0_f64).color(), TEXT_MUTED);
    assert_eq!(trend_indicator(0.02_f64, 0.01_f64).map(|t| t.1.color()), Some(RATE_POSITIVE));
    assert_eq!(trend_indicator(0.01_f64, 0.02_f64).map(|t| t.1.color()), Some(RATE_NEGATIVE));
    for t in [funding_tone(0.01_f64), funding_tone(-0.01_f64)] {
        assert!(t.color() != POSITIVE && t.color() != NEGATIVE);
    }
}

#[test]
fn profit_and_loss_keep_the_success_and_danger_colours() {
    assert_eq!(pnl_tone(1.5_f64), Tone::Positive);
    assert_eq!(pnl_tone(-1.5_f64), Tone::Negative);
    assert_eq!(pnl_tone(0.0_f64), Tone::Muted);
    assert_eq!(Tone::Positive.color(), POSITIVE);
    assert_eq!(Tone::Negative.color(), NEGATIVE);
}

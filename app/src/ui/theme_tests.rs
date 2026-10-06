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
    assert_eq!(funding_tone(0.01_f64), Tone::Positive);
    assert_eq!(funding_tone(-0.005_f64), Tone::Negative);
    assert_eq!(funding_tone(0.0_f64), Tone::Muted);
}

#[test]
fn trend_up_shows_up_arrow_positive() {
    assert_eq!(trend_indicator(0.02_f64, 0.01_f64), Some(('↑', Tone::Positive)));
}

#[test]
fn trend_down_shows_down_arrow_negative() {
    assert_eq!(trend_indicator(0.01_f64, 0.02_f64), Some(('↓', Tone::Negative)));
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

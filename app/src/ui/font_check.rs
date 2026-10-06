//! Developer view for task 2.4: renders sample strings in every bundled weight so a
//! screenshot can show that nothing falls back to missing-glyph boxes.

use gpui_kit::*;

use super::fonts::app_font;
use super::theme;
use super::units::{fs, rx};

pub struct FontCheck;

fn row(label: &str, font: Font, text: &str) -> Div {
    div()
        .flex()
        .gap_4()
        .child(
            div()
                .w(rx(230.0))
                .text_color(rgb(theme::TEXT_SECONDARY))
                .child(label.to_string()),
        )
        .child(div().font(font).text_size(fs(14.0)).child(text.to_string()))
}

impl Render for FontCheck {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let sample = "持倉 Unified Positions 60,200.00 USDT";
        div()
            .size_full()
            .bg(rgb(theme::BG_DEEPEST))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(fs(theme::FONT_SIZE_BODY))
            .font(app_font(FontWeight::NORMAL))
            .child("font check: bundled Plex Mono; Chinese via system font")
            .child(row("Regular (400)", app_font(FontWeight::NORMAL), sample))
            .child(row("Medium (500)", app_font(FontWeight::MEDIUM), sample))
            .child(row("SemiBold (600)", app_font(FontWeight::SEMIBOLD), sample))
            .child(row("總覽 掃幣 合約設定 交易單", app_font(FontWeight::NORMAL), "總覽 掃幣 合約設定 交易單 持倉 風控設定 系統日誌 手動下單"))
            .child(row("Medium Chinese", app_font(FontWeight::MEDIUM), "風控設定 · Risk Management 全域限制"))
            .child(row("symbols (system fallback)", app_font(FontWeight::NORMAL), "◈ ● ○ ☑ □ ▾ ⚠ ✓ ✕ ↑ ↓ 🟢 🔄"))
            .child(row("digits 0O 1lI", app_font(FontWeight::NORMAL), "0123456789 +0.010% -0.005% 0O 1lI"))
    }
}

//! Maps our design tokens onto gpui-component's theme so its widgets (tables, lists, popups)
//! render dark with our colors. Without this the library's default light theme is used and
//! our near-white text is unreadable on its white rows (seen in the table spike).
//! No color literals here: everything comes from `theme`.

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::*;

use super::theme as t;

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let h = |x: u32| -> Hsla { rgb(x).into() };
    // `Theme::update` (not `global_mut`) so the edit also reaches `tokens`, which is what
    // gpui-component actually paints with; `global_mut` left the library's default row colors.
    Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.background = h(t::BG_DEEPEST);
        c.foreground = h(t::TEXT_PRIMARY);
        c.border = h(t::BORDER);
        c.muted = h(t::BG_CARD);
        c.muted_foreground = h(t::TEXT_MUTED);
        c.popover = h(t::BG_CARD);
        c.popover_foreground = h(t::TEXT_PRIMARY);
        c.list = h(t::BG_DEEPEST);
        c.list_even = h(t::BG_BASE);
        c.list_head = h(t::BG_SURFACE);
        c.list_hover = h(t::BG_CARD);
        c.list_active = h(t::BG_CARD);
        c.list_active_border = h(t::ACCENT);
        c.table = h(t::TABLE_ROW);
        c.table_even = h(t::TABLE_STRIPE);
        c.table_head = h(t::BG_SURFACE);
        c.table_head_foreground = h(t::TEXT_SECONDARY);
        c.table_foot = h(t::BG_SURFACE);
        c.table_foot_foreground = h(t::TEXT_SECONDARY);
        c.table_hover = h(t::TABLE_HOVER);
        c.table_active = h(t::BG_CARD);
        c.table_active_border = h(t::ACCENT);
        c.table_row_border = h(t::BORDER);
    });
}

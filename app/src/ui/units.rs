//! Zoom-aware lengths (change: ui-font-zoom). `px` ignores the window rem size, `rems` follows it,
//! and gpui-component sets the rem size from `Theme.font_size` (16 px at 100%). So a design size
//! written in px at 100% becomes `rems(n / 16)`: identical at 100%, scaled by the zoom otherwise.

use gpui_kit::{Rems, rems};

use super::zoom::BASE_REM_PX;

/// rem count that equals `design_px` pixels at 100% zoom.
pub fn rem_count(design_px: f32) -> f32 {
    design_px / BASE_REM_PX
}

/// Font size given in design px.
pub fn fs(design_px: f32) -> Rems {
    rems(rem_count(design_px))
}

/// Fixed width / height given in design px.
pub fn rx(design_px: f32) -> Rems {
    rems(rem_count(design_px))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme;

    #[test]
    fn at_100_percent_design_px_is_unchanged() {
        // 16 px per rem at 100%: every size used by the app converts back exactly.
        for px in theme::FONT_SIZES.iter().chain(theme::FONT_SIZES_LARGE).chain(&[44.0, 28.0, 200.0, 160.0, 110.0, 560.0, 260.0]) {
            assert_eq!(rem_count(*px) * BASE_REM_PX, *px);
        }
    }

    #[test]
    fn scales_with_the_rem_size() {
        // body 11 px at 110% zoom (rem = 17.6 px) is 12.1 px.
        assert!((fs(11.0).0 * 17.6 - 12.1).abs() < 1e-4);
    }
}

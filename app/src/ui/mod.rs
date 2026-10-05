// Shell under construction (change: bootstrap-gpui-shell); items get used as pages land.
#![allow(dead_code)]

pub mod bench;
pub mod clock;
pub mod component_theme;
pub mod font_check;
pub mod frame_stats;
pub mod fonts;
pub mod nav;
pub mod shell;
pub mod status;
pub mod theme;

#[cfg(test)]
mod shell_tests;
#[cfg(test)]
mod theme_tests;

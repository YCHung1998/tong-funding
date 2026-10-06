// Shell under construction (change: bootstrap-gpui-shell); items get used as pages land.
#![allow(dead_code)]

pub mod bench;
pub mod clock;
pub mod live;
pub mod component_theme;
pub mod font_check;
pub mod frame_stats;
pub mod fonts;
pub mod nav;
pub mod pages;
pub mod shell;
pub mod status;
pub mod theme;
pub mod trading_pages;
pub mod symbol_select;
pub mod units;
pub mod zoom_ui;
pub mod wiring;

// Page view-models (ui-readonly-pages, design D1): pure, no GPUI types. Files live in `vm/`,
// module paths stay `ui::<page>` so `cargo test -p tong-funding ui::<page>` selects them.
#[path = "vm/alerts.rs"]
pub mod alerts;
#[path = "vm/banner.rs"]
pub mod banner;
#[path = "vm/bridge.rs"]
pub mod bridge;
#[path = "vm/candidates.rs"]
pub mod candidates;
#[path = "vm/contract_settings.rs"]
pub mod contract_settings;
#[path = "vm/manual_order.rs"]
pub mod manual_order;
#[path = "vm/symbol_options.rs"]
pub mod symbol_options;
#[path = "vm/risk_settings.rs"]
pub mod risk_settings;
#[path = "vm/staged_orders.rs"]
pub mod staged_orders;
#[path = "vm/dashboard.rs"]
pub mod dashboard;
#[path = "vm/engine_view.rs"]
pub mod engine_view;
#[path = "vm/format.rs"]
pub mod format;
#[path = "vm/funding.rs"]
pub mod funding;
#[path = "vm/positions.rs"]
pub mod positions;
#[path = "vm/scan_view.rs"]
pub mod scan_view;
#[path = "vm/scanner.rs"]
pub mod scanner;
#[path = "vm/scanner_refresh.rs"]
pub mod scanner_refresh;
#[path = "vm/system_log.rs"]
pub mod system_log;
#[path = "vm/zoom.rs"]
pub mod zoom;
#[cfg(test)]
#[path = "vm/testkit.rs"]
mod testkit;

#[cfg(test)]
mod risk_help_ui_tests;
#[cfg(test)]
mod scan_table_ui_tests;
#[cfg(test)]
mod shell_tests;
#[cfg(test)]
mod theme_tests;

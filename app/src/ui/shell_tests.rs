//! Tests for app-shell spec (changes/bootstrap-gpui-shell/specs/app-shell/spec.md).
use super::clock::read_clock;
use super::nav::{Page, DEBUG_WARNING};
use super::status::*;

// ---- sidebar navigation -------------------------------------------------

#[test]
fn nav_has_eight_pages_in_fixed_order() {
    let zh: Vec<_> = Page::ALL.iter().map(|p| p.zh()).collect();
    assert_eq!(zh, ["總覽", "掃幣", "合約設定", "交易單", "持倉", "風控設定", "系統日誌", "手動下單"]);
}

#[test]
fn nav_items_carry_english_subtitles() {
    let en: Vec<_> = Page::ALL.iter().map(|p| p.en()).collect();
    assert_eq!(
        en,
        ["Dashboard", "Full-Market Scanner", "Contract Settings", "Staged Orders",
         "Unified Positions", "Risk Management", "System Logs", "Debug Tool"]
    );
}

#[test]
fn only_manual_order_is_debug_and_it_is_last() {
    let debug: Vec<_> = Page::ALL.iter().filter(|p| p.is_debug()).collect();
    assert_eq!(debug, [&Page::ManualOrder]);
    assert_eq!(*Page::ALL.last().unwrap(), Page::ManualOrder);
}

#[test]
fn debug_warning_text_marks_non_standard_flow() {
    assert_eq!(DEBUG_WARNING, "除錯工具，非標準流程");
}

#[test]
fn app_starts_on_overview() {
    assert_eq!(Page::default_page(), Page::Overview);
}

// ---- dual clock ---------------------------------------------------------

#[test]
fn clock_shows_utc_and_taipei() {
    // Expected values computed independently with Python's datetime.
    let c = read_clock(1791187200);
    assert_eq!((c.utc_date.as_str(), c.utc_time.as_str()), ("2026-10-05", "08:00:00"));
    assert_eq!((c.taipei_date.as_str(), c.taipei_time.as_str()), ("2026-10-05", "16:00:00"));
}

#[test]
fn clock_taipei_rolls_over_to_next_day() {
    let c = read_clock(1791230400); // 2026-10-05 20:00:00 UTC
    assert_eq!((c.utc_date.as_str(), c.utc_time.as_str()), ("2026-10-05", "20:00:00"));
    assert_eq!((c.taipei_date.as_str(), c.taipei_time.as_str()), ("2026-10-06", "04:00:00"));
}

#[test]
fn clock_advances_one_second_per_second() {
    let a = read_clock(1791187200);
    let b = read_clock(1791187201);
    let d = read_clock(1791187203);
    assert_eq!(a.utc_time, "08:00:00");
    assert_eq!(b.utc_time, "08:00:01");
    assert_eq!(b.taipei_time, "16:00:01");
    assert_eq!(d.utc_time, "08:00:03");
    assert_eq!(d.taipei_time, "16:00:03");
}

#[test]
fn clock_taipei_is_always_utc_plus_8_across_midnight() {
    let c = read_clock(1791158399); // 2026-10-04 23:59:59 UTC
    assert_eq!((c.utc_date.as_str(), c.utc_time.as_str()), ("2026-10-04", "23:59:59"));
    assert_eq!((c.taipei_date.as_str(), c.taipei_time.as_str()), ("2026-10-05", "07:59:59"));
}

// ---- status bar ---------------------------------------------------------

#[test]
fn first_launch_default_mode_is_simulation() {
    let s = StatusModel::default();
    assert_eq!(s.mode, ExecutionMode::Simulation);
    assert_eq!(s.mode.label(), "SIMULATION");
}

#[test]
fn all_three_exchanges_start_disconnected() {
    let s = StatusModel::default();
    let names: Vec<_> = s.exchanges.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["Binance", "Bybit", "OKX"]);
    assert!(s.exchanges.iter().all(|(_, c)| *c == Connection::Disconnected));
    assert_eq!(Connection::Disconnected.label(), "未連線");
}

#[test]
fn kill_switch_slot_shows_not_enabled() {
    let s = StatusModel::default();
    assert_eq!(s.kill_switch, KillSwitch::Disabled);
    assert_eq!(s.kill_switch.label(), "未啟用");
}

#[test]
fn environment_label_is_demo_testnet_and_no_live_wording_anywhere() {
    assert!(ENVIRONMENT_LABEL.contains("Demo / Testnet"));
    let strings = all_ui_strings();
    assert!(!strings.is_empty(), "string collector returned nothing");
    for s in &strings {
        assert!(!s.to_uppercase().contains("LIVE"), "found LIVE wording in {s:?}");
    }
}

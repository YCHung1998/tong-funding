//! naked-leg-watchdog: the fixed "自動處理規則（授權 B）" block of the risk page, through the real
//! `Shell` in a headless window. The words are tested on the view-model (`risk_settings`); this
//! proves the block is drawn on the page, whatever the settings look like, and cannot be collapsed.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, base::Root, point, px, size};

use super::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, Settings, SourceUpdate};
use super::nav::Page;
use super::shell::Shell;
use super::testkit::complete_settings;
use crate::store::event_query::{EventPage, EventQuery};

struct FakeSource(Mutex<Vec<SourceUpdate>>);

impl ReadOnlyDataSource for FakeSource {
    fn drain_updates(&self) -> Vec<SourceUpdate> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
    fn request_refresh(&self) -> RefreshRequest {
        RefreshRequest::IgnoredInProgress
    }
    fn refresh_in_progress(&self) -> bool {
        false
    }
    fn load_events(&self, _: &EventQuery) -> Result<EventPage, String> {
        Err("no store in this test".into())
    }
}

struct NullSink;
impl CommandSink for NullSink {
    fn send(&self, _: String, _: crate::engine::command::Command) {}
}

fn settle(cx: &mut TestAppContext) {
    for _ in 0..4 {
        cx.executor().advance_clock(Duration::from_millis(120));
        cx.run_until_parked();
    }
}

/// The shell on `page` with `updates` delivered by the data source.
fn open_page(cx: &mut TestAppContext, page: Page, updates: Vec<SourceUpdate>) -> gpui_kit::WindowHandle<Root> {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(updates)));
    let sink: Arc<dyn CommandSink> = Arc::new(NullSink);
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(1000.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(source, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    settle(cx);
    cx.update_window(window.into(), |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.go(page);
            cx.notify();
        });
        w.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update_window(window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
    window
}

/// The shell on the risk page with `settings` delivered by the data source.
fn risk_page(cx: &mut TestAppContext, settings: Settings) -> gpui_kit::WindowHandle<Root> {
    open_page(cx, Page::RiskSettings, vec![SourceUpdate::Settings(settings)])
}

#[gpui_kit::test]
fn the_rules_block_is_drawn_on_the_risk_page_with_complete_and_with_default_settings(cx: &mut TestAppContext) {
    for (what, settings) in [("complete", complete_settings("0.01")), ("default (incomplete)", Settings::default())] {
        let window = risk_page(cx, settings);
        let snap = cx.update_window(window.into(), |_, w, _| w.try_find("naked-leg-rules")).unwrap();
        let snap = snap.unwrap_or_else(|| panic!("{what}: the rules block is missing from the risk page"));
        assert!(snap.visible(), "{what}: {snap:?}");
        let b = snap.bounds();
        assert!(b.size.width > px(100.) && b.size.height > px(20.), "{what}: the block has a real size: {b:?}");
        assert_eq!(snap.expanded(), None, "{what}: it is not an expandable / collapsible control");
    }
}

#[gpui_kit::test]
fn clicking_the_rules_block_does_not_collapse_or_hide_it(cx: &mut TestAppContext) {
    let window = risk_page(cx, complete_settings("0.01"));
    let before = cx.update_window(window.into(), |_, w, _| w.find("naked-leg-rules").bounds()).unwrap();
    for _ in 0..2 {
        cx.update_window(window.into(), |_, w, cx| {
            w.click("naked-leg-rules", cx);
        })
        .unwrap();
        settle(cx);
        cx.update_window(window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
    }
    let after = cx.update_window(window.into(), |_, w, _| w.try_find("naked-leg-rules").map(|s| (s.visible(), s.bounds()))).unwrap();
    assert_eq!(after, Some((true, before)), "same place, same size, still there");
}

#[gpui_kit::test]
fn the_manual_handling_card_draws_the_watchdog_status_of_a_pair_in_an_alert_state(cx: &mut TestAppContext) {
    use crate::engine::command::PairView;
    use crate::store::event_query::StoredEvent;
    use crate::ui::bridge::EngineState;
    use tong_funding_core::pair::PairState;
    use tong_funding_core::risk::{ExecutionMode, TriggerMode};
    use tong_funding_core::types::Exchange;
    let pair = PairView {
        internal_uuid: "u1".into(),
        pair_id: "pid-u1".into(),
        symbol: "BTCUSDT".into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        state: PairState::PartialFailure,
        settlement_ms: 1_800_000_000_000,
        simulated: true,
        flat_confirmed: false,
    };
    let engine = EngineState { now_ms: 1_800_000_020_000, trigger_mode: TriggerMode::Manual, execution_mode: ExecutionMode::Simulation, pairs: vec![pair], blockers: vec![], notices: vec![], alerts: vec![] };
    let ev = |id: i64, ts: i64, ty: &str, payload: serde_json::Value| StoredEvent { id, ts_ms: ts, event_type: ty.into(), pair_id: Some("u1".into()), payload: payload.to_string(), imported: false };
    let events = vec![
        ev(3, 1_800_000_014_000, "NAKED_LEG_WATCHDOG", serde_json::json!({"kind": "ATTEMPT_RESULT", "ok": true, "attempt": 1, "leg": "long", "quantity": "10"})),
        ev(2, 1_800_000_015_000, "NAKED_LEG_WATCHDOG", serde_json::json!({"kind": "ROUND", "conclusion": "FLATTEN", "leg": "long", "quantity": "10", "attempt": 1})),
        ev(1, 1_800_000_005_000, "PAIR_TRANSITION", serde_json::json!({"from": "FILL_MONITOR", "to": "PARTIAL_FAILURE"})),
    ];
    let window = open_page(cx, Page::StagedOrders, vec![SourceUpdate::Settings(complete_settings("0.01")), SourceUpdate::Engine(engine), SourceUpdate::TradeEvents(events)]);
    let lines = cx.update_window(window.into(), |_, w, _| w.try_find("mwatchdog-u1").map(|s| (s.visible(), s.bounds()))).unwrap();
    let (visible, bounds) = lines.expect("the watchdog's lines are drawn on the card of the pair in an alert state");
    assert!(visible && bounds.size.height > px(8.), "{bounds:?}");
}

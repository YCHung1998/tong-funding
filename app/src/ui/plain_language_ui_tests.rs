//! ui-plain-language through the real `Shell` in a headless window: an exchange the risk settings
//! left unticked is ONE muted line on the overview, the staged orders, the positions and the contract
//! settings pages, and a click opens and closes its detail. The words themselves are decided and
//! tested in the view-models (`labels`, `dashboard`, `staged_orders`, `positions`,
//! `contract_settings`); a headless window cannot read drawn text, so these tests prove that the line
//! is on the page, that it can be reached by its id and that clicking it flips the open state.
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, base::Root, point, px, size};
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange::{Binance, Bybit, Okx};

use super::bridge::{AccountState, CommandSink, EngineState, LegAccount, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use super::nav::Page;
use super::shell::Shell;
use super::testkit::{asset, complete_settings, loaded};
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

const OKX_SPOT: &str = "exchange error acct_mode_unsupported: 帳戶模式不支援 (acctLv 1, posMode net_mode)";

/// OKX unticked in the risk settings while its account read keeps failing with the spot-mode error.
fn okx_unticked_and_failing() -> Vec<SourceUpdate> {
    let mut settings = complete_settings("0.01");
    settings.risk.allowed_exchanges = vec![Binance, Bybit];
    let now = super::shell::wall_ms();
    let mut v = vec![SourceUpdate::Settings(settings)];
    for e in [Binance, Bybit] {
        v.push(SourceUpdate::Account { exchange: e, state: loaded(vec![asset("USDT", "1000", None, None)], vec![], now) });
    }
    v.push(SourceUpdate::Account { exchange: Okx, state: AccountState::Failed { error: OKX_SPOT.into(), at: now } });
    for e in [Binance, Bybit, Okx] {
        v.push(SourceUpdate::LegAccount {
            simulated: false,
            exchange: e,
            account: LegAccount { positions: Err("x".into()), open_orders: Err("x".into()), available_margin: Err(if e == Okx { format!("OKX: {OKX_SPOT}") } else { "尚未查詢".into() }), fetched_at: now },
        });
    }
    v.push(SourceUpdate::Engine(EngineState { now_ms: now, trigger_mode: TriggerMode::Manual, execution_mode: ExecutionMode::Simulation, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] }));
    v
}

struct Rig {
    window: WindowHandle<Root>,
    shell: Entity<Shell>,
}

fn open(cx: &mut TestAppContext, updates: Vec<SourceUpdate>) -> Rig {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(updates)));
    let sink: Arc<dyn CommandSink> = Arc::new(NullSink);
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(1600.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(source, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    let rig = Rig { window, shell };
    rig.settle(cx);
    rig
}

impl Rig {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..10 {
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
        }
        cx.update_window(self.window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
    }

    fn go(&self, cx: &mut TestAppContext, page: Page) {
        let shell = self.shell.clone();
        cx.update_window(self.window.into(), |_, w, cx| {
            shell.update(cx, |s, cx| {
                s.go(page);
                cx.notify();
            });
            w.render_frame(cx);
        })
        .unwrap();
        self.settle(cx);
    }

    fn has(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        cx.update_window(self.window.into(), |_, w, cx| {
            w.render_frame(cx);
            w.try_find(id).is_some()
        })
        .unwrap()
    }

    fn click(&self, cx: &mut TestAppContext, id: &'static str) {
        cx.update_window(self.window.into(), |_, w, cx| {
            w.render_frame(cx);
            w.click(id, cx);
        })
        .unwrap();
        self.settle(cx);
    }

    fn expanded(&self, cx: &mut TestAppContext) -> BTreeSet<String> {
        let shell = self.shell.clone();
        cx.update_window(self.window.into(), |_, _, cx| shell.read(cx).trading.expanded.clone()).unwrap()
    }
}

/// The page shows the line, it starts closed, and each click flips it.
fn assert_line_opens_and_closes(cx: &mut TestAppContext, rig: &Rig, id: &'static str, key: &str) {
    assert!(rig.has(cx, id), "{id}: the one-line note is on the page");
    assert!(!rig.expanded(cx).contains(key), "{key} starts closed");
    rig.click(cx, id);
    assert!(rig.expanded(cx).contains(key), "{key} opens on a click");
    assert!(rig.has(cx, id), "{id}: still there while open");
    rig.click(cx, id);
    assert!(!rig.expanded(cx).contains(key), "{key} closes on the next click");
}

#[gpui_kit::test]
fn the_unticked_okx_is_one_line_on_the_overview_that_opens_and_closes(cx: &mut TestAppContext) {
    let rig = open(cx, okx_unticked_and_failing());
    rig.go(cx, Page::Overview);
    assert_line_opens_and_closes(cx, &rig, "disabled-overview-OKX", "overview-OKX");
    // the ticked exchanges get no such line
    assert!(!rig.has(cx, "disabled-overview-Binance") && !rig.has(cx, "disabled-overview-Bybit"));
}

#[gpui_kit::test]
fn the_unticked_okx_is_one_line_in_the_staged_orders_margin_row(cx: &mut TestAppContext) {
    let rig = open(cx, okx_unticked_and_failing());
    rig.go(cx, Page::StagedOrders);
    assert_line_opens_and_closes(cx, &rig, "disabled-staged-OKX", "staged-OKX");
    assert!(!rig.has(cx, "disabled-staged-Binance"));
}

#[gpui_kit::test]
fn the_unticked_okx_is_one_line_on_the_positions_page(cx: &mut TestAppContext) {
    let rig = open(cx, okx_unticked_and_failing());
    rig.go(cx, Page::Positions);
    assert_line_opens_and_closes(cx, &rig, "disabled-positions-OKX", "positions-OKX");
}

#[gpui_kit::test]
fn the_unticked_okx_row_is_one_line_in_the_contract_settings_quote(cx: &mut TestAppContext) {
    let mut updates = okx_unticked_and_failing();
    let now = super::shell::wall_ms();
    for e in [Binance, Bybit, Okx] {
        let mut o = super::testkit::obs(e, "BTCUSDT", "0.0001", 28_800, now + 3_600_000, now);
        o.mark_price = super::testkit::d("60200");
        updates.push(SourceUpdate::Market { exchange: e, observations: vec![o], at: now });
    }
    let rig = open(cx, updates);
    rig.go(cx, Page::ContractSettings);
    assert_line_opens_and_closes(cx, &rig, "disabled-contract-OKX", "contract-OKX");
}

#[gpui_kit::test]
fn with_every_exchange_ticked_no_page_has_a_disabled_line(cx: &mut TestAppContext) {
    let mut updates = okx_unticked_and_failing();
    updates.push(SourceUpdate::Settings(complete_settings("0.01")));
    let rig = open(cx, updates);
    for (page, id) in [(Page::Overview, "disabled-overview-OKX"), (Page::StagedOrders, "disabled-staged-OKX"), (Page::Positions, "disabled-positions-OKX"), (Page::ContractSettings, "disabled-contract-OKX")] {
        rig.go(cx, page);
        assert!(!rig.has(cx, id), "{id}: OKX is ticked again");
    }
}

#[gpui_kit::test]
fn the_staged_page_draws_a_blocked_attempt_of_a_pair_the_engine_no_longer_holds(cx: &mut TestAppContext) {
    use super::bridge::PairRecord;
    use crate::store::event_query::StoredEvent;
    let now = super::shell::wall_ms();
    let mut updates = okx_unticked_and_failing();
    updates.push(SourceUpdate::PairRecords(
        [("ui-1-0-ptbusdt".to_string(), PairRecord { pair_id: "PTBUSDT-1".into(), symbol: "PTBUSDT".into(), long_exchange: Some(Binance), short_exchange: Some(Bybit), status: "BLOCKED".into() })].into(),
    ));
    let ev = |id: i64, ts: i64, payload: serde_json::Value| StoredEvent { id, ts_ms: ts, event_type: "PAIR_TRANSITION".into(), pair_id: Some("ui-1-0-ptbusdt".into()), payload: payload.to_string(), imported: false };
    updates.push(SourceUpdate::TradeEvents(vec![
        ev(2, now - 9_000, serde_json::json!({"from": "PRE_TRADE_CHECK", "to": "BLOCKED", "detail": {"block": "pre-trade fetch failed: request timed out", "simulated": false}})),
        ev(1, now - 10_000, serde_json::json!({"from": "PREPARED", "to": "PRE_TRADE_CHECK", "detail": {"simulated": false}})),
    ]));
    let rig = open(cx, updates);
    rig.go(cx, Page::StagedOrders);
    assert!(rig.has(cx, "disabled-staged-OKX"), "the page drew with the history block present");
}

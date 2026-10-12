//! The automatic adding of qualified scanner rows through the real `Shell` in a headless window:
//! the shell's own tick loop adds a qualified row to the staged orders only after the user turned
//! the switch on (two clicks), and only once per settlement. Orders are never sent from here: the
//! recorded commands are `AddPrepared` (a plan), nothing else.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, base::Root, point, px, size};
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

use super::bridge::{CommandSink, EngineState, MarketFeed, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use super::shell::Shell;
use super::testkit::{complete_settings, d, obs};
use crate::engine::command::Command;
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

#[derive(Default)]
struct RecordingSink(Mutex<Vec<(String, Command)>>);
impl CommandSink for RecordingSink {
    fn send(&self, label: String, command: Command) {
        self.0.lock().unwrap().push((label, command));
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// BTC qualifies (Binance long / Bybit short, settling in an hour); ETH does not.
fn feed(trigger: TriggerMode) -> Vec<SourceUpdate> {
    let now = wall_ms();
    let next = now + 3_600_000;
    let mut v = vec![SourceUpdate::Settings(complete_settings("0.01"))];
    for (ex, btc_rate) in [(Exchange::Binance, "-0.001"), (Exchange::Bybit, "0.001")] {
        let mk = |sym: &str, rate: &str, price: &str| {
            let mut o = obs(ex, sym, rate, 28_800, next, now);
            o.mark_price = d(price);
            o
        };
        v.push(SourceUpdate::Market { exchange: ex, observations: vec![mk("BTCUSDT", btc_rate, "60000"), mk("ETHUSDT", "0.0001", "3000")], at: now });
        let _ = MarketFeed { observations: vec![], last_success_at: None, last_error: None };
    }
    v.push(SourceUpdate::Engine(EngineState { now_ms: now, trigger_mode: trigger, execution_mode: ExecutionMode::Simulation, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] }));
    v
}

struct Rig {
    window: WindowHandle<Root>,
    shell: Entity<Shell>,
    sink: Arc<RecordingSink>,
}

fn open(cx: &mut TestAppContext, trigger: TriggerMode) -> Rig {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(feed(trigger))));
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn CommandSink> = sink.clone();
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(900.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(source, dyn_sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    let rig = Rig { window, shell, sink };
    rig.settle(cx);
    rig
}

impl Rig {
    /// Lets the shell's tick loop run (it applies updates and runs the automatic adding once a second).
    fn run_for(&self, cx: &mut TestAppContext, seconds: u64) {
        for _ in 0..(seconds * 10) {
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
        }
    }
    fn settle(&self, cx: &mut TestAppContext) {
        self.run_for(cx, 1);
    }
    fn act(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Shell)) {
        let shell = self.shell.clone();
        cx.update_window(self.window.into(), |_, window, cx| {
            shell.update(cx, |s, cx| {
                f(s);
                cx.notify();
            });
            window.render_frame(cx);
        })
        .unwrap();
    }
    /// The shell runs the pass once per WALL second; the headless test advances only its own clock,
    /// so the pass is driven here, several times (a repeat must not add again).
    fn passes(&self, cx: &mut TestAppContext, n: usize) {
        for _ in 0..n {
            self.act(cx, |s| {
                s.auto_add_now();
            });
            self.run_for(cx, 1);
        }
    }
    fn added(&self) -> Vec<(String, String)> {
        self.sink.0.lock().unwrap().iter().filter_map(|(label, c)| if let Command::AddPrepared(p) = c { Some((label.clone(), p.symbol.clone())) } else { None }).collect()
    }
    fn only_plans(&self) -> bool {
        self.sink.0.lock().unwrap().iter().all(|(_, c)| matches!(c, Command::AddPrepared(_)))
    }
}

#[gpui_kit::test]
fn nothing_is_added_while_the_switch_is_off(cx: &mut TestAppContext) {
    let rig = open(cx, TriggerMode::Auto);
    rig.passes(cx, 5);
    assert!(rig.added().is_empty(), "off by default: a qualified row is not added by itself");
}

#[gpui_kit::test]
fn the_switch_needs_a_second_click_and_then_adds_the_qualified_row_once(cx: &mut TestAppContext) {
    let rig = open(cx, TriggerMode::Auto);
    rig.act(cx, |s| s.trading.auto_add.request_enable());
    rig.passes(cx, 3);
    assert!(rig.added().is_empty(), "the first click only asks for confirmation");
    rig.act(cx, |s| s.trading.auto_add.confirm_enable());
    rig.passes(cx, 5);
    let added = rig.added();
    assert_eq!(added, vec![("自動加入交易單 BTCUSDT".to_string(), "BTCUSDT".to_string())], "only the qualified row, once, however many ticks pass");
    assert!(rig.only_plans(), "automatic adding sends plans (AddPrepared), never an order");
}

#[gpui_kit::test]
fn turning_the_switch_off_stops_the_adding(cx: &mut TestAppContext) {
    let rig = open(cx, TriggerMode::Manual);
    rig.act(cx, |s| {
        s.trading.auto_add.request_enable();
        s.trading.auto_add.confirm_enable();
        s.trading.auto_add.disable();
    });
    rig.passes(cx, 5);
    assert!(rig.added().is_empty());
}

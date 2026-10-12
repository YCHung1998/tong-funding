//! The notification badge and its drawer driven through the real `Shell` in a headless window
//! (gpui-kit `test-support`): real hit testing and click handlers. Data findings about pairs are
//! not banner rows; they sit behind the header badge, can be acknowledged, and the acknowledgement
//! is stored. No pixels are inspected.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, base::Root, point, px, size};
use serde_json::Value;

use super::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use super::funding::{FundingAlert, LegFunding, PairFunding};
use super::shell::Shell;
use super::testkit::complete_settings;
use crate::store::event_query::{EventPage, EventQuery};

#[derive(Default)]
struct FakeSource {
    updates: Mutex<Vec<SourceUpdate>>,
    /// What was stored by `save_alert_acks`, newest last.
    saved: Mutex<Vec<Value>>,
    /// What `load_alert_acks` answers (an earlier session's acknowledgements).
    stored: Mutex<Option<Value>>,
}

impl ReadOnlyDataSource for FakeSource {
    fn drain_updates(&self) -> Vec<SourceUpdate> {
        std::mem::take(&mut *self.updates.lock().unwrap())
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
    fn load_alert_acks(&self) -> Result<Option<Value>, String> {
        Ok(self.stored.lock().unwrap().clone())
    }
    fn save_alert_acks(&self, acks: &Value) -> Result<(), String> {
        self.saved.lock().unwrap().push(acks.clone());
        Ok(())
    }
}

struct NullSink;
impl CommandSink for NullSink {
    fn send(&self, _: String, _: crate::engine::command::Command) {}
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn funding_with_findings(texts: &[(i64, i64)]) -> BTreeMap<String, PairFunding> {
    let mut f = PairFunding {
        simulated: false,
        legs: [LegFunding::Simulated, LegFunding::Simulated],
        opening_fee: None,
        updated_at_ms: None,
        slots: vec![],
        pnl: None,
        alerts: vec![],
        trace: vec![],
    };
    // One alert per (pair, event type) reaches the screen, so the findings use different types.
    let types = ["PNL", "PNL_RECONCILIATION", "FUNDING_LEDGER_CONFLICT", "FETCH_ERROR"];
    for (i, (event_id, ts_ms)) in texts.iter().enumerate() {
        f.alerts.push(FundingAlert { event_id: *event_id, ts_ms: *ts_ms, event_type: types[i % types.len()].into(), text: format!("NMRUSDT PnL INCOMPLETE（#{event_id}）") });
    }
    BTreeMap::from([("ui-1-nmrusdt".to_string(), f)])
}

struct Rig {
    window: WindowHandle<Root>,
    shell: Entity<Shell>,
    source: Arc<FakeSource>,
}

fn open(cx: &mut TestAppContext, findings: &[(i64, i64)], stored: Option<Value>) -> Rig {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source = Arc::new(FakeSource::default());
    *source.stored.lock().unwrap() = stored;
    *source.updates.lock().unwrap() = vec![SourceUpdate::Settings(complete_settings("0.01")), SourceUpdate::Funding(funding_with_findings(findings))];
    let (src, sink): (Arc<dyn ReadOnlyDataSource>, Arc<dyn CommandSink>) = (source.clone(), Arc::new(NullSink));
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(900.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(src, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    let rig = Rig { window, shell, source };
    rig.settle(cx);
    // Draw a frame (the elements of the window are only registered once one has been drawn).
    let shell = rig.shell.clone();
    cx.update_window(rig.window.into(), |_, window, cx| {
        shell.update(cx, |s, cx| {
            s.go(super::nav::Page::Scanner);
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    rig.settle(cx);
    rig
}

impl Rig {
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..4 {
            cx.executor().advance_clock(Duration::from_millis(120));
            cx.run_until_parked();
        }
    }
    /// The header and the drawer sit outside the element scope of the test harness (it observes the
    /// page content only), so their click handlers' logic is driven through the shell's own methods
    /// and the frame is redrawn.
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
        self.settle(cx);
    }
    /// (banner rows, notices, data findings, drawer open, stored acknowledgements in the shell).
    fn state(&self, cx: &mut TestAppContext) -> (usize, usize, usize, bool, usize) {
        self.shell.read_with(cx, |s, _| s.notification_state())
    }
}

#[gpui_kit::test]
fn findings_about_pairs_are_a_header_badge_not_banner_rows(cx: &mut TestAppContext) {
    let baseline = open(cx, &[], None).state(cx);
    let rig = open(cx, &[(738, wall_ms()), (3523, wall_ms())], None);
    let (banner, notices, findings, drawer, _) = rig.state(cx);
    assert_eq!(banner, baseline.0, "data findings add no banner rows");
    assert_eq!((notices, findings), (baseline.1, 2));
    assert!(!drawer, "the drawer is closed until the badge is clicked");
}

#[gpui_kit::test]
fn acknowledging_a_finding_removes_it_from_the_badge_and_stores_it(cx: &mut TestAppContext) {
    let rig = open(cx, &[(738, wall_ms()), (3523, wall_ms())], None);
    rig.act(cx, |s| s.toggle_drawer());
    assert!(rig.state(cx).3, "the badge opens the drawer");
    // The drawer lists the notices first, then the findings.
    let first_finding = rig.state(cx).1;
    rig.act(cx, |s| s.acknowledge_drawer_item(first_finding));
    let (_, _, findings, _, acks) = rig.state(cx);
    assert_eq!((findings, acks), (1, 1));
    let saved = rig.source.saved.lock().unwrap().clone();
    assert_eq!(saved.len(), 1, "the acknowledgement was stored once");
    assert_eq!(saved[0].as_object().unwrap().len(), 1);
    rig.act(cx, |s| s.acknowledge_drawer_item(first_finding));
    assert_eq!(rig.state(cx).2, 0, "no findings left");
}

#[gpui_kit::test]
fn acknowledge_all_clears_the_findings(cx: &mut TestAppContext) {
    let rig = open(cx, &[(1, wall_ms()), (2, wall_ms()), (3, wall_ms())], None);
    rig.act(cx, |s| s.toggle_drawer());
    rig.act(cx, |s| s.acknowledge_drawer_group(true));
    assert_eq!(rig.state(cx).2, 0);
    assert_eq!(rig.source.saved.lock().unwrap().last().unwrap().as_object().unwrap().len(), 3);
}

#[gpui_kit::test]
fn an_acknowledgement_from_an_earlier_session_is_still_honoured_for_the_same_finding_only(cx: &mut TestAppContext) {
    // Stored by an earlier run: finding #738 of this pair was acknowledged.
    let stored = serde_json::json!({ "funding|ui-1-nmrusdt PNL": "738" });
    let rig = open(cx, &[(738, wall_ms())], Some(stored.clone()));
    assert_eq!(rig.state(cx).2, 0, "same finding: stays acknowledged after a restart");
    // The pair has a new finding with another event: it shows again.
    let rig2 = open(cx, &[(900, wall_ms())], Some(stored));
    assert_eq!(rig2.state(cx).2, 1);
}

#[gpui_kit::test]
fn findings_older_than_three_days_are_not_counted(cx: &mut TestAppContext) {
    let old = wall_ms() - 4 * 24 * 3_600_000;
    let rig = open(cx, &[(738, old), (3523, wall_ms())], None);
    assert_eq!(rig.state(cx).2, 1);
}

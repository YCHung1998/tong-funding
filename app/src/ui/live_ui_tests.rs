//! Live UI smoke test (headless gpui, like `env_banner_ui_tests`): a Live process draws the risk
//! page with the arming panel, and the other pages that carry Live buttons, without panicking;
//! a Demo process draws the same pages. No pixels are inspected; texts and enabling rules are
//! decided and tested in `arming` / `live_actions` / `manual_order`.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, base::Root, point, px, size};

use super::bridge::{CommandSink, LiveStatusView, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use super::nav::Page;
use super::shell::Shell;
use super::testkit::complete_settings;
use crate::identity::ProcessIdentity;
use crate::store::event_query::{EventPage, EventQuery};

struct FakeSource {
    identity: ProcessIdentity,
    updates: Mutex<Vec<SourceUpdate>>,
}

impl ReadOnlyDataSource for FakeSource {
    fn identity(&self) -> ProcessIdentity {
        self.identity
    }
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
}

struct NullSink;
impl CommandSink for NullSink {
    fn send(&self, _: String, _: crate::engine::command::Command) {}
}

/// Draws every page that carries Live controls; returns how many frames were drawn.
fn draw_pages(cx: &mut TestAppContext, identity: ProcessIdentity) -> usize {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let mut settings = complete_settings("0.01");
    settings.risk.live_max_leg_notional_usdt = Some("10".parse().unwrap());
    settings.risk.live_max_pairs_per_arming = Some(2);
    let live = LiveStatusView { is_live: identity.is_live(), ..LiveStatusView::default() };
    let source = Arc::new(FakeSource { identity, updates: Mutex::new(vec![SourceUpdate::Settings(settings), SourceUpdate::LiveStatus(live)]) });
    let (src, sink): (Arc<dyn ReadOnlyDataSource>, Arc<dyn CommandSink>) = (source, Arc::new(NullSink));
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(1000.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(src, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    let mut frames = 0;
    for page in [Page::RiskSettings, Page::Positions, Page::StagedOrders, Page::ManualOrder, Page::Overview] {
        cx.update_window(window.into(), |_, w, cx| {
            shell.update(cx, |s, cx| {
                s.go(page);
                cx.notify();
            });
            w.render_frame(cx);
        })
        .unwrap();
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(120));
            cx.run_until_parked();
            cx.update_window(window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
            frames += 1;
        }
    }
    frames
}

#[gpui_kit::test]
fn a_live_process_draws_the_arming_panel_and_the_live_pages(cx: &mut TestAppContext) {
    assert_eq!(draw_pages(cx, ProcessIdentity::Live), 15);
}

#[gpui_kit::test]
fn a_demo_process_draws_the_same_pages(cx: &mut TestAppContext) {
    assert_eq!(draw_pages(cx, ProcessIdentity::Demo), 15);
}

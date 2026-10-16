//! live-banners smoke test: the real `Shell` in a headless window (gpui-kit `test-support`) draws
//! with the process identity of its data source. The identity must be there from the very first
//! frame (never a flash of DEMO in a Live process) and must survive every snapshot the bridge
//! hands over. No pixels are inspected; the banner text itself is decided (and tested) in
//! `env_label`.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, base::Root, point, px, size};

use super::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use super::env_label::EnvBanner;
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

/// Opens a window, draws `frames` frames and returns (banner right after `Shell::new`, banner after the data flowed).
fn run(cx: &mut TestAppContext, identity: ProcessIdentity) -> (EnvBanner, EnvBanner, ProcessIdentity) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source = Arc::new(FakeSource { identity, updates: Mutex::new(vec![SourceUpdate::Settings(complete_settings("0.01"))]) });
    let (src, sink): (Arc<dyn ReadOnlyDataSource>, Arc<dyn CommandSink>) = (source, Arc::new(NullSink));
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(900.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(src, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    let first = shell.read_with(cx, |s, _| s.env_banner_state());
    for _ in 0..6 {
        cx.executor().advance_clock(Duration::from_millis(120));
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
        })
        .unwrap();
    }
    let (later, status_identity) = shell.read_with(cx, |s, _| (s.env_banner_state(), s.status_identity()));
    (first, later, status_identity)
}

#[gpui_kit::test]
fn a_live_process_shows_live_from_the_first_frame_and_keeps_it(cx: &mut TestAppContext) {
    let (first, later, status_identity) = run(cx, ProcessIdentity::Live);
    for b in [&first, &later] {
        assert!(b.text.contains("LIVE"), "{}", b.text);
        assert!(b.kill_switch_text.is_some(), "a Live banner always states the kill switch");
    }
    assert_eq!(status_identity, ProcessIdentity::Live, "the status bar follows the same identity");
}

#[gpui_kit::test]
fn a_demo_process_never_shows_live(cx: &mut TestAppContext) {
    let (first, later, status_identity) = run(cx, ProcessIdentity::Demo);
    for b in [&first, &later] {
        assert!(!b.text.contains("LIVE") && b.kill_switch_text.is_none(), "{b:?}");
    }
    assert_eq!(status_identity, ProcessIdentity::Demo);
}

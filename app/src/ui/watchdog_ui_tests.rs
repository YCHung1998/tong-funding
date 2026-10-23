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

/// The shell on the risk page with `settings` delivered by the data source.
fn risk_page(cx: &mut TestAppContext, settings: Settings) -> gpui_kit::WindowHandle<Root> {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(vec![SourceUpdate::Settings(settings)])));
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
            s.go(Page::RiskSettings);
            cx.notify();
        });
        w.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update_window(window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
    window
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

//! risk-settings-guidance: the `?` button opens a real dialog in the Root layer (spike for
//! `WindowExt::open_dialog`), driven through the real `Shell` in a headless window.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::component::WindowExt as _;
use gpui_kit::{AppContext, TestAppContext, base::Root, point, px, size};

use super::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
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

fn wall_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}


fn wall_ms_unused() {}

#[gpui_kit::test]
fn the_question_mark_opens_and_closes_the_help_dialog(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(vec![SourceUpdate::Settings(complete_settings("0.01"))])));
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
    let settle = |cx: &mut TestAppContext| {
        for _ in 0..4 {
            cx.executor().advance_clock(Duration::from_millis(120));
            cx.run_until_parked();
        }
    };
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
    let active = |cx: &mut TestAppContext| cx.update_window(window.into(), |_, w, cx| w.has_active_dialog(cx)).unwrap();
    assert!(!active(cx), "no dialog before the click");
    cx.update_window(window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
    settle(cx);
    cx.update_window(window.into(), |_, w, cx| {
        w.render_frame(cx);
        w.click("risk-help", cx);
    })
    .unwrap();
    settle(cx);
    assert!(active(cx), "the ? button opens the dialog");
    cx.update_window(window.into(), |_, w, cx| {
        w.render_frame(cx);
        w.close_dialog(cx);
    })
    .unwrap();
    settle(cx);
    assert!(!active(cx), "the dialog closes");
}

//! loss-circuit-breaker 3.3: the risk page's four titled blocks and the save bar fixed at the bottom,
//! through the real `Shell` in a headless window. The words and the counting are tested on the
//! view-model (`risk_settings`); this proves the blocks and the bar are drawn where the spec says.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
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

fn settle(cx: &mut TestAppContext) {
    for _ in 0..4 {
        cx.executor().advance_clock(Duration::from_millis(120));
        cx.run_until_parked();
    }
}

const WINDOW_H: f32 = 1000.0;

fn risk_page_with_shell(cx: &mut TestAppContext) -> (gpui_kit::WindowHandle<Root>, gpui_kit::Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(vec![SourceUpdate::Settings(complete_settings("0.01"))])));
    let sink: Arc<dyn CommandSink> = Arc::new(NullSink);
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1600.), px(WINDOW_H)) })), ..Default::default() },
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
    (window, shell)
}

fn risk_page(cx: &mut TestAppContext) -> gpui_kit::WindowHandle<Root> {
    risk_page_with_shell(cx).0
}

#[gpui_kit::test]
fn the_four_blocks_are_drawn_in_order_and_the_breaker_fields_sit_in_the_last_one(cx: &mut TestAppContext) {
    let window = risk_page(cx);
    let tops: Vec<_> = (0..4)
        .map(|i| {
            let id = format!("risk-group-{i}");
            let s = cx.update_window(window.into(), |_, w, _| w.try_find(id.clone())).unwrap().unwrap_or_else(|| panic!("{id} is missing"));
            // the page is longer than the window: a lower block is below the fold, which is what the bar is for
            assert!(s.bounds().size.height > px(20.), "{id}: {:?}", s.bounds());
            s.bounds().origin.y
        })
        .collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "進場條件 / 成本估計 / 執行保護 / 部位與熔斷 from top to bottom: {tops:?}");
    let group = cx.update_window(window.into(), |_, w, _| w.find("risk-group-3").bounds()).unwrap();
    for key in ["loss_breaker_max_loss_usdt", "loss_breaker_max_consecutive_losses"] {
        let row = cx.update_window(window.into(), |_, w, _| w.try_find(format!("risk-field-{key}")).map(|s| s.bounds())).unwrap();
        let row = row.unwrap_or_else(|| panic!("{key}: the row is missing"));
        assert!(row.origin.y >= group.origin.y && row.origin.y + row.size.height <= group.origin.y + group.size.height, "{key} {row:?} lies inside the last block {group:?}");
    }
}

#[gpui_kit::test]
fn the_save_bar_is_outside_the_scrolling_content_and_sits_at_the_bottom_of_the_window(cx: &mut TestAppContext) {
    let window = risk_page(cx);
    let bar = cx.update_window(window.into(), |_, w, _| w.try_find("risk-save-bar")).unwrap().expect("the save bar is drawn");
    assert!(bar.visible());
    let (b, content) = (bar.bounds(), cx.update_window(window.into(), |_, w, _| w.find("content").bounds()).unwrap());
    assert!(b.size.height > px(10.), "{b:?}");
    assert!(b.origin.y + b.size.height <= px(WINDOW_H), "inside the window: {b:?}");
    assert!(b.origin.y >= content.origin.y + content.size.height - px(1.), "below the scrolling content, not part of it: bar {b:?} content {content:?}");
    let save = cx.update_window(window.into(), |_, w, _| w.try_find("risk-save")).unwrap().expect("the save button is in the bar");
    let s = save.bounds();
    assert!(s.origin.y >= b.origin.y && s.origin.y + s.size.height <= b.origin.y + b.size.height, "the button is in the bar: {s:?} / {b:?}");
}

#[gpui_kit::test]
fn the_save_button_is_not_inside_the_content_any_more(cx: &mut TestAppContext) {
    // the old button sat at the end of the long card, out of sight
    let window = risk_page(cx);
    let content = cx.update_window(window.into(), |_, w, _| w.find("content").bounds()).unwrap();
    let save = cx.update_window(window.into(), |_, w, _| w.find("risk-save").bounds()).unwrap();
    assert!(save.origin.y >= content.origin.y + content.size.height - px(1.), "{save:?} vs {content:?}");
}

#[gpui_kit::test]
fn other_pages_have_no_save_bar(cx: &mut TestAppContext) {
    let (window, shell) = risk_page_with_shell(cx);
    assert!(cx.update_window(window.into(), |_, w, _| w.try_find("risk-save-bar").is_some()).unwrap(), "on the risk page");
    cx.update_window(window.into(), |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.go(Page::Overview);
            cx.notify();
        });
        w.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update_window(window.into(), |_, w, cx| w.render_frame(cx)).unwrap();
    assert!(cx.update_window(window.into(), |_, w, _| w.try_find("risk-save-bar").is_none()).unwrap(), "not on the overview");
}

#[gpui_kit::test]
fn the_status_bar_is_the_last_row_and_the_save_bar_sits_right_above_it(cx: &mut TestAppContext) {
    let window = risk_page(cx);
    let status = cx.update_window(window.into(), |_, w, _| w.try_find("status-bar")).unwrap().expect("the status bar is drawn on every page");
    let (sb, bar) = (status.bounds(), cx.update_window(window.into(), |_, w, _| w.find("risk-save-bar").bounds()).unwrap());
    assert!(status.visible() && sb.size.height >= px(20.), "{sb:?}");
    assert!(bar.origin.y + bar.size.height <= sb.origin.y + px(1.), "the save bar ends where the status bar begins: {bar:?} / {sb:?}");
    assert!(sb.origin.y + sb.size.height <= px(WINDOW_H) + px(1.), "{sb:?}");
}

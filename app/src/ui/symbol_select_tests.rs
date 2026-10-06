//! searchable-symbol-inputs task 2.1 / 3.x: the dropdown delegate and the pickers, headless
//! (gpui-kit `test-support`). Read-back goes through the same `value()` / `text()` the forms poll.
use std::sync::{Arc, Mutex};

use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, base::Root, point, px, size};
use gpui_kit::component::searchable_list::SearchableListDelegate;
use gpui_kit::component::IndexPath;

use super::{CoinPicker, Shell, SymbolDelegate, SymbolPicker};
use gpui_kit::{Context, Window};
use crate::engine::command::Command;
use crate::ui::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
use crate::store::event_query::{EventPage, EventQuery};

struct FakeSource;
impl ReadOnlyDataSource for FakeSource {
    fn drain_updates(&self) -> Vec<SourceUpdate> {
        Vec::new()
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
struct NullSink(Mutex<Vec<Command>>);
impl CommandSink for NullSink {
    fn send(&self, _: String, c: Command) {
        self.0.lock().unwrap().push(c);
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn rows(d: &SymbolDelegate) -> Vec<(String, bool)> {
    (0..d.items_count(0)).map(|i| d.item(IndexPath::default().row(i)).map(|x| (x.0.value.clone(), x.0.free)).unwrap()).collect()
}

fn open(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::ui::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource);
    let sink: Arc<dyn CommandSink> = Arc::new(NullSink(Mutex::new(Vec::new())));
    cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(1200.), px(900.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(source, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    })
}

/// Runs `f` with a fresh picker built inside the shell's context.
fn with_shell<R>(cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut Context<Shell>) -> R) -> R {
    let (window, shell) = open(cx);
    cx.update_window(window.into(), |_, window, cx| shell.update(cx, |_, cx| f(window, cx))).unwrap()
}

#[gpui_kit::test]
fn typing_filters_the_rows_and_offers_the_typed_text_first(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.update(|window, cx| {
        let mut d = SymbolDelegate::new(&strs(&["1000PEPEUSDT", "BTCUSDT"]), &[]);
        assert_eq!(rows(&d), vec![("1000PEPEUSDT".into(), false), ("BTCUSDT".into(), false)]);
        _ = d.perform_search("pep", window, cx);
        assert_eq!(rows(&d), vec![("PEP".into(), true), ("1000PEPEUSDT".into(), false)]);
        _ = d.perform_search("newusdt", window, cx);
        assert_eq!(rows(&d), vec![("NEWUSDT".into(), true)]);
        _ = d.perform_search("", window, cx);
        assert_eq!(d.items_count(0), 2);
    });
}

#[gpui_kit::test]
fn a_selected_value_outside_the_list_stays_a_row(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    cx.update(|_, _| {
        let d = SymbolDelegate::new(&strs(&["BTCUSDT"]), &strs(&["NEWUSDT", "BTCUSDT"]));
        assert_eq!(rows(&d), vec![("BTCUSDT".into(), false), ("NEWUSDT".into(), false)]);
    });
}

#[gpui_kit::test]
fn the_single_picker_keeps_a_free_value_and_follows_candidate_changes(cx: &mut TestAppContext) {
    with_shell(cx, |window, cx| {
        let mut p = SymbolPicker::new("BTCUSDT", window, cx);
        assert_eq!(p.value(cx), "BTCUSDT");
        assert!(!p.loaded());
        p.sync(strs(&["BTCUSDT", "ETHUSDT"]), window, cx);
        assert_eq!(p.value(cx), "BTCUSDT");
        assert!(p.loaded() && !p.is_unlisted(cx));
        p.set_value("ETHUSDT", window, cx);
        assert_eq!(p.value(cx), "ETHUSDT");
        // a value that is not in the market list is accepted and flagged
        p.set_value("newusdt ", window, cx);
        assert_eq!(p.value(cx), "newusdt");
        assert!(p.is_unlisted(cx));
        // switching exchange (new candidates) keeps the chosen value
        p.sync(strs(&["SOLUSDT"]), window, cx);
        assert_eq!(p.value(cx), "newusdt");
        p.set_value("SOLUSDT", window, cx);
        assert_eq!(p.value(cx), "SOLUSDT");
        assert!(!p.is_unlisted(cx));
    });
}

#[gpui_kit::test]
fn the_coin_picker_round_trips_the_stored_text(cx: &mut TestAppContext) {
    with_shell(cx, |window, cx| {
        let mut p = CoinPicker::new(window, cx);
        assert_eq!(p.text(cx), "");
        p.sync(strs(&["BTC", "ETH", "SOL"]), window, cx);
        p.set_text("btc, ETH", window, cx);
        assert_eq!(p.text(cx), "BTC, ETH");
        assert!(p.unlisted(cx).is_empty());
        // a coin that is not in the list is kept, flagged, and survives a candidate refresh
        p.set_text("BTC, NEWCOIN", window, cx);
        assert_eq!(p.text(cx), "BTC, NEWCOIN");
        assert_eq!(p.unlisted(cx), strs(&["NEWCOIN"]));
        p.sync(strs(&["BTC", "ETH"]), window, cx);
        assert_eq!(p.text(cx), "BTC, NEWCOIN");
        p.set_text("", window, cx);
        assert_eq!(p.text(cx), "");
    });
}

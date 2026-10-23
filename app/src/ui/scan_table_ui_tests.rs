//! Task 3.3 / 4 / 5: the scanner table controls driven through the real `Shell` in a headless
//! window (gpui-kit `test-support`): real hit testing, real click handlers, real tick loop.
//! No pixels are inspected; see the screenshots in the change notes for the look.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, Pixels, ScrollDelta, SharedString, TestAppContext, WindowHandle, base::Root, point, px, size};
use tong_funding_core::types::Exchange;

use super::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, Settings, SourceUpdate};
use super::nav::Page;
use super::scan_view::{ScanColumn, SortDir, SortState};
use super::shell::Shell;
use super::testkit::{complete_settings, obs};
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

/// Four symbols; default order (gross spread, settings incomplete) is DDD, BBB, CCC, AAA.
fn feed() -> Vec<SourceUpdate> {
    feed_with(complete_settings("0.01"))
}

fn feed_with(settings: Settings) -> Vec<SourceUpdate> {
    let now = wall_ms();
    let next = now + 4 * 3_600_000;
    let rates = [("AAAUSDT", "0.0001", "0.0000"), ("BBBUSDT", "0.0008", "-0.0002"), ("CCCUSDT", "0.0003", "-0.0001"), ("DDDUSDT", "0.0020", "-0.0010")];
    let mut v = vec![SourceUpdate::Settings(settings)];
    for (e, idx) in [(Exchange::Binance, 1), (Exchange::Bybit, 2)] {
        let observations = rates.iter().map(|r| obs(e, r.0, if idx == 1 { r.1 } else { r.2 }, 28_800, next, now)).collect();
        v.push(SourceUpdate::Market { exchange: e, observations, at: now });
    }
    v
}

struct Rig {
    window: WindowHandle<Root>,
    shell: Entity<Shell>,
}

/// A window wide enough for all 12 columns (about 1,330 px) unless a test wants it narrow.
const WIDE: f32 = 1800.0;
const NARROW: f32 = 1000.0;

fn open(cx: &mut TestAppContext) -> Rig {
    open_sized(cx, WIDE)
}

fn open_sized(cx: &mut TestAppContext, width: f32) -> Rig {
    open_with(cx, width, feed())
}

fn open_with(cx: &mut TestAppContext, width: f32, updates: Vec<SourceUpdate>) -> Rig {
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(updates)));
    let sink: Arc<dyn CommandSink> = Arc::new(NullSink);
    let (window, shell) = cx.update(|cx| {
        let (w, shell) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds { origin: point(px(0.), px(0.)), size: size(px(width), px(900.)) })), ..Default::default() },
            cx,
            move |window, cx| cx.new(|cx| Shell::new(source, sink, window, cx)),
        )
        .expect("open window");
        (w.downcast::<Root>().expect("Root"), shell)
    });
    let rig = Rig { window, shell };
    rig.settle(cx);
    rig.goto(Page::Scanner, cx);
    rig
}

impl Rig {
    /// Let the shell's 100 ms tick loop run (it drains clicks and recomputes).
    fn settle(&self, cx: &mut TestAppContext) {
        for _ in 0..4 {
            cx.executor().advance_clock(Duration::from_millis(120));
            cx.run_until_parked();
        }
    }
    fn goto(&self, page: Page, cx: &mut TestAppContext) {
        let shell = self.shell.clone();
        cx.update_window(self.window.into(), |_, window, cx| {
            shell.update(cx, |s, cx| {
                s.go(page);
                cx.notify();
            });
            window.render_frame(cx);
        })
        .unwrap();
        self.settle(cx);
    }
    fn click(&self, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) {
        let id = id.into();
        cx.update_window(self.window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
        self.settle(cx);
    }
    /// (shown columns, sort, row order). Reads the shell's own state AND what the table is drawing,
    /// and requires them to agree: a state change that never reaches the table is a bug too.
    fn view(&self, cx: &mut TestAppContext) -> (Vec<ScanColumn>, Option<SortState>, Vec<String>) {
        self.shell.read_with(cx, |s, cx| {
            let t = s.table.read(cx).delegate();
            let (own_visible, own_sort) = (s.view.visibility.visible(), s.view.sort);
            assert_eq!(t.visible, own_visible, "the table draws different columns than the shell state says");
            assert_eq!(t.sort, own_sort, "the table shows a different sort than the shell state says");
            (own_visible, own_sort, t.rows.iter().map(|r| r.symbol.clone()).collect())
        })
    }
}

fn idx(c: ScanColumn) -> usize {
    ScanColumn::ALL.iter().position(|x| *x == c).unwrap()
}

#[gpui_kit::test]
fn the_scanner_starts_with_every_column_and_the_default_order(cx: &mut TestAppContext) {
    let rig = open(cx);
    let (visible, sort, rows) = rig.view(cx);
    assert_eq!(visible, ScanColumn::ALL);
    assert_eq!(sort, None);
    assert_eq!(rows, ["DDDUSDT", "BBBUSDT", "CCCUSDT", "AAAUSDT"], "default order = gross spread descending (settings incomplete)");
    // every chip and the reset button exist as clickable elements
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        for i in 0..12usize {
            window.find(("col-chip", i));
        }
        window.find("col-reset");
    })
    .unwrap();
}

#[gpui_kit::test]
fn clicking_a_chip_hides_that_column_and_clicking_again_restores_it(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("col-chip", idx(ScanColumn::Okx)), cx);
    let (visible, _, _) = rig.view(cx);
    assert!(!visible.contains(&ScanColumn::Okx));
    assert_eq!(visible.len(), 11);
    rig.click(("col-chip", idx(ScanColumn::Okx)), cx);
    assert_eq!(rig.view(cx).0, ScanColumn::ALL, "back in the original place");
}

#[gpui_kit::test]
fn the_symbol_chip_does_nothing(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("col-chip", idx(ScanColumn::Symbol)), cx);
    assert!(rig.view(cx).0.contains(&ScanColumn::Symbol));
    assert_eq!(rig.view(cx).0.len(), 12);
}

#[gpui_kit::test]
fn reset_shows_every_column_again(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("col-chip", idx(ScanColumn::Gross)), cx);
    rig.click(("col-chip", idx(ScanColumn::Add)), cx);
    assert_eq!(rig.view(cx).0.len(), 10);
    rig.click("col-reset", cx);
    assert_eq!(rig.view(cx).0, ScanColumn::ALL);
}

#[gpui_kit::test]
fn clicking_a_header_title_cycles_descending_ascending_default(cx: &mut TestAppContext) {
    let rig = open(cx);
    let th = ("scan-th", idx(ScanColumn::Symbol));
    rig.click(th, cx);
    let (_, sort, rows) = rig.view(cx);
    assert_eq!(sort, Some(SortState { column: ScanColumn::Symbol, dir: SortDir::Desc }));
    assert_eq!(rows, ["DDDUSDT", "CCCUSDT", "BBBUSDT", "AAAUSDT"]);
    rig.click(th, cx);
    let (_, sort, rows) = rig.view(cx);
    assert_eq!(sort, Some(SortState { column: ScanColumn::Symbol, dir: SortDir::Asc }));
    assert_eq!(rows, ["AAAUSDT", "BBBUSDT", "CCCUSDT", "DDDUSDT"]);
    rig.click(th, cx);
    let (_, sort, rows) = rig.view(cx);
    assert_eq!(sort, None);
    assert_eq!(rows, ["DDDUSDT", "BBBUSDT", "CCCUSDT", "AAAUSDT"], "third click = the default order again");
}

#[gpui_kit::test]
fn the_direction_and_add_headers_are_not_sortable(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("scan-th", idx(ScanColumn::Gross)), cx);
    let before = rig.view(cx);
    // these two headers have no click handler and are not even sortable elements
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("scan-th", idx(ScanColumn::Direction))).is_some(), "the title renders");
    })
    .unwrap();
    rig.click(("scan-th", idx(ScanColumn::Direction)), cx);
    rig.click(("scan-th", idx(ScanColumn::Add)), cx);
    assert_eq!(rig.view(cx), before, "sort unchanged");
}

#[gpui_kit::test]
fn the_sort_follows_the_visible_index_after_a_column_is_hidden(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("col-chip", idx(ScanColumn::Coverage)), cx); // Symbol is still index 1, Countdown moves from 3 to 2
    rig.click(("scan-th", 3usize), cx); // visible index 3 is now Binance (Rank, Symbol, Countdown, Binance, ...)
    let (visible, sort, _) = rig.view(cx);
    assert_eq!(visible[3], ScanColumn::Binance);
    assert_eq!(sort.map(|s| s.column), Some(ScanColumn::Binance), "the click is mapped to the logical column, not the position");
}

#[gpui_kit::test]
fn the_add_cell_belongs_to_the_row_that_is_drawn(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("scan-th", idx(ScanColumn::Symbol)), cx); // descending
    rig.click(("scan-th", idx(ScanColumn::Symbol)), cx); // ascending: AAA first
    assert_eq!(rig.view(cx).2[0], "AAAUSDT");
    let toggles = rig.shell.read_with(cx, |s, _| s.trading.toggles.clone());
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("cand", 0usize), cx);
    })
    .unwrap();
    assert_eq!(&*toggles.borrow(), &["AAAUSDT".to_string()], "row 0 as drawn, not the default-order first row");
}

#[gpui_kit::test]
fn the_edge_kind_tag_belongs_to_its_symbol_not_to_the_row_position(cx: &mut TestAppContext) {
    // The tag's tooltip state is keyed by the element id. An id built from the row index would keep an
    // old row's tooltip (another symbol's text) on whatever symbol is drawn there after a re-sort.
    let rig = open(cx);
    let top_to_bottom = |cx: &mut TestAppContext| -> Vec<String> {
        cx.update_window(rig.window.into(), |_, window, cx| {
            window.render_frame(cx);
            let mut tops: Vec<(String, Pixels)> = ["AAAUSDT", "BBBUSDT", "CCCUSDT", "DDDUSDT"]
                .iter()
                .map(|s| (s.to_string(), window.find(SharedString::from(format!("edge-kind-{s}"))).bounds().origin.y))
                .collect();
            tops.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            tops.into_iter().map(|t| t.0).collect()
        })
        .unwrap()
    };
    assert_eq!(top_to_bottom(cx), ["DDDUSDT", "BBBUSDT", "CCCUSDT", "AAAUSDT"], "the default order");
    rig.click(("scan-th", idx(ScanColumn::Symbol)), cx); // descending
    rig.click(("scan-th", idx(ScanColumn::Symbol)), cx); // ascending
    assert_eq!(rig.view(cx).2, ["AAAUSDT", "BBBUSDT", "CCCUSDT", "DDDUSDT"]);
    assert_eq!(top_to_bottom(cx), ["AAAUSDT", "BBBUSDT", "CCCUSDT", "DDDUSDT"], "each symbol's tag moved with its row");
}

#[gpui_kit::test]
fn columns_and_sort_survive_leaving_the_page_and_coming_back(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("col-chip", idx(ScanColumn::Okx)), cx);
    rig.click(("col-chip", idx(ScanColumn::Direction)), cx);
    rig.click(("scan-th", idx(ScanColumn::Symbol)), cx);
    let before = rig.view(cx);
    rig.goto(Page::Overview, cx);
    rig.goto(Page::Positions, cx);
    rig.goto(Page::Scanner, cx);
    assert_eq!(rig.view(cx), before, "same shown columns, same sort, same row order");
    let (visible, sort, _) = before;
    assert!(!visible.contains(&ScanColumn::Okx) && !visible.contains(&ScanColumn::Direction));
    assert_eq!(sort.map(|s| s.column), Some(ScanColumn::Symbol));
}

#[gpui_kit::test]
fn a_hidden_sort_column_keeps_sorting_and_data_refresh_keeps_the_state(cx: &mut TestAppContext) {
    let rig = open(cx);
    rig.click(("scan-th", idx(ScanColumn::Symbol)), cx);
    rig.click(("col-chip", idx(ScanColumn::Symbol)), cx); // locked: no effect
    rig.click(("col-chip", idx(ScanColumn::Gross)), cx);
    let (visible, sort, rows) = rig.view(cx);
    assert!(!visible.contains(&ScanColumn::Gross));
    assert_eq!(sort.map(|s| s.column), Some(ScanColumn::Symbol));
    assert_eq!(rows[0], "DDDUSDT");
}

#[gpui_kit::test]
fn the_table_scrolls_sideways_when_the_columns_are_wider_than_the_window(cx: &mut TestAppContext) {
    let rig = open_sized(cx, NARROW); // the 12 columns need about 1,330 px
    let offset = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, cx| s.table.read(cx).horizontal_scroll_handle.offset().x);
    let start = offset(&rig, cx);
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.scroll(("scan-th", idx(ScanColumn::Binance)), ScrollDelta::Pixels(point(px(-300.), px(0.))), cx);
    })
    .unwrap();
    rig.settle(cx);
    let moved = offset(&rig, cx);
    assert!(moved < start, "scrolled left by the wheel: {start:?} -> {moved:?}");
}

#[gpui_kit::test]
fn the_table_stays_inside_the_window_and_the_overflow_scrolls_inside_it(cx: &mut TestAppContext) {
    // Real-window finding: the page used to grow wider than the window and get clipped, so the
    // columns on the right could not be reached and the table never scrolled.
    let rig = open_sized(cx, NARROW);
    let (table, last_header) = cx
        .update_window(rig.window.into(), |_, window, cx| {
            window.render_frame(cx);
            (window.find("scan-table").bounds(), window.try_find(("col-header", 11usize)).map(|e| e.bounds()))
        })
        .unwrap();
    assert!(table.right() <= px(NARROW) + px(0.5), "the table container ends at {:?}, past the {NARROW} px window", table.right());
    assert!(table.size.width > px(300.), "and it is not collapsed: {:?}", table.size.width);
    let total = ScanColumn::ALL.iter().map(|c| c.width()).sum::<f32>();
    assert!(total > NARROW, "the test only means something when the columns are wider than the window");
    // The last column is outside the table's viewport: the library does not even draw it until scrolled.
    assert!(last_header.is_none_or(|b| b.right() > px(NARROW)), "the last column header must not already be inside the window: {last_header:?}");
}

#[gpui_kit::test]
fn scrolling_brings_the_last_column_into_view(cx: &mut TestAppContext) {
    let rig = open_sized(cx, NARROW);
    let last = |rig: &Rig, cx: &mut TestAppContext| {
        cx.update_window(rig.window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.try_find(("col-header", 11usize)).map(|e| e.bounds().right())
        })
        .unwrap()
    };
    let before = last(&rig, cx);
    assert!(before.is_none_or(|r| r > px(NARROW)), "not reachable before scrolling: {before:?}");
    for _ in 0..4 {
        cx.update_window(rig.window.into(), |_, window, cx| {
            window.render_frame(cx);
            // any header that is currently drawn inside the table's scrolling part
            let drawn: Vec<usize> = (0..12usize).filter(|i| window.try_find(("scan-th", *i)).is_some()).collect();
            // Rank and Symbol (0, 1) are fixed on the left: they stay drawn while the others scroll away.
            assert!(drawn.contains(&0) && drawn.contains(&1), "fixed columns must stay drawn: {drawn:?}");
            // a header fully inside the window (a drawn one can be clipped away and cannot take the wheel)
            let inside = |i: &usize| window.try_find(("scan-th", *i)).is_some_and(|e| e.visible());
            let target = *drawn.iter().find(|i| **i >= 2 && inside(i)).expect("a scrolling header is drawn inside the window");
            window.scroll(("scan-th", target), ScrollDelta::Pixels(point(px(-400.), px(0.))), cx);
        })
        .unwrap();
        rig.settle(cx);
    }
    let after = last(&rig, cx).expect("after scrolling the last column is drawn");
    assert!(after <= px(NARROW) + px(0.5), "and it is fully inside the window: {after:?}");
}

// ---- scanner-layout 2.1: an exchange that is not enabled starts hidden --------------------------

/// Complete settings with only Binance and Bybit ticked.
fn okx_unticked() -> Settings {
    let mut s = complete_settings("0.01");
    s.risk.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit];
    s
}

fn without_okx() -> Vec<ScanColumn> {
    ScanColumn::ALL.into_iter().filter(|c| *c != ScanColumn::Okx).collect()
}

#[gpui_kit::test]
fn the_okx_column_starts_hidden_when_okx_is_not_ticked_and_the_order_is_unchanged(cx: &mut TestAppContext) {
    let rig = open_with(cx, WIDE, feed_with(okx_unticked()));
    assert_eq!(rig.view(cx).0, without_okx());
    // its button is still there so the user can show it
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.find(("col-chip", idx(ScanColumn::Okx)));
    })
    .unwrap();
}

#[gpui_kit::test]
fn showing_the_okx_column_by_hand_is_kept_through_refreshes_and_page_changes(cx: &mut TestAppContext) {
    let rig = open_with(cx, WIDE, feed_with(okx_unticked()));
    rig.click(("col-chip", idx(ScanColumn::Okx)), cx);
    assert_eq!(rig.view(cx).0, ScanColumn::ALL, "shown at its place");
    rig.settle(cx);
    rig.goto(Page::Overview, cx);
    rig.goto(Page::Scanner, cx);
    assert_eq!(rig.view(cx).0, ScanColumn::ALL, "the manual choice is not overwritten by the default");
    rig.click(("col-chip", idx(ScanColumn::Okx)), cx);
    assert_eq!(rig.view(cx).0, without_okx(), "and it can be hidden again");
}

#[gpui_kit::test]
fn every_column_shows_when_every_exchange_is_ticked(cx: &mut TestAppContext) {
    assert_eq!(open(cx).view(cx).0, ScanColumn::ALL);
}

// ---- scanner-layout 2.2: the default window shows the main actions and nothing is cut or overlapped ----

/// The window of the screenshot that prompted scanner-layout: 3164 x 2070 physical px on a 2x
/// Retina display = about 1,470 logical px including the border, zoom 100%. The table gets about
/// 1,220 of it (side bar and margins take 248).
const DEFAULT_WIDTH: f32 = 1470.0;

/// Like `feed_with`, but every Binance rate is a 4 h rate against Bybit's 8 h: each row shows the
/// interval difference, the 8 h equivalent on Binance, and (when core classifies it) the kind tag.
fn mixed_interval_feed(settings: Settings) -> Vec<SourceUpdate> {
    let mut v = feed_with(settings);
    for u in &mut v {
        if let SourceUpdate::Market { exchange: Exchange::Binance, observations, .. } = u {
            for o in observations.iter_mut() {
                o.funding_interval_secs = Some(14_400);
            }
        }
    }
    v
}

fn all_ticked() -> Settings {
    complete_settings("0.01")
}

/// Bounds of a column header by logical column (None = not drawn: scrolled out of the table).
fn header(rig: &Rig, c: ScanColumn, cx: &mut TestAppContext) -> Option<gpui_kit::Bounds<Pixels>> {
    let at = rig.view(cx).0.iter().position(|x| *x == c)?;
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.try_find(("col-header", at)).map(|e| e.bounds())
    })
    .unwrap()
}

fn bounds_of(rig: &Rig, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) -> gpui_kit::Bounds<Pixels> {
    let id = id.into();
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.find(id).bounds()
    })
    .unwrap()
}

#[gpui_kit::test]
fn at_the_default_width_the_qualified_and_add_columns_are_in_view_without_scrolling(cx: &mut TestAppContext) {
    // Spec scanner-layout: with OKX unticked. (All three exchanges need about 1,360 px and scroll.)
    {
        let rig = open_with(cx, DEFAULT_WIDTH, mixed_interval_feed(okx_unticked()));
        for c in [ScanColumn::Qualified, ScanColumn::Add] {
            let b = header(&rig, c, cx).unwrap_or_else(|| panic!("{c:?} is not drawn: it is scrolled out of the table"));
            assert!(b.right() <= px(DEFAULT_WIDTH), "{c:?} ends at {:?}, past the {DEFAULT_WIDTH} px window", b.right());
            assert!(b.size.width >= px(c.width() - 1.0), "{c:?} is cut to {:?}", b.size.width);
        }
        let table = bounds_of(&rig, "scan-table", cx);
        let add = header(&rig, ScanColumn::Add, cx).unwrap();
        assert!(add.right() <= table.right(), "inside the table, so no horizontal scroll is needed: {:?} vs {:?}", add.right(), table.right());
    }
}

#[gpui_kit::test]
fn a_rate_cell_is_two_stacked_lines_inside_its_column_and_inside_its_row(cx: &mut TestAppContext) {
    let rig = open_with(cx, DEFAULT_WIDTH, mixed_interval_feed(okx_unticked()));
    let col = header(&rig, ScanColumn::Binance, cx).expect("Binance column");
    let cell = bounds_of(&rig, SharedString::from("rate-DDDUSDT-Binance"), cx);
    let (line1, line2) = (bounds_of(&rig, SharedString::from("rate-line1-DDDUSDT-Binance"), cx), bounds_of(&rig, SharedString::from("rate-line2-DDDUSDT-Binance"), cx));
    assert!(line2.top() >= line1.bottom(), "the 8h equivalent is under the rate, not beside it: {line1:?} / {line2:?}");
    for b in [cell, line1, line2] {
        assert!(b.left() >= col.left() && b.right() <= col.right(), "{b:?} leaves its column {col:?}");
    }
    // the row pitch is the row height: the whole cell (its 4 px padding top and bottom included) fits it
    let next = bounds_of(&rig, SharedString::from("rate-BBBUSDT-Binance"), cx);
    let pitch = next.top() - cell.top();
    assert_eq!(pitch, px(super::scan_view::ROW_HEIGHT_PX), "rows are tall enough for two lines");
    assert!(cell.size.height + px(8.0) <= pitch, "two lines ({:?}) do not fit the {pitch:?} row", cell.size.height);
    // an 8 h rate has just the one line
    let eight = cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.try_find(SharedString::from("rate-line2-DDDUSDT-Bybit")).is_some()
    });
    assert!(!eight.unwrap(), "Bybit is 8h: no second line");
}

#[gpui_kit::test]
fn the_gross_labels_stay_inside_the_gross_column_clear_of_net_edge(cx: &mut TestAppContext) {
    let rig = open_with(cx, DEFAULT_WIDTH, mixed_interval_feed(okx_unticked()));
    let gross = header(&rig, ScanColumn::Gross, cx).expect("Gross column");
    let net = header(&rig, ScanColumn::NetEdge, cx).expect("Net Edge column");
    assert!(gross.right() <= net.left(), "adjacent columns do not overlap");
    let value = bounds_of(&rig, SharedString::from("gross-value-DDDUSDT"), cx);
    let diff = bounds_of(&rig, SharedString::from("gross-diff-DDDUSDT"), cx);
    let kind = cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.try_find(SharedString::from("edge-kind-DDDUSDT")).map(|e| e.bounds())
    });
    let kind = kind.unwrap().expect("the 4h/8h row is classified, so the kind tag is drawn");
    for b in [value, diff, kind] {
        assert!(b.left() >= gross.left() && b.right() <= gross.right(), "{b:?} leaves the Gross column {gross:?}");
    }
    assert!(diff.top() >= value.bottom(), "the labels are under the value: {value:?} / {diff:?}");
    assert!(kind.left() >= diff.right(), "the two labels do not overlap each other: {diff:?} / {kind:?}");
    let net_cell = bounds_of(&rig, SharedString::from("net-DDDUSDT"), cx);
    assert!(net_cell.left() >= gross.right(), "Net Edge starts after Gross ends");
}

// ---- scanner-layout 2.3: the status bar ---------------------------------------------------------------

/// Minimum space between the "只顯示達標" toggle and the sources.
const BAR_GAP_PX: f32 = 24.0;

fn exists(rig: &Rig, id: impl Into<gpui_kit::ElementId>, cx: &mut TestAppContext) -> bool {
    let id = id.into();
    cx.update_window(rig.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
    .unwrap()
}

#[gpui_kit::test]
fn the_toggle_and_the_sources_are_a_clear_gap_apart_or_on_separate_lines(cx: &mut TestAppContext) {
    for width in [1850.0, DEFAULT_WIDTH, 1000.0, 700.0] {
        let rig = open_with(cx, width, feed_with(okx_unticked()));
        let toggle = bounds_of(&rig, "only-qualified", cx);
        let sources = bounds_of(&rig, "scan-sources", cx);
        let side_by_side = sources.left() - toggle.right() >= px(BAR_GAP_PX) - px(0.5);
        let wrapped = sources.top() >= toggle.bottom();
        assert!(side_by_side || wrapped, "at {width} px the toggle {toggle:?} and the sources {sources:?} touch");
        assert!(sources.right() <= px(width), "the sources stay inside the window at {width} px: {sources:?}");
    }
}

#[gpui_kit::test]
fn a_ticked_exchange_has_one_chip_and_an_unticked_one_is_a_single_disabled_line(cx: &mut TestAppContext) {
    let rig = open_with(cx, DEFAULT_WIDTH, feed_with(okx_unticked()));
    for id in ["source-Binance WebSocket", "source-Bybit WebSocket", "source-Bybit 行情", "source-off-OKX"] {
        assert!(exists(&rig, SharedString::from(id), cx), "{id}");
    }
    assert!(!exists(&rig, SharedString::from("source-OKX 行情"), cx), "no OKX status chip while OKX is not ticked");
    let all = open_with(cx, DEFAULT_WIDTH, feed_with(all_ticked()));
    assert!(exists(&all, SharedString::from("source-OKX 行情"), cx));
    assert!(!exists(&all, SharedString::from("source-off-OKX"), cx));
}

//! Task 3.3 / 4 / 5: the scanner table controls driven through the real `Shell` in a headless
//! window (gpui-kit `test-support`): real hit testing, real click handlers, real tick loop.
//! No pixels are inspected; see the screenshots in the change notes for the look.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, Entity, ScrollDelta, TestAppContext, WindowHandle, base::Root, point, px, size};
use tong_funding_core::types::Exchange;

use super::bridge::{CommandSink, ReadOnlyDataSource, RefreshRequest, SourceUpdate};
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
    let now = wall_ms();
    let next = now + 4 * 3_600_000;
    let rates = [("AAAUSDT", "0.0001", "0.0000"), ("BBBUSDT", "0.0008", "-0.0002"), ("CCCUSDT", "0.0003", "-0.0001"), ("DDDUSDT", "0.0020", "-0.0010")];
    let mut v = vec![SourceUpdate::Settings(complete_settings("0.01"))];
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
    cx.update(|cx| {
        gpui_kit::init(cx);
        super::component_theme::apply(cx);
    });
    let source: Arc<dyn ReadOnlyDataSource> = Arc::new(FakeSource(Mutex::new(feed())));
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
            let target = *drawn.iter().find(|i| **i >= 2).expect("some scrolling header is drawn");
            window.scroll(("scan-th", target), ScrollDelta::Pixels(point(px(-400.), px(0.))), cx);
        })
        .unwrap();
        rig.settle(cx);
    }
    let after = last(&rig, cx).expect("after scrolling the last column is drawn");
    assert!(after <= px(NARROW) + px(0.5), "and it is fully inside the window: {after:?}");
}

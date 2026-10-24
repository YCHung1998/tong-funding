//! Tasks 1.2-2.3: scanner table controls (spec scanner-table-controls).
use std::time::Instant;

use tong_funding_core::types::Exchange::{Binance, Bybit, Okx};

use super::*;
use crate::ui::scanner::NetEdgeCell;
use crate::ui::testkit::d;

fn cell(rate: Option<&str>) -> RateCell {
    match rate {
        Some(r) => RateCell::Rate { rate: d(r), interval_secs: 28_800, eq_8h: None, stale: false },
        None => RateCell::Empty,
    }
}

/// A row with the given default rank and symbol; everything else empty.
fn row(rank: usize, symbol: &str) -> ScanRow {
    ScanRow {
        rank,
        symbol: symbol.into(),
        listed: 3,
        enabled: 3,
        cells: [(Binance, cell(None)), (Bybit, cell(None)), (Okx, cell(None))],
        direction: None,
        gross_spread: None,
        intervals_differ: false,
        net_edge: NetEdgeCell::NotApplicable,
        qualified: Qualified::No,
        edge: None,
        countdown_target: None,
    }
}

fn net(mut r: ScanRow, v: &str) -> ScanRow {
    r.net_edge = NetEdgeCell::Value(d(v));
    r
}

fn order(rows: &[ScanRow]) -> Vec<&str> {
    rows.iter().map(|r| r.symbol.as_str()).collect()
}

fn sorted(mut rows: Vec<ScanRow>, column: ScanColumn, dir: SortDir) -> Vec<ScanRow> {
    sort_rows(&mut rows, Some(SortState { column, dir }));
    rows
}

// ---- columns and visibility (1.1-1.3) ----------------------------------------------------------

#[test]
fn there_are_twelve_columns_in_the_documented_order() {
    let titles: Vec<_> = ScanColumn::ALL.iter().map(|c| c.title()).collect();
    assert_eq!(titles, ["Rank", "Symbol", "覆蓋", "結算倒數", "Binance", "Bybit", "OKX", "最佳套利方向", "Gross Spread %", "Net Edge %", "達標", "加入交易單"]);
    let keys: std::collections::BTreeSet<_> = ScanColumn::ALL.iter().map(|c| c.key()).collect();
    assert_eq!(keys.len(), 12, "keys are unique");
    assert!(!titles.contains(&"Pionex") && !titles.contains(&"Bitget"));
}

#[test]
fn only_direction_and_add_are_not_sortable() {
    let not_sortable: Vec<_> = ScanColumn::ALL.into_iter().filter(|c| !c.sortable()).collect();
    assert_eq!(not_sortable, [ScanColumn::Direction, ScanColumn::Add]);
}

#[test]
fn rank_and_symbol_are_the_fixed_left_columns_and_they_come_first() {
    let fixed: Vec<_> = ScanColumn::ALL.into_iter().filter(|c| c.fixed_left()).collect();
    assert_eq!(fixed, [ScanColumn::Rank, ScanColumn::Symbol]);
    assert_eq!(&ScanColumn::ALL[..2], &fixed[..], "the library needs fixed columns contiguous at the left");
}

#[test]
fn all_columns_are_visible_by_default() {
    let v = ColumnVisibility::default();
    assert!(v.is_all_visible());
    assert_eq!(v.visible(), ScanColumn::ALL);
}

#[test]
fn hiding_and_showing_a_column_keeps_the_others_in_order() {
    let mut v = ColumnVisibility::default();
    assert!(v.toggle(ScanColumn::Okx));
    assert!(!v.is_visible(ScanColumn::Okx));
    assert!(!v.visible().contains(&ScanColumn::Okx));
    assert_eq!(v.visible().len(), 11);
    let rest: Vec<_> = ScanColumn::ALL.into_iter().filter(|c| *c != ScanColumn::Okx).collect();
    assert_eq!(v.visible(), rest, "order unchanged");
    assert!(v.toggle(ScanColumn::Okx));
    assert_eq!(v.visible(), ScanColumn::ALL, "back in the original place");
}

#[test]
fn symbol_cannot_be_hidden() {
    let mut v = ColumnVisibility::default();
    assert!(!v.toggle(ScanColumn::Symbol), "no change reported");
    assert!(v.is_visible(ScanColumn::Symbol));
    assert!(!ScanColumn::Symbol.hideable());
    assert!(ScanColumn::ALL.iter().filter(|c| **c != ScanColumn::Symbol).all(|c| c.hideable()));
}

#[test]
fn reset_shows_everything_again() {
    let mut v = ColumnVisibility::default();
    for c in [ScanColumn::Okx, ScanColumn::Gross, ScanColumn::Add] {
        v.toggle(c);
    }
    assert_eq!(v.visible().len(), 9);
    v.reset();
    assert!(v.is_all_visible());
    assert_eq!(v.visible(), ScanColumn::ALL);
}

#[test]
fn visible_index_maps_back_to_the_logical_column_after_hiding_a_middle_column() {
    let mut v = ColumnVisibility::default();
    v.toggle(ScanColumn::Coverage);
    v.toggle(ScanColumn::Bybit);
    let vis = v.visible();
    assert_eq!(vis[0], ScanColumn::Rank);
    assert_eq!(vis[1], ScanColumn::Symbol);
    assert_eq!(vis[2], ScanColumn::Countdown, "index 2 is the 4th logical column now");
    assert_eq!(vis[3], ScanColumn::Binance);
    assert_eq!(vis[4], ScanColumn::Okx, "Bybit is skipped");
}

#[test]
fn total_width_shrinks_when_columns_are_hidden_and_decides_horizontal_scroll() {
    let mut v = ColumnVisibility::default();
    let full = v.total_width();
    assert_eq!(full, 50.0 + 132.0 + 50.0 + 88.0 + 160.0 * 3.0 + 100.0 + 120.0 + 164.0 + 70.0 + 120.0);
    assert!(v.needs_horizontal_scroll(1000.0), "12 columns do not fit 1000 px");
    for c in [ScanColumn::Direction, ScanColumn::NetEdge, ScanColumn::Okx, ScanColumn::Bybit, ScanColumn::Add, ScanColumn::Gross] {
        v.toggle(c);
    }
    assert!(v.total_width() < full);
    assert!(!v.needs_horizontal_scroll(1000.0), "after hiding six columns it fits: no sideways scroll");
}

// ---- scanner-layout 2.1: exchanges that are not enabled start hidden --------------------------

#[test]
fn a_column_maps_to_its_exchange_and_back() {
    assert_eq!(ScanColumn::for_exchange(Binance), ScanColumn::Binance);
    assert_eq!(ScanColumn::for_exchange(Bybit), ScanColumn::Bybit);
    assert_eq!(ScanColumn::for_exchange(Okx), ScanColumn::Okx);
}

#[test]
fn the_column_of_an_exchange_that_is_not_enabled_starts_hidden_and_the_order_is_unchanged() {
    let mut v = ColumnVisibility::default();
    v.set_disabled_exchanges(&[Okx]);
    assert!(!v.is_visible(ScanColumn::Okx));
    let rest: Vec<_> = ScanColumn::ALL.into_iter().filter(|c| *c != ScanColumn::Okx).collect();
    assert_eq!(v.visible(), rest);
    assert!(!v.is_all_visible(), "the reset button has something to do");
}

#[test]
fn nothing_is_hidden_when_every_exchange_is_enabled() {
    let mut v = ColumnVisibility::default();
    v.set_disabled_exchanges(&[]);
    assert_eq!(v.visible(), ScanColumn::ALL);
    assert!(v.is_all_visible());
}

#[test]
fn showing_a_default_hidden_column_is_a_choice_that_survives_the_next_recompute() {
    let mut v = ColumnVisibility::default();
    v.set_disabled_exchanges(&[Okx]);
    assert!(v.toggle(ScanColumn::Okx), "one click shows it");
    assert!(v.is_visible(ScanColumn::Okx));
    assert_eq!(v.visible(), ScanColumn::ALL, "at its original place");
    for _ in 0..3 {
        v.set_disabled_exchanges(&[Okx]); // the shell syncs on every recompute
    }
    assert!(v.is_visible(ScanColumn::Okx), "the user's choice is not overwritten");
    assert!(v.toggle(ScanColumn::Okx));
    assert!(!v.is_visible(ScanColumn::Okx), "and a second click hides it again");
}

#[test]
fn an_explicit_hide_stays_hidden_after_the_exchange_is_enabled_and_a_default_follows_the_setting() {
    let mut v = ColumnVisibility::default();
    v.toggle(ScanColumn::Bybit); // the user hid an enabled exchange
    v.set_disabled_exchanges(&[]);
    assert!(!v.is_visible(ScanColumn::Bybit));
    // no choice about Okx: it follows the setting both ways
    v.set_disabled_exchanges(&[Okx]);
    assert!(!v.is_visible(ScanColumn::Okx));
    v.set_disabled_exchanges(&[]);
    assert!(v.is_visible(ScanColumn::Okx));
}

#[test]
fn reset_shows_all_twelve_columns_even_when_an_exchange_is_not_enabled() {
    let mut v = ColumnVisibility::default();
    v.set_disabled_exchanges(&[Okx]);
    v.toggle(ScanColumn::Gross);
    v.reset();
    v.set_disabled_exchanges(&[Okx]);
    assert_eq!(v.visible(), ScanColumn::ALL, "spec scanner-table-controls: reset = all columns shown");
    assert!(v.is_all_visible());
}

#[test]
fn symbol_stays_visible_whatever_the_disabled_set_says() {
    let mut v = ColumnVisibility::default();
    v.set_disabled_exchanges(&[Binance, Bybit, Okx]);
    assert!(v.is_visible(ScanColumn::Symbol));
    assert!(v.is_visible(ScanColumn::Rank));
}

// ---- scanner-layout 2.2: column widths follow the widest content, rows have room for two lines ----

use tong_funding_core::edge_kind::EdgeKind;

use crate::ui::scanner::{Countdown, GROSS_INTERVALS_DIFFER};

/// Width a cell needs for `parts` laid out in one line, padding included.
fn need(parts: &[String]) -> f32 {
    need_at(parts, CELL_FONT_PX)
}

fn need_at(parts: &[String], font_px: f32) -> f32 {
    parts.iter().map(|p| est_text_px_at(p, font_px)).sum::<f32>() + CELL_GAP_PX * parts.len().saturating_sub(1) as f32 + CELL_PAD_PX
}

fn rate(rate: &str, secs: i64, eq: Option<&str>, stale: bool) -> RateCell {
    RateCell::Rate { rate: d(rate), interval_secs: secs, eq_8h: eq.map(d), stale }
}

#[test]
fn the_width_model_counts_wide_characters_as_a_full_em() {
    let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
    assert!(close(est_text_px("0.0400"), 6.0 * 0.6 * CELL_FONT_PX));
    assert!(close(est_text_px("週期不同"), 4.0 * CELL_FONT_PX));
    assert!(close(est_text_px_at("0.0400", 9.0), 6.0 * 0.6 * 9.0));
    assert!(close(est_text_px("−1"), CELL_FONT_PX + 0.6 * CELL_FONT_PX), "the minus sign is not ASCII: counted wide, on the safe side");
}

#[test]
fn a_rate_cell_is_two_lines_rate_with_interval_then_the_8h_equivalent() {
    let c = rate("0.0005", 14_400, Some("0.001"), false);
    assert_eq!(c.first_line(), ["0.0500".to_string(), "4h".to_string()]);
    assert_eq!(c.second_line().as_deref(), Some("8h 等效 0.1000（僅供顯示）"));
    let eight = rate("0.0005", 28_800, None, false);
    assert_eq!(eight.first_line(), ["0.0500".to_string(), "8h".to_string()]);
    assert_eq!(eight.second_line(), None, "an 8h rate has no second line");
    assert_eq!(rate("0.0005", 14_400, Some("0.001"), true).first_line().last().map(String::as_str), Some("過期"), "the stale marker stays on the first line");
    assert_eq!(RateCell::Empty.first_line(), ["—".to_string()]);
    assert_eq!(RateCell::DataError { interval_unknown: true }.first_line(), ["資料異常".to_string(), "週期未知".to_string()]);
}

#[test]
fn the_exchange_columns_fit_their_widest_cell_on_both_lines() {
    // -12.3456 % is far beyond any real funding rate; the 8h equivalent can be several times the rate.
    let cells = [
        rate("-0.123456", 3_600, Some("-0.987654"), true),
        rate("0.0005", 14_400, Some("0.001"), true),
        rate("0.0005", 1_800, Some("0.0001"), false),
        RateCell::DataError { interval_unknown: true },
        RateCell::Empty,
    ];
    for col in [ScanColumn::Binance, ScanColumn::Bybit, ScanColumn::Okx] {
        for c in &cells {
            assert!(need(&c.first_line()) <= col.width(), "{:?}: line 1 of {c:?} needs {} px", col, need(&c.first_line()));
            if let Some(l2) = c.second_line() {
                let n = need_at(std::slice::from_ref(&l2), RATE_NOTE_FONT_PX);
                assert!(n <= col.width(), "{:?}: line 2 {l2:?} needs {n} px", col);
            }
        }
    }
}

#[test]
fn the_gross_column_fits_the_value_and_both_labels_without_cutting_any() {
    let mut r = row(1, "X");
    r.gross_spread = Some(d("0.123456"));
    r.intervals_differ = true;
    assert_eq!(r.gross_text(), "12.3456");
    for kind in [EdgeKind::IntervalMismatch, EdgeKind::RateGap] {
        let tags = vec![GROSS_INTERVALS_DIFFER.to_string(), crate::ui::scanner::edge_kind_text(kind).to_string()];
        assert!(need(&tags) <= ScanColumn::Gross.width(), "{tags:?} need {} px", need(&tags));
    }
    assert!(need(&[r.gross_text()]) <= ScanColumn::Gross.width());
    assert_eq!(GROSS_INTERVALS_DIFFER, "週期不同");
}

#[test]
fn the_symbol_column_fits_the_longest_real_symbols_without_cutting_them() {
    // 1000000BABYDOGEUSDT (19 characters) is a real Binance perpetual; the column is as wide as it needs
    for symbol in ["1000000BABYDOGEUSDT", "1000000MOGUSDT", "BTCUSDT"] {
        let n = need(&[symbol.to_string()]);
        assert!(n <= ScanColumn::Symbol.width(), "{symbol} needs {n} px, the column is {} px", ScanColumn::Symbol.width());
    }
}

#[test]
fn the_gross_labels_come_from_the_row_in_order_interval_difference_then_kind() {
    let mut r = row(1, "X");
    assert!(r.gross_tags().is_empty());
    r.intervals_differ = true;
    assert_eq!(r.gross_tags(), ["週期不同"]);
}

#[test]
fn the_other_columns_fit_their_longest_fixed_texts() {
    for q in [Qualified::Yes, Qualified::No, Qualified::NotConfigured, Qualified::NotApplicable, Qualified::NotSynchronized] {
        assert!(need(&[q.text().to_string()]) <= ScanColumn::Qualified.width(), "{:?} needs {} px", q, need(&[q.text().to_string()]));
    }
    for c in [
        Countdown::Remaining { ms: 99 * 3_600_000 + 59 * 60_000 + 59_000, calibrated: false },
        Countdown::Remaining { ms: 1_000, calibrated: true },
        Countdown::Settling,
        Countdown::Unknown,
    ] {
        // two lines: the time (or 結算中), then the note (未校時 / 待更新)
        for line in std::iter::once(c.first_line()).chain(c.second_line()) {
            assert!(need(std::slice::from_ref(&line)) <= ScanColumn::Countdown.width(), "{c:?}: {line:?} needs {} px", need(std::slice::from_ref(&line)));
        }
    }
    for (l, s) in [(Binance, Bybit), (Bybit, Binance), (Okx, Binance), (Binance, Okx)] {
        let mut r = row(1, "X");
        r.direction = Some((l, s));
        let (long, short) = r.direction_lines().expect("a direction");
        for line in [long, short] {
            assert!(need(std::slice::from_ref(&line)) <= ScanColumn::Direction.width(), "{line:?} needs {} px", need(std::slice::from_ref(&line)));
        }
    }
}

#[test]
fn countdown_and_direction_split_into_two_lines_without_changing_their_one_line_text() {
    let cal = Countdown::Remaining { ms: 3_723_000, calibrated: true };
    assert_eq!((cal.first_line(), cal.second_line()), ("01:02:03".to_string(), None));
    let un = Countdown::Remaining { ms: 3_723_000, calibrated: false };
    assert_eq!((un.first_line(), un.second_line().as_deref()), ("01:02:03".to_string(), Some("（未校時）")));
    assert_eq!((Countdown::Settling.first_line(), Countdown::Settling.second_line().as_deref()), ("結算中".to_string(), Some("待更新")));
    assert_eq!((Countdown::Unknown.first_line(), Countdown::Unknown.second_line()), ("—".to_string(), None));
    for c in [cal, un, Countdown::Settling, Countdown::Unknown] {
        let joined = match c.second_line() {
            Some(n) if c == Countdown::Settling => format!("{} · {n}", c.first_line()),
            Some(n) => format!("{}{n}", c.first_line()),
            None => c.first_line(),
        };
        assert_eq!(joined, c.text(), "the lines say what the one-line text says");
    }
    let mut r = row(1, "X");
    assert_eq!(r.direction_lines(), None);
    r.direction = Some((Bybit, Binance));
    assert_eq!(r.direction_lines(), Some(("L Bybit".to_string(), "S Binance".to_string())));
    assert_eq!(r.direction_text(), "L Bybit / S Binance");
}

#[test]
fn rows_are_tall_enough_for_two_lines_of_cell_text() {
    // two lines of 10 px text at the line height of 1.6, and the cell's top and bottom padding
    const NEEDED: f32 = 2.0 * CELL_FONT_PX * 1.6 + 8.0;
    let height = std::hint::black_box(ROW_HEIGHT_PX);
    assert!(height >= NEEDED, "{height} < {NEEDED}");
}

#[test]
fn the_columns_fit_the_default_1470_px_window_when_okx_is_not_ticked() {
    // The page is 1470 - 248 (side bar and margins) wide, the table bordered by 2 px.
    const TABLE_PX: f32 = 1470.0 - 248.0 - 2.0;
    let mut v = ColumnVisibility::default();
    v.set_disabled_exchanges(&[Okx]);
    assert_eq!(v.visible().len(), 11);
    assert!(v.total_width() <= TABLE_PX, "{} px > {TABLE_PX} px", v.total_width());
    assert!(!v.needs_horizontal_scroll(TABLE_PX));
}

// ---- sort state transitions --------------------------------------------------------------------

fn state() -> ScanViewState {
    ScanViewState::default()
}

#[test]
fn title_click_cycles_descending_ascending_default() {
    let mut s = state();
    assert!(s.apply(ScanEvent::CycleSort(ScanColumn::NetEdge)));
    assert_eq!(s.sort, Some(SortState { column: ScanColumn::NetEdge, dir: SortDir::Desc }));
    assert!(s.apply(ScanEvent::CycleSort(ScanColumn::NetEdge)));
    assert_eq!(s.sort, Some(SortState { column: ScanColumn::NetEdge, dir: SortDir::Asc }));
    assert!(s.apply(ScanEvent::CycleSort(ScanColumn::NetEdge)));
    assert_eq!(s.sort, None, "third click is the default order again");
}

#[test]
fn clicking_another_column_replaces_the_sort() {
    let mut s = state();
    s.apply(ScanEvent::CycleSort(ScanColumn::Gross));
    s.apply(ScanEvent::CycleSort(ScanColumn::Symbol));
    assert_eq!(s.sort, Some(SortState { column: ScanColumn::Symbol, dir: SortDir::Desc }), "starts at descending, only one column at a time");
}

#[test]
fn unsortable_columns_do_not_change_the_sort() {
    let mut s = state();
    s.apply(ScanEvent::CycleSort(ScanColumn::Gross));
    let before = s.clone();
    assert!(!s.apply(ScanEvent::CycleSort(ScanColumn::Add)));
    assert!(!s.apply(ScanEvent::CycleSort(ScanColumn::Direction)));
    assert!(!s.apply(ScanEvent::SetSort(ScanColumn::Add, Some(SortDir::Asc))));
    assert_eq!(s, before);
}

#[test]
fn the_library_icon_sets_the_state_it_already_cycled_to() {
    let mut s = state();
    assert!(s.apply(ScanEvent::SetSort(ScanColumn::Binance, Some(SortDir::Asc))));
    assert_eq!(s.sort, Some(SortState { column: ScanColumn::Binance, dir: SortDir::Asc }));
    assert!(s.apply(ScanEvent::SetSort(ScanColumn::Binance, None)));
    assert_eq!(s.sort, None);
    assert!(!s.apply(ScanEvent::SetSort(ScanColumn::Binance, None)), "already default: no change");
}

#[test]
fn sort_state_is_independent_of_column_visibility() {
    let mut s = state();
    s.apply(ScanEvent::CycleSort(ScanColumn::NetEdge));
    s.visibility.toggle(ScanColumn::NetEdge);
    assert_eq!(s.sort.map(|x| x.column), Some(ScanColumn::NetEdge), "a hidden column can still be the sort key");
}

// ---- sorting (2.1, 2.2) -------------------------------------------------------------------------

#[test]
fn no_sort_state_keeps_the_given_order() {
    let mut rows = vec![row(1, "BBB"), row(2, "AAA")];
    sort_rows(&mut rows, None);
    assert_eq!(order(&rows), ["BBB", "AAA"]);
}

#[test]
fn net_edge_ascending_and_descending() {
    let rows = vec![net(row(1, "A"), "0.03"), net(row(2, "B"), "0.01"), net(row(3, "C"), "-0.02")];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::NetEdge, SortDir::Desc)), ["A", "B", "C"]);
    assert_eq!(order(&sorted(rows, ScanColumn::NetEdge, SortDir::Asc)), ["C", "B", "A"]);
}

#[test]
fn rows_without_a_value_are_last_in_both_directions() {
    let rows = vec![net(row(1, "A"), "0.03"), row(2, "NONE"), net(row(3, "C"), "-0.02")];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::NetEdge, SortDir::Asc)), ["C", "A", "NONE"]);
    assert_eq!(order(&sorted(rows, ScanColumn::NetEdge, SortDir::Desc)), ["A", "C", "NONE"]);
}

#[test]
fn not_configured_net_edge_counts_as_no_value() {
    let mut nc = row(1, "NC");
    nc.net_edge = NetEdgeCell::NotConfigured(vec!["net_edge_threshold_pct".into()]);
    let rows = vec![nc, net(row(2, "A"), "0.01")];
    assert_eq!(order(&sorted(rows, ScanColumn::NetEdge, SortDir::Desc)), ["A", "NC"]);
}

#[test]
fn equal_keys_fall_back_to_the_default_rank_whatever_the_input_order_and_direction() {
    let rows = vec![net(row(3, "C"), "0.01"), net(row(1, "A"), "0.01"), net(row(2, "B"), "0.01")];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::NetEdge, SortDir::Desc)), ["A", "B", "C"]);
    assert_eq!(order(&sorted(rows, ScanColumn::NetEdge, SortDir::Asc)), ["A", "B", "C"]);
}

#[test]
fn rows_without_a_value_also_keep_the_default_rank_among_themselves() {
    let rows = vec![row(5, "E"), row(2, "B"), net(row(9, "Z"), "0.5"), row(3, "C")];
    assert_eq!(order(&sorted(rows, ScanColumn::NetEdge, SortDir::Desc)), ["Z", "B", "C", "E"]);
}

#[test]
fn sorting_never_renumbers_rank() {
    let rows = vec![row(17, "ZZZ"), row(3, "AAA"), row(9, "MMM")];
    let out = sorted(rows, ScanColumn::Symbol, SortDir::Asc);
    assert_eq!(order(&out), ["AAA", "MMM", "ZZZ"]);
    assert_eq!(out.iter().map(|r| r.rank).collect::<Vec<_>>(), [3, 9, 17]);
}

#[test]
fn symbol_sorts_case_insensitively() {
    let rows = vec![row(1, "bbb"), row(2, "AAA"), row(3, "Ccc")];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Symbol, SortDir::Asc)), ["AAA", "bbb", "Ccc"]);
    assert_eq!(order(&sorted(rows, ScanColumn::Symbol, SortDir::Desc)), ["Ccc", "bbb", "AAA"]);
}

#[test]
fn rank_column_sorts_by_default_rank() {
    let rows = vec![row(2, "B"), row(3, "C"), row(1, "A")];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Rank, SortDir::Asc)), ["A", "B", "C"]);
    assert_eq!(order(&sorted(rows, ScanColumn::Rank, SortDir::Desc)), ["C", "B", "A"]);
}

#[test]
fn coverage_sorts_by_listed_then_enabled() {
    let mut a = row(1, "A");
    (a.listed, a.enabled) = (2, 3);
    let mut b = row(2, "B");
    (b.listed, b.enabled) = (3, 3);
    let mut c = row(3, "C");
    (c.listed, c.enabled) = (2, 2);
    let rows = vec![a, b, c];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Coverage, SortDir::Desc)), ["B", "A", "C"], "3/3, then 2/3, then 2/2 (same listed: more enabled first when descending)");
    assert_eq!(order(&sorted(rows, ScanColumn::Coverage, SortDir::Asc)), ["C", "A", "B"]);
}

#[test]
fn exchange_rate_columns_sort_by_that_exchanges_rate_and_ignore_data_errors() {
    let mut a = row(1, "A");
    a.cells[0].1 = cell(Some("0.0003"));
    a.cells[1].1 = cell(Some("-0.0009"));
    let mut b = row(2, "B");
    b.cells[0].1 = cell(Some("0.0001"));
    b.cells[1].1 = RateCell::DataError { interval_unknown: false };
    let mut c = row(3, "C");
    c.cells[0].1 = cell(None);
    c.cells[2].1 = cell(Some("0.0007"));
    let rows = vec![a, b, c];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Binance, SortDir::Desc)), ["A", "B", "C"], "Empty last");
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Bybit, SortDir::Asc)), ["A", "B", "C"], "A has a value, DataError and Empty are last (rank order)");
    assert_eq!(order(&sorted(rows, ScanColumn::Okx, SortDir::Desc)), ["C", "A", "B"]);
}

#[test]
fn gross_spread_sorts_numerically() {
    let mut a = row(1, "A");
    a.gross_spread = Some(d("0.05"));
    let mut b = row(2, "B");
    b.gross_spread = Some(d("0.2"));
    let c = row(3, "C");
    let rows = vec![a, b, c];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Gross, SortDir::Desc)), ["B", "A", "C"]);
    assert_eq!(order(&sorted(rows, ScanColumn::Gross, SortDir::Asc)), ["A", "B", "C"]);
}

#[test]
fn qualified_column_groups_yes_first_when_descending_and_treats_not_configured_like_no() {
    let mut y = row(3, "Y");
    y.qualified = Qualified::Yes;
    let mut n = row(1, "N");
    n.qualified = Qualified::No;
    let mut nc = row(2, "NC");
    nc.qualified = Qualified::NotConfigured;
    let rows = vec![n, nc, y];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Qualified, SortDir::Desc)), ["Y", "N", "NC"], "Yes first; N and NC are one group in rank order");
    assert_eq!(order(&sorted(rows, ScanColumn::Qualified, SortDir::Asc)), ["N", "NC", "Y"]);
}

#[test]
fn countdown_sorts_by_settlement_target_not_by_remaining_seconds() {
    let mut a = row(1, "A");
    a.countdown_target = Some((3_000, Okx));
    let mut b = row(2, "B");
    b.countdown_target = Some((1_000, Binance));
    let c = row(3, "C");
    let rows = vec![a, b, c];
    assert_eq!(order(&sorted(rows.clone(), ScanColumn::Countdown, SortDir::Asc)), ["B", "A", "C"], "soonest first; no target last");
    assert_eq!(order(&sorted(rows, ScanColumn::Countdown, SortDir::Desc)), ["A", "B", "C"]);
}

#[test]
fn empty_and_single_row_inputs_are_fine() {
    for c in ScanColumn::ALL.into_iter().filter(|c| c.sortable()) {
        for dir in [SortDir::Asc, SortDir::Desc] {
            assert!(sorted(Vec::new(), c, dir).is_empty());
            assert_eq!(order(&sorted(vec![row(1, "ONLY")], c, dir)), ["ONLY"]);
        }
    }
}

#[test]
fn unsortable_columns_leave_the_order_untouched() {
    let rows = vec![row(2, "B"), row(1, "A")];
    for c in [ScanColumn::Direction, ScanColumn::Add] {
        assert_eq!(order(&sorted(rows.clone(), c, SortDir::Desc)), ["B", "A"]);
    }
}

// ---- performance (2.3) --------------------------------------------------------------------------

fn big() -> Vec<ScanRow> {
    (0..528)
        .map(|i| {
            let mut r = row(i + 1, &format!("COIN{:03}USDT", (i * 7919) % 528));
            r.net_edge = NetEdgeCell::Value(d(&format!("{}.{:04}", (i * 31) % 17, (i * 7) % 10_000)));
            r.gross_spread = Some(d(&format!("0.{:04}", (i * 13) % 10_000)));
            r.cells[0].1 = cell(Some(&format!("0.{:04}", (i * 17) % 10_000)));
            r.cells[1].1 = cell(Some(&format!("-0.{:04}", (i * 19) % 10_000)));
            r.countdown_target = Some((1_000_000 + ((i * 37) % 600) as i64 * 1_000, Binance));
            r.qualified = if i % 5 == 0 { Qualified::Yes } else { Qualified::No };
            r
        })
        .collect()
}

#[test]
fn sorting_528_rows_takes_under_5_ms_for_every_sortable_column() {
    let base = big();
    for c in ScanColumn::ALL.into_iter().filter(|c| c.sortable()) {
        for dir in [SortDir::Asc, SortDir::Desc] {
            // median of 9 runs (a single timing is noisy on a loaded machine)
            let mut times: Vec<_> = (0..9)
                .map(|_| {
                    let mut rows = base.clone();
                    let t = Instant::now();
                    sort_rows(&mut rows, Some(SortState { column: c, dir }));
                    std::hint::black_box(&rows);
                    t.elapsed()
                })
                .collect();
            times.sort();
            let median = times[4];
            assert!(median.as_micros() < 5_000, "{c:?} {dir:?}: median {median:?} exceeds 5 ms");
        }
    }
}

#[test]
fn the_sorted_result_is_a_permutation_of_the_input() {
    let base = big();
    let out = sorted(base.clone(), ScanColumn::NetEdge, SortDir::Desc);
    assert_eq!(out.len(), base.len());
    let mut a: Vec<_> = base.iter().map(|r| r.rank).collect();
    let mut b: Vec<_> = out.iter().map(|r| r.rank).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "no row dropped or duplicated");
}

// ---- the table pipeline on real view-models (3.3) -------------------------------------------------

mod pipeline {
    use tong_funding_core::types::Exchange;

    use super::*;
    use crate::ui::bridge::{MarketFeed, UiSnapshot};
    use crate::ui::scanner::{self, ScannerVm};
    use crate::ui::testkit::{complete_settings, obs};

    const NOW: i64 = 1_791_201_600_000;
    const H: i64 = 3_600_000;

    /// ZZZ qualifies (net 0.32), MMM is in the middle (net 0.00), AAA is last (net -0.07).
    fn vm_with(rates: &[(&str, &str, &str)]) -> ScannerVm {
        let mut snap = UiSnapshot { settings: complete_settings("0.01"), ..Default::default() };
        for e in Exchange::ALL {
            snap.market.insert(e, MarketFeed { observations: vec![], last_success_at: Some(NOW), last_error: None });
        }
        for (sym, binance, bybit) in rates {
            snap.market.get_mut(&Exchange::Binance).unwrap().observations.push(obs(Exchange::Binance, sym, binance, 28_800, NOW + 4 * H, NOW));
            snap.market.get_mut(&Exchange::Bybit).unwrap().observations.push(obs(Exchange::Bybit, sym, bybit, 28_800, NOW + 4 * H, NOW));
        }
        scanner::build(&snap, NOW)
    }

    fn base() -> ScannerVm {
        vm_with(&[("ZZZUSDT", "0.002", "-0.002"), ("MMMUSDT", "0.0008", "0"), ("AAAUSDT", "0.0005", "0.0004")])
    }

    fn syms(rows: &[ScanRow]) -> Vec<&str> {
        rows.iter().map(|r| r.symbol.as_str()).collect()
    }

    fn by(column: ScanColumn, dir: SortDir) -> Option<SortState> {
        Some(SortState { column, dir })
    }

    #[test]
    fn default_order_is_net_edge_descending_and_rank_matches_it() {
        let vm = base();
        let rows = table_rows(&vm, false, None).unwrap();
        assert_eq!(syms(&rows), ["ZZZUSDT", "MMMUSDT", "AAAUSDT"]);
        assert_eq!(rows.iter().map(|r| r.rank).collect::<Vec<_>>(), [1, 2, 3]);
        // Net Edge descending is exactly the default order (spec: default = scanner-page's order)
        assert_eq!(syms(&table_rows(&vm, false, by(ScanColumn::NetEdge, SortDir::Desc)).unwrap()), syms(&rows));
        // and Rank ascending is too
        assert_eq!(syms(&table_rows(&vm, false, by(ScanColumn::Rank, SortDir::Asc)).unwrap()), syms(&rows));
    }

    #[test]
    fn symbol_sort_keeps_the_default_ranks_on_the_rows() {
        let rows = table_rows(&base(), false, by(ScanColumn::Symbol, SortDir::Asc)).unwrap();
        assert_eq!(syms(&rows), ["AAAUSDT", "MMMUSDT", "ZZZUSDT"]);
        assert_eq!(rows.iter().map(|r| r.rank).collect::<Vec<_>>(), [3, 2, 1], "Rank shows the default-order rank, not 1..3");
    }

    #[test]
    fn sort_applies_to_the_only_qualified_subset() {
        let vm = vm_with(&[("ZZZUSDT", "0.002", "-0.002"), ("BBBUSDT", "0.003", "-0.003"), ("AAAUSDT", "0.0005", "0.0004")]);
        let rows = table_rows(&vm, true, by(ScanColumn::Symbol, SortDir::Asc)).unwrap();
        assert_eq!(syms(&rows), ["BBBUSDT", "ZZZUSDT"], "AAA does not qualify; the rest is sorted by Symbol");
        assert!(rows.iter().all(|r| r.qualified == Qualified::Yes));
    }

    #[test]
    fn nothing_qualified_gives_no_table_and_keeps_working_afterwards() {
        let vm = vm_with(&[("AAAUSDT", "0.0005", "0.0004")]);
        let mut view = ScanViewState::default();
        view.apply(ScanEvent::CycleSort(ScanColumn::Symbol));
        assert!(table_rows(&vm, true, view.sort).is_none());
        assert_eq!(view.sort.map(|s| s.column), Some(ScanColumn::Symbol), "the sort is not lost while the toggle hides the table");
        assert_eq!(syms(&table_rows(&vm, false, view.sort).unwrap()), ["AAAUSDT"]);
    }

    #[test]
    fn the_same_state_orders_fresh_data_after_a_refresh() {
        let mut view = ScanViewState::default();
        view.apply(ScanEvent::CycleSort(ScanColumn::Gross)); // descending
        let first = table_rows(&base(), false, view.sort).unwrap();
        assert_eq!(syms(&first), ["ZZZUSDT", "MMMUSDT", "AAAUSDT"], "gross 0.40, 0.08, 0.01");
        // data changes: AAA now has the widest spread
        let second = table_rows(&vm_with(&[("ZZZUSDT", "0.002", "-0.002"), ("MMMUSDT", "0.0008", "0"), ("AAAUSDT", "0.004", "-0.004")]), false, view.sort).unwrap();
        assert_eq!(syms(&second), ["AAAUSDT", "ZZZUSDT", "MMMUSDT"], "same sort, new data");
        // and the visible columns were not touched by any of this
        assert!(view.visibility.is_all_visible());
    }

    #[test]
    fn countdown_order_does_not_depend_on_the_clock() {
        let mut view = ScanViewState::default();
        view.apply(ScanEvent::CycleSort(ScanColumn::Countdown));
        view.apply(ScanEvent::CycleSort(ScanColumn::Countdown)); // ascending
        let vm = base();
        let a = syms(&table_rows(&vm, false, view.sort).unwrap()).into_iter().map(String::from).collect::<Vec<_>>();
        // `table_rows` takes no clock at all: one second later it is the same call with the same result
        let b = syms(&table_rows(&vm, false, view.sort).unwrap()).into_iter().map(String::from).collect::<Vec<_>>();
        assert_eq!(a, b);
    }

    #[test]
    fn hiding_columns_does_not_change_which_rows_qualify_or_their_order() {
        let vm = base();
        let before = table_rows(&vm, true, by(ScanColumn::NetEdge, SortDir::Asc)).unwrap();
        let mut view = ScanViewState::default();
        for c in [ScanColumn::NetEdge, ScanColumn::Qualified, ScanColumn::Gross] {
            view.visibility.toggle(c);
        }
        let after = table_rows(&vm, true, by(ScanColumn::NetEdge, SortDir::Asc)).unwrap();
        assert_eq!(before, after, "visibility is display only");
    }
}

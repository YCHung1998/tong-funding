//! Task 2.4: scanner view-model (spec scanner-page).
use tong_funding_core::funding::DataStatus;
use tong_funding_core::types::Exchange::{self, Binance, Bybit, Okx};

use super::*;
use crate::ui::bridge::{ClockState, MarketFeed, Settings, UiSnapshot};
use crate::ui::testkit::{complete_settings, d, obs, with_status};

const NOW: i64 = 1_791_201_600_000; // 2026-10-05 12:00:00 UTC
const H: i64 = 3_600_000;
const T8: i64 = NOW + 4 * H; // 16:00

fn snap(settings: Settings, rows: &[(Exchange, &str, &str)]) -> UiSnapshot {
    let mut s = UiSnapshot { settings, ..Default::default() };
    for e in Exchange::ALL {
        s.market.insert(e, MarketFeed { observations: vec![], last_success_at: Some(NOW), last_error: None });
    }
    for (e, sym, rate) in rows {
        s.market.get_mut(e).unwrap().observations.push(obs(*e, sym, rate, 28_800, T8, NOW));
    }
    s
}

fn row<'a>(vm: &'a ScannerVm, sym: &str) -> &'a ScanRow {
    vm.rows.iter().find(|r| r.symbol == sym).unwrap_or_else(|| panic!("no row {sym}"))
}

fn cell(r: &ScanRow, e: Exchange) -> &RateCell {
    &r.cells.iter().find(|(x, _)| *x == e).unwrap().1
}

#[test]
fn columns_are_binance_bybit_okx_only() {
    let vm = build(&snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0001")]), NOW);
    let cols: Vec<_> = vm.rows[0].cells.iter().map(|(e, _)| e.name()).collect();
    assert_eq!(cols, ["Binance", "Bybit", "OKX"], "no Pionex / Bitget placeholder columns");
}

#[test]
fn core_hand_example_gross_0_04_and_net_minus_0_04_in_negative_tone() {
    // core net-edge example: Binance 0.0003, Bybit -0.0001, same settlement, fees 0.02, slippage/safety 0
    let vm = build(&snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0003"), (Bybit, "BTCUSDT", "-0.0001")]), NOW);
    let r = row(&vm, "BTCUSDT");
    assert_eq!(r.gross_text(), "0.0400");
    assert_eq!(r.net_edge.text(), "−0.0400");
    assert_eq!(r.net_edge.tone(), Tone::Negative);
    assert_eq!(r.direction_text(), "L Bybit / S Binance");
    assert_eq!(r.qualified, Qualified::No);
    assert_eq!(r.qualified.text(), "—");
}

#[test]
fn four_hour_interval_shows_tag_and_display_only_8h_equivalent() {
    let mut s = snap(complete_settings("0.01"), &[(Bybit, "BTCUSDT", "0.0001")]);
    s.market.get_mut(&Binance).unwrap().observations.push(obs(Binance, "BTCUSDT", "0.0005", 14_400, T8, NOW));
    let vm = build(&s, NOW);
    let r = row(&vm, "BTCUSDT");
    let c = cell(r, Binance);
    assert_eq!(c.text(), "0.0500");
    assert_eq!(c.tag().as_deref(), Some("4h"));
    assert!(c.notes().contains(&"8h 等效 0.1000（僅供顯示）".to_string()), "{:?}", c.notes());
    assert_eq!(cell(r, Bybit).tag().as_deref(), Some("8h"));
    assert!(cell(r, Bybit).notes().iter().all(|n| !n.contains("8h 等效")));
    assert!(r.intervals_differ, "4h vs 8h legs get the 週期不同 mark");
}

#[test]
fn missing_bybit_fee_shows_not_configured_with_the_missing_item_never_zero() {
    let mut settings = complete_settings("0.01");
    settings.risk.taker_fee_pct.remove(&Bybit);
    let vm = build(&snap(settings, &[(Binance, "BTCUSDT", "0.0003"), (Bybit, "BTCUSDT", "-0.0001"), (Binance, "ETHUSDT", "0.0001"), (Okx, "ETHUSDT", "0.0002")]), NOW);
    let r = row(&vm, "BTCUSDT");
    match &r.net_edge {
        NetEdgeCell::NotConfigured(m) => assert!(m.contains(&"Bybit taker_fee_pct".to_string()), "{m:?}"),
        other => panic!("expected 未設定, got {other:?}"),
    }
    assert!(r.net_edge.text().starts_with("未設定"));
    assert_eq!(r.qualified, Qualified::NotConfigured);
    assert_eq!(r.gross_text(), "0.0400", "Gross Spread is still shown");
    assert!(!vm.decidable);
    assert_eq!(vm.match_text(), "設定不完整，無法判斷達標");
    assert_eq!(vm.summary.qualified, None);
}

#[test]
fn okx_largest_spread_is_never_chosen_and_is_marked_compare_only() {
    let vm = build(&snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0001"), (Bybit, "BTCUSDT", "0.0002"), (Okx, "BTCUSDT", "-0.0050")]), NOW);
    let r = row(&vm, "BTCUSDT");
    let (l, s) = r.direction.unwrap();
    assert!(l != Okx && s != Okx, "direction only between Binance and Bybit");
    assert_eq!(r.gross_text(), "0.0100");
    assert!(cell(r, Okx).notes().contains(&"僅比價".to_string()));
    assert_eq!(cell(r, Okx).text(), "−0.5000");
    assert_eq!(r.coverage_text(), "3/3");
}

#[test]
fn one_tradable_exchange_gives_dashes_but_coverage_counts_okx() {
    let vm = build(&snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0001"), (Okx, "BTCUSDT", "0.0009")]), NOW);
    let r = row(&vm, "BTCUSDT");
    assert_eq!((r.gross_text(), r.net_edge.text(), r.direction_text(), r.qualified.text()), ("—".into(), "—".into(), "—".into(), "—"));
    assert_eq!(r.coverage_text(), "2/3");
}

#[test]
fn data_error_cell_shows_anomaly_and_is_excluded_from_pairing() {
    let mut s = snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0003")]);
    s.market.get_mut(&Bybit).unwrap().observations.push(with_status(obs(Bybit, "BTCUSDT", "-0.0100", 28_800, T8, NOW), DataStatus::DataError));
    s.market.get_mut(&Bybit).unwrap().observations[0].funding_interval_secs = None;
    let vm = build(&s, NOW);
    let r = row(&vm, "BTCUSDT");
    assert_eq!(cell(r, Bybit).text(), "資料異常");
    assert_eq!(cell(r, Bybit).tag().as_deref(), Some("週期未知"));
    assert_eq!(r.net_edge, NetEdgeCell::NotApplicable);
    assert_eq!(r.coverage_text(), "1/3");
}

#[test]
fn not_listed_or_missing_cells_show_a_dash() {
    let mut s = snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0003")]);
    s.market.get_mut(&Bybit).unwrap().observations.push(with_status(obs(Bybit, "BTCUSDT", "0", 28_800, T8, NOW), DataStatus::NotListed));
    let vm = build(&s, NOW);
    let r = row(&vm, "BTCUSDT");
    assert_eq!(cell(r, Bybit).text(), "—");
    assert_eq!(cell(r, Okx).text(), "—");
}

#[test]
fn allowed_coins_limit_the_rows() {
    let mut settings = complete_settings("0.01");
    settings.risk.allowed_coins = vec!["BTC".into(), "ETH".into()];
    let vm = build(&snap(settings, &[(Binance, "BTCUSDT", "0.0001"), (Binance, "ETHUSDT", "0.0001"), (Binance, "SOLUSDT", "0.0001")]), NOW);
    let syms: BTreeSet<_> = vm.rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(syms, ["BTCUSDT", "ETHUSDT"].into());
}

#[test]
fn qualifies_when_net_edge_reaches_threshold() {
    // spread 0.10% − fees 0.08% = 0.02 ≥ 0.01
    let vm = build(&snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0006"), (Bybit, "BTCUSDT", "-0.0004")]), NOW);
    let r = row(&vm, "BTCUSDT");
    assert_eq!(r.net_edge.text(), "0.0200");
    assert_eq!(r.qualified.text(), "達標");
}

#[test]
fn big_gross_spread_with_small_net_edge_does_not_qualify() {
    let mut settings = complete_settings("0.01");
    for e in Exchange::ALL {
        settings.risk.taker_fee_pct.insert(e, d("0.04875"));
    }
    let vm = build(&snap(settings, &[(Binance, "BTCUSDT", "0.0010"), (Bybit, "BTCUSDT", "-0.0010")]), NOW);
    let r = row(&vm, "BTCUSDT");
    assert_eq!((r.gross_text(), r.net_edge.text()), ("0.2000".into(), "0.0050".into()));
    assert_eq!(r.qualified.text(), "—");
}

#[test]
fn default_order_is_by_net_edge_descending_and_rank_follows() {
    // net = spread − 0.08: 0.09 → 0.01, 0.11 → 0.03, 0.06 → −0.02
    let vm = build(
        &snap(complete_settings("0.01"), &[
            (Binance, "AUSDT", "0.0009"), (Bybit, "AUSDT", "0"),
            (Binance, "BUSDT", "0.0011"), (Bybit, "BUSDT", "0"),
            (Binance, "CUSDT", "0.0006"), (Bybit, "CUSDT", "0"),
            (Binance, "DUSDT", "0.0050"),
        ]),
        NOW,
    );
    let order: Vec<_> = vm.rows.iter().map(|r| (r.rank, r.net_edge.text())).collect();
    assert_eq!(order[..3], [(1, "0.0300".into()), (2, "0.0100".into()), (3, "−0.0200".into())]);
    assert_eq!(vm.rows[3].symbol, "DUSDT", "rows without Net Edge go last");
}

#[test]
fn missing_threshold_marks_every_row_not_configured_and_sorts_by_gross() {
    let mut settings = complete_settings("0.01");
    settings.risk.net_edge_threshold_pct = None;
    let vm = build(
        &snap(settings, &[(Binance, "AUSDT", "0.0001"), (Bybit, "AUSDT", "0"), (Binance, "BUSDT", "0.0005"), (Bybit, "BUSDT", "0")]),
        NOW,
    );
    assert!(vm.rows.iter().all(|r| r.qualified == Qualified::NotConfigured));
    assert_eq!(vm.rows[0].symbol, "BUSDT");
    assert_eq!(vm.threshold_text, "Net Edge 門檻 %：未設定");
}

#[test]
fn threshold_is_shown_read_only_when_set() {
    let vm = build(&snap(complete_settings("0.01"), &[]), NOW);
    assert_eq!(vm.threshold_text, "Net Edge 門檻 %：0.0100");
}

fn twenty_rows_twelve_qualified() -> ScannerVm {
    let mut rows = Vec::new();
    let syms: Vec<String> = (0..20).map(|i| format!("S{i:02}USDT")).collect();
    for (i, s) in syms.iter().enumerate() {
        let rate = if i < 12 { "0.0010" } else { "0.0001" };
        rows.push((Binance, s.as_str(), rate));
        rows.push((Bybit, s.as_str(), "0"));
    }
    build(&snap(complete_settings("0.01"), &rows), NOW)
}

#[test]
fn toggle_shows_only_qualified_rows_and_the_count_never_changes() {
    let vm = twenty_rows_twelve_qualified();
    assert_eq!(vm.summary, ScannerSummary { scanned: 20, multi_coverage: 20, qualified: Some(12) });
    match vm.visible(true) {
        RowsView::Rows(r) => assert_eq!(r.len(), 12),
        other => panic!("{other:?}"),
    }
    match vm.visible(false) {
        RowsView::Rows(r) => assert_eq!(r.len(), 20),
        other => panic!("{other:?}"),
    }
    assert_eq!(vm.match_text(), "符合 12 筆");
}

#[test]
fn toggle_on_with_nothing_qualified_says_so() {
    let vm = build(&snap(complete_settings("0.01"), &[(Binance, "AUSDT", "0.0001"), (Bybit, "AUSDT", "0")]), NOW);
    assert_eq!(vm.visible(true), RowsView::NoneQualified);
    assert_eq!(vm.match_text(), "符合 0 筆");
}

#[test]
fn rows_have_independent_countdowns() {
    let mut s = snap(complete_settings("0.01"), &[]);
    s.market.get_mut(&Binance).unwrap().observations.push(obs(Binance, "AUSDT", "0.0001", 14_400, NOW + 4 * H, NOW));
    s.market.get_mut(&Binance).unwrap().observations.push(obs(Binance, "BUSDT", "0.0001", 28_800, NOW + 12 * H, NOW));
    let vm = build(&s, NOW);
    let synced = ClockState::Synced { offset_ms: 0 };
    assert_eq!(countdown(row(&vm, "AUSDT").countdown_target, synced, NOW).text(), "04:00:00");
    assert_eq!(countdown(row(&vm, "BUSDT").countdown_target, synced, NOW).text(), "12:00:00");
}

#[test]
fn pair_countdown_targets_the_earlier_leg() {
    let now = NOW - H; // 11:00
    let mut s = snap(complete_settings("0.01"), &[]);
    s.market.get_mut(&Binance).unwrap().observations.push(obs(Binance, "BTCUSDT", "-0.0005", 28_800, NOW + 4 * H, now));
    s.market.get_mut(&Bybit).unwrap().observations.push(obs(Bybit, "BTCUSDT", "0.0005", 14_400, NOW, now));
    let vm = build(&s, now);
    let r = row(&vm, "BTCUSDT");
    assert_eq!(r.countdown_target.map(|t| t.0), Some(NOW));
    assert_eq!(countdown(r.countdown_target, ClockState::Synced { offset_ms: 0 }, now).text(), "01:00:00");
}

#[test]
fn countdown_uses_the_calibrated_exchange_time() {
    // local clock 2000 ms ahead of the exchange: offset −2000; uncorrected remaining 60 s → 62 s
    let c = countdown(Some((NOW + 60_000, Binance)), ClockState::Synced { offset_ms: -2_000 }, NOW);
    assert_eq!(c, Countdown::Remaining { ms: 62_000, calibrated: true });
}

#[test]
fn countdown_past_zero_is_settling_never_negative() {
    let c = countdown(Some((NOW - 1, Binance)), ClockState::Synced { offset_ms: 0 }, NOW);
    assert_eq!(c, Countdown::Settling);
    assert_eq!(c.text(), "結算中 · 待更新");
    assert_eq!(countdown(Some((NOW, Binance)), ClockState::Synced { offset_ms: 0 }, NOW), Countdown::Settling);
    let u = countdown(Some((NOW + 1_000, Binance)), ClockState::Unsynced, NOW);
    assert_eq!(u, Countdown::Remaining { ms: 1_000, calibrated: false });
}

#[test]
fn summary_matches_the_table() {
    let vm = build(
        &snap(complete_settings("0.01"), &[(Binance, "AUSDT", "0.0010"), (Bybit, "AUSDT", "0"), (Binance, "BUSDT", "0.0001"), (Okx, "CUSDT", "0.0001")]),
        NOW,
    );
    assert_eq!(vm.summary.scanned, vm.rows.len());
    assert_eq!(vm.summary.scanned, 3);
    assert_eq!(vm.summary.multi_coverage, 1);
    assert_eq!(vm.summary.qualified, Some(vm.rows.iter().filter(|r| r.qualified == Qualified::Yes).count()));
}

#[test]
fn loading_unavailable_and_partial_failure_are_distinct() {
    let empty = UiSnapshot::default();
    assert_eq!(build(&empty, NOW).state, TableState::Loading);

    let mut failed = UiSnapshot::default();
    for e in Exchange::ALL {
        failed.market.insert(e, MarketFeed { observations: vec![], last_success_at: None, last_error: Some(("request timed out".into(), NOW)) });
    }
    match build(&failed, NOW).state {
        TableState::Unavailable { errors } => assert_eq!(errors.len(), 3),
        other => panic!("{other:?}"),
    }

    let mut partial = snap(complete_settings("0.01"), &[(Binance, "BTCUSDT", "0.0003"), (Bybit, "BTCUSDT", "-0.0001")]);
    partial.market.get_mut(&Bybit).unwrap().last_error = Some(("request timed out".into(), NOW + 1));
    let vm = build(&partial, NOW + 2);
    assert_eq!(vm.state, TableState::Ready);
    assert_eq!(vm.source_errors, vec![(Bybit, "request timed out".to_string())]);
    assert!(cell(row(&vm, "BTCUSDT"), Bybit).notes().contains(&"過期".to_string()));
    assert!(!cell(row(&vm, "BTCUSDT"), Binance).notes().contains(&"過期".to_string()));
}

#[test]
fn disabled_exchanges_are_not_counted_or_shown() {
    let mut settings = complete_settings("0.01");
    settings.risk.allowed_exchanges = vec![Binance, Bybit];
    let vm = build(&snap(settings, &[(Binance, "BTCUSDT", "0.0001"), (Okx, "BTCUSDT", "0.0001"), (Okx, "ETHUSDT", "0.0001")]), NOW);
    assert_eq!(vm.rows.len(), 1, "ETHUSDT is only on the disabled OKX");
    assert_eq!(vm.rows[0].coverage_text(), "1/2");
    assert_eq!(cell(&vm.rows[0], Okx).text(), "—");
}

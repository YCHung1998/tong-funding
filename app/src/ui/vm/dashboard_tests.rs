//! Task 2.1: dashboard view-model (spec dashboard-page), Figma numbers as test cases.
use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange::{self, Binance, Bybit, Okx};

use super::*;
use crate::exchange::health::feed::HealthSnapshot;
use crate::ui::bridge::{AccountState, ContractEquity, SourceHealth, SourceId, UiSnapshot};
use crate::ui::testkit::{asset, d, loaded, pair, position};

const NOW: i64 = 1_791_201_600_000;

fn figma_binance() -> AccountState {
    let mut a = loaded(
        vec![asset("USDT", "16200", None, None), asset("BTC", "0.05", None, Some("60200")), asset("ETH", "0.5", None, Some("2980"))],
        // 2,400 notional BTC position: must NOT be added to the Value
        vec![position(Binance, "BTCUSDT", "0.04", "60000", "60000", "3", "0", Some("800"))],
        NOW,
    );
    if let AccountState::Loaded { data, .. } = &mut a {
        data.contract_equity = Some(ContractEquity { used_margin: d("900"), available_margin: d("400") });
    }
    a
}

fn snap_with(binance: AccountState, bybit: AccountState) -> UiSnapshot {
    let mut s = UiSnapshot::default();
    s.accounts.insert(Binance, binance);
    s.accounts.insert(Bybit, bybit);
    s
}

fn connected(vm: &DashboardVm, e: Exchange) -> &ConnectedCard {
    vm.cards
        .iter()
        .find_map(|c| match c {
            ExchangeCard::Connected(c) if c.exchange == e => Some(c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{e:?} not connected"))
}

#[test]
fn figma_binance_value_is_22000_without_position_notional() {
    let vm = build(&snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW)), NOW);
    let b = connected(&vm, Binance);
    assert_eq!(format::money(b.value, 2), "22,000.00");
    let names: Vec<_> = b.assets.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["USDT", "BTC", "ETH", CONTRACT_EQUITY_LABEL]);
    let values: Vec<_> = b.assets.iter().map(AssetRow::value_text).collect();
    assert_eq!(values, ["16,200.00", "3,010.00", "1,490.00", "1,300.00"]);
}

#[test]
fn figma_shares_use_one_denominator_with_four_decimals() {
    let vm = build(&snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW)), NOW);
    let pcts: Vec<_> = connected(&vm, Binance).assets.iter().map(AssetRow::pct_text).collect();
    assert_eq!(pcts, ["73.6364%", "13.6818%", "6.7727%", "5.9091%"]);
    let sum: Decimal = connected(&vm, Binance).assets.iter().filter_map(|a| a.pct).sum();
    assert_eq!(format::fixed(sum, 4), "100.0000");
}

#[test]
fn figma_two_exchange_totals_and_shares() {
    let vm = build(&snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW)), NOW);
    assert_eq!(format::money(vm.total, 2), "40,000.00");
    assert_eq!(DashboardVm::pct_text(connected(&vm, Binance)), "55.00%");
    assert_eq!(DashboardVm::pct_text(connected(&vm, Bybit)), "45.00%");
    assert_eq!(vm.excluded_note, None);
    assert_eq!(connected(&vm, Binance).status_label, "● CONNECTED · DEMO");
}

#[test]
fn usdt_price_is_one_and_exchange_valuation_wins_over_mark() {
    let data = AccountData {
        assets: vec![asset("USDT", "10", None, None), asset("BNB", "2", Some("1200"), Some("1"))],
        contract_equity: None,
        positions: vec![],
        positions_incomplete: None,
        environment: "TESTNET",
    };
    let (rows, total, unvalued) = asset_rows(&data);
    assert_eq!(rows[0].price_text(), "1.00");
    assert_eq!(rows[1].value_text(), "1,200.00");
    assert_eq!(rows[1].price_text(), "600.00");
    assert_eq!((format::money(total, 2), unvalued), ("1,210.00".into(), 0));
}

#[test]
fn an_asset_without_any_valuation_is_not_counted_and_is_announced() {
    let a = loaded(vec![asset("USDT", "100", None, None), asset("XYZ", "5", None, None)], vec![], NOW);
    let vm = build(&snap_with(a, AccountState::NotConnected { reason: "no key".into() }), NOW);
    let b = connected(&vm, Binance);
    let xyz = b.assets.iter().find(|r| r.name == "XYZ").unwrap();
    assert_eq!(xyz.value_text(), "無法估值");
    assert_eq!(xyz.value, None);
    assert_eq!(format::money(b.value, 2), "100.00", "never valued as 0 or as its quantity");
    assert_eq!(vm.unvalued_note.as_deref(), Some("不含 1 項無法估值的資產"));
}

#[test]
fn margin_distribution_from_reported_initial_margin() {
    let m = margin_distribution(&[
        position(Bybit, "BTCUSDT", "0.02", "60000", "60000", "3", "0", Some("400")),
        position(Bybit, "ETHUSDT", "1", "1500", "1500", "3", "0", Some("500")),
    ]);
    match m {
        MarginDistribution::Slices { used_total, slices, estimated, unknown } => {
            assert_eq!(format::money(used_total, 2), "900.00");
            let pcts: Vec<_> = slices.iter().map(|s| (s.label.clone(), format::fixed(s.pct, 2))).collect();
            assert_eq!(pcts, [("BTC".to_string(), "44.44".to_string()), ("ETH".to_string(), "55.56".to_string())]);
            assert!(!estimated);
            assert_eq!(unknown, 0);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn margin_is_estimated_from_notional_and_leverage_when_not_reported() {
    // notional 1,200 at 3x → 400, marked as estimated
    let m = margin_distribution(&[position(Binance, "BTCUSDT", "0.02", "60000", "60000", "3", "0", None)]);
    match m {
        MarginDistribution::Slices { used_total, estimated, .. } => {
            assert_eq!(format::money(used_total, 2), "400.00");
            assert!(estimated);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_positions_shows_no_positions_not_an_empty_chart() {
    assert_eq!(margin_distribution(&[]), MarginDistribution::NoPositions);
}

fn four_positions() -> (AccountState, AccountState) {
    (
        loaded(
            vec![asset("USDT", "1000", None, None)],
            vec![
                position(Binance, "BTCUSDT", "0.02", "60000", "60200", "3", "4", Some("400")),
                position(Binance, "ETHUSDT", "1", "1500", "1500", "3", "0", Some("500")),
            ],
            NOW,
        ),
        loaded(
            vec![asset("USDT", "1000", None, None)],
            vec![
                position(Bybit, "BTCUSDT", "-0.02", "60000", "60200", "3", "-4", Some("400")),
                position(Bybit, "ETHUSDT", "-1", "1500", "1500", "3", "0", Some("500")),
            ],
            NOW,
        ),
    )
}

#[test]
fn open_positions_and_pairs_count() {
    let (b, y) = four_positions();
    let mut s = snap_with(b, y);
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled), pair("p2", "ETHUSDT", Binance, Bybit, PairState::Reconciled)];
    let vm = build(&s, NOW);
    assert_eq!(vm.open_positions, 4);
    assert_eq!(vm.positions_note, "2 對避險組合 · 4 個交易所倉位");
    assert_eq!(vm.exposure, "BTC 與 ETH 各 1 對中性組合 · 已用保證金 1,800.00 USDT · 未實現 PnL 0.00 USDT · 無未避險單腿");
    assert_eq!(vm.unhedged, 0);
}

#[test]
fn without_reconciled_pairs_every_position_is_unhedged() {
    let (b, y) = four_positions();
    let vm = build(&snap_with(b, y), NOW);
    assert_eq!(vm.positions_note, "0 對避險組合 · 4 個交易所倉位");
    assert_eq!(vm.unhedged, 4);
    assert!(vm.exposure.contains("4 個未避險單腿"), "{}", vm.exposure);
    assert_eq!(vm.exposure_tone, Tone::Warning);
}

#[test]
fn one_unpaired_leg_is_shown_in_the_warning_tone() {
    let (b, y) = four_positions();
    let mut s = snap_with(b, y);
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled)];
    if let Some(AccountState::Loaded { data, .. }) = s.accounts.get_mut(&Bybit) {
        data.positions.retain(|p| p.symbol == "BTCUSDT");
    }
    let vm = build(&s, NOW);
    assert_eq!(vm.unhedged, 1);
    assert!(vm.exposure.contains("1 個未避險單腿"), "{}", vm.exposure);
    assert!(vm.exposure.starts_with("BTC 1 對中性組合"), "{}", vm.exposure);
    assert_eq!(vm.exposure_tone, Tone::Warning);
}

#[test]
fn unconnected_exchange_is_excluded_and_named() {
    let vm = build(&snap_with(figma_binance(), AccountState::NotConnected { reason: "金鑰不存在".into() }), NOW);
    assert_eq!(format::money(vm.total, 2), "22,000.00");
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：Bybit"));
    assert_eq!(DashboardVm::pct_text(connected(&vm, Binance)), "100.00%");
    assert!(vm.cards.contains(&ExchangeCard::NotConnected { exchange: Bybit, reason: "金鑰不存在".into() }));
}

#[test]
fn okx_is_a_compare_only_card_without_amounts() {
    let vm = build(&snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW)), NOW);
    assert!(vm.cards.contains(&ExchangeCard::CompareOnly { exchange: Okx }));
    assert_eq!(format::money(vm.total, 2), "40,000.00", "OKX not in the total");
}

#[test]
fn failed_poll_keeps_the_old_amounts_and_marks_their_age() {
    let mut b = figma_binance();
    if let AccountState::Loaded { fetched_at, error, .. } = &mut b {
        *fetched_at = NOW - 45_000;
        *error = Some(("request timed out".into(), NOW - 1_000));
    }
    let vm = build(&snap_with(b, AccountState::NotConnected { reason: "x".into() }), NOW);
    let card = connected(&vm, Binance);
    assert_eq!(format::money(card.value, 2), "22,000.00", "never reset to 0");
    assert_eq!(card.stale_note.as_deref(), Some("45 秒前的資料 · 可能已過期"));
}

#[test]
fn account_refresh_countdown_comes_from_the_next_poll() {
    let mut s = snap_with(figma_binance(), AccountState::Loading);
    s.health.push(SourceHealth {
        source: SourceId::Account(Binance),
        health: Some(HealthSnapshot { source: "a".into(), connected: true, last_success_at: Some(NOW), consecutive_failures: 0, expected_period_ms: 30_000, stale_threshold_ms: 90_000, stale: false }),
        next_poll_at: Some(NOW + 12_400),
        rate_limited_until: None,
    });
    let vm = build(&s, NOW);
    assert_eq!(vm.refresh_text.as_deref(), Some("帳戶刷新 12s"));
    assert!(vm.cards.contains(&ExchangeCard::Loading { exchange: Bybit }));
}

//! Task 2.2: positions view-model (spec positions-page); pairs are test fakes (design D12).
use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange::{Binance, Bybit};

use super::*;
use crate::ui::bridge::{AccountData, AccountState, UiSnapshot};
use crate::ui::testkit::{d, loaded, pair, position};

const NOW: i64 = 1_791_201_600_000;

fn four() -> UiSnapshot {
    let mut s = UiSnapshot::default();
    s.accounts.insert(
        Binance,
        loaded(vec![], vec![
            position(Binance, "BTCUSDT", "0.02", "60000", "60200", "3", "4", Some("400")),
            position(Binance, "ETHUSDT", "1", "1500", "1500", "3", "0", Some("500")),
        ], NOW),
    );
    s.accounts.insert(
        Bybit,
        loaded(vec![], vec![
            position(Bybit, "BTCUSDT", "-0.02", "60000", "60200", "3", "-4", Some("400")),
            position(Bybit, "ETHUSDT", "-1", "1500", "1500", "3", "0", Some("500")),
        ], NOW),
    );
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled), pair("p2", "ETHUSDT", Binance, Bybit, PairState::Reconciled)];
    s
}

fn rows(vm: &PositionsVm) -> &Vec<PosRow> {
    match &vm.table {
        TableView::Rows(r) => r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn title_counts_and_default_filter_shows_everything() {
    let vm = build(&four(), &Filter::default());
    assert_eq!(vm.title, "4 OPEN · 2 PAIRS");
    assert_eq!(vm.exchange_options, [Binance, Bybit]);
    assert_eq!(vm.coin_options, ["BTC", "ETH"]);
    assert_eq!(vm.filter_text, "已選 2 個交易所 / 2 個幣種 · 顯示 4 / 4");
    assert_eq!(rows(&vm).len(), 4);
}

#[test]
fn selecting_one_exchange_filters_rows_only() {
    let f = Filter { exchanges: Some([Binance].into()), coins: None };
    let vm = build(&four(), &f);
    assert_eq!(rows(&vm).len(), 2);
    assert_eq!(vm.filter_text, "已選 1 個交易所 / 2 個幣種 · 顯示 2 / 4");
    assert_eq!(vm.title, "4 OPEN · 2 PAIRS", "header counts ignore filters");
    // pairs are computed on all rows: both legs still on the cards
    assert_eq!(vm.pair_cards.len(), 2);
    let c = &vm.pair_cards[0];
    assert_eq!((c.long.as_ref().unwrap().exchange, c.short.as_ref().unwrap().exchange), (Binance, Bybit));
}

#[test]
fn unticking_everything_shows_the_nothing_selected_state() {
    let f = Filter { exchanges: Some(BTreeSet::new()), coins: None };
    let vm = build(&four(), &f);
    assert_eq!(vm.table, TableView::NothingSelected);
}

#[test]
fn summary_cards_entry_notional_and_pnl() {
    let mut s = four();
    // BTC legs 1,200 each (2,400), ETH legs 1,500 each (3,000)
    for e in [Binance, Bybit] {
        if let Some(AccountState::Loaded { data, .. }) = s.accounts.get_mut(&e) {
            data.positions[1].entry_price = Some(d("1500"));
        }
    }
    let vm = build(&s, &Filter::default());
    assert_eq!(format::money(vm.notional_total, 2), "5,400.00");
    assert_eq!(vm.notional_breakdown, "BTC 2,400 + ETH 3,000 USDT");
    assert_eq!(format::money(vm.pnl_total, 2), "0.00");
    assert_eq!(vm.pnl_breakdown, "Binance +4.00 · Bybit −4.00");
    assert_eq!(vm.open_card, "4 · 與總覽一致 · 2 對 / 4 腿");
}

#[test]
fn a_row_renders_like_the_spec() {
    let vm = build(&four(), &Filter::default());
    let r = &rows(&vm)[0];
    // funding-pnl: no ledger data yet: a dash with the reason, never 0.00.
    assert_eq!(r.cells().join(" · "), "Binance · BTCUSDT · LONG · 0.020000 BTC · 60,000.00 · 60,200.00 · 3× · +4.00 USDT · —（尚未取得）");
    assert_eq!(r.pnl_tone(), Tone::Positive);
    assert_eq!(rows(&vm)[2].pnl_tone(), Tone::Negative);
}

#[test]
fn tiny_size_is_not_shown_as_zero() {
    let p = position(Binance, "BTCUSDT", "0.0000004", "60000", "60000", "3", "0", None);
    let row = PosRow { exchange: p.exchange, symbol: p.symbol, side: p.side, quantity: p.quantity, entry_price: None, mark_price: None, leverage: None, unrealized_pnl: None, unpaired: true, funding: "—".into(), funding_tone: Tone::Muted };
    assert_eq!(row.cells()[3], "0.0000004 BTC");
}

#[test]
fn funding_column_is_a_dash_for_every_row_without_ledger_data() {
    // funding-pnl: paired rows say why ("尚未取得"); never 0.00.
    let vm = build(&four(), &Filter::default());
    assert!(rows(&vm).iter().all(|r| r.cells()[8] == "—（尚未取得）"), "{:?}", rows(&vm).iter().map(|r| r.cells()[8].clone()).collect::<Vec<_>>());
}

#[test]
fn balanced_pair_is_hedged_with_zero_imbalance() {
    let vm = build(&four(), &Filter::default());
    let c = &vm.pair_cards[0];
    assert_eq!(c.label.as_deref(), Some("HEDGED · 0.00% IMBALANCE"));
    assert!(!c.imbalanced);
    assert_eq!(c.pnl_text.as_deref(), Some("Pair Unrealized PnL 0.00 USDT（+4.00 / −4.00）"));
    assert_eq!(c.state_warning, None);
}

#[test]
fn imbalance_beyond_tolerance_is_labelled_imbalanced() {
    assert_eq!(format::fixed(imbalance_pct(d("0.020"), d("-0.018")), 2), "10.00");
    assert_eq!(imbalance_pct(d("0"), d("0")), Decimal::ZERO);
    let mut s = four();
    if let Some(AccountState::Loaded { data, .. }) = s.accounts.get_mut(&Bybit) {
        data.positions[0].quantity = d("-0.018");
    }
    s.settings.risk.max_leg_imbalance_pct = d("1.0");
    let vm = build(&s, &Filter::default());
    let c = vm.pair_cards.iter().find(|c| c.symbol == "BTCUSDT").unwrap();
    assert_eq!(c.label.as_deref(), Some("IMBALANCED · 10.00% IMBALANCE"));
    assert!(c.imbalanced);
}

#[test]
fn locked_pair_state_is_shown_in_warning_with_manual_handling() {
    let mut s = four();
    s.pairs[0].state = Ok(PairState::PartialFailure);
    let vm = build(&s, &Filter::default());
    let c = vm.pair_cards.iter().find(|c| c.symbol == "BTCUSDT").unwrap();
    assert_eq!(c.state_warning.as_deref(), Some("PARTIAL_FAILURE · 需人工處理"));
    // grouping is RECONCILED-only: the BTC legs are unpaired and only one pair remains grouped
    assert_eq!(vm.title, "4 OPEN · 1 PAIRS");
    assert_eq!(rows(&vm).iter().filter(|r| r.unpaired).count(), 2);
}

#[test]
fn unpaired_rows_are_tagged_and_match_the_dashboard_count() {
    let mut s = four();
    s.pairs.truncate(1);
    let vm = build(&s, &Filter::default());
    let unpaired: Vec<_> = rows(&vm).iter().filter(|r| r.unpaired).map(|r| r.symbol.as_str()).collect();
    assert_eq!(unpaired, ["ETHUSDT", "ETHUSDT"]);
    assert!(vm.pair_cards.iter().all(|c| c.symbol != "ETHUSDT"));
    let dash = crate::ui::dashboard::build(&s, NOW);
    assert_eq!(dash.unhedged, unpaired.len());
    assert_eq!(dash.open_positions, rows(&vm).len());
}

#[test]
fn incomplete_lists_unconnected_exchanges_and_okx_are_announced() {
    let mut s = four();
    if let Some(AccountState::Loaded { data, .. }) = s.accounts.get_mut(&Bybit) {
        data.positions_incomplete = Some("page 2 failed".into());
    }
    let vm = build(&s, &Filter::default());
    assert!(vm.notices.contains(&"Bybit 持倉列表可能不完整".to_string()), "{:?}", vm.notices);
    assert_eq!(rows(&vm).len(), 4, "the rows already fetched are still shown");
    assert!(vm.notices.contains(&OKX_NOTE.to_string()));

    s.accounts.insert(Binance, AccountState::NotConnected { reason: "金鑰不存在".into() });
    let vm = build(&s, &Filter::default());
    assert!(vm.notices.contains(&"Binance 未連線：金鑰不存在".to_string()), "{:?}", vm.notices);
    assert!(vm.notices.iter().all(|n| !n.contains("Binance 無持倉")));
}

#[test]
fn grouping_is_shared_and_uses_only_reconciled_pairs() {
    let mut s = four();
    s.pairs.push(pair("p3", "BTCUSDT", Binance, Bybit, PairState::Closing));
    let g = group(&s);
    assert_eq!(g.pairs.len(), 2);
    assert_eq!(g.grouping.groups.len(), 2);
    let _ = AccountData { assets: vec![], contract_equity: None, positions: vec![], positions_incomplete: None, environment: "DEMO" };
}

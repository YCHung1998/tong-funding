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
    // OKX connected with nothing in it (so these two-exchange cases keep their numbers)
    s.accounts.insert(Okx, loaded(vec![], vec![], NOW));
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
    assert!(vm.cards.contains(&ExchangeCard::NotConnected { exchange: Bybit, reason: "金鑰不存在".into(), disabled_note: None }));
}

fn okx_connected() -> AccountState {
    // as `live::okx_account_data` produces it: wallet rows and positions, no contract-equity row (the
    // wallet already includes the margin in use, so it would be counted twice)
    loaded(vec![asset("USDT", "5000", Some("4999.8"), None), asset("BTC", "0.1", Some("6000"), None)], vec![position(Okx, "BTCUSDT", "-0.03", "60000", "60000", "5", "0", Some("360"))], NOW)
}

#[test]
fn okx_is_a_real_account_card_counted_in_the_total_and_the_shares() {
    let mut s = snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW));
    s.accounts.insert(Okx, okx_connected());
    let vm = build(&s, NOW);
    let o = connected(&vm, Okx);
    assert_eq!(o.status_label, "● CONNECTED · DEMO");
    // USDT 5,000 + BTC 6,000 (exchange valuation); no contract-equity row
    assert_eq!(format::money(o.value, 2), "11,000.00");
    assert_eq!(format::money(vm.total, 2), "51,000.00", "40,000 + OKX");
    assert!(vm.excluded_note.is_none(), "{:?}", vm.excluded_note);
    assert_eq!(o.assets.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["USDT", "BTC"]);
    assert!(matches!(&o.margin, MarginDistribution::Slices { used_total, .. } if *used_total == d("360")));
    assert!(!vm.cards.iter().any(|c| format!("{c:?}").contains("CompareOnly")));
    assert_eq!(vm.open_positions, 2, "Binance BTC + OKX BTC");
}

#[test]
fn okx_not_connected_failed_and_loading_are_named_states_never_zero_and_not_in_the_total() {
    let mut s = snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW));
    // nothing read yet
    s.accounts.remove(&Okx);
    let vm = build(&s, NOW);
    assert!(vm.cards.contains(&ExchangeCard::Loading { exchange: Okx, disabled_note: None }));
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：OKX"));
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    let vm = build(&s, NOW);
    assert!(vm.cards.contains(&ExchangeCard::NotConnected { exchange: Okx, reason: "Keychain 沒有 API key".into(), disabled_note: None }));
    assert_eq!(format::money(vm.total, 2), "40,000.00");
    s.accounts.insert(Okx, AccountState::Failed { error: "帳戶模式不支援 (acctLv 2, posMode long_short_mode)".into(), at: NOW });
    let vm = build(&s, NOW);
    assert!(matches!(vm.cards.last(), Some(ExchangeCard::Failed { exchange: Okx, error, .. }) if error.contains("long_short_mode")));
    assert_eq!(format::money(vm.total, 2), "40,000.00");
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：OKX"));
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
    assert!(vm.cards.contains(&ExchangeCard::Loading { exchange: Bybit, disabled_note: None }));
}

// ---- ui-plain-language 2.1: an exchange the risk settings left unticked ----------------------------

const OKX_SPOT: &str = "exchange error acct_mode_unsupported: 帳戶模式不支援 (acctLv 1, posMode net_mode)";

fn unticked_okx(s: &mut UiSnapshot) {
    s.settings.risk.allowed_exchanges = vec![Binance, Bybit];
}

fn disabled_note(vm: &DashboardVm, e: Exchange) -> Option<&DisabledNote> {
    vm.cards.iter().find_map(|c| match c {
        ExchangeCard::Disabled(n) if n.exchange == e => Some(n),
        _ => None,
    })
}

fn no_error_card_for(vm: &DashboardVm, e: Exchange) -> bool {
    !vm.cards.iter().any(|c| match c {
        ExchangeCard::NotConnected { exchange, .. } | ExchangeCard::Loading { exchange, .. } | ExchangeCard::Failed { exchange, .. } => *exchange == e,
        ExchangeCard::Connected(_) | ExchangeCard::Disabled(_) => false,
    })
}

fn two_exchanges() -> UiSnapshot {
    snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW))
}

#[test]
fn an_unticked_okx_that_fails_is_one_muted_line_and_no_error_card() {
    let mut s = two_exchanges();
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    let n = disabled_note(&vm, Okx).expect("one line for the unticked exchange");
    // the read failed, so the positions are not confirmed: a warning line, never the quiet "unticked" one
    assert_eq!(n.text, "OKX 已停用 · 無法確認持倉（OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式））");
    assert_eq!(n.tone, Tone::Warning);
    assert!(no_error_card_for(&vm, Okx), "no red error card for an exchange nobody trades on: {:?}", vm.cards);
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：OKX"), "its positions are unconfirmed and its value is not in the total, so it is named");
    // the error is only behind the line: plain words with the fix, and the raw message after them
    let detail = n.detail.as_deref().expect("detail for the expanded line");
    assert!(detail.contains("OKX 帳戶模式不支援") && detail.contains("在 OKX 改為單幣種或跨幣種保證金模式"), "{detail}");
    assert!(detail.contains("acct_mode_unsupported"), "the raw message stays reachable: {detail}");
    assert!(!n.text.contains("acct_mode_unsupported") && !n.text.contains("沒有持倉"), "the raw code and a false all-clear stay off the visible line");
    assert_eq!(crate::ui::labels::DISABLED_TONE, Tone::Muted);
}

#[test]
fn the_total_description_lists_only_the_exchanges_it_counts() {
    // OKX unticked and failing: the total is Binance + Bybit and says so
    let mut s = two_exchanges();
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert_eq!(vm.exchanges_text, "Binance + Bybit");
    assert_eq!(format::money(vm.total, 2), "40,000.00");
    // OKX ticked but not connected: not listed as counted AND named as left out, never both
    let mut s = two_exchanges();
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    let vm = build(&s, NOW);
    assert_eq!(vm.exchanges_text, "Binance + Bybit");
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：OKX"));
    // everything connected: all three
    let vm = build(&snap_with(figma_binance(), loaded(vec![asset("USDT", "18000", None, None)], vec![], NOW)), NOW);
    assert_eq!(vm.exchanges_text, "Binance + Bybit + OKX");
    // nothing connected: a sentence, not an empty line
    let mut none = UiSnapshot::default();
    for e in [Binance, Bybit, Okx] {
        none.accounts.insert(e, AccountState::NotConnected { reason: "x".into() });
    }
    assert_eq!(build(&none, NOW).exchanges_text, "尚無已連線的交易所");
}

#[test]
fn an_unticked_exchange_that_cannot_be_confirmed_is_named_in_the_unconnected_note_too() {
    let mut s = snap_with(figma_binance(), AccountState::NotConnected { reason: "金鑰不存在".into() });
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：Bybit、OKX"));
    assert!(disabled_note(&vm, Okx).is_some());
}

#[test]
fn an_unticked_exchange_that_still_holds_positions_keeps_its_card_and_says_so() {
    let mut s = two_exchanges();
    s.accounts.insert(Okx, okx_connected());
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).is_none(), "its exposure is shown, not folded into a line");
    let o = connected(&vm, Okx);
    assert_eq!(o.disabled_note.as_deref(), Some("OKX 已停用（風控設定未勾選） · 此交易所已停用，但仍有持倉或持倉狀態未確認"));
    assert!(matches!(&o.margin, MarginDistribution::Slices { used_total, .. } if *used_total == d("360")));
    assert_eq!(vm.open_positions, 2, "Binance BTC + the OKX position, still counted");
    assert!(vm.exposure.contains("已用保證金"), "{}", vm.exposure);
    // what the card shows is what the total counts
    assert_eq!(vm.exchanges_text, "Binance + Bybit + OKX");
    assert_eq!(format::money(vm.total, 2), "51,000.00");
    // a ticked exchange carries no such note
    assert_eq!(connected(&build(&two_exchanges(), NOW), Binance).disabled_note, None);
}

#[test]
fn an_unticked_exchange_that_is_loaded_without_positions_is_a_line_and_not_counted() {
    let mut s = two_exchanges();
    s.accounts.insert(Okx, loaded(vec![asset("USDT", "5000", None, None)], vec![], NOW));
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    let n = disabled_note(&vm, Okx).expect("a line");
    assert!(n.detail.as_deref().is_some_and(|d| d.contains("未計入總資產")), "{:?}", n.detail);
    assert_eq!(format::money(vm.total, 2), "40,000.00");
    assert_eq!(vm.exchanges_text, "Binance + Bybit");
}

#[test]
fn an_unticked_exchange_that_is_not_connected_or_still_loading_is_a_line_with_the_state_behind_it() {
    let mut s = two_exchanges();
    unticked_okx(&mut s);
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).and_then(|n| n.detail.as_deref()).is_some_and(|d| d.contains("Keychain 沒有 API key")));
    assert!(no_error_card_for(&vm, Okx));
    s.accounts.remove(&Okx);
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).and_then(|n| n.detail.as_deref()).is_some_and(|d| d.contains("載入中")));
    assert!(no_error_card_for(&vm, Okx));
}

#[test]
fn a_ticked_okx_with_the_known_error_reads_in_plain_words_with_the_raw_message_kept() {
    let mut s = two_exchanges();
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).is_none());
    match vm.cards.last() {
        Some(ExchangeCard::Failed { exchange: Okx, error, plain, .. }) => {
            assert_eq!(plain, "OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式）");
            assert_eq!(error, OKX_SPOT, "the raw message is kept for the secondary line");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn settings_that_cannot_be_read_hide_nothing() {
    let mut s = two_exchanges();
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    unticked_okx(&mut s);
    s.settings.error = Some("config.risk is not valid JSON".into());
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).is_none(), "an unknown setting is not read as 'unticked'");
    assert!(matches!(vm.cards.last(), Some(ExchangeCard::Failed { exchange: Okx, .. })));
}

// ---- ui-plain-language review fix: a disabled exchange never hides exposure ------------------------

use crate::ui::bridge::PairRecord;
use crate::ui::labels::DISABLED_WITH_EXPOSURE;

const INCOMPLETE: &str = "ETHUSDT 空 7 張（無法換算：ctVal 未知）";

fn record(id: &str, long: Exchange, short: Exchange, status: &str) -> (String, PairRecord) {
    (id.into(), PairRecord { pair_id: format!("{id}-pair"), symbol: "ETHUSDT".into(), long_exchange: Some(long), short_exchange: Some(short), status: status.into() })
}

fn okx_with_stale_error(positions: Vec<crate::exchange::signed::models::Position>) -> AccountState {
    let mut a = loaded(vec![asset("USDT", "5000", None, None)], positions, NOW - 60_000);
    if let AccountState::Loaded { error, .. } = &mut a {
        *error = Some(("request timed out".into(), NOW - 1_000));
    }
    a
}

fn exposure_note(vm: &DashboardVm, e: Exchange) -> Option<String> {
    vm.cards.iter().find_map(|c| match c {
        ExchangeCard::Connected(c) if c.exchange == e => c.disabled_note.clone(),
        ExchangeCard::NotConnected { exchange, disabled_note, .. } | ExchangeCard::Loading { exchange, disabled_note } | ExchangeCard::Failed { exchange, disabled_note, .. } if *exchange == e => {
            disabled_note.clone()
        }
        _ => None,
    })
}

#[test]
fn an_unticked_exchange_with_an_incomplete_position_list_keeps_its_card_and_warns() {
    // OKX positions whose ctVal is unknown are moved out of `positions` into `positions_incomplete`
    let mut s = two_exchanges();
    let mut okx = loaded(vec![asset("USDT", "5000", None, None)], vec![], NOW);
    if let AccountState::Loaded { data, .. } = &mut okx {
        data.positions_incomplete = Some(INCOMPLETE.into());
    }
    s.accounts.insert(Okx, okx);
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).is_none(), "an empty list that is known to be incomplete is not 'no positions': {:?}", vm.cards);
    assert!(connected(&vm, Okx).disabled_note.as_deref().is_some_and(|n| n.contains(DISABLED_WITH_EXPOSURE)));
}

#[test]
fn an_unticked_exchange_with_old_data_and_a_failed_latest_read_is_not_called_empty() {
    // nothing in the old data, but the latest read failed: the warning line, never "沒有持倉"
    let mut s = two_exchanges();
    s.accounts.insert(Okx, okx_with_stale_error(vec![]));
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    let n = disabled_note(&vm, Okx).expect("a line");
    assert_eq!(n.tone, Tone::Warning);
    assert!(n.text.starts_with("OKX 已停用 · 無法確認持倉（") && n.text.contains("request timed out"), "{}", n.text);
    assert!(!n.text.contains("沒有持倉") && !n.detail.clone().unwrap_or_default().contains("沒有持倉"), "{n:?}");
    assert_eq!(format::money(vm.total, 2), "40,000.00", "the old data is not counted");
    // old data WITH a position and a failed latest read: the card stays (stale note) and warns
    let mut s = two_exchanges();
    s.accounts.insert(Okx, okx_with_stale_error(vec![position(Okx, "ETHUSDT", "-0.07", "1500", "1500", "3", "0", Some("35"))]));
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).is_none());
    let c = connected(&vm, Okx);
    assert!(c.stale_note.as_deref().is_some_and(|n| n.contains("可能已過期")));
    assert!(c.disabled_note.as_deref().is_some_and(|n| n.contains(DISABLED_WITH_EXPOSURE)));
}

#[test]
fn an_unticked_exchange_that_failed_while_a_pair_is_open_on_it_keeps_the_failed_card() {
    for (what, set) in [("engine pair", 0), ("stored pair record", 1)] {
        let mut s = two_exchanges();
        s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
        unticked_okx(&mut s);
        if set == 0 {
            s.pairs = vec![crate::ui::testkit::pair("p1", "ETHUSDT", Binance, Okx, PairState::Reconciled)];
        } else {
            s.pair_records = [record("u1", Okx, Bybit, "RECONCILED")].into();
        }
        let vm = build(&s, NOW);
        assert!(disabled_note(&vm, Okx).is_none(), "{what}: the exposure is behind a failed read, so no folded line");
        assert!(matches!(vm.cards.last(), Some(ExchangeCard::Failed { exchange: Okx, .. })), "{what}: {:?}", vm.cards);
        assert!(exposure_note(&vm, Okx).is_some_and(|n| n.contains(DISABLED_WITH_EXPOSURE)), "{what}");
        assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：OKX"), "{what}");
    }
}

#[test]
fn finished_pairs_do_not_count_as_exposure() {
    for status in ["FINALIZED", "CANCELLED", "BLOCKED"] {
        let mut s = two_exchanges();
        s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
        unticked_okx(&mut s);
        s.pair_records = [record("u1", Okx, Bybit, status)].into();
        let vm = build(&s, NOW);
        assert!(disabled_note(&vm, Okx).is_some_and(|n| n.tone == Tone::Warning), "{status}");
    }
    // an unfinished pair on the OTHER two exchanges is not OKX's exposure
    let mut s = two_exchanges();
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    unticked_okx(&mut s);
    s.pairs = vec![crate::ui::testkit::pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled)];
    assert!(disabled_note(&build(&s, NOW), Okx).is_some());
}

#[test]
fn an_unticked_exchange_that_is_not_connected_or_loading_is_a_warning_line_unless_a_pair_is_open() {
    let mut s = two_exchanges();
    unticked_okx(&mut s);
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    let n = disabled_note(&build(&s, NOW), Okx).cloned().expect("a line");
    assert_eq!((n.text.as_str(), n.tone), ("OKX 已停用 · 無法確認持倉（未連線：Keychain 沒有 API key）", Tone::Warning));
    s.accounts.remove(&Okx);
    let n = disabled_note(&build(&s, NOW), Okx).cloned().expect("a line");
    assert_eq!((n.text.as_str(), n.tone), ("OKX 已停用 · 無法確認持倉（載入中）", Tone::Warning));
    // with an unfinished pair on OKX the same states keep their cards
    s.pairs = vec![crate::ui::testkit::pair("p1", "ETHUSDT", Binance, Okx, PairState::OrderSubmit)];
    let vm = build(&s, NOW);
    assert!(matches!(vm.cards.last(), Some(ExchangeCard::Loading { exchange: Okx, disabled_note: Some(_) })), "{:?}", vm.cards);
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "x".into() });
    assert!(matches!(build(&s, NOW).cards.last(), Some(ExchangeCard::NotConnected { exchange: Okx, disabled_note: Some(_), .. })));
}

#[test]
fn a_successful_empty_read_with_no_unfinished_pair_is_the_quiet_muted_line() {
    let mut s = two_exchanges();
    s.accounts.insert(Okx, loaded(vec![asset("USDT", "5000", None, None)], vec![], NOW));
    unticked_okx(&mut s);
    // finished pairs on OKX do not change that
    s.pair_records = [record("u1", Okx, Bybit, "FINALIZED")].into();
    let vm = build(&s, NOW);
    let n = disabled_note(&vm, Okx).expect("a line");
    assert_eq!((n.text.as_str(), n.tone), ("OKX 已停用（風控設定未勾選）", Tone::Muted));
    assert!(n.detail.as_deref().is_some_and(|d| d.contains("已連線，沒有持倉")), "{:?}", n.detail);
    // an old error that a later successful read replaced is not a failed read
    let mut a = loaded(vec![], vec![], NOW);
    if let AccountState::Loaded { error, .. } = &mut a {
        *error = Some(("old".into(), NOW - 10_000));
    }
    s.accounts.insert(Okx, a);
    assert_eq!(disabled_note(&build(&s, NOW), Okx).map(|n| n.tone), Some(Tone::Muted));
}

// ---- ui-clarity red-team E2 / E4 ----

fn empty_two() -> UiSnapshot {
    snap_with(loaded(vec![asset("USDT", "100", None, None)], vec![], NOW), loaded(vec![asset("USDT", "100", None, None)], vec![], NOW))
}

#[test]
fn the_exposure_summary_says_when_an_unticked_exchanges_positions_cannot_be_confirmed() {
    let mut s = empty_two();
    s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert!(vm.exposure.ends_with("（OKX 持倉無法確認）"), "{}", vm.exposure);
    assert_eq!(vm.exposure_tone, Tone::Warning, "nothing unhedged, yet the line must not read as all clear");
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：OKX"));
    // two such exchanges are both named
    s.settings.risk.allowed_exchanges = vec![Binance];
    s.accounts.insert(Bybit, AccountState::NotConnected { reason: "x".into() });
    let vm = build(&s, NOW);
    assert!(vm.exposure.ends_with("（Bybit、OKX 持倉無法確認）"), "{}", vm.exposure);
    assert_eq!(vm.excluded_note.as_deref(), Some("不含未連線的交易所：Bybit、OKX"));
    // a confirmed-quiet unticked exchange adds nothing
    let mut s = empty_two();
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert!(!vm.exposure.contains("無法確認"), "{}", vm.exposure);
    assert_eq!((vm.exposure_tone, vm.excluded_note), (Tone::Muted, None));
}

#[test]
fn a_quiet_line_says_how_old_the_read_is_and_an_old_read_is_not_quiet() {
    let mut s = empty_two();
    s.accounts.insert(Okx, loaded(vec![], vec![], NOW - 12_000));
    unticked_okx(&mut s);
    let n = disabled_note(&build(&s, NOW), Okx).cloned().expect("a line");
    assert_eq!(n.tone, Tone::Muted);
    assert!(n.detail.as_deref().is_some_and(|d| d.contains("已連線，沒有持倉") && d.contains("12 秒前讀取")), "{n:?}");
    // older than three account polls: warning, and the age is said
    s.accounts.insert(Okx, loaded(vec![], vec![], NOW - 100_000));
    let vm = build(&s, NOW);
    let n = disabled_note(&vm, Okx).cloned().expect("a line");
    assert_eq!(n.tone, Tone::Warning);
    assert!(n.text.starts_with("OKX 已停用 · 無法確認持倉（") && !n.text.contains("沒有持倉"), "{}", n.text);
    assert!(n.detail.as_deref().is_some_and(|d| d.contains("100 秒前")), "{n:?}");
    assert!(vm.exposure.ends_with("（OKX 持倉無法確認）"), "{}", vm.exposure);
}

#[test]
fn a_lost_connection_keeps_the_card_of_an_unticked_exchange_that_held_a_position() {
    let mut s = empty_two();
    let mut held = okx_connected();
    if let AccountState::Loaded { error, .. } = &mut held {
        *error = Some((format!("{}Keychain 沒有 API key", crate::ui::bridge::NOT_CONNECTED_ERROR_PREFIX), NOW));
    }
    s.accounts.insert(Okx, held);
    unticked_okx(&mut s);
    let vm = build(&s, NOW);
    assert!(disabled_note(&vm, Okx).is_none(), "{:?}", vm.cards);
    let c = connected(&vm, Okx);
    assert!(c.disabled_note.as_deref().is_some_and(|n| n.contains(DISABLED_WITH_EXPOSURE)));
    assert!(c.stale_note.as_deref().is_some_and(|n| n.contains("可能已過期")));
}

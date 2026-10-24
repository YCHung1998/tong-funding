//! Task 2.2: positions view-model (spec positions-page); pairs are test fakes (design D12).
use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange::{Binance, Bybit, Okx};

use super::*;
use crate::engine::ports::OrderRules;
use tong_funding_core::quantity::LotSize;
use crate::ui::bridge::{AccountData, AccountState, UiSnapshot};
use crate::ui::labels::DISABLED_WITH_EXPOSURE;
use crate::ui::testkit::{d, loaded, pair, position};
use crate::ui::theme::Tone;

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
    let vm = build(&four(), &Filter::default(), NOW);
    assert_eq!(vm.title, "4 OPEN · 2 PAIRS");
    assert_eq!(vm.exchange_options, [Binance, Bybit]);
    assert_eq!(vm.coin_options, ["BTC", "ETH"]);
    assert_eq!(vm.filter_text, "已選 2 個交易所 / 2 個幣種 · 顯示 4 / 4");
    assert_eq!(rows(&vm).len(), 4);
}

#[test]
fn selecting_one_exchange_filters_rows_only() {
    let f = Filter { exchanges: Some([Binance].into()), coins: None };
    let vm = build(&four(), &f, NOW);
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
    let vm = build(&four(), &f, NOW);
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
    let vm = build(&s, &Filter::default(), NOW);
    assert_eq!(format::money(vm.notional_total, 2), "5,400.00");
    assert_eq!(vm.notional_breakdown, "BTC 2,400 + ETH 3,000 USDT");
    assert_eq!(format::money(vm.pnl_total, 2), "0.00");
    assert_eq!(vm.pnl_breakdown, "Binance +4.00 · Bybit −4.00");
    assert_eq!(vm.open_card, "4 · 與總覽一致 · 2 對 / 4 腿");
}

#[test]
fn a_row_renders_like_the_spec() {
    let vm = build(&four(), &Filter::default(), NOW);
    let r = &rows(&vm)[0];
    // funding-pnl: no ledger data yet: a dash with the reason, never 0.00.
    assert_eq!(r.cells().join(" · "), "Binance · BTCUSDT · LONG · 0.020000 BTC · 60,000.00 · 60,200.00 · 3× · +4.00 USDT · —（尚未取得）");
    assert_eq!(r.pnl_tone(), Tone::Positive);
    assert_eq!(rows(&vm)[2].pnl_tone(), Tone::Negative);
}

#[test]
fn tiny_size_is_not_shown_as_zero() {
    let p = position(Binance, "BTCUSDT", "0.0000004", "60000", "60000", "3", "0", None);
    let row = PosRow { exchange: p.exchange, symbol: p.symbol, side: p.side, quantity: p.quantity, entry_price: None, mark_price: None, leverage: None, unrealized_pnl: None, unpaired: true, funding: "—".into(), funding_tone: Tone::Muted, contracts: None, note: None };
    assert_eq!(row.cells()[3], "0.0000004 BTC");
}

#[test]
fn funding_column_is_a_dash_for_every_row_without_ledger_data() {
    // funding-pnl: paired rows say why ("尚未取得"); never 0.00.
    let vm = build(&four(), &Filter::default(), NOW);
    assert!(rows(&vm).iter().all(|r| r.cells()[8] == "—（尚未取得）"), "{:?}", rows(&vm).iter().map(|r| r.cells()[8].clone()).collect::<Vec<_>>());
}

#[test]
fn balanced_pair_is_hedged_with_zero_imbalance() {
    let vm = build(&four(), &Filter::default(), NOW);
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
    let vm = build(&s, &Filter::default(), NOW);
    let c = vm.pair_cards.iter().find(|c| c.symbol == "BTCUSDT").unwrap();
    assert_eq!(c.label.as_deref(), Some("IMBALANCED · 10.00% IMBALANCE"));
    assert!(c.imbalanced);
}

#[test]
fn locked_pair_state_is_shown_in_warning_with_manual_handling() {
    let mut s = four();
    s.pairs[0].state = Ok(PairState::PartialFailure);
    let vm = build(&s, &Filter::default(), NOW);
    let c = vm.pair_cards.iter().find(|c| c.symbol == "BTCUSDT").unwrap();
    assert_eq!(c.state_warning.as_deref(), Some("單腿失敗（PARTIAL_FAILURE） · 需人工處理"));
    // grouping is RECONCILED-only: the BTC legs are unpaired and only one pair remains grouped
    assert_eq!(vm.title, "4 OPEN · 1 PAIRS");
    assert_eq!(rows(&vm).iter().filter(|r| r.unpaired).count(), 2);
}

#[test]
fn unpaired_rows_are_tagged_and_match_the_dashboard_count() {
    let mut s = four();
    s.pairs.truncate(1);
    let vm = build(&s, &Filter::default(), NOW);
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
    let vm = build(&s, &Filter::default(), NOW);
    assert!(vm.notices.contains(&"Bybit 持倉列表可能不完整".to_string()), "{:?}", vm.notices);
    assert_eq!(rows(&vm).len(), 4, "the rows already fetched are still shown");
    assert!(vm.notices.iter().all(|n| !n.contains("僅比價")), "OKX is no longer compare-only: {:?}", vm.notices);
    assert!(vm.notices.contains(&"OKX 持倉載入中".to_string()), "OKX not read yet: {:?}", vm.notices);

    s.accounts.insert(Binance, AccountState::NotConnected { reason: "金鑰不存在".into() });
    let vm = build(&s, &Filter::default(), NOW);
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

// ---- okx-trading-enablement: OKX rows (positions arrive in coins, contracts shown beside them) ----

fn with_okx_short() -> UiSnapshot {
    let mut s = four();
    // OKX short 3 contracts of BTCUSDT (ctVal 0.01) = -0.03 BTC; Binance long 0.03 BTC
    s.accounts.insert(Okx, loaded(vec![], vec![position(Okx, "BTCUSDT", "-0.03", "60000", "60100", "5", "-3", Some("360"))], NOW));
    if let Some(AccountState::Loaded { data, .. }) = s.accounts.get_mut(&Binance) {
        data.positions = vec![position(Binance, "BTCUSDT", "0.03", "60000", "60200", "5", "6", Some("360"))];
    }
    s.accounts.remove(&Bybit);
    s.accounts.insert(Bybit, loaded(vec![], vec![], NOW));
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Okx, PairState::Reconciled)];
    s.rules.insert((Okx, "BTCUSDT".into()), Ok(OrderRules { lot: LotSize { step_size: d("1"), min_qty: d("1") }, okx_ct_val: Some(d("0.01")) }));
    s
}

#[test]
fn an_okx_row_shows_coins_with_the_contracts_and_groups_with_the_other_exchanges_leg() {
    let vm = build(&with_okx_short(), &Filter::default(), NOW);
    let okx = rows(&vm).iter().find(|r| r.exchange == Okx).expect("OKX row");
    assert_eq!(okx.cells()[3], "0.030000 BTC（3 張）");
    assert_eq!(okx.side, Side::Short);
    assert!(!okx.unpaired, "grouped with Binance's long");
    assert_eq!(vm.pair_cards.len(), 1);
    let c = &vm.pair_cards[0];
    assert_eq!((c.long.as_ref().unwrap().exchange, c.short.as_ref().unwrap().exchange), (Binance, Okx));
    assert_eq!(c.label.as_deref(), Some("HEDGED · 0.00% IMBALANCE"));
    assert_eq!(vm.exchange_options, [Binance, Okx]);
    // the Binance row says nothing about contracts
    assert_eq!(rows(&vm).iter().find(|r| r.exchange == Binance).unwrap().cells()[3], "0.030000 BTC");
}

#[test]
fn an_okx_row_without_a_known_ct_val_shows_coins_only_never_contracts_as_coins() {
    let mut s = with_okx_short();
    s.rules.remove(&(Okx, "BTCUSDT".into()));
    let vm = build(&s, &Filter::default(), NOW);
    assert_eq!(rows(&vm).iter().find(|r| r.exchange == Okx).unwrap().cells()[3], "0.030000 BTC");
}

#[test]
fn okx_states_are_announced_like_the_other_exchanges_with_the_reason() {
    let mut s = with_okx_short();
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    let vm = build(&s, &Filter::default(), NOW);
    assert!(vm.notices.contains(&"OKX 未連線：Keychain 沒有 API key".to_string()), "{:?}", vm.notices);
    assert!(vm.notices.iter().all(|n| !n.contains("僅比價")));
    s.accounts.insert(Okx, AccountState::Failed { error: "帳戶模式不支援 (acctLv 2, posMode long_short_mode)".into(), at: NOW });
    let vm = build(&s, &Filter::default(), NOW);
    assert!(vm.notices.iter().any(|n| n.starts_with("OKX 持倉讀取失敗：") && n.contains("long_short_mode")), "{:?}", vm.notices);
    // an incomplete list names its reason (a position that could not be converted is never silently dropped)
    let mut s = with_okx_short();
    if let Some(AccountState::Loaded { data, .. }) = s.accounts.get_mut(&Okx) {
        data.positions_incomplete = Some("ETHUSDT 空 7 張（無法換算：ctVal 未知）".into());
    }
    let vm = build(&s, &Filter::default(), NOW);
    assert!(vm.notices.contains(&"OKX 持倉列表可能不完整".to_string()), "{:?}", vm.notices);
    assert!(vm.notices.iter().any(|n| n.contains("7 張") && n.contains("無法換算")), "{:?}", vm.notices);
}

// ---- spread-position-tag ---------------------------------------------------------------------

use crate::ui::bridge::MarketFeed;
use crate::ui::testkit::obs;
use tong_funding_core::spread_tag::SpreadKind;

/// `four()` plus a market feed per exchange carrying BTCUSDT's mark price (observed at `at`).
fn with_prices(long_px: &str, short_px: &str, at: i64) -> UiSnapshot {
    let mut s = four();
    for (ex, px) in [(Binance, long_px), (Bybit, short_px)] {
        let mut o = obs(ex, "BTCUSDT", "0.0001", 28_800, NOW + 3_600_000, at);
        o.mark_price = d(px);
        s.market.insert(ex, MarketFeed { observations: vec![o], last_success_at: Some(at), last_error: None });
    }
    s
}

fn btc_card(vm: &PositionsVm) -> &PairCard {
    vm.pair_cards.iter().find(|c| c.symbol == "BTCUSDT").unwrap()
}

#[test]
fn fresh_mark_price_needs_a_healthy_fresh_observation() {
    let s = with_prices("100", "100.2", NOW - 1_000);
    assert_eq!(fresh_mark_price(&s, Binance, "BTCUSDT", NOW), Some(d("100")));
    // exactly at the threshold is still fresh (default 3000 ms), one ms beyond is stale
    assert_eq!(fresh_mark_price(&s, Binance, "BTCUSDT", NOW - 1_000 + 3_000), Some(d("100")));
    assert_eq!(fresh_mark_price(&s, Binance, "BTCUSDT", NOW - 1_000 + 3_001), None);
    // symbol missing / exchange without a feed
    assert_eq!(fresh_mark_price(&s, Binance, "ETHUSDT", NOW), None);
    assert_eq!(fresh_mark_price(&s, Okx, "BTCUSDT", NOW), None);
    // source failing (error newer than the last success): the kept price is not used
    let mut f = with_prices("100", "100.2", NOW - 1_000);
    f.market.get_mut(&Bybit).unwrap().last_error = Some(("timeout".into(), NOW - 500));
    assert_eq!(fresh_mark_price(&f, Bybit, "BTCUSDT", NOW), None);
    assert_eq!(fresh_mark_price(&f, Binance, "BTCUSDT", NOW), Some(d("100")));
    // never succeeded
    let mut n = with_prices("100", "100.2", NOW - 1_000);
    n.market.get_mut(&Bybit).unwrap().last_success_at = None;
    assert_eq!(fresh_mark_price(&n, Bybit, "BTCUSDT", NOW), None);
}

#[test]
fn spread_view_shows_the_word_the_percentage_and_the_tone() {
    let v = spread_view(&with_prices("100", "100.2", NOW - 500), Binance, Bybit, "BTCUSDT", NOW).unwrap();
    assert_eq!(v.tag.kind, SpreadKind::Positive);
    assert_eq!(v.text, "價差 positive +0.20%（標記價）");
    assert_eq!(v.tone, Tone::Positive);
    let v = spread_view(&with_prices("100.3", "100", NOW - 500), Binance, Bybit, "BTCUSDT", NOW).unwrap();
    assert_eq!(v.tag.kind, SpreadKind::Negative);
    assert!(v.text.starts_with("價差 negative −0.29") || v.text.starts_with("價差 negative −0.30"), "{}", v.text);
    assert_eq!(v.tone, Tone::Negative);
    let v = spread_view(&with_prices("100", "100", NOW - 500), Binance, Bybit, "BTCUSDT", NOW).unwrap();
    assert_eq!(v.text, "價差 neutral 0.00%（標記價）");
    assert_eq!(v.tone, Tone::Muted);
}

#[test]
fn spread_view_is_none_when_a_leg_is_failing_stale_or_missing() {
    let mut s = with_prices("100", "100.2", NOW - 500);
    s.market.get_mut(&Bybit).unwrap().last_error = Some(("HTTP 500".into(), NOW));
    assert_eq!(spread_view(&s, Binance, Bybit, "BTCUSDT", NOW), None);
    assert_eq!(spread_view(&with_prices("100", "100.2", NOW - 10_000), Binance, Bybit, "BTCUSDT", NOW), None);
    assert_eq!(spread_view(&four(), Binance, Bybit, "BTCUSDT", NOW), None);
}

#[test]
fn a_reconciled_card_shows_the_tag_and_it_flips_when_the_quotes_move() {
    let vm = build(&with_prices("100", "100.2", NOW - 500), &Filter::default(), NOW);
    let tag = btc_card(&vm).spread.as_ref().expect("tag shown");
    assert_eq!((tag.tag.kind, tag.text.as_str()), (SpreadKind::Positive, "價差 positive +0.20%（標記價）"));
    // long 100 → 100.5 while short stays 100.2: positive → negative, no other input changed
    let vm = build(&with_prices("100.5", "100.2", NOW - 500), &Filter::default(), NOW);
    assert_eq!(btc_card(&vm).spread.as_ref().unwrap().tag.kind, SpreadKind::Negative);
    // the same snapshot goes stale as time passes: the tag disappears, no old price is used
    let vm = build(&with_prices("100.5", "100.2", NOW - 500), &Filter::default(), NOW + 60_000);
    assert_eq!(btc_card(&vm).spread, None);
}

#[test]
fn a_reconciled_card_without_prices_shows_no_tag() {
    let vm = build(&four(), &Filter::default(), NOW);
    assert!(vm.pair_cards.iter().all(|c| c.spread.is_none()));
}

#[test]
fn a_locked_card_shows_the_tag_only_while_a_leg_is_held() {
    let mut s = with_prices("100", "100.2", NOW - 500);
    s.pairs[0].state = Ok(PairState::PartialFailure);
    let vm = build(&s, &Filter::default(), NOW);
    assert_eq!(btc_card(&vm).spread.as_ref().unwrap().tag.kind, SpreadKind::Positive);
    // both legs gone from the account lists: nothing is held, nothing is shown
    for ex in [Binance, Bybit] {
        s.accounts.insert(ex, loaded(vec![], vec![], NOW));
    }
    let vm = build(&s, &Filter::default(), NOW);
    assert_eq!(btc_card(&vm).spread, None);
}

// ---- live-banners 3.4: the LIVE badge on pair cards ----------------------------------------------

#[test]
fn live_pair_cards_carry_the_live_badge_and_demo_ones_do_not() {
    let mut s = four();
    s.identity = crate::identity::ProcessIdentity::Live;
    let vm = build(&s, &Filter::default(), NOW);
    assert!(!vm.pair_cards.is_empty() && vm.pair_cards.iter().all(|c| c.badge == Some("LIVE")));
    s.identity = crate::identity::ProcessIdentity::Demo;
    assert!(build(&s, &Filter::default(), NOW).pair_cards.iter().all(|c| c.badge.is_none()));
}

#[test]
fn locked_pair_cards_carry_the_badge_too() {
    let mut s = four();
    s.identity = crate::identity::ProcessIdentity::Live;
    s.pairs.push(pair("p3", "SOLUSDT", Binance, Bybit, PairState::PartialFailure));
    let vm = build(&s, &Filter::default(), NOW);
    let locked = vm.pair_cards.iter().find(|c| c.symbol == "SOLUSDT").expect("locked pair card");
    assert_eq!(locked.badge, Some("LIVE"));
}

// ---- ui-plain-language 2.3: an exchange the risk settings left unticked ------------------------------

mod unticked_exchange {
    use super::*;

    const OKX_SPOT: &str = "exchange error acct_mode_unsupported: 帳戶模式不支援 (acctLv 1, posMode net_mode)";

    fn unticked_okx(s: &mut UiSnapshot) {
        s.settings.risk.allowed_exchanges = vec![Binance, Bybit];
    }

    #[test]
    fn a_read_failure_of_an_unticked_exchange_is_one_muted_line_and_no_yellow_notice() {
        let mut s = four();
        s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.notices.iter().all(|n| !n.contains("OKX")), "no yellow OKX notice: {:?}", vm.notices);
        assert_eq!(vm.disabled.len(), 1, "exactly one line for the unticked exchange");
        let n = &vm.disabled[0];
        // the read failed: the positions are not confirmed, so the line is a warning, not the quiet one
        assert_eq!((n.exchange, n.text.as_str(), n.tone), (Okx, "OKX 已停用 · 無法確認持倉（OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式））", Tone::Warning));
        let detail = n.detail.as_deref().expect("detail behind the line");
        assert!(detail.contains("OKX 持倉讀取失敗") && detail.contains("OKX 帳戶模式不支援"), "{detail}");
        assert!(detail.contains("acct_mode_unsupported"), "the raw message stays reachable: {detail}");
        assert!(!n.text.contains("失敗"));
    }

    #[test]
    fn a_position_on_an_unticked_exchange_is_listed_as_usual_and_marked() {
        let mut s = with_okx_short();
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        let okx = rows(&vm).iter().find(|r| r.exchange == Okx).expect("the position is listed");
        assert_eq!(okx.note, Some("此交易所已停用，但仍有持倉或持倉狀態未確認"));
        assert_eq!(okx.cells()[3], "0.030000 BTC（3 張）", "the row itself is unchanged");
        assert!(rows(&vm).iter().filter(|r| r.exchange != Okx).all(|r| r.note.is_none()));
        assert_eq!(vm.title, "2 OPEN · 1 PAIRS", "the exposure is still counted and grouped");
        assert_eq!(vm.pair_cards.len(), 1);
        assert!(vm.disabled.is_empty(), "an exchange with exposure is not folded into a line: {:?}", vm.disabled);
        assert!(vm.notices.contains(&format!("OKX 已停用（風控設定未勾選） · {DISABLED_WITH_EXPOSURE}")), "{:?}", vm.notices);
        assert_eq!(vm.exchange_options, [Binance, Okx], "its filter chip stays while it has rows");
    }

    #[test]
    fn a_ticked_exchange_row_has_no_note_and_its_notices_read_in_plain_words() {
        let mut s = four();
        s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.disabled.is_empty());
        assert!(vm.notices.contains(&"OKX 持倉讀取失敗：OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式）".to_string()), "{:?}", vm.notices);
        assert!(rows(&vm).iter().all(|r| r.note.is_none()));
        // an error that is not in the table stays as it is
        s.accounts.insert(Okx, AccountState::Failed { error: "request timed out".into(), at: NOW });
        assert!(build(&s, &Filter::default(), NOW).notices.contains(&"OKX 持倉讀取失敗：request timed out".to_string()));
    }

    #[test]
    fn every_unticked_exchange_gets_its_line_even_when_nothing_is_wrong() {
        let mut s = four();
        s.accounts.insert(Okx, loaded(vec![], vec![], NOW));
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        assert_eq!(vm.disabled.len(), 1);
        assert_eq!((vm.disabled[0].exchange, vm.disabled[0].detail.clone()), (Okx, None), "nothing to open");
        // and still-loading / not-connected states sit behind the line
        s.accounts.remove(&Okx);
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.disabled[0].detail.as_deref().is_some_and(|d| d.contains("OKX 持倉載入中")), "{:?}", vm.disabled);
        s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.disabled[0].detail.as_deref().is_some_and(|d| d.contains("OKX 未連線：Keychain 沒有 API key")), "{:?}", vm.disabled);
        assert!(vm.notices.iter().all(|n| !n.contains("OKX")));
    }

    const INCOMPLETE: &str = "ETHUSDT 空 7 張（無法換算：ctVal 未知）";

    fn exposure_line() -> String {
        format!("OKX 已停用（風控設定未勾選） · {DISABLED_WITH_EXPOSURE}")
    }

    #[test]
    fn an_incomplete_list_of_an_unticked_exchange_stays_a_notice_and_is_not_folded() {
        let mut s = four();
        let mut okx = loaded(vec![], vec![], NOW);
        if let AccountState::Loaded { data, .. } = &mut okx {
            data.positions_incomplete = Some(INCOMPLETE.into());
        }
        s.accounts.insert(Okx, okx);
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.disabled.is_empty(), "{:?}", vm.disabled);
        assert!(vm.notices.contains(&"OKX 持倉列表可能不完整".to_string()), "{:?}", vm.notices);
        assert!(vm.notices.iter().any(|n| n.contains(INCOMPLETE)), "{:?}", vm.notices);
        assert!(vm.notices.contains(&exposure_line()), "{:?}", vm.notices);
    }

    #[test]
    fn old_data_with_a_failed_latest_read_of_an_unticked_exchange_is_unconfirmed_not_empty() {
        let mut s = four();
        let mut okx = loaded(vec![], vec![], NOW - 60_000);
        if let AccountState::Loaded { error, .. } = &mut okx {
            *error = Some(("request timed out".into(), NOW - 1_000));
        }
        s.accounts.insert(Okx, okx);
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        assert_eq!(vm.disabled.len(), 1);
        let n = &vm.disabled[0];
        assert_eq!(n.tone, Tone::Warning);
        assert!(n.text.starts_with("OKX 已停用 · 無法確認持倉（") && n.text.contains("request timed out"), "{}", n.text);
        assert!(!n.text.contains("沒有持倉") && !n.detail.clone().unwrap_or_default().contains("沒有持倉"), "{n:?}");
    }

    #[test]
    fn a_failed_read_with_an_unfinished_pair_on_the_unticked_exchange_is_a_notice_with_the_exposure_line() {
        let mut s = four();
        s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
        s.pairs.push(pair("p3", "SOLUSDT", Binance, Okx, PairState::OrderSubmit));
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.disabled.is_empty(), "{:?}", vm.disabled);
        assert!(vm.notices.iter().any(|n| n.contains("OKX 持倉讀取失敗")), "{:?}", vm.notices);
        assert!(vm.notices.contains(&exposure_line()), "{:?}", vm.notices);
    }

    #[test]
    fn a_successful_empty_read_is_the_quiet_line_even_with_finished_pairs_on_the_exchange() {
        let mut s = four();
        s.accounts.insert(Okx, loaded(vec![], vec![], NOW));
        s.pair_records = [("u1".to_string(), crate::ui::bridge::PairRecord { pair_id: "p9".into(), symbol: "SOLUSDT".into(), long_exchange: Some(Binance), short_exchange: Some(Okx), status: "FINALIZED".into() })].into();
        unticked_okx(&mut s);
        let vm = build(&s, &Filter::default(), NOW);
        assert_eq!((vm.disabled.len(), vm.disabled[0].tone, vm.disabled[0].text.as_str()), (1, Tone::Muted, "OKX 已停用（風控設定未勾選）"));
        assert!(vm.notices.iter().all(|n| !n.contains("OKX")));
    }

    #[test]
    fn nothing_is_hidden_when_the_settings_cannot_be_read() {
        let mut s = four();
        s.accounts.insert(Okx, AccountState::Failed { error: OKX_SPOT.into(), at: NOW });
        unticked_okx(&mut s);
        s.settings.error = Some("config.risk is not valid JSON".into());
        let vm = build(&s, &Filter::default(), NOW);
        assert!(vm.disabled.is_empty());
        assert!(vm.notices.iter().any(|n| n.contains("OKX 持倉讀取失敗")));
    }
}

#[test]
fn every_locked_state_reads_in_plain_words_with_the_code_after_it() {
    for (st, text) in [(PairState::PartialFailure, "單腿失敗（PARTIAL_FAILURE） · 需人工處理"), (PairState::Imbalanced, "兩腿不平衡（IMBALANCED） · 需人工處理"), (PairState::Unresolved, "狀態不明（UNRESOLVED） · 需人工處理")] {
        let mut s = four();
        s.pairs[0].state = Ok(st);
        let vm = build(&s, &Filter::default(), NOW);
        assert_eq!(vm.pair_cards.iter().find(|c| c.symbol == "BTCUSDT").unwrap().state_warning.as_deref(), Some(text));
    }
}

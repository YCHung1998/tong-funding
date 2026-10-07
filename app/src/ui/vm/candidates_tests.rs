use super::*;
use crate::engine::command::{Command, PairView};
use crate::ui::bridge::{CommandSink, ContractTemplate, EngineState, MarketFeed, UiSnapshot};
use crate::ui::scanner::{self, Qualified};
use crate::ui::testkit::{complete_settings, d, obs};
use std::cell::RefCell;
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

const NOW: i64 = 1_800_000_000_000;
const T: i64 = NOW + 3_600_000;

#[derive(Default)]
struct Sink(RefCell<Vec<Command>>);
impl CommandSink for Sink {
    fn send(&self, _label: String, command: Command) {
        self.0.borrow_mut().push(command);
    }
}

/// BTC qualifies on Binance (long) / Bybit (short); ETH does not; SOL's best pair includes OKX.
fn snap() -> UiSnapshot {
    let mut s = UiSnapshot { settings: complete_settings("0.01"), ..UiSnapshot::default() };
    let mk = |ex: Exchange, sym: &str, rate: &str, price: &str| {
        let mut o = obs(ex, sym, rate, 28_800, T, NOW - 100);
        o.mark_price = d(price);
        o
    };
    s.market.insert(Exchange::Binance, MarketFeed { observations: vec![mk(Exchange::Binance, "BTCUSDT", "-0.001", "60000"), mk(Exchange::Binance, "ETHUSDT", "0.0001", "3000")], last_success_at: Some(NOW - 100), last_error: None });
    s.market.insert(Exchange::Bybit, MarketFeed { observations: vec![mk(Exchange::Bybit, "BTCUSDT", "0.001", "60010"), mk(Exchange::Bybit, "ETHUSDT", "0.0001", "3000")], last_success_at: Some(NOW - 100), last_error: None });
    s.engine = Some(EngineState { now_ms: NOW, trigger_mode: TriggerMode::Manual, execution_mode: ExecutionMode::Simulation, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] });
    s.settings.contract = ContractTemplate { notional_usdt: d("1200"), leverage: d("3") };
    s
}

fn row<'a>(vm: &'a scanner::ScannerVm, sym: &str) -> &'a scanner::ScanRow {
    vm.rows.iter().find(|r| r.symbol == sym).unwrap()
}

#[test]
fn a_qualified_tradable_fresh_row_can_be_added_and_nothing_is_sent() {
    let s = snap();
    let vm = scanner::build(&s, NOW);
    assert_eq!(eligibility(row(&vm, "BTCUSDT"), &s, NOW), Ok(()));
    let mut list = CandidateList::default();
    assert!(list.toggle(row(&vm, "BTCUSDT"), &s, NOW));
    assert!(list.contains("BTCUSDT"));
    assert!(list.toggle(row(&vm, "BTCUSDT"), &s, NOW), "toggling again removes it");
    assert!(!list.contains("BTCUSDT"));
}

#[test]
fn a_row_that_does_not_qualify_cannot_be_added() {
    let s = snap();
    let vm = scanner::build(&s, NOW);
    assert_eq!(eligibility(row(&vm, "ETHUSDT"), &s, NOW), Err(CandidateBlock::NotQualified));
    assert_eq!(CandidateBlock::NotQualified.text(), "未達標");
    let mut list = CandidateList::default();
    assert!(!list.toggle(row(&vm, "ETHUSDT"), &s, NOW));
    assert!(!list.contains("ETHUSDT"));
}

#[test]
fn an_okx_leg_stale_data_and_an_incomplete_config_are_reasons() {
    let s = snap();
    let mut r = row(&scanner::build(&s, NOW), "BTCUSDT").clone();
    r.direction = Some((Exchange::Okx, Exchange::Bybit));
    assert_eq!(eligibility(&r, &s, NOW), Err(CandidateBlock::CompareOnly));
    assert_eq!(CandidateBlock::CompareOnly.text(), "OKX 僅比價");

    let mut stale = snap();
    stale.market.get_mut(&Exchange::Bybit).unwrap().last_error = Some(("timeout".into(), NOW));
    let vm = scanner::build(&stale, NOW);
    assert_eq!(eligibility(row(&vm, "BTCUSDT"), &stale, NOW), Err(CandidateBlock::StaleData(Exchange::Bybit)));

    let mut inc = snap();
    inc.settings.risk.est_slippage_pct = None;
    let vm = scanner::build(&inc, NOW);
    assert_eq!(eligibility(row(&vm, "BTCUSDT"), &inc, NOW), Err(CandidateBlock::NotDecidable));

    let s = snap();
    let r = row(&scanner::build(&s, NOW), "BTCUSDT").clone();
    assert_eq!(eligibility(&r, &s, T), Err(CandidateBlock::SettlementPassed));
}

#[test]
fn a_symbol_already_staged_cannot_be_added() {
    let mut s = snap();
    s.engine.as_mut().unwrap().pairs.push(PairView {
        internal_uuid: "u1".into(),
        pair_id: "p1".into(),
        symbol: "BTCUSDT".into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        state: PairState::Prepared,
        settlement_ms: T,
        simulated: true,
        flat_confirmed: false,
    });
    let vm = scanner::build(&s, NOW);
    assert_eq!(eligibility(row(&vm, "BTCUSDT"), &s, NOW), Err(CandidateBlock::AlreadyStaged));
    assert_eq!(CandidateBlock::AlreadyStaged.text(), "已在交易單");
}

#[test]
fn scanner_qualification_also_requires_min_expected_net_pnl() {
    let mut s = snap();
    // BTC: income 0.2 % − fees 0.08 % = 0.12 % expected; a 0.5 % minimum fails it.
    s.settings.risk.min_expected_net_pnl_pct = d("0.5");
    let vm = scanner::build(&s, NOW);
    assert_eq!(row(&vm, "BTCUSDT").qualified, Qualified::No);
}

#[test]
fn candidate_views_use_the_contract_template_and_turn_invalid_when_data_changes() {
    let s = snap();
    let vm = scanner::build(&s, NOW);
    let mut list = CandidateList::default();
    list.toggle(row(&vm, "BTCUSDT"), &s, NOW);
    let v = &views(&list, &vm, &s, NOW)[0];
    assert_eq!((v.long, v.short), (Exchange::Binance, Exchange::Bybit));
    assert_eq!((v.notional, v.leverage, v.margin), (d("1200"), d("3"), d("400")));
    assert_eq!(v.settlement_ms, Some(T));
    assert_eq!(v.valid, Ok(()));

    let mut later = s.clone();
    later.settings.risk.min_expected_net_pnl_pct = d("0.5");
    let vm2 = scanner::build(&later, NOW);
    assert_eq!(views(&list, &vm2, &later, NOW)[0].valid, Err("未達標".to_string()));
    let sink = Sink::default();
    assert_eq!(add_to_staged(&list, &vm2, &later, NOW, &sink), 0);
    assert!(sink.0.borrow().is_empty());
}

#[test]
fn adding_sends_one_add_prepared_per_valid_candidate_and_no_order() {
    let s = snap();
    let vm = scanner::build(&s, NOW);
    let mut list = CandidateList::default();
    list.toggle(row(&vm, "BTCUSDT"), &s, NOW);
    let sink = Sink::default();
    assert_eq!(add_to_staged(&list, &vm, &s, NOW, &sink), 1);
    let sent = sink.0.borrow();
    assert_eq!(sent.len(), 1);
    match &sent[0] {
        Command::AddPrepared(p) => {
            assert_eq!(p.symbol, "BTCUSDT");
            assert_eq!((p.long_exchange, p.short_exchange, p.settlement_ms), (Exchange::Binance, Exchange::Bybit, T));
            assert!(!p.internal_uuid.is_empty() && !p.pair_id.is_empty());
            let entry = crate::engine::node0::EntrySnapshot::from_json(&p.entry).expect("Node 0 can read the snapshot");
            assert_eq!((entry.long_scan_price, entry.short_scan_price), (d("60000"), d("60010")));
            assert_eq!((entry.notional_usdt, entry.leverage), (d("1200"), d("3")));
            assert!(p.entry.get("net_edge_pct").is_some() && p.entry.get("gross_spread").is_some());
        }
        other => panic!("{other:?}"),
    }
    assert!(sent.iter().all(|c| !c.opens_exposure()), "adding never opens exposure");
}

#[test]
fn an_invalid_contract_template_blocks_adding() {
    let mut s = snap();
    s.settings.contract_error = Some("contract_template.leverage must be > 0".into());
    let vm = scanner::build(&s, NOW);
    let mut list = CandidateList::default();
    list.toggle(row(&vm, "BTCUSDT"), &s, NOW);
    assert!(views(&list, &vm, &s, NOW)[0].valid.as_ref().unwrap_err().contains("合約模板"));
}

// ---- candidate-readd-after-close ------------------------------------------------------------

use crate::engine::ports::{AccountOrder, AccountPosition, Listed};
use crate::ui::bridge::LegAccount;

fn pair_in(s: &mut UiSnapshot, uuid: &str, state: PairState, flat_confirmed: bool) {
    s.engine.as_mut().unwrap().pairs.push(PairView {
        internal_uuid: uuid.into(),
        pair_id: format!("pid-{uuid}"),
        symbol: "BTCUSDT".into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        state,
        settlement_ms: T,
        simulated: false,
        flat_confirmed,
    });
}

/// Both legs' (demo) accounts read at `fetched_at`; `positions` / `orders` as (exchange, symbol, qty).
fn accounts(s: &mut UiSnapshot, positions: &[(Exchange, &str, &str)], orders: &[(Exchange, &str, &str)], complete: bool, fetched_at: i64) {
    for ex in [Exchange::Binance, Exchange::Bybit] {
        let pos = positions.iter().filter(|p| p.0 == ex).map(|p| AccountPosition { exchange: ex, symbol: p.1.into(), quantity: d(p.2) }).collect();
        let ord = orders.iter().filter(|o| o.0 == ex).map(|o| AccountOrder { exchange: ex, symbol: o.1.into(), client_order_id: None, remaining_quantity: d(o.2) }).collect();
        s.leg_accounts.insert(
            (false, ex),
            LegAccount { positions: Ok(Listed { items: pos, complete }), open_orders: Ok(Listed { items: ord, complete }), available_margin: Ok(d("1000")), fetched_at },
        );
    }
}

fn eligible_btc(s: &UiSnapshot) -> Result<(), CandidateBlock> {
    eligibility(row(&scanner::build(s, NOW), "BTCUSDT"), s, NOW)
}

#[test]
fn a_closing_pair_with_the_flat_confirmation_does_not_block_re_adding() {
    // NMRUSDT in the recorded data: CLOSE_CONFIRMED verified_flat, then CLOSING until the PnL settles.
    let mut s = snap();
    pair_in(&mut s, "u1", PairState::Closing, true);
    assert_eq!(eligible_btc(&s), Ok(()));
}

#[test]
fn a_closing_pair_without_the_flat_confirmation_still_blocks() {
    let mut s = snap();
    pair_in(&mut s, "u1", PairState::Closing, false);
    assert_eq!(eligible_btc(&s), Err(CandidateBlock::AlreadyStaged));
}

#[test]
fn a_locked_pair_whose_legs_are_flat_does_not_block_re_adding() {
    // OGNUSDT in the recorded data: both legs closed by manual orders, scheduled close then left PARTIAL_FAILURE.
    for state in [PairState::PartialFailure, PairState::Imbalanced, PairState::Unresolved] {
        let mut s = snap();
        pair_in(&mut s, "u1", state, false);
        // A position on ANOTHER symbol and a zero-size row on this one are not exposure of the pair.
        accounts(&mut s, &[(Exchange::Binance, "ETHUSDT", "3"), (Exchange::Bybit, "BTCUSDT", "0")], &[], true, NOW - 1_000);
        assert_eq!(eligible_btc(&s), Ok(()), "{state:?}");
    }
}

#[test]
fn a_locked_pair_with_a_position_or_an_open_order_on_the_symbol_still_blocks() {
    for (positions, orders) in [
        (vec![(Exchange::Binance, "BTCUSDT", "0.5")], vec![]),
        (vec![(Exchange::Bybit, "BTCUSDT", "-0.5")], vec![]),
        (vec![], vec![(Exchange::Bybit, "BTCUSDT", "0.5")]),
    ] {
        let mut s = snap();
        pair_in(&mut s, "u1", PairState::PartialFailure, false);
        accounts(&mut s, &positions, &orders, true, NOW - 1_000);
        assert_eq!(eligible_btc(&s), Err(CandidateBlock::AlreadyStaged), "{positions:?} {orders:?}");
    }
}

#[test]
fn an_unknown_incomplete_failed_or_stale_account_read_is_never_treated_as_flat() {
    let locked = || {
        let mut s = snap();
        pair_in(&mut s, "u1", PairState::Imbalanced, false);
        s
    };
    let mut incomplete = locked();
    accounts(&mut incomplete, &[], &[], false, NOW - 1_000);

    let mut stale = locked();
    accounts(&mut stale, &[], &[], true, NOW - 60_000);

    let mut failed = locked();
    accounts(&mut failed, &[], &[], true, NOW - 1_000);
    failed.leg_accounts.get_mut(&(false, Exchange::Bybit)).unwrap().positions = Err("timeout".into());

    let mut one_leg_missing = locked();
    accounts(&mut one_leg_missing, &[], &[], true, NOW - 1_000);
    one_leg_missing.leg_accounts.remove(&(false, Exchange::Binance));

    for (name, s) in [("incomplete", incomplete), ("stale", stale), ("failed", failed), ("missing", one_leg_missing), ("never read", locked())] {
        assert_eq!(eligible_btc(&s), Err(CandidateBlock::AlreadyStaged), "{name}");
    }
}

#[test]
fn a_flat_old_pair_does_not_hide_a_running_pair_on_the_same_symbol() {
    let mut s = snap();
    pair_in(&mut s, "old", PairState::Closing, true);
    pair_in(&mut s, "new", PairState::Prepared, false);
    assert_eq!(eligible_btc(&s), Err(CandidateBlock::AlreadyStaged));
}

// ---- symbol-leverage-cap: candidate list ------------------------------------------------------

fn put_caps(s: &mut UiSnapshot, sym: &str, bin: &str, byb: &str) {
    for (ex, c) in [(Exchange::Binance, bin), (Exchange::Bybit, byb)] {
        s.leverage_caps.insert((ex, sym.into()), crate::ui::bridge::CapReading { notional: d("1200"), cap: Ok(d(c)), fetched_at: NOW - 1_000 });
    }
}

fn add_one(s: &UiSnapshot) -> (usize, Vec<CandidateView>) {
    let vm = scanner::build(s, NOW);
    let mut list = CandidateList::default();
    assert!(list.toggle(row(&vm, "BTCUSDT"), s, NOW));
    let sink = Sink::default();
    (add_to_staged(&list, &vm, s, NOW, &sink), views(&list, &vm, s, NOW))
}

#[test]
fn a_known_cap_below_the_contract_leverage_refuses_the_add_and_says_why() {
    let mut s = snap(); // contract leverage 3x
    put_caps(&mut s, "BTCUSDT", "20", "2");
    let (sent, v) = add_one(&s);
    assert_eq!(sent, 0);
    assert_eq!(v[0].valid, Err("槓桿 3× 超過 Bybit 上限 2×".to_string()));
    assert!(v[0].cap.text().contains("Bybit 2×"));
}

#[test]
fn caps_that_fit_or_are_unknown_do_not_refuse_the_add() {
    let mut s = snap();
    put_caps(&mut s, "BTCUSDT", "20", "10");
    let (sent, v) = add_one(&s);
    assert_eq!(sent, 1);
    assert!(v[0].cap.text().ends_with('✓'));
    let unknown = snap(); // never read: shown, not blocking
    let (sent, v) = add_one(&unknown);
    assert_eq!(sent, 1);
    assert!(v[0].cap.text().contains("未知"), "{}", v[0].cap.text());
}

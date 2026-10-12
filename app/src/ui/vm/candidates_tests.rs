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
fn stale_data_an_incomplete_config_and_a_passed_settlement_are_reasons() {
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

#[test]
fn an_okx_pair_is_an_ordinary_candidate_under_the_general_rules() {
    // SOL: OKX is the cheapest leg, Bybit the dearest; everything else is as for any other pair.
    let mut s = snap();
    let mk = |ex: Exchange, rate: &str| {
        let mut o = obs(ex, "SOLUSDT", rate, 28_800, T, NOW - 100);
        o.mark_price = d("150");
        o
    };
    s.market.insert(Exchange::Okx, MarketFeed { observations: vec![mk(Exchange::Okx, "-0.002")], last_success_at: Some(NOW - 100), last_error: None });
    s.market.get_mut(&Exchange::Bybit).unwrap().observations.push(mk(Exchange::Bybit, "0.002"));
    let vm = scanner::build(&s, NOW);
    let sol = row(&vm, "SOLUSDT");
    assert_eq!(sol.direction, Some((Exchange::Okx, Exchange::Bybit)));
    assert_eq!(eligibility(sol, &s, NOW), Ok(()));
    let mut list = CandidateList::default();
    assert!(list.toggle(sol, &s, NOW));
    // the OKX leg is judged like any other: its failing feed blocks the row
    s.market.get_mut(&Exchange::Okx).unwrap().last_error = Some(("timeout".into(), NOW));
    let vm = scanner::build(&s, NOW);
    assert_eq!(eligibility(row(&vm, "SOLUSDT"), &s, NOW), Err(CandidateBlock::StaleData(Exchange::Okx)));
}

// ---- automatic adding ------------------------------------------------------------------------------------

fn auto_on() -> AutoAddState {
    let mut a = AutoAddState::default();
    a.request_enable();
    a.confirm_enable();
    assert!(a.enabled);
    a
}

fn auto_run(state: &mut AutoAddState, s: &UiSnapshot, now: i64) -> (usize, Vec<Command>) {
    let vm = scanner::build(s, now);
    let sink = Sink::default();
    let n = auto_add_tick(state, &vm, s, now, &sink);
    (n, sink.0.borrow().clone())
}

fn added_symbols(cmds: &[Command]) -> Vec<String> {
    cmds.iter().filter_map(|c| if let Command::AddPrepared(p) = c { Some(p.symbol.clone()) } else { None }).collect()
}

#[test]
fn auto_add_is_off_until_it_is_confirmed_and_never_stored() {
    let s = snap();
    let mut off = AutoAddState::default();
    assert_eq!(auto_run(&mut off, &s, NOW).0, 0, "off by default");
    off.request_enable();
    assert!(off.confirming && !off.enabled, "the first click only asks");
    assert_eq!(auto_run(&mut off, &s, NOW).0, 0, "still nothing while the confirmation is open");
    off.cancel_confirm();
    off.confirm_enable();
    assert!(!off.enabled, "no confirmation was open: confirm does nothing");
    off.request_enable();
    off.confirm_enable();
    assert!(off.enabled && !off.confirming);
    off.disable();
    assert!(!off.enabled && !off.confirming);
    assert_eq!(AutoAddState::default(), AutoAddState { ..Default::default() }, "a fresh state (a restart) is off");
}

#[test]
fn auto_add_sends_only_the_qualified_row_once_and_marks_its_origin() {
    let s = snap();
    let mut a = auto_on();
    let (n, cmds) = auto_run(&mut a, &s, NOW);
    assert_eq!((n, added_symbols(&cmds)), (1, vec!["BTCUSDT".to_string()]), "ETH does not qualify");
    let Command::AddPrepared(p) = &cmds[0] else { panic!() };
    assert_eq!(p.entry["source"], serde_json::json!("auto_add"));
    assert_eq!((p.settlement_ms, p.long_exchange, p.short_exchange), (T, Exchange::Binance, Exchange::Bybit));
    assert_eq!((a.added_total, a.last_added_ms, a.last_symbols.clone()), (1, Some(NOW), vec!["BTCUSDT".to_string()]));
    assert_eq!(auto_run(&mut a, &s, NOW + 1_000).0, 0, "not again for the same settlement");
}

#[test]
fn auto_add_does_not_add_a_cancelled_pair_again_for_the_same_settlement() {
    let mut s = snap();
    let mut a = auto_on();
    assert_eq!(auto_run(&mut a, &s, NOW).0, 1);
    // The engine auto-cancelled it (net edge fell below the threshold): no add → cancel → add loop.
    pair_in(&mut s, "u1", PairState::Cancelled, false);
    assert_eq!(auto_run(&mut a, &s, NOW + 30_000).0, 0);
    // After a restart the memory is gone, but the engine still lists the pair for that settlement.
    let mut fresh = auto_on();
    assert_eq!(auto_run(&mut fresh, &s, NOW + 60_000).0, 0, "a pair of any state for this settlement blocks it");
}

#[test]
fn auto_add_needs_enough_time_before_the_entry() {
    let s = snap();
    let mut a = auto_on();
    assert_eq!(auto_run(&mut a, &s, T - AUTO_ADD_MIN_LEAD_MS + 1_000).0, 0, "too close to the settlement");
    assert_eq!(auto_run(&mut a, &s, T - AUTO_ADD_MIN_LEAD_MS - 1_000).0, 1);
}

#[test]
fn auto_add_adds_nothing_while_the_engine_refuses_or_is_not_running() {
    let mut s = snap();
    let mut a = auto_on();
    s.engine.as_mut().unwrap().blockers = vec![crate::engine::command::Blocker::KillSwitch];
    assert_eq!(auto_run(&mut a, &s, NOW).0, 0, "kill switch");
    assert!(auto_add_blocked(&s).unwrap().contains("緊急停止"));
    s.engine = None;
    assert_eq!(auto_run(&mut a, &s, NOW).0, 0, "no engine");
    assert_eq!(auto_add_blocked(&s).as_deref(), Some("引擎未啟動"));
    let mut bad = snap();
    bad.settings.contract_error = Some("x".into());
    assert!(auto_add_blocked(&bad).unwrap().contains("合約模板無效"));
}

#[test]
fn auto_add_never_exceeds_the_free_pair_slots_and_takes_the_best_net_edge_first() {
    let mut s = snap();
    // A second qualifying symbol with a wider spread (a better Net Edge).
    for (ex, rate, price) in [(Exchange::Binance, "-0.004", "1.2"), (Exchange::Bybit, "0.004", "1.2")] {
        let mut o = obs(ex, "XRPUSDT", rate, 28_800, T, NOW - 100);
        o.mark_price = d(price);
        s.market.get_mut(&ex).unwrap().observations.push(o);
    }
    let vm = scanner::build(&s, NOW);
    assert_eq!(row(&vm, "XRPUSDT").qualified, Qualified::Yes);
    assert!(row(&vm, "XRPUSDT").net_edge.value() > row(&vm, "BTCUSDT").net_edge.value(), "XRP has the better edge");
    s.settings.risk.max_concurrent_pairs = 1;
    let mut a = auto_on();
    let (n, cmds) = auto_run(&mut a, &s, NOW);
    assert_eq!((n, added_symbols(&cmds)), (1, vec!["XRPUSDT".to_string()]), "one free slot: the better edge");
    // A slot taken by another pair (PREPARED) leaves no room at all.
    let mut full = snap();
    full.settings.risk.max_concurrent_pairs = 1;
    pair_in(&mut full, "u1", PairState::Prepared, false);
    full.engine.as_mut().unwrap().pairs[0].symbol = "SOLUSDT".into();
    assert_eq!(auto_run(&mut auto_on(), &full, NOW).0, 0);
    // A flat-confirmed CLOSING pair (waiting only for its funding PnL) holds no slot.
    let mut closing = snap();
    closing.settings.risk.max_concurrent_pairs = 1;
    pair_in(&mut closing, "u2", PairState::Closing, true);
    closing.engine.as_mut().unwrap().pairs[0].symbol = "SOLUSDT".into();
    assert_eq!(auto_run(&mut auto_on(), &closing, NOW).0, 1);
}

#[test]
fn auto_add_respects_the_leverage_cap_check() {
    let mut s = snap(); // contract leverage 3x
    put_caps(&mut s, "BTCUSDT", "20", "2");
    assert_eq!(auto_run(&mut auto_on(), &s, NOW).0, 0, "a known cap below the leverage: never added");
    let mut ok = snap();
    put_caps(&mut ok, "BTCUSDT", "20", "10");
    assert_eq!(auto_run(&mut auto_on(), &ok, NOW).0, 1);
}

#[test]
fn auto_add_does_not_touch_a_symbol_that_is_already_staged() {
    let mut s = snap();
    pair_in(&mut s, "u1", PairState::Prepared, false);
    assert_eq!(auto_run(&mut auto_on(), &s, NOW).0, 0, "already staged (by hand or by an earlier pass)");
}

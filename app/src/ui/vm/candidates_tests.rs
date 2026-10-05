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

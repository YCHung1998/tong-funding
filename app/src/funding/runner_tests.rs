//! The running funding loop: merging, pacing, the reconciliation schedule and one tick against a
//! stored closed pair (fake ledger sources, injected time, no network).
use std::sync::Mutex;

use serde_json::json;
use tong_funding_core::pair::PairState;
use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::types::Exchange;

use super::*;
use crate::exchange::error::AdapterError;
use crate::exchange::signed::ledger::LedgerPage;
use crate::funding::fetch::BoxFut;
use crate::funding::pnl_record::tests::{Fx, T, ledger, scenario};
use crate::funding::pnl_record::{latest_pnl, settle_pnl};
use crate::funding::{FUNDING_LEDGER_FETCHED, PNL_RECONCILIATION};
use crate::store::events::EventStore;
use crate::store::state::NewPair;

/// Answers every page with the given entries (one page, exhausted) or a failure; counts calls.
struct Src {
    exchange: Exchange,
    entries: Result<Vec<FundingLedgerEntry>, AdapterError>,
    calls: Mutex<Vec<(String, i64, i64)>>,
}

impl Src {
    fn new(exchange: Exchange, entries: Vec<FundingLedgerEntry>) -> Src {
        Src { exchange, entries: Ok(entries), calls: Mutex::default() }
    }
    fn calls(&self) -> Vec<(String, i64, i64)> {
        self.calls.lock().unwrap().clone()
    }
}

impl LedgerSource for Src {
    fn exchange(&self) -> Exchange {
        self.exchange
    }
    fn per_symbol(&self) -> bool {
        matches!(self.exchange, Exchange::Binance | Exchange::Okx)
    }
    fn page<'a>(&'a self, symbol: &'a str, start: i64, end: i64, _t: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        self.calls.lock().unwrap().push((symbol.to_string(), start, end));
        let r = self.entries.clone().map(|e| LedgerPage { rows: e.len(), entries: e, next_cursor: None });
        Box::pin(std::future::ready(r))
    }
}

struct NoPause;
impl Pause for NoPause {
    fn pause(&self, _ms: u64) -> BoxFut<'_, ()> {
        Box::pin(std::future::ready(()))
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

fn all_ready(_: Exchange) -> Result<(), String> {
    Ok(())
}

fn plan(pair: &str, exchange: Exchange, symbol: &str, start: i64, end: i64) -> FetchPlan {
    FetchPlan { pair: pair.into(), exchange, symbol: symbol.into(), start_ms: start, end_ms: end }
}

fn count(fx: &Fx, ty: &str) -> usize {
    fx.db.query_events(&Default::default()).unwrap().rows.iter().filter(|e| e.event_type == ty).count()
}

/// The closed pair of `pnl_record` tests (open T−10 s, close T+15 s) and the exchange's answers.
fn sources() -> (Src, Src) {
    (Src::new(Exchange::Binance, vec![ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T)]), Src::new(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]))
}

fn run_tick(fx: &Fx, b: &Src, y: &Src, ready: Readiness<'_>, state: &mut FundingLoop, now: i64) -> TickReport {
    fx.clock.set(now);
    block_on(tick(&fx.db, &[b, y], ready, &NoPause, state, now))
}

// ---- pure parts -------------------------------------------------------------------------------

#[test]
fn funding_loop_merges_bybit_into_one_all_symbol_query_and_keeps_binance_per_symbol() {
    let plans = [
        plan("p1", Exchange::Binance, "BTCUSDT", 100, 200),
        plan("p1", Exchange::Bybit, "BTCUSDT", 100, 200),
        plan("p2", Exchange::Binance, "ETHUSDT", 50, 300),
        plan("p2", Exchange::Bybit, "ETHUSDT", 50, 300),
    ];
    let m = merge_plans(&plans, |ex| ex == Exchange::Binance);
    assert_eq!(m.len(), 3, "{m:?}");
    let bybit = m.iter().find(|x| x.key.exchange == Exchange::Bybit).unwrap();
    assert_eq!((bybit.key.symbol.as_deref(), bybit.start_ms, bybit.end_ms), (None, 50, 300), "union of both windows");
    assert_eq!(bybit.pairs, ["p1", "p2"]);
    let binance: Vec<_> = m.iter().filter(|x| x.key.exchange == Exchange::Binance).map(|x| (x.key.symbol.clone().unwrap(), x.start_ms, x.end_ms)).collect();
    assert_eq!(binance, [("BTCUSDT".to_string(), 100, 200), ("ETHUSDT".to_string(), 50, 300)]);
}

#[test]
fn funding_loop_paces_each_query_by_the_retry_period() {
    let mut s = FundingLoop::default();
    let k = FetchKey { exchange: Exchange::Bybit, symbol: None };
    assert!(s.fetch_due(&k, 1_000));
    s.mark(k.clone(), 1_000);
    assert!(!s.fetch_due(&k, 1_000 + FETCH_RETRY_MS - 1));
    assert!(s.fetch_due(&k, 1_000 + FETCH_RETRY_MS));
    assert!(s.fetch_due(&FetchKey { exchange: Exchange::Binance, symbol: Some("BTCUSDT".into()) }, 1_001), "other queries are independent");
}

#[test]
fn funding_loop_reconciles_once_after_the_pnl_and_retries_failures_inside_the_window() {
    use ReconcileResult::{Failed, Mismatch, Ok};
    let p = 10_000;
    assert!(!reconcile_due(None, &[], p * 100), "no PnL yet: nothing to reconcile");
    assert!(!reconcile_due(Some(p), &[], p + RECONCILE_DELAY_MS - 1));
    assert!(reconcile_due(Some(p), &[], p + RECONCILE_DELAY_MS));
    assert!(!reconcile_due(Some(p), &[(p + RECONCILE_DELAY_MS, Ok)], i64::MAX / 2), "OK is final");
    assert!(!reconcile_due(Some(p), &[(p + RECONCILE_DELAY_MS, Mismatch)], i64::MAX / 2), "MISMATCH stays an alert, never redone");
    let f = p + RECONCILE_DELAY_MS;
    assert!(!reconcile_due(Some(p), &[(f, Failed)], f + FETCH_RETRY_MS - 1));
    assert!(reconcile_due(Some(p), &[(f, Failed)], f + FETCH_RETRY_MS));
    assert!(reconcile_due(Some(p), &[(f, Failed), (f + FETCH_RETRY_MS, Failed)], f + 2 * FETCH_RETRY_MS));
    assert!(!reconcile_due(Some(p), &[(f, Failed), (f + PNL_RETRY_WINDOW_MS - 1, Failed)], f + PNL_RETRY_WINDOW_MS + FETCH_RETRY_MS), "window over: the last FAILED stays");
    assert!(!reconcile_due(Some(p), &[(f, Failed), (f + FETCH_RETRY_MS, Ok)], i64::MAX / 2), "a later OK ends it");
}

// ---- one tick against the store ---------------------------------------------------------------

#[test]
fn funding_loop_fetches_a_closed_pair_once_per_retry_period_then_reconciles_once_after_its_pnl() {
    let fx = scenario(true);
    let (b, y) = sources();
    let mut state = FundingLoop::default();
    let now = T + 20_000;
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, now);
    assert_eq!(r.fetched.len(), 2, "{r:?}");
    assert!(r.fetched.iter().all(|(_, s)| *s == Result::Ok(FetchStatus::Complete)), "{r:?}");
    assert!(r.reconciled.is_empty(), "no PnL yet");
    assert!(r.wrote());
    assert_eq!(b.calls(), [("BTCUSDT".to_string(), T - 10_000, T + 15_000)], "the leg's holding window");
    assert_eq!(count(&fx, FUNDING_LEDGER_FETCHED), 2);
    assert_eq!(fx.db.funding_ledger_entries().unwrap().len(), 2, "entries written through the store");
    // Paced: the same tick time again does nothing.
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, now + 1_000);
    assert_eq!(r, TickReport::default());
    // The engine records the PnL (what its wait does once the entries are there).
    let pnl_at = now + 2_000;
    fx.clock.set(pnl_at);
    settle_pnl(&fx.db, "p1", pnl_at, false).unwrap();
    // (INCOMPLETE here: the fixture's close orders carry no close reference price.)
    assert!(latest_pnl(&fx.db, "p1").unwrap().is_some());
    // Not before the reconcile delay; the closed leg's PnL is recorded, so no more fetches.
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, pnl_at + RECONCILE_DELAY_MS - 1);
    assert!(r.reconciled.is_empty() && r.fetched.is_empty(), "{r:?}");
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, pnl_at + RECONCILE_DELAY_MS);
    assert_eq!(r.reconciled, [("p1".to_string(), Result::Ok(ReconcileResult::Ok))]);
    assert_eq!(count(&fx, PNL_RECONCILIATION), 1);
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, pnl_at + RECONCILE_DELAY_MS + 10 * FETCH_RETRY_MS);
    assert!(r.reconciled.is_empty(), "reconciled once");
}

#[test]
fn funding_loop_missing_demo_keys_requests_and_writes_nothing_for_that_exchange() {
    let fx = scenario(true);
    let (b, y) = sources();
    let mut state = FundingLoop::default();
    let no_binance = |ex: Exchange| if ex == Exchange::Binance { Err("Binance keys unavailable (NoKey)".to_string()) } else { Ok(()) };
    let r = run_tick(&fx, &b, &y, &no_binance, &mut state, T + 20_000);
    assert!(b.calls().is_empty(), "no request without keys");
    assert_eq!(r.skipped, [(Exchange::Binance, "Binance keys unavailable (NoKey)".to_string())]);
    assert_eq!(r.fetched.len(), 1, "Bybit still fetched");
    assert_eq!(count(&fx, FUNDING_LEDGER_FETCHED), 1, "no fetched / error event for the skipped exchange");
    assert_eq!(count(&fx, crate::funding::FETCH_ERROR), 0);
    // A recorded PnL is not reconciled (nor marked FAILED) while a leg's keys are missing.
    fx.clock.set(T + 30_000);
    settle_pnl(&fx.db, "p1", T + 30_000, true).unwrap();
    let r = run_tick(&fx, &b, &y, &no_binance, &mut state, T + 30_000 + RECONCILE_DELAY_MS);
    assert!(r.reconciled.is_empty());
    assert_eq!(count(&fx, PNL_RECONCILIATION), 0);
    // With the keys back, the next tick reconciles.
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, T + 30_000 + RECONCILE_DELAY_MS + 1);
    assert_eq!(r.reconciled.len(), 1, "{r:?}");
}

#[test]
fn funding_loop_a_failed_reconciliation_is_retried_after_the_retry_period() {
    let fx = scenario(true);
    let (b, y) = sources();
    let failing = Src { exchange: Exchange::Binance, entries: Err(AdapterError::Timeout), calls: Mutex::default() };
    let mut state = FundingLoop::default();
    run_tick(&fx, &b, &y, &all_ready, &mut state, T + 20_000);
    fx.clock.set(T + 30_000);
    settle_pnl(&fx.db, "p1", T + 30_000, true).unwrap();
    let t1 = T + 30_000 + RECONCILE_DELAY_MS;
    let r = run_tick(&fx, &failing, &y, &all_ready, &mut state, t1);
    assert_eq!(r.reconciled, [("p1".to_string(), Result::Ok(ReconcileResult::Failed))]);
    assert!(run_tick(&fx, &failing, &y, &all_ready, &mut state, t1 + FETCH_RETRY_MS - 1).reconciled.is_empty());
    let r = run_tick(&fx, &b, &y, &all_ready, &mut state, t1 + FETCH_RETRY_MS);
    assert_eq!(r.reconciled, [("p1".to_string(), Result::Ok(ReconcileResult::Ok))]);
}

#[test]
fn funding_loop_is_not_stopped_by_the_kill_switch_but_by_a_halted_store() {
    let fx = scenario(true);
    let (b, y) = sources();
    fx.db.set_kill_switch(true).unwrap();
    let r = run_tick(&fx, &b, &y, &all_ready, &mut FundingLoop::default(), T + 20_000);
    assert_eq!(r.fetched.len(), 2, "read-only: the kill switch does not block it");
    let fx = scenario(true);
    fx.db.halt(crate::store::db::HaltReason::EventWriteFailed("disk".into()));
    let r = run_tick(&fx, &b, &y, &all_ready, &mut FundingLoop::default(), T + 20_000);
    assert_eq!(r, TickReport::default());
}

#[test]
fn funding_loop_never_reconciles_a_simulated_pair() {
    let fx = scenario(true);
    let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T, simulated: true, scan: json!({}) };
    let p = NewPair { internal_uuid: "sim".into(), pair_id: "pid-sim".into(), symbol: "ETHUSDT".into(), status: PairState::Prepared, entry: serde_json::to_value(env).unwrap() };
    fx.db.add_pair_if_not_pending(&p).unwrap();
    fx.clock.set(T);
    EventStore::new(fx.db.clone()).append(PAIR_PNL_COMPUTED, Some("sim"), json!({ "status": "COMPLETE" })).unwrap();
    let c = reconcile_candidates(&fx.db, T + RECONCILE_DELAY_MS * 10).unwrap();
    assert!(c.iter().all(|c| c.pair != "sim"), "{c:?}");
}

// ---- okx-funding-ledger: OKX legs are fetched and reconciled like the others --------------------------

use crate::funding::pnl_record::tests::{both_okx_fetched, okx_round};

fn okx_entry(amount: &str, id: &str, ts: i64) -> FundingLedgerEntry {
    FundingLedgerEntry::new(Exchange::Okx, "BTCUSDT", amount.parse().unwrap(), "USDT", ts, id, "173", json!({}))
}

#[test]
fn funding_loop_fetches_the_okx_leg_per_symbol_and_reconciles_it() {
    let fx = okx_round(Some("0.01"), "60300");
    let bybit = Src::new(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]);
    let okx = Src::new(Exchange::Okx, vec![okx_entry("-0.42", "623950854533513219", T + 500)]);
    let mut state = FundingLoop::default();
    let sources: [&dyn LedgerSource; 2] = [&bybit, &okx];
    let r = block_on(tick(&fx.db, &sources, &all_ready, &NoPause, &mut state, T + 20_000));
    assert!(r.skipped.is_empty() && r.errors.is_empty(), "{r:?}");
    let keys: Vec<_> = r.fetched.iter().map(|(k, _)| k.clone()).collect();
    assert!(keys.contains(&FetchKey { exchange: Exchange::Okx, symbol: Some("BTCUSDT".into()) }), "OKX is queried per symbol: {keys:?}");
    assert!(keys.contains(&FetchKey { exchange: Exchange::Bybit, symbol: None }));
    assert_eq!(okx.calls().len(), 1);
    // the OKX funding is attributed and the PnL can be recorded
    both_okx_fetched(&fx); // (moves the fixture clock to T + 70 s)
    let pnl_at = T + 80_000;
    fx.clock.set(pnl_at);
    settle_pnl(&fx.db, "p1", pnl_at, true).unwrap();
    let r = block_on(tick(&fx.db, &sources, &all_ready, &NoPause, &mut state, pnl_at + RECONCILE_DELAY_MS));
    assert_eq!(r.reconciled, [("p1".to_string(), Result::Ok(ReconcileResult::Ok))], "{r:?}");
}

#[test]
fn funding_loop_missing_okx_keys_skip_okx_only() {
    let fx = okx_round(Some("0.01"), "60300");
    let bybit = Src::new(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]);
    let okx = Src::new(Exchange::Okx, vec![]);
    let no_okx = |ex: Exchange| if ex == Exchange::Okx { Err("OKX keys unavailable (NoPassphrase)".to_string()) } else { Ok(()) };
    let sources: [&dyn LedgerSource; 2] = [&bybit, &okx];
    let r = block_on(tick(&fx.db, &sources, &no_okx, &NoPause, &mut FundingLoop::default(), T + 20_000));
    assert!(okx.calls().is_empty(), "no request without OKX keys");
    assert_eq!(r.skipped, [(Exchange::Okx, "OKX keys unavailable (NoPassphrase)".to_string())]);
    assert_eq!(r.fetched.len(), 1, "Bybit still fetched");
}

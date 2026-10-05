//! funding-pnl task 2.4 (app side): refetch, compare per leg, PNL_RECONCILIATION, no correction.
use std::sync::Mutex;

use serde_json::Value;
use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::types::Exchange;

use super::*;
use crate::exchange::error::AdapterError;
use crate::exchange::signed::ledger::LedgerPage;
use crate::funding::fetch::BoxFut;
use crate::funding::pnl_record::latest_pnl;
use crate::funding::pnl_record::tests::{Fx, T, both_fetched, ledger, scenario};

/// Answers every page with the scripted entries (one page, exhausted), or a failure.
struct Remote {
    exchange: Exchange,
    entries: Result<Vec<FundingLedgerEntry>, AdapterError>,
    calls: Mutex<usize>,
}

impl LedgerSource for Remote {
    fn exchange(&self) -> Exchange {
        self.exchange
    }
    fn per_symbol(&self) -> bool {
        self.exchange == Exchange::Binance
    }
    fn page<'a>(&'a self, _s: &'a str, _a: i64, _b: i64, _t: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        *self.calls.lock().unwrap() += 1;
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

fn remote(exchange: Exchange, entries: Vec<FundingLedgerEntry>) -> Remote {
    Remote { exchange, entries: Ok(entries), calls: Mutex::new(0) }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

fn local_fx() -> Fx {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    fx.clock.set(T + 100_000);
    fx
}

fn last_event(fx: &Fx) -> Value {
    crate::funding::pnl_record::pair_events(&fx.db, "p1").unwrap().into_iter().rev().find(|e| e.event_type == PNL_RECONCILIATION).unwrap().payload
}

fn run(fx: &Fx, binance: &Remote, bybit: &Remote) -> ReconcileResult {
    block_on(reconcile_pair(&fx.db, &[binance, bybit], &NoPause, "p1", T + 100_000)).unwrap()
}

#[test]
fn reconcile_identical_is_ok() {
    let fx = local_fx();
    let b = remote(Exchange::Binance, vec![ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T)]);
    let y = remote(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T), ledger(Exchange::Bybit, "ETHUSDT", "x", "9", T)]);
    assert_eq!(run(&fx, &b, &y), ReconcileResult::Ok, "other symbols in Bybit's all-symbol answer are not the leg's");
    let e = last_event(&fx);
    assert_eq!(e["result"], "OK");
    assert_eq!(e["alert"], false);
    assert_eq!((*b.calls.lock().unwrap(), *y.calls.lock().unwrap()), (1, 1), "refetched, no local cache");
}

#[test]
fn reconcile_a_different_amount_is_a_mismatch_with_both_sums_and_the_difference() {
    let fx = local_fx();
    let b = remote(Exchange::Binance, vec![ledger(Exchange::Binance, "BTCUSDT", "1", "-0.15", T)]);
    let y = remote(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]);
    assert_eq!(run(&fx, &b, &y), ReconcileResult::Mismatch);
    let e = last_event(&fx);
    assert_eq!(e["alert"], true);
    let leg = &e["legs"][0];
    assert_eq!((leg["result"].as_str(), leg["local_sum"].as_str(), leg["remote_sum"].as_str(), leg["diff"].as_str()), (Some("MISMATCH"), Some("-0.12"), Some("-0.15"), Some("-0.03")));
    assert_eq!(leg["exchange"], "Binance");
    // Nothing stored was changed, and the PnL now says INCOMPLETE ("對帳差異").
    assert_eq!(fx.db.funding_ledger_entries().unwrap()[0].entry.amount, crate::funding::pnl_record::tests::d("-0.12"));
}

#[test]
fn reconcile_one_more_on_the_exchange_or_locally_is_a_mismatch() {
    let fx = local_fx();
    let b = remote(Exchange::Binance, vec![ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Binance, "BTCUSDT", "3", "0", T + 1)]);
    let y = remote(Exchange::Bybit, vec![]);
    assert_eq!(run(&fx, &b, &y), ReconcileResult::Mismatch);
    let e = last_event(&fx);
    assert_eq!(e["legs"][0]["remote_count"], 2);
    assert_eq!(e["legs"][0]["missing_locally"][0], "binance:FUNDING_FEE:3");
    assert_eq!(e["legs"][1]["missing_remotely"][0], "bybit:2", "the local entry the exchange no longer lists");
    assert_eq!(fx.db.funding_ledger_entries().unwrap().len(), 2, "the extra exchange row is reported, not written");
}

#[test]
fn reconcile_a_failed_refetch_is_failed_never_ok() {
    let fx = local_fx();
    let b = Remote { exchange: Exchange::Binance, entries: Err(AdapterError::Timeout), calls: Mutex::new(0) };
    let y = remote(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]);
    assert_eq!(run(&fx, &b, &y), ReconcileResult::Failed);
    assert_eq!(last_event(&fx)["result"], "FAILED");
    assert_eq!(last_event(&fx)["alert"], true);
}

#[test]
fn reconcile_a_mismatch_recomputes_an_existing_pnl_as_incomplete() {
    let fx = local_fx();
    crate::funding::pnl_record::settle_pnl(&fx.db, "p1", T + 100_000, true).unwrap();
    let before = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    let b = remote(Exchange::Binance, vec![ledger(Exchange::Binance, "BTCUSDT", "1", "-0.15", T)]);
    let y = remote(Exchange::Bybit, vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]);
    run(&fx, &b, &y);
    let after = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert_ne!(after.event_id, before.event_id);
    assert!(after.payload["reasons"].to_string().contains("對帳差異"));
}

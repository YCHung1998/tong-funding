//! The slot runner against a stored closed pair (the `pnl_record` fixture: open T−10 s, close
//! T+15 s, one settlement at T per leg), fake ledger sources, injected time, no network.
use std::sync::Mutex;

use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::types::{Decimal, Exchange};

use super::*;
use crate::exchange::error::AdapterError;
use crate::exchange::signed::ledger::LedgerPage;
use crate::funding::pnl_record::tests::{Fx, T, ledger, scenario};
use crate::store::funding_slots::{PAIR_FUNDING_LINKED, Resolution, SlotLeg};

const M: i64 = 60_000;

/// Answers every page with the current entries, or a failure.
struct Src {
    exchange: Exchange,
    entries: Mutex<Result<Vec<FundingLedgerEntry>, AdapterError>>,
}

impl Src {
    fn new(exchange: Exchange, entries: Vec<FundingLedgerEntry>) -> Src {
        Src { exchange, entries: Mutex::new(Ok(entries)) }
    }
    fn set(&self, entries: Result<Vec<FundingLedgerEntry>, AdapterError>) {
        *self.entries.lock().unwrap() = entries;
    }
}

impl LedgerSource for Src {
    fn exchange(&self) -> Exchange {
        self.exchange
    }
    fn per_symbol(&self) -> bool {
        self.exchange == Exchange::Binance
    }
    fn page<'a>(&'a self, _symbol: &'a str, _start: i64, _end: i64, _t: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        let r = self.entries.lock().unwrap().clone().map(|e| LedgerPage { rows: e.len(), entries: e, next_cursor: None });
        Box::pin(std::future::ready(r))
    }
}

struct NoPause;
impl Pause for NoPause {
    fn pause(&self, _ms: u64) -> BoxFut<'_, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// A public-rate fake: the same answer for everything.
struct Rate(Result<Option<Decimal>, String>);
impl RateSource for Rate {
    fn rate_at<'a>(&'a self, _: Exchange, _: &'a str, _: i64) -> BoxFut<'a, Result<Option<Decimal>, String>> {
        Box::pin(std::future::ready(self.0.clone()))
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

fn ready(_: Exchange) -> Result<(), String> {
    Ok(())
}

fn no_keys(ex: Exchange) -> Result<(), String> {
    Err(format!("{} keys unavailable", ex.name()))
}

fn rows_both() -> (Vec<FundingLedgerEntry>, Vec<FundingLedgerEntry>) {
    (vec![ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T)], vec![ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T + 1_000)])
}

fn tick_at(fx: &Fx, b: &Src, y: &Src, rates: &dyn RateSource, ready: Readiness<'_>, now: i64) -> SlotTickReport {
    fx.clock.set(now);
    block_on(slots_tick(&fx.db, &[b, y], rates, ready, &NoPause, now))
}

fn slot(fx: &Fx, leg: SlotLeg) -> SlotRow {
    fx.db.get_slot("p1", leg, T).unwrap().unwrap()
}

fn linked_events(fx: &Fx) -> usize {
    fx.db.query_events(&Default::default()).unwrap().rows.iter().filter(|e| e.event_type == PAIR_FUNDING_LINKED).count()
}

#[test]
fn funding_slot_runner_a_held_settlement_creates_one_slot_per_leg_once() {
    let fx = scenario(true);
    assert_eq!(create_slots(&fx.db, T + 20_000).unwrap(), 2);
    assert_eq!(create_slots(&fx.db, T + 21_000).unwrap(), 0, "idempotent");
    let (l, s) = (slot(&fx, SlotLeg::Long), slot(&fx, SlotLeg::Short));
    assert_eq!((l.exchange, l.state, l.next_check_ms), (Exchange::Binance, SlotState::Tracing1, T + 5 * M));
    assert_eq!((s.exchange, s.state, s.next_check_ms), (Exchange::Bybit, SlotState::Tracing1, T + 5 * M));
}

#[test]
fn funding_slot_runner_no_slot_before_the_settlement_has_passed() {
    let fx = scenario(true);
    assert_eq!(create_slots(&fx.db, T - 1_000).unwrap(), 0);
}

#[test]
fn funding_slot_runner_a_pair_that_is_found_incomplete_long_after_gets_a_backfill_slot_due_now() {
    let fx = scenario(true);
    let now = T + 2 * 60 * M;
    assert_eq!(create_slots(&fx.db, now).unwrap(), 2);
    let l = slot(&fx, SlotLeg::Long);
    assert_eq!((l.state, l.manual_reason, l.next_check_ms), (SlotState::ManualCheck, Some(ManualReason::Backfill), now));
}

#[test]
fn funding_slot_runner_the_first_check_links_both_legs_to_the_pair_and_records_the_delay() {
    let fx = scenario(true);
    let (bin, byb) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, byb));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000); // creates the slots
    let r = tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    assert_eq!(r.settled.len(), 2, "{r:?}");
    let (l, s) = (slot(&fx, SlotLeg::Long), slot(&fx, SlotLeg::Short));
    assert_eq!((l.state, l.amount, l.resolution, l.ledger_dedupe_key.as_deref()), (SlotState::Settled, Some("-0.12".parse().unwrap()), Some(Resolution::Ledger), Some("binance:FUNDING_FEE:1")));
    assert_eq!((s.state, s.amount), (SlotState::Settled, Some("0.36".parse().unwrap())));
    assert_eq!(linked_events(&fx), 2);
    let delay = l.first_seen_ms.unwrap() - T;
    assert!(delay >= 0 && delay <= 5 * M, "first seen at the tick that stored it: {delay}");
}

#[test]
fn funding_slot_runner_a_row_that_appears_later_is_found_at_a_later_check() {
    let fx = scenario(true);
    let (b, y) = (Src::new(Exchange::Binance, vec![]), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    let r = tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    assert!(r.settled.is_empty() && r.checked.len() == 2, "{r:?}");
    assert_eq!((slot(&fx, SlotLeg::Long).state, slot(&fx, SlotLeg::Long).next_check_ms), (SlotState::Tracing1, T + 10 * M));
    let (bin, byb) = rows_both();
    b.set(Ok(bin));
    y.set(Ok(byb));
    // Not due yet at +6 min; found at the +10 min check.
    assert!(tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 6 * M).checked.is_empty());
    assert_eq!(tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 10 * M).settled.len(), 2);
}

#[test]
fn funding_slot_runner_an_offline_stage_ends_in_fetch_failed_and_the_sweep_still_settles_a_late_row() {
    let fx = scenario(true);
    let (b, y) = (Src::new(Exchange::Binance, vec![]), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    b.set(Err(AdapterError::Timeout));
    y.set(Err(AdapterError::Timeout));
    for m in [5, 10, 15, 20, 25, 30, 40, 50, 60] {
        tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + m * M);
    }
    let l = slot(&fx, SlotLeg::Long);
    assert_eq!((l.state, l.manual_reason), (SlotState::ManualCheck, Some(ManualReason::FetchFailed)));
    assert_eq!(l.checks_failed, 9);
    // The network is back; the hourly sweep finds the row that was always there.
    let (bin, byb) = rows_both();
    b.set(Ok(bin));
    y.set(Ok(byb));
    assert_eq!(tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 120 * M).settled.len(), 2);
    assert_eq!(slot(&fx, SlotLeg::Long).state, SlotState::Settled);
}

#[test]
fn funding_slot_runner_missing_keys_count_as_failed_checks_not_as_nothing_found() {
    let fx = scenario(true);
    let (b, y) = (Src::new(Exchange::Binance, vec![]), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    let r = tick_at(&fx, &b, &y, &Rate(Ok(None)), &no_keys, T + 5 * M);
    assert_eq!(r.checked.iter().map(|c| c.3).collect::<Vec<_>>(), vec!["failed", "failed"]);
    let l = slot(&fx, SlotLeg::Long);
    assert_eq!((l.checks_failed, l.checks_ok, l.last_error.as_deref()), (1, 0, Some("Binance keys unavailable")));
}

#[test]
fn funding_slot_runner_after_a_sleep_the_overdue_slot_is_checked_once_and_found_not_given_up() {
    let fx = scenario(true);
    let (bin, byb) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, byb));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    // The machine slept: the next tick is 80 minutes after the settlement (the NMR case).
    let r = tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 80 * M);
    assert_eq!(r.settled.len(), 2, "{r:?}");
    assert_eq!(slot(&fx, SlotLeg::Long).state, SlotState::Settled);
}

#[test]
fn funding_slot_runner_a_zero_rate_with_no_row_settles_as_zero_and_a_nonzero_rate_does_not() {
    let fx = scenario(true);
    let (b, y) = (Src::new(Exchange::Binance, vec![]), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    let r = tick_at(&fx, &b, &y, &Rate(Ok(Some("0.0001".parse().unwrap()))), &ready, T + 5 * M);
    assert!(r.settled.is_empty(), "non-zero rate and no row must not settle: {r:?}");
    let r = tick_at(&fx, &b, &y, &Rate(Ok(Some(Decimal::ZERO))), &ready, T + 10 * M);
    assert_eq!(r.settled.len(), 2);
    let l = slot(&fx, SlotLeg::Long);
    assert_eq!((l.state, l.resolution, l.amount, l.ledger_dedupe_key), (SlotState::Settled, Some(Resolution::ZeroRate), Some(Decimal::ZERO), None));
    assert!(l.zero_rate_source.unwrap().contains("Binance"));
    assert_eq!(linked_events(&fx), 2);
}

#[test]
fn funding_slot_runner_a_failed_rate_lookup_changes_nothing() {
    let fx = scenario(true);
    let (b, y) = (Src::new(Exchange::Binance, vec![]), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    let r = tick_at(&fx, &b, &y, &Rate(Err("rate endpoint down".into())), &ready, T + 5 * M);
    assert!(r.settled.is_empty());
    assert_eq!(slot(&fx, SlotLeg::Long).state, SlotState::Tracing1);
}

#[test]
fn funding_slot_runner_a_row_is_linked_to_one_slot_only_even_when_returned_again() {
    let fx = scenario(true);
    let (bin, byb) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, byb));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 10 * M);
    assert_eq!(linked_events(&fx), 2, "exactly one linked event per leg");
}

// ---- PnL follows the slots (task 4.1) ------------------------------------------------------------

use crate::funding::pnl_record::{latest_pnl, settle_pnl};

fn pnl(fx: &Fx, now: i64) -> serde_json::Value {
    fx.clock.set(now);
    settle_pnl(&fx.db, "p1", now, true).unwrap();
    latest_pnl(&fx.db, "p1").unwrap().unwrap().payload
}

#[test]
fn funding_slot_runner_pnl_is_complete_when_every_slot_is_settled_by_a_ledger_row() {
    let fx = scenario(true);
    let (bin, byb) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, byb));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    let p = pnl(&fx, T + 6 * M);
    assert_eq!(p["funding_tracing"], serde_json::json!([]));
    assert!(!p["reasons"].to_string().contains("資金費"), "{}", p["reasons"]);
    assert_eq!(p["breakdown"]["total"]["funding"], serde_json::json!("0.24"));
}

#[test]
fn funding_slot_runner_a_zero_rate_slot_counts_as_a_received_settlement_of_zero() {
    let fx = scenario(true);
    let (b, y) = (Src::new(Exchange::Binance, vec![]), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    tick_at(&fx, &b, &y, &Rate(Ok(Some(Decimal::ZERO))), &ready, T + 5 * M);
    let p = pnl(&fx, T + 6 * M);
    assert_eq!(p["breakdown"]["total"]["funding"], serde_json::json!("0"));
    for leg in p["breakdown"]["legs"].as_array().unwrap() {
        assert_eq!((leg["settlements_received"].as_i64(), leg["expected_settlements"].as_i64()), (Some(1), Some(1)), "{leg}");
        assert!(!leg["missing"].to_string().contains("Funding"), "zero funding is not a missing component: {leg}");
    }
    assert!(!p["reasons"].to_string().contains("缺少結算流水"), "{}", p["reasons"]);
}

#[test]
fn funding_slot_runner_an_unsettled_slot_keeps_the_pnl_incomplete_and_names_the_tracing_state() {
    let fx = scenario(true);
    let (bin, _) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    let p = pnl(&fx, T + 6 * M);
    assert_eq!(p["status"], serde_json::json!("INCOMPLETE"));
    let reasons = p["reasons"].to_string();
    assert!(reasons.contains("資金費追蹤中（Bybit short：TRACING_1）"), "{reasons}");
    assert!(!reasons.contains("Binance long：TRACING"), "the settled leg is not listed: {reasons}");
}

#[test]
fn funding_slot_runner_a_late_settlement_recomputes_the_pnl_to_complete() {
    let fx = scenario(true);
    let (bin, byb) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, vec![]));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    assert_eq!(pnl(&fx, T + 6 * M)["status"], serde_json::json!("INCOMPLETE"));
    y.set(Ok(byb));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 10 * M);
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    // (The fixture has no close reference prices, so other reasons remain; the funding ones are gone.)
    let reasons = latest.payload["reasons"].to_string();
    assert_eq!(latest.event_type, "PAIR_PNL_RECOMPUTED");
    assert!(!reasons.contains("資金費") && !reasons.contains("缺少結算流水"), "{reasons}");
    assert_eq!(latest.payload["breakdown"]["total"]["funding"], serde_json::json!("0.24"));
}

#[test]
fn funding_slot_runner_the_delay_report_lists_percentiles_per_exchange() {
    let fx = scenario(true);
    let (bin, byb) = rows_both();
    let (b, y) = (Src::new(Exchange::Binance, bin), Src::new(Exchange::Bybit, byb));
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 20_000);
    tick_at(&fx, &b, &y, &Rate(Ok(None)), &ready, T + 5 * M);
    let report = delay_report(&fx.db).unwrap();
    assert_eq!(report.keys().copied().collect::<Vec<_>>(), vec![Exchange::Binance, Exchange::Bybit]);
    assert_eq!(report[&Exchange::Binance].0, 1);
}

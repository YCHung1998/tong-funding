//! funding-pnl 2.3 / 3.1 (app side): assembling a pair's PnL from stored engine events and
//! ledger entries, waiting vs recording, recomputation.
use serde_json::{Value, json};
use tong_funding_core::pair::PairState;
use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::types::{Decimal, Exchange};

use super::*;
use crate::engine::actor::PairEnvelope;
use crate::ports::ManualClock;
use crate::store::db::test_support::open_tmp;
use crate::store::events::EventStore;
use crate::store::state::NewPair;

/// An 8-hour settlement boundary.
pub(crate) const T: i64 = 1_791_216_000_000;
const H8: i64 = 28_800;

pub(crate) fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

pub(crate) struct Fx {
    _dir: tempfile::TempDir,
    pub(crate) db: Db,
    pub(crate) clock: ManualClock,
    events: EventStore,
}

fn add_pair(db: &Db, uuid: &str, symbol: &str, simulated: bool) {
    let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T, simulated, scan: json!({}) };
    let p = NewPair { internal_uuid: uuid.into(), pair_id: format!("pid-{uuid}"), symbol: symbol.into(), status: PairState::Prepared, entry: serde_json::to_value(env).unwrap() };
    db.add_pair_if_not_pending(&p).unwrap();
    db.set_pair_status(uuid, PairState::Closing).unwrap();
}

fn at<F: FnOnce(&EventStore)>(fx: &Fx, ts: i64, f: F) {
    fx.clock.set(ts);
    f(&fx.events);
}

fn order(fx: &Fx, ts: i64, pair: &str, leg: &str, action: &str, exchange: Exchange, avg: &str, fee: &str) {
    at(fx, ts, |es| {
        es.append(
            "ORDER_SUBMITTED",
            Some(pair),
            json!({
                "outcome": "accepted", "state": "Filled", "filled_quantity": "0.02", "avg_price": avg, "fee": fee, "fee_asset": "USDT",
                "client_order_id": format!("demo-{pair}-{leg}-{action}"), "leg": leg, "action": action, "simulated": false,
                "exchange": exchange.name(), "symbol": "BTCUSDT",
            }),
        )
        .unwrap();
    });
}

fn snapshot() -> Value {
    let leg = |ex: &str| json!({ "exchange": ex, "expected_price": "60000", "next_funding_time": T, "funding_interval_secs": H8 });
    json!({
        "long": leg("Binance"), "short": leg("Bybit"), "notional_usdt": "1200", "leverage": "5",
        "net_edge": { "net_edge_usdt": "-0.63", "funding_income_usdt": "0.40", "fee_usdt": "0.96", "slippage_usdt": "0.05", "safety_margin_usdt": "0.02" },
    })
}

/// Entry at T−10 s, close at T+15 s, the spec's −0.82 numbers (funding −0.12 + 0.36).
pub(crate) fn scenario(with_snapshot: bool) -> Fx {
    let (dir, db, clock) = open_tmp();
    let fx = Fx { events: EventStore::new(db.clone()), _dir: dir, db, clock };
    add_pair(&fx.db, "p1", "BTCUSDT", false);
    at(&fx, T - 10_000, |es| {
        let detail = if with_snapshot { json!({ "checks": "pass", "entry_snapshot": snapshot() }) } else { json!({ "checks": "pass" }) };
        es.append("PAIR_TRANSITION", Some("p1"), json!({ "from": "PRE_TRADE_CHECK", "to": "ORDER_SUBMIT", "detail": detail })).unwrap();
    });
    order(&fx, T - 10_000, "p1", "long", "open", Exchange::Binance, "60002.5", "0.24");
    order(&fx, T - 10_000, "p1", "short", "open", Exchange::Bybit, "60000", "0.24");
    order(&fx, T + 15_000, "p1", "long", "close", Exchange::Binance, "59997.5", "0.24");
    order(&fx, T + 15_000, "p1", "short", "close", Exchange::Bybit, "60000", "0.24");
    fx
}

pub(crate) fn ledger(exchange: Exchange, symbol: &str, id: &str, amount: &str, ts: i64) -> FundingLedgerEntry {
    let kind = if exchange == Exchange::Binance { "FUNDING_FEE" } else { "SETTLEMENT" };
    FundingLedgerEntry::new(exchange, symbol, d(amount), "USDT", ts, id, kind, json!({}))
}

fn fetched(fx: &Fx, exchange: Exchange, symbol: Option<&str>, start: i64, end: i64) {
    at(fx, T + 70_000, |es| {
        es.append(FUNDING_LEDGER_FETCHED, None, json!({ "exchange": exchange.name(), "symbol": symbol, "start_ms": start, "end_ms": end, "outcome": "complete" })).unwrap();
    });
}

pub(crate) fn both_fetched(fx: &Fx) {
    fetched(fx, Exchange::Binance, Some("BTCUSDT"), T - 10_000, T + 15_000);
    fetched(fx, Exchange::Bybit, None, T - 10_000, T + 15_000);
}

fn count(db: &Db, ty: &str) -> usize {
    pair_events(db, "p1").unwrap().iter().filter(|e| e.event_type == ty).count()
}

#[test]
fn pnl_record_assembles_fills_ledger_and_settlements_from_the_store() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T + 1_000)]).unwrap();
    both_fetched(&fx);
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    assert_eq!(a.slots, [vec![T], vec![T]], "one settlement per leg inside (T−10 s, T+15 s]");
    assert_eq!(a.input.legs[0].funding.len(), 1);
    assert_eq!(a.input.legs[0].fills.len(), 2);
    assert_eq!(a.input.legs[0].fills[0].expected_price, Some(d("60000")), "entry reference price from entry_snapshot");
    assert_eq!(a.input.legs[0].fills[1].expected_price, None, "the engine records no close reference price");
    assert_eq!(a.expected.as_ref().unwrap().funding_income, d("0.40"));
    let b = tong_funding_core::pnl::compute_pnl(&a.input);
    assert_eq!(b.total.funding, d("0.24"));
    assert_eq!(b.total.opening_fee + b.total.closing_fee, d("0.96"));
}

#[test]
fn pnl_record_funding_settled_records_at_once_even_if_other_parts_are_incomplete() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    fx.clock.set(T + 80_000);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    let PnlAttempt::Recorded { status, .. } = r else { panic!("{r:?}") };
    assert_eq!(status, "INCOMPLETE");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert_eq!(latest.event_type, PAIR_PNL_COMPUTED);
    let reasons: Vec<String> = serde_json::from_value(latest.payload["reasons"].clone()).unwrap();
    assert!(reasons.iter().all(|r| r.starts_with("無參考價")), "{reasons:?}");
    assert_eq!(latest.payload["breakdown"]["total"]["funding"], json!("0.24"));
    assert_eq!(latest.payload["ledger_keys"].as_array().unwrap().len(), 2);
}

#[test]
fn pnl_record_missing_settlement_waits_then_records_incomplete_on_the_final_attempt() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    assert!(matches!(&r, PnlAttempt::Waiting { reasons } if reasons.iter().any(|x| x.contains("缺少結算流水"))), "{r:?}");
    assert_eq!(count(&fx.db, PAIR_PNL_COMPUTED), 0, "nothing written while waiting");
    fx.clock.set(T + 700_000);
    let r = settle_pnl(&fx.db, "p1", T + 700_000, true).unwrap();
    assert!(matches!(&r, PnlAttempt::Recorded { status, .. } if status == "INCOMPLETE"), "{r:?}");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert!(latest.payload["reasons"].to_string().contains("缺少結算流水"));
    assert!(latest.payload["breakdown"]["missing"].as_array().unwrap().contains(&json!("Funding")), "funding is not a complete 0");
}

#[test]
fn pnl_record_never_fetched_is_not_fetched_not_zero() {
    let fx = scenario(true);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    assert!(matches!(&r, PnlAttempt::Waiting { reasons } if reasons.iter().any(|x| x.contains("尚未取得"))), "{r:?}");
}

#[test]
fn pnl_record_late_entries_write_a_recomputed_event_and_keep_the_old_one() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    fx.clock.set(T + 700_000);
    settle_pnl(&fx.db, "p1", T + 700_000, true).unwrap();
    let first = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert_eq!(recompute_if_changed(&fx.db, "p1", T + 700_000).unwrap(), None, "nothing changed: nothing written");
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T)]).unwrap();
    fx.clock.set(T + 800_000);
    let id = recompute_if_changed(&fx.db, "p1", T + 800_000).unwrap().expect("recomputed");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert_eq!((latest.event_id, latest.event_type.as_str()), (id, PAIR_PNL_RECOMPUTED));
    assert_eq!(latest.payload["breakdown"]["total"]["funding"], json!("0.24"));
    let old: Vec<_> = pair_events(&fx.db, "p1").unwrap().into_iter().filter(|e| e.id == first.event_id).collect();
    assert_eq!(old[0].payload, first.payload, "the first event is unchanged");
}

#[test]
fn pnl_record_simulation_pairs_get_no_pnl() {
    let (_dir, db, _) = open_tmp();
    add_pair(&db, "sim1", "BTCUSDT", true);
    assert!(settle_pnl(&db, "sim1", T, true).is_err());
    assert_eq!(recompute_if_changed(&db, "sim1", T).unwrap(), None);
}

#[test]
fn pnl_record_unattributed_entries_stay_stored_and_are_not_counted() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Bybit, "ETHUSDT", "9", "5", T), ledger(Exchange::Bybit, "BTCUSDT", "8", "5", T - 3_600_000)]).unwrap();
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    assert!(a.input.legs.iter().all(|l| l.funding.is_empty()), "other symbol / before the open fill");
    assert_eq!(fx.db.funding_ledger_entries().unwrap().len(), 2, "kept in the store");
}

#[test]
fn pnl_record_without_entry_snapshot_has_no_expected_values_and_no_derivable_settlements() {
    let fx = scenario(false);
    both_fetched(&fx);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    assert!(matches!(r, PnlAttempt::Recorded { .. }), "nothing to wait for: settlements are not derivable");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert_eq!(latest.payload["no_expected_snapshot"], json!(true));
    assert!(latest.payload["reasons"].to_string().contains("無法推算預期結算次數"));
}

#[test]
fn pnl_record_a_reconciliation_mismatch_makes_the_result_incomplete() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    at(&fx, T + 90_000, |es| {
        es.append(PNL_RECONCILIATION, Some("p1"), json!({ "result": "MISMATCH" })).unwrap();
    });
    let a = assemble(&fx.db, "p1", T + 90_000).unwrap();
    assert!(a.input.reconciliation_mismatch);
}

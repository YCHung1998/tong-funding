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
    add_pair_on(db, uuid, symbol, simulated, Exchange::Binance, Exchange::Bybit);
}

fn add_pair_on(db: &Db, uuid: &str, symbol: &str, simulated: bool, long: Exchange, short: Exchange) {
    let env = PairEnvelope { long_exchange: long, short_exchange: short, settlement_ms: T, simulated, scan: json!({}) };
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
    assert_eq!(a.input.legs[0].fills[1].expected_price, None, "no close reference recorded in this fixture");
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

// ---- gap 2: close reference price recorded by the engine when the close orders are sent ------

/// The engine's close events: the fill details plus the per-leg reference price it fetched right
/// before sending the reduce-only close (`reference_price`), or why it has none.
fn close_with_reference(fx: &Fx, ts: i64, leg: &str, exchange: Exchange, avg: &str, reference: Result<&str, &str>) {
    at(fx, ts, |es| {
        let mut p = json!({
            "outcome": "accepted", "state": "Filled", "filled_quantity": "0.02", "avg_price": avg, "fee": "0.24", "fee_asset": "USDT",
            "client_order_id": format!("demo-p1-{leg}-close"), "leg": leg, "action": "close", "simulated": false,
            "exchange": exchange.name(), "symbol": "BTCUSDT",
        });
        match reference {
            Ok(r) => {
                p["reference_price"] = json!(r);
                p["reference_observed_at_ms"] = json!(ts - 50);
                p["reference_source"] = json!("refetch_before_close");
            }
            Err(e) => p["reference_error"] = json!(e),
        }
        es.append("ORDER_SUBMITTED", Some("p1"), p).unwrap();
    });
}

/// `scenario` without its close orders (they are added with or without a reference).
fn opened_round() -> Fx {
    let (dir, db, clock) = open_tmp();
    let fx = Fx { events: EventStore::new(db.clone()), _dir: dir, db, clock };
    add_pair(&fx.db, "p1", "BTCUSDT", false);
    at(&fx, T - 10_000, |es| {
        es.append("PAIR_TRANSITION", Some("p1"), json!({ "from": "PRE_TRADE_CHECK", "to": "ORDER_SUBMIT", "detail": { "checks": "pass", "entry_snapshot": snapshot() } })).unwrap();
    });
    order(&fx, T - 10_000, "p1", "long", "open", Exchange::Binance, "60002.5", "0.24");
    order(&fx, T - 10_000, "p1", "short", "open", Exchange::Bybit, "60000", "0.24");
    fx
}

#[test]
fn pnl_record_a_demo_round_with_close_references_and_funding_is_complete() {
    let fx = opened_round();
    close_with_reference(&fx, T + 15_000, "long", Exchange::Binance, "59997.5", Ok("60000"));
    close_with_reference(&fx, T + 15_000, "short", Exchange::Bybit, "60000", Ok("60001"));
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    assert_eq!(a.input.legs[0].fills[1].expected_price, Some(d("60000")), "close reference from the close event");
    assert_eq!(a.input.legs[1].fills[1].expected_price, Some(d("60001")));
    fx.clock.set(T + 80_000);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    assert!(matches!(&r, PnlAttempt::Recorded { status, .. } if status == "COMPLETE"), "{r:?}");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert_eq!(latest.payload["status"], json!("COMPLETE"), "{}", latest.payload);
    // long: open buy 0.02 x (60002.5 − 60000) = 0.05, close sell 0.02 x (60000 − 59997.5) = 0.05;
    // short: close buy 0.02 x (60000 − 60001) = −0.02 (better than the reference).
    assert_eq!(latest.payload["breakdown"]["legs"][0]["components"]["slippage"], json!("0.1"), "{}", latest.payload["breakdown"]);
    assert_eq!(latest.payload["breakdown"]["total"]["slippage"], json!("0.08"));
}

#[test]
fn pnl_record_a_missing_close_reference_keeps_the_result_incomplete_with_the_reason() {
    let fx = opened_round();
    close_with_reference(&fx, T + 15_000, "long", Exchange::Binance, "59997.5", Ok("60000"));
    close_with_reference(&fx, T + 15_000, "short", Exchange::Bybit, "60000", Err("refetch failed: timeout"));
    fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    assert_eq!(a.input.legs[1].fills[1].expected_price, None, "never the fill price as the reference");
    fx.clock.set(T + 80_000);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    assert!(matches!(&r, PnlAttempt::Recorded { status, .. } if status == "INCOMPLETE"), "{r:?}");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    let reasons: Vec<String> = serde_json::from_value(latest.payload["reasons"].clone()).unwrap();
    assert_eq!(reasons, vec!["無參考價（Bybit demo-p1-short-close）".to_string()], "only the short close lacks a reference");
}

// ---- okx-funding-ledger: OKX legs are in contracts; ct_val makes them coins ------------------------

/// Bybit long + OKX short on BTCUSDT; the OKX orders are 3 contracts, `ct_val` (when given) on every event.
pub(crate) fn okx_round(ct_val: Option<&str>, close_reference: &str) -> Fx {
    let (dir, db, clock) = open_tmp();
    let fx = Fx { events: EventStore::new(db.clone()), _dir: dir, db, clock };
    add_pair_on(&fx.db, "p1", "BTCUSDT", false, Exchange::Bybit, Exchange::Okx);
    let snap = {
        let leg = |ex: &str| json!({ "exchange": ex, "expected_price": "60000", "next_funding_time": T, "funding_interval_secs": H8 });
        json!({ "long": leg("Bybit"), "short": leg("Okx"), "notional_usdt": "1800", "leverage": "5",
            "net_edge": { "net_edge_usdt": "1", "funding_income_usdt": "2", "fee_usdt": "0.5", "slippage_usdt": "0.1", "safety_margin_usdt": "0.1" } })
    };
    at(&fx, T - 10_000, |es| {
        es.append("PAIR_TRANSITION", Some("p1"), json!({ "from": "PRE_TRADE_CHECK", "to": "ORDER_SUBMIT", "detail": { "checks": "pass", "entry_snapshot": snap } })).unwrap();
    });
    let ev = |fx: &Fx, ts: i64, leg: &str, action: &str, exchange: Exchange, qty: &str, avg: &str, reference: Option<&str>| {
        at(fx, ts, |es| {
            let mut p = json!({
                "outcome": "accepted", "state": "Filled", "filled_quantity": qty, "avg_price": avg, "fee": "0.3", "fee_asset": "USDT",
                "client_order_id": format!("demo-p1-{leg}-{action}"), "leg": leg, "action": action, "simulated": false,
                "exchange": exchange.name(), "symbol": "BTCUSDT",
            });
            if exchange == Exchange::Okx && let Some(c) = ct_val {
                p["ct_val"] = json!(c);
            }
            if let Some(r) = reference {
                p["reference_price"] = json!(r);
            }
            es.append("ORDER_SUBMITTED", Some("p1"), p).unwrap();
        });
    };
    ev(&fx, T - 10_000, "long", "open", Exchange::Bybit, "0.03", "60000", None);
    ev(&fx, T - 10_000, "short", "open", Exchange::Okx, "3", "60000", None);
    ev(&fx, T + 15_000, "long", "close", Exchange::Bybit, "0.03", "60300", Some("60300"));
    ev(&fx, T + 15_000, "short", "close", Exchange::Okx, "3", "60300", Some(close_reference));
    fx
}

#[test]
fn okx_fills_become_coin_quantities_with_their_own_prices() {
    let fx = okx_round(Some("0.01"), "60300");
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    let short = &a.input.legs[1];
    assert_eq!(short.exchange, Exchange::Okx);
    assert_eq!(short.fills.iter().map(|f| f.quantity).collect::<Vec<_>>(), vec![d("0.03"), d("0.03")], "3 contracts x 0.01 = 0.03 BTC");
    assert_eq!((short.fills[0].expected_price, short.fills[0].actual_price), (Some(d("60000")), Some(d("60000"))));
    assert_eq!((short.fills[1].expected_price, short.fills[1].actual_price), (Some(d("60300")), Some(d("60300"))));
    assert!(short.fills.iter().all(|f| !f.contract_value_missing));
    // the short opened at 60000 and closed at 60300: price PnL (60000 - 60300) x 0.03 = -9
    let b = tong_funding_core::pnl::compute_pnl(&a.input);
    assert_eq!(b.legs[1].components.price_actual, d("-9"));
    assert_eq!(b.legs[0].components.price_actual, d("9"), "the Bybit long gains the same 9");
}

#[test]
fn okx_fills_without_a_contract_value_are_unknown_and_named() {
    let fx = okx_round(None, "60300");
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    assert!(a.input.legs[1].fills.iter().all(|f| f.contract_value_missing));
    assert!(a.input.legs[0].fills.iter().all(|f| !f.contract_value_missing), "Bybit is unaffected");
    both_okx_fetched(&fx);
    fx.clock.set(T + 700_000);
    let r = settle_pnl(&fx.db, "p1", T + 700_000, true).unwrap();
    assert!(matches!(&r, PnlAttempt::Recorded { status, .. } if status == "INCOMPLETE"), "{r:?}");
    let latest = latest_pnl(&fx.db, "p1").unwrap().unwrap();
    assert!(latest.payload["reasons"].to_string().contains("OKX 成交缺合約面值"), "{}", latest.payload["reasons"]);
}

pub(crate) fn both_okx_fetched(fx: &Fx) {
    fetched(fx, Exchange::Bybit, None, T - 10_000, T + 15_000);
    fetched(fx, Exchange::Okx, Some("BTCUSDT"), T - 10_000, T + 15_000);
}

#[test]
fn okx_funding_follows_the_fetch_state_instead_of_being_never_fetched() {
    let fx = okx_round(Some("0.01"), "60300");
    let state = |fx: &Fx| assemble(&fx.db, "p1", T + 80_000).unwrap().input.legs[1].funding_fetch.clone();
    assert_eq!(state(&fx), FundingFetchState::NotFetched, "nothing fetched yet");
    both_okx_fetched(&fx);
    assert_eq!(state(&fx), FundingFetchState::Fetched);
    let fx2 = okx_round(Some("0.01"), "60300");
    at(&fx2, T + 70_000, |es| {
        es.append(FUNDING_LEDGER_FETCHED, None, json!({ "exchange": Exchange::Okx.name(), "symbol": "BTCUSDT", "start_ms": T - 10_000, "end_ms": T + 15_000, "outcome": "incomplete", "reason": "page 2 failed" })).unwrap();
    });
    assert_eq!(state(&fx2), FundingFetchState::Failed("page 2 failed".into()));
}

#[test]
fn okx_funding_entries_are_attributed_to_the_okx_leg() {
    let fx = okx_round(Some("0.01"), "60300");
    let okx = FundingLedgerEntry::new(Exchange::Okx, "BTCUSDT", d("-0.42"), "USDT", T + 500, "623950854533513219", "173", json!({}));
    fx.db.write_funding_ledger(&[okx, ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    let a = assemble(&fx.db, "p1", T + 80_000).unwrap();
    assert_eq!(a.input.legs[1].funding.len(), 1);
    assert_eq!(a.input.legs[1].funding[0].amount, d("-0.42"));
    both_okx_fetched(&fx);
    fx.clock.set(T + 80_000);
    let r = settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
    assert!(matches!(&r, PnlAttempt::Recorded { status, .. } if status == "COMPLETE"), "{r:?}");
}

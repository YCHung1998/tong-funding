//! Assembles a pair's PnL input from the store (engine events: `ORDER_SUBMIT` transition with
//! `entry_snapshot`, `ORDER_SUBMITTED` / `ORDER_FILL`; `FUNDING_LEDGER_ENTRY` /
//! `FUNDING_LEDGER_FETCHED` / `PNL_RECONCILIATION`), computes it with core and records it as an
//! immutable `PAIR_PNL_COMPUTED` (first) or `PAIR_PNL_RECOMPUTED` (later, only when the result
//! changed) event (spec pnl-accounting "PnL 結果以不可變事件保存"). The effective result of a pair is
//! its newest PnL event.
//!
//! Conservative readings (design.md 實作紀錄):
//! - A fill's time is the store time of the newest `ORDER_SUBMITTED` / `ORDER_FILL` of that order
//!   (the engine records no exchange fill time).
//! - Close orders carry no reference price (the engine records one only at entry): their slippage
//!   is "無參考價" and the PnL INCOMPLETE until a close reference price is recorded.
//! - OKX legs: quantities are contracts and no contract value is recorded with the fill, and OKX
//!   has no ledger client; their prices are treated as unknown and their funding as not fetched.

use std::collections::BTreeMap;
use std::str::FromStr;

use rusqlite::params;
use serde_json::{Value, json};
use tong_funding_core::pnl::{
    Attribution, Comparison, ExpectedSnapshot, FillAction, FillRecord, FundingFetchState, IncompleteReason, LegInput, LegWindow, PairInput,
    PnlBreakdown, attribute, compare_expected, compute_pnl, expected_settlements, slippage_summary,
};
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::{FUNDING_LEDGER_FETCHED, PAIR_PNL_COMPUTED, PAIR_PNL_RECOMPUTED, PNL_RECONCILIATION};
use crate::engine::actor::{ORDER_FILL, ORDER_SUBMITTED, PairEnvelope};
use crate::engine::transition::PAIR_TRANSITION;
use crate::store::db::Db;
use crate::store::events::{EventRow, EventStore};
use crate::store::funding_ledger::StoredLedgerEntry;

/// Everything the PnL of one pair is computed from.
#[derive(Debug, Clone, PartialEq)]
pub struct PairPnlInput {
    pub pair: String,
    pub simulated: bool,
    pub input: PairInput,
    pub expected: Option<ExpectedSnapshot>,
    /// Settlements the pair went through (max over legs; for the expected-vs-actual note).
    pub actual_settlements: usize,
    /// Per leg (long, short): expected settlement times inside the holding window.
    pub slots: [Vec<i64>; 2],
    pub windows: [Option<LegWindow>; 2],
    pub ledger_keys: Vec<String>,
    pub fill_ids: Vec<String>,
}

/// The newest PnL event of a pair.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPnl {
    pub event_id: i64,
    pub event_type: String,
    pub ts_ms: i64,
    pub payload: Value,
}

impl StoredPnl {
    pub fn status(&self) -> &str {
        self.payload["status"].as_str().unwrap_or("INCOMPLETE")
    }
}

/// Result of one attempt to settle a closing pair's PnL.
#[derive(Debug, Clone, PartialEq)]
pub enum PnlAttempt {
    /// A PnL event exists now (new or already there): the "PnL computed" confirmation.
    Recorded { event_id: i64, status: String },
    /// Funding entries are still missing and the retry window is not over: nothing written.
    Waiting { reasons: Vec<String> },
}

fn dec(v: &Value) -> Option<Decimal> {
    match v {
        Value::String(s) => Decimal::from_str(s).ok(),
        Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
        _ => None,
    }
}

fn exchange_named(s: &str) -> Option<Exchange> {
    Exchange::ALL.into_iter().find(|e| e.name() == s)
}

/// Every event of a pair, oldest first.
pub fn pair_events(db: &Db, pair: &str) -> Result<Vec<EventRow>, String> {
    db.with_conn(|c| {
        let mut st = c.prepare("SELECT id, ts_ms, event_type, pair_id, payload FROM events WHERE pair_id = ?1 ORDER BY id")?;
        let rows = st.query_map(params![pair], |r| {
            let payload: String = r.get(4)?;
            Ok(EventRow { id: r.get(0)?, ts_ms: r.get(1)?, event_type: r.get(2)?, pair_id: r.get(3)?, payload: serde_json::from_str(&payload).unwrap_or(Value::Null) })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .map_err(|e| e.to_string())
}

/// Events of a type, oldest first (all pairs).
fn events_of_type(db: &Db, ty: &str) -> Result<Vec<EventRow>, String> {
    db.with_conn(|c| {
        let mut st = c.prepare("SELECT id, ts_ms, event_type, pair_id, payload FROM events WHERE event_type = ?1 ORDER BY id")?;
        let rows = st.query_map(params![ty], |r| {
            let payload: String = r.get(4)?;
            Ok(EventRow { id: r.get(0)?, ts_ms: r.get(1)?, event_type: r.get(2)?, pair_id: r.get(3)?, payload: serde_json::from_str(&payload).unwrap_or(Value::Null) })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .map_err(|e| e.to_string())
}

/// The newest `PAIR_PNL_COMPUTED` / `PAIR_PNL_RECOMPUTED` of a pair.
pub fn latest_pnl(db: &Db, pair: &str) -> Result<Option<StoredPnl>, String> {
    Ok(pair_events(db, pair)?
        .into_iter()
        .filter(|e| e.event_type == PAIR_PNL_COMPUTED || e.event_type == PAIR_PNL_RECOMPUTED)
        .next_back()
        .map(|e| StoredPnl { event_id: e.id, event_type: e.event_type, ts_ms: e.ts_ms, payload: e.payload }))
}

/// One order's latest known fill, from the pair's order events.
#[derive(Debug, Clone)]
struct OrderFill {
    leg: Side,
    action: FillAction,
    exchange: Option<Exchange>,
    record: FillRecord,
}

fn order_fills(events: &[EventRow], entry_prices: [Option<Decimal>; 2]) -> Vec<OrderFill> {
    let mut by_id: BTreeMap<String, OrderFill> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for e in events.iter().filter(|e| e.event_type == ORDER_SUBMITTED || e.event_type == ORDER_FILL) {
        let p = &e.payload;
        let Some(id) = p["client_order_id"].as_str() else { continue };
        let leg = match p["leg"].as_str() {
            Some("long") => Side::Long,
            Some("short") => Side::Short,
            _ => continue,
        };
        let action = match p["action"].as_str() {
            Some("open") => FillAction::Open,
            Some("close") => FillAction::Close,
            _ => continue,
        };
        let exchange = p["exchange"].as_str().and_then(exchange_named);
        let quantity = dec(&p["filled_quantity"]).unwrap_or(Decimal::ZERO);
        let idx = match leg {
            Side::Long => 0,
            Side::Short => 1,
        };
        let okx = exchange == Some(Exchange::Okx);
        let record = FillRecord {
            id: id.to_string(),
            action,
            quantity,
            expected_price: match action {
                FillAction::Open if !okx => entry_prices[idx],
                FillAction::Open | FillAction::Close => None,
            },
            actual_price: if okx { None } else { dec(&p["avg_price"]) },
            fee: dec(&p["fee"]),
            fee_asset: p["fee_asset"].as_str().map(str::to_string),
            filled_at_ms: e.ts_ms,
        };
        if !by_id.contains_key(id) {
            order.push(id.to_string());
        }
        by_id.insert(id.to_string(), OrderFill { leg, action, exchange, record });
    }
    order.into_iter().filter_map(|id| by_id.remove(&id)).collect()
}

/// `entry_snapshot` of the pair's ORDER_SUBMIT transition (engine-simulation's entry record).
fn entry_snapshot(events: &[EventRow]) -> Option<Value> {
    events
        .iter()
        .filter(|e| e.event_type == PAIR_TRANSITION && e.payload["to"].as_str() == Some("ORDER_SUBMIT"))
        .next_back()
        .map(|e| e.payload["detail"]["entry_snapshot"].clone())
        .filter(|v| v.is_object())
}

fn expected_snapshot(snap: Option<&Value>) -> Option<ExpectedSnapshot> {
    let ne = &snap?["net_edge"];
    Some(ExpectedSnapshot {
        funding_income: dec(&ne["funding_income_usdt"])?,
        fee: dec(&ne["fee_usdt"])?,
        slippage: dec(&ne["slippage_usdt"])?,
        safety_margin: dec(&ne["safety_margin_usdt"])?,
        net_edge: dec(&ne["net_edge_usdt"])?,
        assumed_settlements: 1,
    })
}

/// Holding window of each leg: last opening fill to last closing fill (None = not opened).
fn windows_of(fills: &[OrderFill], exchanges: [Exchange; 2], symbol: &str) -> [Option<LegWindow>; 2] {
    let mut out: [Option<LegWindow>; 2] = [None, None];
    for (i, side) in [Side::Long, Side::Short].into_iter().enumerate() {
        let leg_fills: Vec<&OrderFill> = fills.iter().filter(|f| f.leg == side && !f.record.quantity.is_zero()).collect();
        let opened = leg_fills.iter().filter(|f| f.action == FillAction::Open).map(|f| f.record.filled_at_ms).max();
        let closed = leg_fills.iter().filter(|f| f.action == FillAction::Close).map(|f| f.record.filled_at_ms).max();
        out[i] = opened.map(|o| LegWindow { exchange: exchanges[i], symbol: symbol.to_string(), opened_at_ms: o, closed_at_ms: closed });
    }
    out
}

/// Whether a completed fetch covers `[from, to]` for `exchange` (and the symbol, when the fetch
/// was per symbol); the newest fetch outcome for the exchange decides failures.
fn fetch_state(fetches: &[EventRow], exchange: Exchange, symbol: &str, from: i64, to: i64) -> FundingFetchState {
    let mut failed: Option<String> = None;
    let mut beyond = false;
    for e in fetches {
        let p = &e.payload;
        if p["exchange"].as_str() != Some(exchange.name()) {
            continue;
        }
        if let Some(s) = p["symbol"].as_str()
            && s != symbol
        {
            continue;
        }
        let (Some(start), Some(end)) = (p["start_ms"].as_i64(), p["end_ms"].as_i64()) else { continue };
        if start > from || end < to {
            continue;
        }
        match p["outcome"].as_str() {
            Some("complete") => return FundingFetchState::Fetched,
            Some("beyond_retention") => beyond = true,
            _ => failed = Some(p["reason"].as_str().unwrap_or("fetch incomplete").to_string()),
        }
    }
    if beyond {
        FundingFetchState::BeyondRetention
    } else {
        failed.map_or(FundingFetchState::NotFetched, FundingFetchState::Failed)
    }
}

/// Collects the PnL input of `pair` from the store at `now_ms`.
pub fn assemble(db: &Db, pair: &str, now_ms: i64) -> Result<PairPnlInput, String> {
    let row = db.get_pair(pair).map_err(|e| e.to_string())?.ok_or_else(|| format!("pair {pair} not found"))?;
    let env: PairEnvelope = serde_json::from_value(row.entry.clone()).map_err(|e| format!("pair {pair} entry unreadable: {e}"))?;
    let events = pair_events(db, pair)?;
    let snap = entry_snapshot(&events);
    let entry_price = |leg: &str| snap.as_ref().and_then(|s| dec(&s[leg]["expected_price"]));
    let fills = order_fills(&events, [entry_price("long"), entry_price("short")]);
    let exchanges = [env.long_exchange, env.short_exchange];
    let windows = windows_of(&fills, exchanges, &row.symbol);

    // Windows of every other pair on the same symbol, for the ambiguity check.
    let mut all_windows: Vec<(String, usize, LegWindow)> = Vec::new();
    for (i, w) in windows.iter().enumerate() {
        if let Some(w) = w {
            all_windows.push((pair.to_string(), i, w.clone()));
        }
    }
    for other in db.list_pairs().map_err(|e| e.to_string())?.into_iter().filter(|r| r.internal_uuid != pair && r.symbol == row.symbol) {
        let Ok(oenv) = serde_json::from_value::<PairEnvelope>(other.entry.clone()) else { continue };
        let oevents = pair_events(db, &other.internal_uuid)?;
        let ofills = order_fills(&oevents, [None, None]);
        for (i, w) in windows_of(&ofills, [oenv.long_exchange, oenv.short_exchange], &other.symbol).into_iter().enumerate() {
            if let Some(w) = w {
                all_windows.push((other.internal_uuid.clone(), i, w));
            }
        }
    }
    let plain: Vec<LegWindow> = all_windows.iter().map(|(_, _, w)| w.clone()).collect();
    let ledger: Vec<StoredLedgerEntry> = db.funding_ledger_entries().map_err(|e| e.to_string())?;
    let mut leg_entries: [Vec<tong_funding_core::pnl::FundingLedgerEntry>; 2] = [Vec::new(), Vec::new()];
    let mut ambiguous = false;
    for s in &ledger {
        match attribute(&s.entry, &plain, now_ms) {
            Attribution::Leg(i) => {
                let (owner, leg, _) = &all_windows[i];
                if owner == pair {
                    leg_entries[*leg].push(s.entry.clone());
                }
            }
            Attribution::Ambiguous(ix) => {
                if ix.iter().any(|i| all_windows[*i].0 == pair) {
                    ambiguous = true;
                }
            }
            Attribution::Unattributed => {}
        }
    }

    let fetches = events_of_type(db, FUNDING_LEDGER_FETCHED)?;
    let mut legs = Vec::new();
    let mut slots: [Vec<i64>; 2] = [Vec::new(), Vec::new()];
    for (i, side) in [Side::Long, Side::Short].into_iter().enumerate() {
        let exchange = exchanges[i];
        let leg_name = if i == 0 { "long" } else { "short" };
        let leg_fills: Vec<FillRecord> = fills.iter().filter(|f| f.leg == side).map(|f| f.record.clone()).collect();
        let (expected, fetch) = match &windows[i] {
            // Never opened: nothing to settle, nothing to fetch.
            None => (Some(0), FundingFetchState::Fetched),
            Some(w) => {
                let to = w.closed_at_ms.unwrap_or(now_ms);
                let nft = snap.as_ref().and_then(|s| s[leg_name]["next_funding_time"].as_i64());
                let interval = snap.as_ref().and_then(|s| s[leg_name]["funding_interval_secs"].as_i64());
                let expected = match (nft, interval) {
                    (Some(n), Some(iv)) if iv > 0 => {
                        slots[i] = expected_settlements(n, iv, w.opened_at_ms, to);
                        Some(slots[i].len())
                    }
                    _ => None,
                };
                let fetch = match exchange {
                    Exchange::Okx => FundingFetchState::NotFetched,
                    Exchange::Binance | Exchange::Bybit => fetch_state(&fetches, exchange, &row.symbol, w.opened_at_ms, to),
                };
                (expected, fetch)
            }
        };
        legs.push(LegInput {
            exchange,
            symbol: row.symbol.clone(),
            side,
            fills: leg_fills,
            funding: leg_entries[i].clone(),
            expected_settlements: expected,
            funding_fetch: fetch,
        });
    }
    let reconciliation_mismatch = events
        .iter()
        .filter(|e| e.event_type == PNL_RECONCILIATION)
        .next_back()
        .is_some_and(|e| e.payload["result"].as_str() != Some("OK"));
    let ledger_keys = leg_entries.iter().flatten().map(|e| e.dedupe_key.clone()).collect();
    let fill_ids = fills.iter().map(|f| f.record.id.clone()).collect();
    Ok(PairPnlInput {
        pair: pair.to_string(),
        simulated: env.simulated,
        input: PairInput { legs, ambiguous_attribution: ambiguous, reconciliation_mismatch },
        expected: expected_snapshot(snap.as_ref()),
        actual_settlements: slots.iter().map(Vec::len).max().unwrap_or(0),
        slots,
        windows,
        ledger_keys,
        fill_ids,
    })
}

/// Funding reasons that waiting (fetching again) can still resolve.
fn waiting_helps(r: &IncompleteReason) -> bool {
    match r {
        IncompleteReason::MissingSettlement { .. } | IncompleteReason::FundingNotFetched { .. } | IncompleteReason::FundingFetchFailed { .. } => true,
        IncompleteReason::SettlementsNotDerivable { .. }
        | IncompleteReason::BeyondRetention { .. }
        | IncompleteReason::MissingFillDetail { .. }
        | IncompleteReason::FeeNotConvertible { .. }
        | IncompleteReason::MissingReferencePrice { .. }
        | IncompleteReason::OpenCloseQuantityMismatch { .. }
        | IncompleteReason::AmbiguousAttribution
        | IncompleteReason::ReconciliationMismatch
        | IncompleteReason::NoFillsRecorded => false,
    }
}

/// The event payload of a computed result.
pub fn pnl_payload(a: &PairPnlInput, b: &PnlBreakdown, now_ms: i64) -> Value {
    let comparison = compare_expected(a.expected.as_ref(), b, a.actual_settlements);
    json!({
        "status": b.status.as_str(),
        "reasons": b.status.reasons().iter().map(|r| r.label()).collect::<Vec<_>>(),
        "breakdown": b,
        "slippage": slippage_summary(b),
        "expected_vs_actual": comparison,
        "no_expected_snapshot": matches!(comparison, Comparison::NoSnapshot),
        "settlement_slots": { "long": a.slots[0], "short": a.slots[1] },
        "ledger_keys": a.ledger_keys,
        "fill_ids": a.fill_ids,
        "computed_at_ms": now_ms,
    })
}

/// Writes `PAIR_PNL_COMPUTED` (none yet) or `PAIR_PNL_RECOMPUTED` (the result changed); returns
/// the newest PnL event id and whether a new event was written.
fn record(db: &Db, a: &PairPnlInput, b: &PnlBreakdown, now_ms: i64) -> Result<(i64, bool), String> {
    let latest = latest_pnl(db, &a.pair)?;
    let payload = pnl_payload(a, b, now_ms);
    if let Some(l) = &latest
        && l.payload["breakdown"] == payload["breakdown"]
        && l.payload["status"] == payload["status"]
        && l.payload["expected_vs_actual"] == payload["expected_vs_actual"]
    {
        return Ok((l.event_id, false));
    }
    let ty = if latest.is_some() { PAIR_PNL_RECOMPUTED } else { PAIR_PNL_COMPUTED };
    let id = EventStore::new(db.clone()).append(ty, Some(&a.pair), payload).map_err(|e| e.to_string())?.ok_or("PnL event not stored")?;
    Ok((id, true))
}

/// For a closing (or manually confirmed) pair: compute the PnL; record it when the funding part
/// is settled or `final_attempt` (retry window over / manual confirmation / restart), else wait.
/// SIMULATION pairs never get a PnL: callers must not call this for them (an error is returned).
pub fn settle_pnl(db: &Db, pair: &str, now_ms: i64, final_attempt: bool) -> Result<PnlAttempt, String> {
    let a = assemble(db, pair, now_ms)?;
    if a.simulated {
        return Err("a SIMULATION pair has no PnL (pnl-accounting)".into());
    }
    let b = compute_pnl(&a.input);
    if !final_attempt && b.status.reasons().iter().any(waiting_helps) {
        return Ok(PnlAttempt::Waiting { reasons: b.status.reasons().iter().map(|r| r.label()).collect() });
    }
    let (event_id, _) = record(db, &a, &b, now_ms)?;
    Ok(PnlAttempt::Recorded { event_id, status: b.status.as_str().to_string() })
}

/// After late data (new ledger entries, a reconciliation): recompute a pair that already has a
/// PnL and write `PAIR_PNL_RECOMPUTED` when the result changed. `Ok(None)` = nothing new.
pub fn recompute_if_changed(db: &Db, pair: &str, now_ms: i64) -> Result<Option<i64>, String> {
    if latest_pnl(db, pair)?.is_none() {
        return Ok(None);
    }
    let a = assemble(db, pair, now_ms)?;
    if a.simulated {
        return Ok(None);
    }
    let b = compute_pnl(&a.input);
    let (id, written) = record(db, &a, &b, now_ms)?;
    Ok(written.then_some(id))
}

#[cfg(test)]
#[path = "pnl_record_tests.rs"]
mod tests;

//! Reconciliation of a pair's funding with the exchange (spec pnl-accounting "與交易所流水對帳且
//! 差異必須警示", task 2.4): every held leg's window is fetched again (no local cache), compared
//! exactly (design D8, no tolerance) with what the store attributes to the leg, and one
//! `PNL_RECONCILIATION` event per run records `OK`, `MISMATCH` or `FAILED` per leg and overall.
//! Nothing stored is changed: entries found only on the exchange are reported, not written
//! (no automatic correction, design Non-Goals). The pair's PnL is then recomputed, so a mismatch
//! makes it INCOMPLETE.

use serde_json::{Value, json};
use tong_funding_core::pnl::{Reconciliation, reconcile};
use tong_funding_core::types::Decimal;

use super::PNL_RECONCILIATION;
use super::fetch::{FetchStatus, LedgerSource, Pause, fetch_range};
use super::pnl_record::{assemble, recompute_if_changed};
use crate::store::db::Db;
use crate::store::events::EventStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileResult {
    Ok,
    Mismatch,
    Failed,
}

impl ReconcileResult {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReconcileResult::Ok => "OK",
            ReconcileResult::Mismatch => "MISMATCH",
            ReconcileResult::Failed => "FAILED",
        }
    }
}

/// Reconcile `pair` with the given sources (one per exchange). Returns the overall result; the
/// event (and an alert-worthy `MISMATCH` / `FAILED`) is written in every case.
pub async fn reconcile_pair(db: &Db, sources: &[&dyn LedgerSource], pause: &dyn Pause, pair: &str, now_ms: i64) -> Result<ReconcileResult, String> {
    let a = assemble(db, pair, now_ms)?;
    if a.simulated {
        return Err("a SIMULATION pair has no funding to reconcile".into());
    }
    let mut legs: Vec<Value> = Vec::new();
    let mut overall = ReconcileResult::Ok;
    for (i, w) in a.windows.iter().enumerate() {
        let Some(w) = w else { continue };
        let end = w.closed_at_ms.unwrap_or(now_ms);
        let local: Vec<(String, Decimal)> = a.input.legs[i].funding.iter().map(|e| (e.dedupe_key.clone(), e.amount)).collect();
        let window = json!({ "start_ms": w.opened_at_ms, "end_ms": end });
        let base = json!({ "leg": if i == 0 { "long" } else { "short" }, "exchange": w.exchange.name(), "symbol": w.symbol, "window": window });
        let Some(source) = sources.iter().find(|s| s.exchange() == w.exchange) else {
            overall = ReconcileResult::Failed;
            legs.push(merge(base, json!({ "result": "FAILED", "reason": "no ledger source for this exchange" })));
            continue;
        };
        let fetched = fetch_range(*source, pause, &w.symbol, w.opened_at_ms, end, now_ms).await;
        match fetched.status {
            FetchStatus::Complete => {}
            FetchStatus::Incomplete(reason) => {
                overall = ReconcileResult::Failed;
                legs.push(merge(base, json!({ "result": "FAILED", "reason": reason })));
                continue;
            }
            FetchStatus::BeyondRetention => {
                overall = ReconcileResult::Failed;
                legs.push(merge(base, json!({ "result": "FAILED", "reason": "超出交易所保留範圍" })));
                continue;
            }
        }
        let remote: Vec<(String, Decimal)> = fetched
            .entries
            .iter()
            .filter(|e| w.holds(e.exchange, &e.symbol, e.settled_at_ms, now_ms))
            .map(|e| (e.dedupe_key.clone(), e.amount))
            .collect();
        match reconcile(&local, &remote) {
            Reconciliation::Ok { count, sum } => legs.push(merge(base, json!({ "result": "OK", "count": count, "sum": sum.to_string() }))),
            Reconciliation::Mismatch { local_sum, remote_sum, diff, local_count, remote_count, missing_locally, missing_remotely } => {
                if overall == ReconcileResult::Ok {
                    overall = ReconcileResult::Mismatch;
                }
                legs.push(merge(
                    base,
                    json!({
                        "result": "MISMATCH",
                        "local_sum": local_sum.to_string(),
                        "remote_sum": remote_sum.to_string(),
                        "diff": diff.to_string(),
                        "local_count": local_count,
                        "remote_count": remote_count,
                        "missing_locally": missing_locally,
                        "missing_remotely": missing_remotely,
                    }),
                ));
            }
        }
    }
    let payload = json!({ "result": overall.as_str(), "legs": legs, "alert": overall != ReconcileResult::Ok, "reconciled_at_ms": now_ms });
    EventStore::new(db.clone()).append(PNL_RECONCILIATION, Some(pair), payload).map_err(|e| e.to_string())?;
    recompute_if_changed(db, pair, now_ms)?;
    Ok(overall)
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Value::Object(m), Value::Object(n)) = (&mut a, b) {
        m.extend(n);
    }
    a
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;

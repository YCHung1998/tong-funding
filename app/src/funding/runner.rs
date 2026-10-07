//! The running funding loop (closes funding-pnl 實作紀錄 #17): one [`tick`] decides, from the
//! store and an injected `now_ms`, which ledger fetches and which reconciliations are due, runs
//! them through the injected [`LedgerSource`]s and reports what happened. The UI composition root
//! (`ui::live`) calls it on a timer with the real signed ledger sources (Keychain keys); nothing
//! here reads the system clock.
//!
//! Schedule (design D9; every value provisional, NOT verified, task 5.1 calibrates them):
//! - Fetches: `plan_fetches` says what is due (an expected settlement older than
//!   `FETCH_DELAY_MS` without its entry, inside the retry window; or a closed leg whose PnL is not
//!   recorded yet). Plans are merged per query (Binance: per symbol; Bybit: one all-symbol query)
//!   into the union of their windows, and each query runs at most once per `FETCH_RETRY_MS`.
//! - Reconciliation: once per non-simulated pair after its first PnL event, `RECONCILE_DELAY_MS`
//!   later (lets the post-close fetch and late entries land first). `OK` and `MISMATCH` are final
//!   (a mismatch stays an alert; nothing is corrected automatically). `FAILED` is retried every
//!   `FETCH_RETRY_MS` until `PNL_RETRY_WINDOW_MS` after the first failure; the last `FAILED`
//!   then stays (alert) and nothing more is attempted.
//! - OKX (okx-funding-ledger) is fetched per symbol like Binance and reconciled like every leg.
//! - Missing demo keys: neither fetches nor reconciliations are attempted for that exchange (the
//!   funding status stays "未取得", spec funding-history-fetch), and nothing is written. They
//!   become due again once the keys are readable.
//! - The kill switch does not stop the loop (read-only); a halted store does (`plan_fetches`,
//!   `fetch_and_store`), and SIMULATION pairs never fetch or reconcile.

use std::collections::BTreeMap;

use tong_funding_core::types::Exchange;

use super::fetch::{FetchPlan, FetchStatus, LedgerSource, Pause, fetch_and_store, plan_fetches};
use super::pnl_record::pair_events;
use super::reconcile::{ReconcileResult, reconcile_pair};
use super::{FETCH_DELAY_MS, FETCH_RETRY_MS, PAIR_PNL_COMPUTED, PAIR_PNL_RECOMPUTED, PNL_RECONCILIATION, PNL_RETRY_WINDOW_MS};
use crate::engine::actor::PairEnvelope;
use crate::store::db::Db;

/// Delay between a pair's first PnL event and its reconciliation (design D9, provisional).
pub const RECONCILE_DELAY_MS: i64 = FETCH_DELAY_MS;
/// How often the running loop wakes up; every decision is still made from `now_ms`.
pub const LOOP_TICK_MS: u64 = 15_000;

/// One ledger query: Binance per symbol, Bybit for every symbol (`symbol = None`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FetchKey {
    pub exchange: Exchange,
    pub symbol: Option<String>,
}

/// Plans merged into one query over the union of their windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedFetch {
    pub key: FetchKey,
    /// The symbol passed to the source (any of the merged plans' for an all-symbol source).
    pub symbol: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub pairs: Vec<String>,
}

/// Merges plans that are the same query. `per_symbol` says whether an exchange's source queries
/// per symbol; an exchange without a source counts as per symbol (it is skipped later anyway).
pub fn merge_plans(plans: &[FetchPlan], per_symbol: impl Fn(Exchange) -> bool) -> Vec<MergedFetch> {
    let mut out: BTreeMap<FetchKey, MergedFetch> = BTreeMap::new();
    for p in plans {
        let key = FetchKey { exchange: p.exchange, symbol: per_symbol(p.exchange).then(|| p.symbol.clone()) };
        let m = out.entry(key.clone()).or_insert_with(|| MergedFetch { key, symbol: p.symbol.clone(), start_ms: p.start_ms, end_ms: p.end_ms, pairs: Vec::new() });
        m.start_ms = m.start_ms.min(p.start_ms);
        m.end_ms = m.end_ms.max(p.end_ms);
        if !m.pairs.contains(&p.pair) {
            m.pairs.push(p.pair.clone());
        }
    }
    out.into_values().collect()
}

/// In-memory pacing of the loop (a restart simply fetches once more: writes are idempotent).
#[derive(Debug, Default)]
pub struct FundingLoop {
    last_fetch: BTreeMap<FetchKey, i64>,
}

impl FundingLoop {
    /// A query runs at most once per `FETCH_RETRY_MS`.
    pub fn fetch_due(&self, key: &FetchKey, now_ms: i64) -> bool {
        self.last_fetch.get(key).is_none_or(|t| now_ms.saturating_sub(*t) >= FETCH_RETRY_MS)
    }

    fn mark(&mut self, key: FetchKey, now_ms: i64) {
        self.last_fetch.insert(key, now_ms);
    }
}

/// Whether a pair's reconciliation is due (pure; see the module docs for the schedule).
/// `first_pnl_ms` = time of the pair's first PnL event; `history` = earlier reconciliations,
/// oldest first.
pub fn reconcile_due(first_pnl_ms: Option<i64>, history: &[(i64, ReconcileResult)], now_ms: i64) -> bool {
    let Some(first_pnl) = first_pnl_ms else { return false };
    let mut first_failed: Option<i64> = None;
    let mut last_failed: Option<i64> = None;
    for (ts, r) in history {
        match r {
            ReconcileResult::Ok | ReconcileResult::Mismatch => return false,
            ReconcileResult::Failed => {
                first_failed.get_or_insert(*ts);
                last_failed = Some(*ts);
            }
        }
    }
    match (first_failed, last_failed) {
        (Some(first), Some(last)) => now_ms >= last.saturating_add(FETCH_RETRY_MS) && now_ms <= first.saturating_add(PNL_RETRY_WINDOW_MS),
        _ => now_ms >= first_pnl.saturating_add(RECONCILE_DELAY_MS),
    }
}

fn parse_result(s: Option<&str>) -> Option<ReconcileResult> {
    match s? {
        "OK" => Some(ReconcileResult::Ok),
        "MISMATCH" => Some(ReconcileResult::Mismatch),
        "FAILED" => Some(ReconcileResult::Failed),
        _ => None,
    }
}

/// A pair whose reconciliation is due, with the exchanges of its legs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileCandidate {
    pub pair: String,
    pub exchanges: [Exchange; 2],
}

/// Non-simulated pairs whose reconciliation is due at `now_ms` (read from the store, so the
/// schedule survives a restart). A halted store reconciles nothing.
pub fn reconcile_candidates(db: &Db, now_ms: i64) -> Result<Vec<ReconcileCandidate>, String> {
    if db.is_halted() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for row in db.list_pairs().map_err(|e| e.to_string())? {
        let Ok(env) = serde_json::from_value::<PairEnvelope>(row.entry.clone()) else { continue };
        if env.simulated {
            continue;
        }
        let events = pair_events(db, &row.internal_uuid)?;
        let first_pnl = events.iter().find(|e| e.event_type == PAIR_PNL_COMPUTED || e.event_type == PAIR_PNL_RECOMPUTED).map(|e| e.ts_ms);
        let history: Vec<(i64, ReconcileResult)> =
            events.iter().filter(|e| e.event_type == PNL_RECONCILIATION).filter_map(|e| parse_result(e.payload["result"].as_str()).map(|r| (e.ts_ms, r))).collect();
        if reconcile_due(first_pnl, &history, now_ms) {
            out.push(ReconcileCandidate { pair: row.internal_uuid.clone(), exchanges: [env.long_exchange, env.short_exchange] });
        }
    }
    Ok(out)
}

/// What one tick did.
#[derive(Debug, Default, PartialEq)]
pub struct TickReport {
    /// Queries run, with their outcome (`Err` = nothing fetched, e.g. the store halted).
    pub fetched: Vec<(FetchKey, Result<FetchStatus, String>)>,
    /// Queries or reconciliations not attempted: (exchange, why), e.g. demo keys missing.
    pub skipped: Vec<(Exchange, String)>,
    pub reconciled: Vec<(String, Result<ReconcileResult, String>)>,
    /// The planning itself failed (store read).
    pub errors: Vec<String>,
}

impl TickReport {
    /// Whether anything may have been written (the UI's funding data should be re-read).
    pub fn wrote(&self) -> bool {
        !self.fetched.is_empty() || !self.reconciled.is_empty()
    }
}

/// Whether an exchange's ledger can be read now (demo keys present); `Err` = why not. Never
/// returns or logs a key value.
pub type Readiness<'a> = &'a (dyn Fn(Exchange) -> Result<(), String> + Sync);

/// One pass of the loop at `now_ms`: due fetches first (so a reconciliation sees them), then due
/// reconciliations.
pub async fn tick(db: &Db, sources: &[&dyn LedgerSource], ready: Readiness<'_>, pause: &dyn Pause, state: &mut FundingLoop, now_ms: i64) -> TickReport {
    let mut report = TickReport::default();
    let source_of = |ex: Exchange| sources.iter().copied().find(|s| s.exchange() == ex);
    match plan_fetches(db, now_ms) {
        Err(e) => report.errors.push(e),
        Ok(plans) => {
            for m in merge_plans(&plans, |ex| source_of(ex).is_none_or(|s| s.per_symbol())) {
                if !state.fetch_due(&m.key, now_ms) {
                    continue;
                }
                state.mark(m.key.clone(), now_ms);
                let Some(source) = source_of(m.key.exchange) else {
                    report.skipped.push((m.key.exchange, "no ledger source for this exchange".into()));
                    continue;
                };
                if let Err(why) = ready(m.key.exchange) {
                    report.skipped.push((m.key.exchange, why));
                    continue;
                }
                let r = fetch_and_store(db, source, pause, &m.symbol, m.start_ms, m.end_ms, now_ms).await.map(|s| s.status);
                report.fetched.push((m.key, r));
            }
        }
    }
    match reconcile_candidates(db, now_ms) {
        Err(e) => report.errors.push(e),
        Ok(candidates) => {
            for c in candidates {
                // A leg without a source is recorded as FAILED by reconcile_pair (truthful); only
                // exchanges that have a source are checked for readiness (keys) here.
                let not_ready = c.exchanges.iter().filter(|ex| source_of(**ex).is_some()).find_map(|ex| ready(*ex).err().map(|why| (*ex, why)));
                if let Some(skip) = not_ready {
                    report.skipped.push(skip);
                    continue;
                }
                let r = reconcile_pair(db, sources, pause, &c.pair, now_ms).await;
                report.reconciled.push((c.pair, r));
            }
        }
    }
    report
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;

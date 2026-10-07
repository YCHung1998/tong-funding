//! Funding ledger fetching (spec funding-history-fetch, task 1.3): the query range is cut into
//! windows of at most 7 days, each window is paged until the exchange says it is exhausted, and
//! anything that cannot be confirmed complete is a failure (fail closed), never "no entries".
//! 429 / `Retry-After` is honoured with a bounded number of retries. Entries fetched before a
//! failure are still written (writes are idempotent).
//!
//! Sources are injected (`LedgerSource`); the real ones wrap the signed ledger clients of
//! `exchange::signed::ledger`. Waiting goes through an injected `Pause` (no clock reads here).

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::json;
use tong_funding_core::pnl::{FundingLedgerEntry, match_slots};
use tong_funding_core::types::Exchange;

use super::pnl_record::{assemble, latest_pnl, recompute_if_changed};
use super::{FETCH_DELAY_MS, FETCH_ERROR, FUNDING_LEDGER_FETCHED, PNL_RETRY_WINDOW_MS};
use crate::exchange::error::AdapterError;
use crate::exchange::signed::ledger::{BINANCE_INCOME_LIMIT, BinanceLedgerClient, BybitLedgerClient, LedgerPage};
use crate::exchange::signed::okx::OkxSignedClient;
use crate::exchange::transport::HttpTransport;
use crate::store::db::Db;
use crate::store::events::EventStore;
use crate::store::funding_ledger::LedgerWriteReport;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Longest window per query (Bybit: both times given → at most 7 days; Binance: 7 days is what it
/// returns without times). Windows are inclusive, so one window spans at most this many ms.
pub const MAX_WINDOW_MS: i64 = 7 * 24 * 3_600_000;
/// Binance keeps income for the last three months; 90 days is used (UNVERIFIED exact length).
pub const BINANCE_RETENTION_MS: i64 = 90 * 24 * 3_600_000;
/// Page cap per window (as the signed clients); reaching it means "not confirmed complete".
pub const MAX_PAGES: usize = 20;
/// Rate-limit retries per request, and the wait when the exchange gives no `Retry-After`.
pub const MAX_RATE_LIMIT_RETRIES: usize = 3;
pub const DEFAULT_BACKOFF_MS: u64 = 2_000;

/// One exchange's ledger, page by page.
pub trait LedgerSource: Send + Sync {
    fn exchange(&self) -> Exchange;
    /// Whether a query is per symbol (Binance) or covers every symbol (Bybit).
    fn per_symbol(&self) -> bool;
    /// One page; `token` = `None` for the first page, else the previous page's `next_cursor`.
    /// The returned `next_cursor` is `None` once the window is exhausted.
    fn page<'a>(&'a self, symbol: &'a str, start_ms: i64, end_ms: i64, token: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>>;
}

/// Waiting for a rate-limit backoff.
pub trait Pause: Send + Sync {
    fn pause(&self, ms: u64) -> BoxFut<'_, ()>;
}

/// `tokio::time::sleep` (paused time in tests).
pub struct TokioPause;
impl Pause for TokioPause {
    fn pause(&self, ms: u64) -> BoxFut<'_, ()> {
        Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchStatus {
    Complete,
    /// Some window could not be confirmed complete (a failed page, a page cap, a repeated cursor).
    Incomplete(String),
    /// The range starts before what the exchange keeps: nothing was requested.
    BeyondRetention,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FetchOutcome {
    pub entries: Vec<FundingLedgerEntry>,
    pub status: FetchStatus,
}

/// Inclusive windows `[s, e]` covering `[start, end]`, each spanning at most `MAX_WINDOW_MS`.
pub fn split_windows(start_ms: i64, end_ms: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut s = start_ms;
    while s <= end_ms {
        let e = s.saturating_add(MAX_WINDOW_MS - 1).min(end_ms);
        out.push((s, e));
        if e == i64::MAX {
            break;
        }
        s = e + 1;
    }
    out
}

async fn page_with_backoff(
    source: &dyn LedgerSource,
    pause: &dyn Pause,
    symbol: &str,
    window: (i64, i64),
    token: Option<&str>,
) -> Result<LedgerPage, AdapterError> {
    let mut retries = 0;
    loop {
        match source.page(symbol, window.0, window.1, token).await {
            Err(AdapterError::RateLimited { retry_after_ms }) if retries < MAX_RATE_LIMIT_RETRIES => {
                retries += 1;
                pause.pause(retry_after_ms.unwrap_or(DEFAULT_BACKOFF_MS)).await;
            }
            other => return other,
        }
    }
}

/// Fetches `[start, end]` of `source` (for `symbol` when the source is per symbol).
pub async fn fetch_range(source: &dyn LedgerSource, pause: &dyn Pause, symbol: &str, start_ms: i64, end_ms: i64, now_ms: i64) -> FetchOutcome {
    if source.exchange() == Exchange::Binance && start_ms < now_ms.saturating_sub(BINANCE_RETENTION_MS) {
        return FetchOutcome { entries: Vec::new(), status: FetchStatus::BeyondRetention };
    }
    let mut entries = Vec::new();
    for window in split_windows(start_ms, end_ms) {
        let mut token: Option<String> = None;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut done = false;
        for page_no in 1..=MAX_PAGES {
            match page_with_backoff(source, pause, symbol, window, token.as_deref()).await {
                Err(e) => {
                    let reason = format!("{} window {}..{} page {page_no} failed: {e}", source.exchange().name(), window.0, window.1);
                    return FetchOutcome { entries, status: FetchStatus::Incomplete(reason) };
                }
                Ok(page) => {
                    entries.extend(page.entries);
                    match page.next_cursor {
                        None => {
                            done = true;
                            break;
                        }
                        Some(c) if !seen.insert(c.clone()) => {
                            let reason = format!("{} window {}..{}: cursor repeated at page {page_no}", source.exchange().name(), window.0, window.1);
                            return FetchOutcome { entries, status: FetchStatus::Incomplete(reason) };
                        }
                        Some(c) => token = Some(c),
                    }
                }
            }
        }
        if !done {
            let reason = format!("{} window {}..{}: page cap of {MAX_PAGES} reached", source.exchange().name(), window.0, window.1);
            return FetchOutcome { entries, status: FetchStatus::Incomplete(reason) };
        }
    }
    FetchOutcome { entries, status: FetchStatus::Complete }
}

/// What `fetch_and_store` did.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFetch {
    pub status: FetchStatus,
    pub write: LedgerWriteReport,
    /// PnL events written because late entries changed a pair's result.
    pub recomputed: Vec<i64>,
}

/// Fetch, write the entries (idempotent), record the outcome (`FUNDING_LEDGER_FETCHED`, plus
/// `FETCH_ERROR` when incomplete) and recompute the pairs whose PnL already exists. A halted
/// store fetches nothing.
pub async fn fetch_and_store(db: &Db, source: &dyn LedgerSource, pause: &dyn Pause, symbol: &str, start_ms: i64, end_ms: i64, now_ms: i64) -> Result<StoredFetch, String> {
    if let Some(r) = db.halt_reason() {
        return Err(format!("store halted, nothing fetched: {r}"));
    }
    let outcome = fetch_range(source, pause, symbol, start_ms, end_ms, now_ms).await;
    let write = db.write_funding_ledger(&outcome.entries).map_err(|e| e.to_string())?;
    let (label, reason) = match &outcome.status {
        FetchStatus::Complete => ("complete", None),
        FetchStatus::Incomplete(r) => ("incomplete", Some(r.clone())),
        FetchStatus::BeyondRetention => ("beyond_retention", Some("超出交易所保留範圍".to_string())),
    };
    let events = EventStore::new(db.clone());
    let payload = json!({
        "source": "funding_ledger",
        "exchange": source.exchange().name(),
        "symbol": source.per_symbol().then_some(symbol),
        "start_ms": start_ms,
        "end_ms": end_ms,
        "outcome": label,
        "reason": reason,
        "inserted": write.inserted,
        "skipped": write.skipped,
        "conflicts": write.conflicts,
    });
    events.append(FUNDING_LEDGER_FETCHED, None, payload.clone()).map_err(|e| e.to_string())?;
    if matches!(outcome.status, FetchStatus::Incomplete(_)) {
        events.append(FETCH_ERROR, None, payload).map_err(|e| e.to_string())?;
    }
    let mut recomputed = Vec::new();
    if write.inserted > 0 || matches!(outcome.status, FetchStatus::Complete) {
        for row in db.list_pairs().map_err(|e| e.to_string())? {
            if latest_pnl(db, &row.internal_uuid)?.is_some()
                && let Some(id) = recompute_if_changed(db, &row.internal_uuid, now_ms)?
            {
                recomputed.push(id);
            }
        }
    }
    Ok(StoredFetch { status: outcome.status, write, recomputed })
}

/// One fetch the scheduler should run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPlan {
    pub pair: String,
    pub exchange: Exchange,
    pub symbol: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Which ledger fetches are due at `now_ms` (spec "取得流水的時機"): for every non-simulated pair
/// with a held leg on Binance / Bybit / OKX, an expected settlement older than `FETCH_DELAY_MS` without
/// a matching entry (until the retry window after it is over), or a closed leg whose PnL is not
/// recorded yet. The kill switch does not stop this (read-only); a halted store does, and
/// SIMULATION pairs never fetch. The caller paces repeats with `FETCH_RETRY_MS`.
pub fn plan_fetches(db: &Db, now_ms: i64) -> Result<Vec<FetchPlan>, String> {
    if db.is_halted() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for row in db.list_pairs().map_err(|e| e.to_string())? {
        let Ok(a) = assemble(db, &row.internal_uuid, now_ms) else { continue };
        if a.simulated {
            continue;
        }
        let has_pnl = latest_pnl(db, &row.internal_uuid)?.is_some();
        for (i, w) in a.windows.iter().enumerate() {
            let Some(w) = w else { continue };
            let leg = &a.input.legs[i];
            let times: Vec<i64> = leg.funding.iter().map(|e| e.settled_at_ms).collect();
            let slots = match_slots(&a.slots[i], &times, now_ms, PNL_RETRY_WINDOW_MS + FETCH_DELAY_MS);
            let missing_due = slots.iter().any(|(t, _, hit)| hit.is_none() && now_ms >= t + FETCH_DELAY_MS && now_ms <= t + FETCH_DELAY_MS + PNL_RETRY_WINDOW_MS);
            let closed_unrecorded = w.closed_at_ms.is_some() && !has_pnl;
            if missing_due || closed_unrecorded {
                out.push(FetchPlan {
                    pair: row.internal_uuid.clone(),
                    exchange: w.exchange,
                    symbol: w.symbol.clone(),
                    start_ms: w.opened_at_ms,
                    end_ms: w.closed_at_ms.unwrap_or(now_ms).min(now_ms),
                });
            }
        }
    }
    Ok(out)
}

// ---- the real sources ----------------------------------------------------------------------------

/// Binance income as a `LedgerSource`: pages are numbered; a page shorter than the limit ends
/// the window (a full page means "there may be more").
pub struct BinanceLedgerSource<T>(pub Arc<BinanceLedgerClient<T>>);

impl<T: HttpTransport + 'static> LedgerSource for BinanceLedgerSource<T> {
    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }
    fn per_symbol(&self) -> bool {
        true
    }
    fn page<'a>(&'a self, symbol: &'a str, start_ms: i64, end_ms: i64, token: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        Box::pin(async move {
            let page_no: u32 = match token {
                None => 1,
                Some(t) => t.parse().map_err(|_| AdapterError::parse("bad Binance page token"))?,
            };
            let mut page = self.0.income_page(symbol, start_ms, end_ms, page_no).await?;
            page.next_cursor = (page.rows >= BINANCE_INCOME_LIMIT as usize).then(|| (page_no + 1).to_string());
            Ok(page)
        })
    }
}

/// Bybit transaction-log as a `LedgerSource` (every symbol; `nextPageCursor`).
pub struct BybitLedgerSource<T>(pub Arc<BybitLedgerClient<T>>);

impl<T: HttpTransport + 'static> LedgerSource for BybitLedgerSource<T> {
    fn exchange(&self) -> Exchange {
        Exchange::Bybit
    }
    fn per_symbol(&self) -> bool {
        false
    }
    fn page<'a>(&'a self, _symbol: &'a str, start_ms: i64, end_ms: i64, token: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        Box::pin(self.0.transaction_log_page(start_ms, end_ms, token))
    }
}

/// OKX `bills-archive` as a `LedgerSource`: per symbol, paged by the last `billId` (`after`,
/// "older than"); the cursor is the page's `next_cursor` (a full page only).
pub struct OkxLedgerSource<T>(pub Arc<OkxSignedClient<T>>);

impl<T: HttpTransport + 'static> LedgerSource for OkxLedgerSource<T> {
    fn exchange(&self) -> Exchange {
        Exchange::Okx
    }
    fn per_symbol(&self) -> bool {
        true
    }
    fn page<'a>(&'a self, symbol: &'a str, start_ms: i64, end_ms: i64, token: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        Box::pin(self.0.bills_page(symbol, start_ms, end_ms, token))
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;

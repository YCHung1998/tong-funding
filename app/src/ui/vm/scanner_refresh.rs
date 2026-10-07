//! "立即刷新" (task 2.5, spec scanner-page "立即刷新必須重新抓取並重算"). A refresh sends new
//! batch requests to every enabled market source (`ExchangeAdapter::fetch_snapshot` always sends
//! fresh market requests), then the table is recomputed from the new observations. A gate makes a
//! second click while one is running a no-op, so there are never parallel duplicate requests.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use tong_funding_core::funding::FundingObservation;
use tong_funding_core::types::Exchange;

use super::bridge::SourceUpdate;
use crate::exchange::error::AdapterError;
use crate::exchange::public::adapter::{Books, ExchangeAdapter, MarketSnapshot};
use crate::ports::Clock;

/// Allows one refresh at a time. Cheap to clone; clones share the state.
#[derive(Clone, Default)]
pub struct RefreshGate(Arc<Mutex<bool>>);

/// Held while a refresh runs; dropping it re-opens the gate (also on panic or cancellation).
pub struct RefreshGuard(Arc<Mutex<bool>>);

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }
}

impl RefreshGate {
    /// `Some(guard)` when no refresh is running; `None` = ignore this click.
    pub fn try_begin(&self) -> Option<RefreshGuard> {
        let mut running = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *running {
            return None;
        }
        *running = true;
        Some(RefreshGuard(Arc::clone(&self.0)))
    }

    pub fn in_progress(&self) -> bool {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What one refresh produced, per enabled source.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshOutcome {
    pub started_at: i64,
    pub finished_at: i64,
    pub results: Vec<(Exchange, Result<Vec<FundingObservation>, AdapterError>)>,
    /// Best bid / ask of the same round, per source whose market request succeeded. `Err` = the
    /// quotes are unavailable (e.g. Binance `bookTicker` rate limited); observations are unaffected.
    pub books: Vec<(Exchange, Result<Books, String>)>,
}

impl RefreshOutcome {
    pub fn all_failed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|(_, r)| r.is_err())
    }

    /// Snapshot updates: successes replace that exchange's observations, failures only record the
    /// error (old values stay, marked stale; never presented as new data).
    pub fn updates(&self) -> Vec<SourceUpdate> {
        let market = self.results.iter().map(|(exchange, r)| match r {
            Ok(observations) => SourceUpdate::Market { exchange: *exchange, observations: observations.clone(), at: self.finished_at },
            Err(e) => SourceUpdate::MarketError { exchange: *exchange, error: e.to_string(), at: self.finished_at },
        });
        let books = self.books.iter().map(|(exchange, b)| SourceUpdate::Books { exchange: *exchange, books: b.clone(), at: self.finished_at });
        market.chain(books).collect()
    }
}

type Fetched = (Exchange, Result<MarketSnapshot, AdapterError>);

async fn fetch_if<A: ExchangeAdapter>(adapter: &A, enabled: &BTreeSet<Exchange>) -> Option<Fetched> {
    let ex = adapter.exchange();
    if enabled.contains(&ex) { Some((ex, adapter.fetch_market().await)) } else { None }
}

/// Re-fetches every enabled market source concurrently. Disabled exchanges get no request.
pub async fn refresh_sources<A, B, C>(binance: &A, bybit: &B, okx: &C, enabled: &BTreeSet<Exchange>, clock: &dyn Clock) -> RefreshOutcome
where
    A: ExchangeAdapter,
    B: ExchangeAdapter,
    C: ExchangeAdapter,
{
    let started_at = clock.now_ms();
    let (a, b, c) = futures_util::join!(fetch_if(binance, enabled), fetch_if(bybit, enabled), fetch_if(okx, enabled));
    let mut results = Vec::new();
    let mut books = Vec::new();
    for (ex, r) in [a, b, c].into_iter().flatten() {
        match r {
            Ok(m) => {
                books.push((ex, m.books));
                results.push((ex, Ok(m.observations)));
            }
            Err(e) => results.push((ex, Err(e))),
        }
    }
    RefreshOutcome { started_at, finished_at: clock.now_ms(), results, books }
}

#[cfg(test)]
#[path = "scanner_refresh_tests.rs"]
mod tests;

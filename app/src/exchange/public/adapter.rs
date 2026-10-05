//! The read-only `ExchangeAdapter` trait, the normalised `InstrumentRules`, and the helpers the
//! three public adapters share (HTTP status classification, JSON number parsing, TTL cell,
//! observation assembly with the interval consistency check).
//!
//! Spec: exchange-readonly-adapters / exchange-adapter. Nothing here reads a wall clock; adapters
//! get `observed_at` from the injected `Clock`.
#![allow(dead_code)]

use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::quantity::LotSize;
use tong_funding_core::types::{Decimal, Exchange};

use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpResponse, HttpTransport};

/// Single-symbol requests (pre-trade re-fetch). Proposed value, not yet validated (design D10).
pub const SINGLE_TIMEOUT: Duration = Duration::from_secs(2);
/// Batch / catalog requests. Proposed value, not yet validated (design D10).
pub const BATCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Catalog and funding-interval cache lifetime (design D5; copied from the Python version, unvalidated).
pub const META_TTL_MS: i64 = 3_600_000;
/// Tolerance of the "interval vs next settlement" consistency check (design D5, unvalidated).
pub const CONSISTENCY_TOLERANCE_MS: i64 = 60_000;
/// Upper bound of pages fetched from a paged endpoint (design D6, unvalidated).
pub const MAX_PAGES: usize = 20;

/// Whether the exchange catalog lists the symbol as a tradable USDT perpetual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingStatus {
    Listed,
    NotListed,
}

/// Quantity constraints of one instrument, normalised across exchanges.
///
/// `step_size` / `min_qty` / `max_qty` are the ordinary (limit) order constraints in the unit the
/// exchange takes orders in: base-coin quantity for Binance and Bybit, **contracts** for OKX
/// (`lotSz` / `minSz`; one contract is `ct_val` of the base coin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRules {
    pub exchange: Exchange,
    pub symbol: String,
    pub step_size: Decimal,
    pub min_qty: Decimal,
    pub max_qty: Option<Decimal>,
    /// Binance `MARKET_LOT_SIZE`; kept apart from `LOT_SIZE`, never merged.
    pub market_step_size: Option<Decimal>,
    pub market_min_qty: Option<Decimal>,
    /// Binance `MARKET_LOT_SIZE.maxQty`, Bybit `maxMktOrderQty`.
    pub market_max_qty: Option<Decimal>,
    /// Bybit `minNotionalValue`.
    pub min_notional: Option<Decimal>,
    /// OKX only.
    pub ct_val: Option<Decimal>,
    pub ct_mult: Option<Decimal>,
}

impl InstrumentRules {
    /// The lot size `core` uses for rounding (`step_size`, `min_qty`).
    pub fn lot_size(&self) -> LotSize {
        LotSize { step_size: self.step_size, min_qty: self.min_qty }
    }
}

/// Result of a rules lookup. A missing required field is `Unavailable`; callers must not fall back
/// to a default step size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RulesLookup {
    Available(InstrumentRules),
    Unavailable { reason: String },
    /// The (complete) catalog does not contain the symbol.
    UnknownSymbol,
}

/// Read-only access to one exchange's public market data. There is deliberately no method that
/// places, cancels or amends an order or changes leverage; the order path is a separate change.
pub trait ExchangeAdapter: Send + Sync {
    fn exchange(&self) -> Exchange;

    /// Batch snapshot of every symbol the market-data endpoints return. Always sends fresh market
    /// requests (it doubles as the batch "refresh now"); only the catalog / interval metadata may
    /// come from its TTL cache. Symbols that are not tradable USDT perpetuals come back `NotListed`.
    fn fetch_snapshot(&self) -> impl Future<Output = Result<Vec<FundingObservation>, AdapterError>> + Send;

    /// Re-fetch one symbol. Every call sends new HTTP requests and reads no observation cache.
    fn refetch_symbol(&self, symbol: &str) -> impl Future<Output = Result<FundingObservation, AdapterError>> + Send;

    fn instrument_rules(&self, symbol: &str) -> impl Future<Output = Result<RulesLookup, AdapterError>> + Send;

    fn listing_status(&self, symbol: &str) -> impl Future<Output = Result<ListingStatus, AdapterError>> + Send;
}

// ---------------------------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------------------------

/// `Retry-After` in whole seconds to milliseconds; an HTTP-date or garbage gives `None`.
pub(crate) fn retry_after_ms(resp: &HttpResponse) -> Option<u64> {
    let secs: u64 = resp.header_value("retry-after")?.trim().parse().ok()?;
    secs.checked_mul(1000)
}

/// 429 gives `RateLimited`, other non-2xx gives `Http`, 2xx gives the body.
pub(crate) fn classify_response(resp: HttpResponse) -> Result<String, AdapterError> {
    if resp.status == 429 {
        return Err(AdapterError::RateLimited { retry_after_ms: retry_after_ms(&resp) });
    }
    if !(200..300).contains(&resp.status) {
        return Err(AdapterError::Http { status: resp.status });
    }
    Ok(resp.body)
}

pub(crate) async fn http_get<T: HttpTransport>(
    transport: &T,
    url: String,
    timeout: Duration,
) -> Result<String, AdapterError> {
    classify_response(transport.get(HttpRequest::get(url, timeout)).await?)
}

pub(crate) fn parse_json(body: &str) -> Result<Value, AdapterError> {
    serde_json::from_str(body).map_err(|e| AdapterError::parse(format!("invalid JSON: {e}")))
}

/// A decimal from a JSON string or number; empty strings and anything unparsable give `None`.
/// Numbers go through their text form, never through `f64` arithmetic.
pub(crate) fn dec(v: &Value) -> Option<Decimal> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() { None } else { Decimal::from_str(s).ok() }
        }
        Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
        _ => None,
    }
}

/// An integer (e.g. a millisecond timestamp) from a JSON string or number.
pub(crate) fn int(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

pub(crate) fn str_field<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key)?.as_str()
}

/// A cached value with the time (ms) it was stored. A cache whose stored time lies in the future
/// (clock moved back) counts as stale.
pub(crate) struct TtlCell<T>(Mutex<Option<(i64, Arc<T>)>>);

impl<T> TtlCell<T> {
    pub(crate) fn new() -> Self {
        TtlCell(Mutex::new(None))
    }
    fn guard(&self) -> std::sync::MutexGuard<'_, Option<(i64, Arc<T>)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn fresh(&self, now_ms: i64, ttl_ms: i64) -> Option<Arc<T>> {
        match &*self.guard() {
            Some((at, v)) if now_ms >= *at && now_ms - *at < ttl_ms => Some(Arc::clone(v)),
            _ => None,
        }
    }
    pub(crate) fn store(&self, now_ms: i64, value: T) -> Arc<T> {
        let v = Arc::new(value);
        *self.guard() = Some((now_ms, Arc::clone(&v)));
        v
    }
    pub(crate) fn invalidate(&self) {
        *self.guard() = None;
    }
}

/// Raw, already unit-normalised fields of one symbol, before status decisions.
pub(crate) struct RawObservation {
    pub exchange: Exchange,
    pub symbol: String,
    pub funding_rate: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub next_funding_time: Option<i64>,
    /// `None` when the response body carries no timestamp (stored as 0 in the observation).
    pub exchange_timestamp: Option<i64>,
    pub volume_24h_quote: Option<Decimal>,
}

/// `next_funding_time - reference` may not exceed the interval plus the tolerance; the reference is
/// the exchange timestamp, or `observed_at` when the body has none.
pub(crate) fn interval_consistent(
    next_funding_time: i64,
    interval_secs: i64,
    exchange_timestamp: Option<i64>,
    observed_at: i64,
) -> bool {
    let reference = exchange_timestamp.filter(|t| *t > 0).unwrap_or(observed_at);
    let limit = interval_secs.saturating_mul(1000).saturating_add(CONSISTENCY_TOLERANCE_MS);
    next_funding_time.saturating_sub(reference) <= limit
}

/// Builds the final observation. `base` is the verdict of the catalog (and any exchange-specific
/// check); only `Listed` is examined further. Returns `true` as the second value when the interval
/// looks outdated, so the caller can invalidate its interval cache.
pub(crate) fn assemble(
    raw: RawObservation,
    base: DataStatus,
    interval_secs: Option<i64>,
    observed_at: i64,
) -> (FundingObservation, bool) {
    let mut status = base;
    let mut interval_outdated = false;
    if status == DataStatus::Listed {
        match (raw.funding_rate, raw.mark_price, raw.next_funding_time, interval_secs.filter(|s| *s > 0)) {
            (Some(_), Some(_), Some(next), Some(interval)) => {
                if !interval_consistent(next, interval, raw.exchange_timestamp, observed_at) {
                    status = DataStatus::DataError;
                    interval_outdated = true;
                }
            }
            _ => status = DataStatus::DataError,
        }
    }
    let obs = FundingObservation::new(
        raw.exchange,
        raw.symbol,
        raw.funding_rate.unwrap_or(Decimal::ZERO),
        interval_secs,
        raw.next_funding_time.unwrap_or(0),
        raw.mark_price.unwrap_or(Decimal::ZERO),
        raw.volume_24h_quote,
        raw.exchange_timestamp.unwrap_or(0),
        observed_at,
        status,
    );
    (obs, interval_outdated)
}

/// Helpers shared by the adapter test modules.
#[cfg(test)]
pub(crate) mod testkit {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::exchange::transport::FakeTransport;
    use crate::ports::ManualClock;

    pub fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    /// Reads a recorded response from `app/tests/fixtures/<rel>`.
    pub fn fixture(rel: &str) -> String {
        let path = format!("{}/tests/fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"))
    }

    pub fn json_fixture(rel: &str) -> Value {
        serde_json::from_str(&fixture(rel)).unwrap_or_else(|e| panic!("fixture {rel} is not JSON: {e}"))
    }

    /// A `FakeTransport` that moves a `ManualClock` forward by `step_ms` before every request except
    /// the first, so tests can give the requests of one re-fetch different receive times.
    pub struct ClockedTransport {
        pub fake: FakeTransport,
        clock: ManualClock,
        step_ms: i64,
        calls: AtomicI64,
    }

    impl ClockedTransport {
        pub fn new(fake: FakeTransport, clock: ManualClock, step_ms: i64) -> Self {
            ClockedTransport { fake, clock, step_ms, calls: AtomicI64::new(0) }
        }
        pub fn requests(&self) -> Vec<HttpRequest> {
            self.fake.requests()
        }
        pub fn count(&self, url_contains: &str) -> usize {
            self.requests().iter().filter(|r| r.url.contains(url_contains)).count()
        }
    }

    impl HttpTransport for ClockedTransport {
        fn get(&self, req: HttpRequest) -> impl Future<Output = Result<HttpResponse, AdapterError>> + Send {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n > 0 {
                self.clock.advance(self.step_ms);
            }
            self.fake.get(req)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn raw() -> RawObservation {
        RawObservation {
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            funding_rate: Some(d("0.0001")),
            mark_price: Some(d("100")),
            next_funding_time: Some(8 * 3_600_000),
            exchange_timestamp: Some(1_000),
            volume_24h_quote: None,
        }
    }

    #[test]
    fn status_429_reads_retry_after_header() {
        let mut r = HttpResponse::with_status(429, "");
        r.headers.push(("Retry-After".into(), "7".into()));
        assert_eq!(classify_response(r), Err(AdapterError::RateLimited { retry_after_ms: Some(7000) }));
    }

    #[test]
    fn status_429_without_or_with_unparsable_header_has_no_retry_after() {
        assert_eq!(
            classify_response(HttpResponse::with_status(429, "")),
            Err(AdapterError::RateLimited { retry_after_ms: None })
        );
        let mut r = HttpResponse::with_status(429, "");
        r.headers.push(("retry-after".into(), "Wed, 21 Oct 2026 07:28:00 GMT".into()));
        assert_eq!(classify_response(r), Err(AdapterError::RateLimited { retry_after_ms: None }));
    }

    #[test]
    fn other_non_2xx_is_http_and_2xx_is_the_body() {
        assert_eq!(classify_response(HttpResponse::with_status(503, "x")), Err(AdapterError::Http { status: 503 }));
        assert_eq!(classify_response(HttpResponse::with_status(400, "x")), Err(AdapterError::Http { status: 400 }));
        assert_eq!(classify_response(HttpResponse::ok("body")), Ok("body".to_string()));
    }

    #[test]
    fn invalid_json_is_parse_error() {
        assert!(matches!(parse_json("<html>"), Err(AdapterError::Parse(_))));
        assert!(parse_json("{\"a\":1}").is_ok());
    }

    #[test]
    fn decimals_parse_from_strings_exactly_and_empty_is_none() {
        assert_eq!(dec(&serde_json::json!("0.1")), Some(d("0.1")));
        assert_eq!(dec(&serde_json::json!("0.0000784623357725")), Some(d("0.0000784623357725")));
        assert_eq!(dec(&serde_json::json!("")), None);
        assert_eq!(dec(&serde_json::json!(null)), None);
        assert_eq!(dec(&serde_json::json!("abc")), None);
        assert_eq!(dec(&serde_json::json!(12)), Some(d("12")));
    }

    #[test]
    fn integers_parse_from_strings_and_numbers() {
        assert_eq!(int(&serde_json::json!("1791216000000")), Some(1_791_216_000_000));
        assert_eq!(int(&serde_json::json!(1791216000000_i64)), Some(1_791_216_000_000));
        assert_eq!(int(&serde_json::json!("")), None);
    }

    #[test]
    fn ttl_cell_expires_invalidates_and_ignores_future_timestamps() {
        let c: TtlCell<i32> = TtlCell::new();
        assert!(c.fresh(0, 100).is_none());
        c.store(1_000, 5);
        assert_eq!(c.fresh(1_099, 100).as_deref(), Some(&5));
        assert!(c.fresh(1_100, 100).is_none(), "expired exactly at ttl");
        assert!(c.fresh(999, 100).is_none(), "clock moved back is stale");
        c.invalidate();
        assert!(c.fresh(1_000, 100).is_none());
    }

    #[test]
    fn consistent_interval_passes_and_outdated_one_fails() {
        // spec: interval 8h, next settlement 7.05h away -> consistent
        let ref_ts = 1_000_000;
        let next = ref_ts + (7.05 * 3_600_000.0) as i64;
        assert!(interval_consistent(next, 8 * 3600, Some(ref_ts), 0));
        // spec: cached 4h interval but 7h to next settlement -> inconsistent
        assert!(!interval_consistent(ref_ts + 7 * 3_600_000, 4 * 3600, Some(ref_ts), 0));
    }

    #[test]
    fn tolerance_boundary_is_60_seconds_inclusive() {
        assert!(interval_consistent(3_600_000 + 60_000, 3600, Some(0), 0));
        assert!(!interval_consistent(3_600_000 + 60_001, 3600, Some(0), 0));
    }

    #[test]
    fn missing_exchange_timestamp_falls_back_to_observed_at() {
        assert!(!interval_consistent(10_000_000, 3600, None, 0));
        assert!(interval_consistent(10_000_000, 3600, None, 9_000_000));
        assert!(interval_consistent(10_000_000, 3600, Some(0), 9_000_000), "0 means missing");
    }

    #[test]
    fn assemble_listed_with_valid_data_stays_listed() {
        let (o, outdated) = assemble(raw(), DataStatus::Listed, Some(28_800), 2_000);
        assert_eq!((o.data_status, outdated), (DataStatus::Listed, false));
        assert_eq!(o.funding_interval_secs, Some(28_800));
        assert_eq!((o.exchange_timestamp, o.observed_at), (1_000, 2_000));
    }

    #[test]
    fn assemble_missing_interval_is_data_error_and_never_guesses_8h() {
        let (o, _) = assemble(raw(), DataStatus::Listed, None, 2_000);
        assert_eq!(o.data_status, DataStatus::DataError);
        assert_eq!(o.funding_interval_secs, None);
    }

    #[test]
    fn assemble_missing_price_or_rate_is_data_error() {
        let mut r = raw();
        r.mark_price = None;
        assert_eq!(assemble(r, DataStatus::Listed, Some(28_800), 2_000).0.data_status, DataStatus::DataError);
        let mut r = raw();
        r.funding_rate = None;
        assert_eq!(assemble(r, DataStatus::Listed, Some(28_800), 2_000).0.data_status, DataStatus::DataError);
    }

    #[test]
    fn assemble_outdated_interval_is_data_error_and_flags_refresh() {
        let mut r = raw();
        r.next_funding_time = Some(1_000 + 7 * 3_600_000);
        let (o, outdated) = assemble(r, DataStatus::Listed, Some(4 * 3600), 2_000);
        assert_eq!((o.data_status, outdated), (DataStatus::DataError, true));
    }

    #[test]
    fn assemble_keeps_not_listed_and_data_error_verdicts() {
        assert_eq!(assemble(raw(), DataStatus::NotListed, Some(28_800), 2_000).0.data_status, DataStatus::NotListed);
        assert_eq!(assemble(raw(), DataStatus::DataError, Some(28_800), 2_000).0.data_status, DataStatus::DataError);
    }

    // ----- read-only trait (spec: ExchangeAdapter 是唯讀介面) -----

    /// A test double that implements every method of the trait and nothing else. If the trait ever
    /// gains a method (e.g. an order method), this stops compiling.
    struct Double;
    impl ExchangeAdapter for Double {
        fn exchange(&self) -> Exchange {
            Exchange::Okx
        }
        async fn fetch_snapshot(&self) -> Result<Vec<FundingObservation>, AdapterError> {
            Ok(vec![])
        }
        async fn refetch_symbol(&self, _symbol: &str) -> Result<FundingObservation, AdapterError> {
            Err(AdapterError::NotConnected)
        }
        async fn instrument_rules(&self, _symbol: &str) -> Result<RulesLookup, AdapterError> {
            Ok(RulesLookup::UnknownSymbol)
        }
        async fn listing_status(&self, _symbol: &str) -> Result<ListingStatus, AdapterError> {
            Ok(ListingStatus::NotListed)
        }
    }

    #[test]
    fn double_with_only_the_read_methods_satisfies_the_trait() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let d = Double;
        assert_eq!(d.exchange(), Exchange::Okx);
        assert_eq!(rt.block_on(d.fetch_snapshot()), Ok(vec![]));
        assert_eq!(rt.block_on(d.listing_status("X")), Ok(ListingStatus::NotListed));
    }

    #[test]
    fn trait_source_has_no_order_or_leverage_methods() {
        let src = include_str!("adapter.rs");
        let start = src.find("pub trait ExchangeAdapter").expect("trait present");
        let end = start + src[start..].find("\n}\n").expect("trait end");
        let body = src[start..end].to_lowercase();
        let method_names: Vec<&str> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("fn "))
            .map(|l| l.split('(').next().unwrap_or(""))
            .collect();
        assert_eq!(
            method_names,
            vec!["exchange", "fetch_snapshot", "refetch_symbol", "instrument_rules", "listing_status"]
        );
        for banned in ["order", "cancel", "amend", "leverage", "place", "submit", "post"] {
            assert!(
                !method_names.iter().any(|m| m.contains(banned)),
                "trait must not declare a state-changing method ({banned})"
            );
        }
    }

    #[test]
    fn instrument_rules_expose_the_lot_size_used_by_core() {
        let r = InstrumentRules {
            exchange: Exchange::Bybit,
            symbol: "X".into(),
            step_size: d("0.1"),
            min_qty: d("0.2"),
            max_qty: None,
            market_step_size: None,
            market_min_qty: None,
            market_max_qty: None,
            min_notional: None,
            ct_val: None,
            ct_mult: None,
        };
        assert_eq!(r.lot_size(), LotSize { step_size: d("0.1"), min_qty: d("0.2") });
    }
}

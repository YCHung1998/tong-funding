//! Binance USDT-M futures public market data (spec: exchange-adapter; task 2.1).
//!
//! Endpoints: `premiumIndex` (rate, mark price, next settlement), `ticker/24hr` (quote volume),
//! `exchangeInfo` (listing status and `LOT_SIZE` / `MARKET_LOT_SIZE`), `fundingInfo` (interval).
//! The catalog and the intervals are cached for `META_TTL_MS`; market data never is.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tong_funding_core::funding::{DataStatus, FundingObservation, binance_interval_secs};
use tong_funding_core::types::{Decimal, Exchange};

use super::adapter::{
    BATCH_TIMEOUT, ExchangeAdapter, InstrumentRules, ListingStatus, META_TTL_MS, RawObservation, RulesLookup,
    SINGLE_TIMEOUT, TtlCell, assemble, dec_field, http_get, int, parse_json, percent_encode_value, str_field,
    validate_symbol,
};
use super::endpoints::{
    BINANCE_EXCHANGE_INFO, BINANCE_FUNDING_INFO, BINANCE_PREMIUM_INDEX, BINANCE_TICKER_24H, binance_url,
};
use super::refetch::earliest_observed_at;
use crate::exchange::error::AdapterError;
use crate::exchange::transport::HttpTransport;
use crate::ports::Clock;

/// One `exchangeInfo.symbols[]` entry, reduced to what the adapter needs.
struct Instrument {
    /// `status = TRADING`, `contractType = PERPETUAL`, `quoteAsset = USDT`.
    tradable_usdt_perpetual: bool,
    rules: Result<InstrumentRules, String>,
}

struct Catalog(HashMap<String, Instrument>);
struct Intervals(HashMap<String, i64>);

pub struct BinanceAdapter<T: HttpTransport> {
    transport: Arc<T>,
    clock: Arc<dyn Clock>,
    catalog: TtlCell<Catalog>,
    intervals: TtlCell<Intervals>,
}

/// Binance reports failures as `{code, msg}`, sometimes with HTTP 200.
fn parse_binance_body(body: &str) -> Result<Value, AdapterError> {
    let v = parse_json(body)?;
    if let Some(obj) = v.as_object() {
        if obj.contains_key("code") && obj.contains_key("msg") {
            let code = obj.get("code").map(|c| c.to_string()).unwrap_or_default();
            let msg = obj.get("msg").and_then(Value::as_str).unwrap_or("");
            return Err(AdapterError::exchange(code, msg));
        }
    }
    Ok(v)
}

/// Outer error: a field is a JSON number instead of a string (`Parse`). Inner error: a required
/// field is missing, so the rules are unavailable.
fn parse_rules(symbol: &str, filters: &[Value]) -> Result<Result<InstrumentRules, String>, AdapterError> {
    let filter = |name: &str| filters.iter().find(|f| str_field(f, "filterType") == Some(name));
    let Some(lot) = filter("LOT_SIZE") else { return Ok(Err("LOT_SIZE filter missing".into())) };
    let step_size = dec_field(lot, "stepSize")?.filter(|s| *s > Decimal::ZERO);
    let min_qty = dec_field(lot, "minQty")?;
    let (Some(step_size), Some(min_qty)) = (step_size, min_qty) else {
        return Ok(Err("LOT_SIZE.stepSize or minQty missing".into()));
    };
    let market = filter("MARKET_LOT_SIZE");
    let market_field = |key: &str| -> Result<Option<Decimal>, AdapterError> {
        match market {
            Some(m) => dec_field(m, key),
            None => Ok(None),
        }
    };
    Ok(Ok(InstrumentRules {
        exchange: Exchange::Binance,
        symbol: symbol.to_string(),
        step_size,
        min_qty,
        max_qty: dec_field(lot, "maxQty")?,
        market_step_size: market_field("stepSize")?,
        market_min_qty: market_field("minQty")?,
        market_max_qty: market_field("maxQty")?,
        min_notional: None,
        ct_val: None,
        ct_mult: None,
    }))
}

fn parse_catalog(body: &str) -> Result<Catalog, AdapterError> {
    let v = parse_binance_body(body)?;
    let symbols =
        v.get("symbols").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("exchangeInfo has no symbols array"))?;
    let mut map = HashMap::new();
    for s in symbols {
        let Some(symbol) = str_field(s, "symbol") else { continue };
        let tradable = str_field(s, "status") == Some("TRADING")
            && str_field(s, "contractType") == Some("PERPETUAL")
            && str_field(s, "quoteAsset") == Some("USDT");
        let filters = s.get("filters").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
        map.insert(symbol.to_string(), Instrument { tradable_usdt_perpetual: tradable, rules: parse_rules(symbol, filters)? });
    }
    Ok(Catalog(map))
}

fn parse_intervals(body: &str) -> Result<Intervals, AdapterError> {
    let v = parse_binance_body(body)?;
    let rows = v.as_array().ok_or_else(|| AdapterError::parse("fundingInfo is not an array"))?;
    let mut map = HashMap::new();
    for r in rows {
        let Some(symbol) = str_field(r, "symbol") else { continue };
        if let Some(secs) = binance_interval_secs(r.get("fundingIntervalHours").and_then(int)) {
            map.insert(symbol.to_string(), secs);
        }
    }
    Ok(Intervals(map))
}

/// One premiumIndex row as raw fields (`None` where the exchange gave nothing usable); a JSON number
/// in a decimal field is a `Parse` error.
fn raw_from_premium(row: &Value, volume: Option<Decimal>) -> Result<Option<RawObservation>, AdapterError> {
    let Some(symbol) = str_field(row, "symbol") else { return Ok(None) };
    Ok(Some(RawObservation {
        exchange: Exchange::Binance,
        symbol: symbol.to_string(),
        funding_rate: dec_field(row, "lastFundingRate")?,
        mark_price: dec_field(row, "markPrice")?,
        next_funding_time: row.get("nextFundingTime").and_then(int),
        exchange_timestamp: row.get("time").and_then(int),
        volume_24h_quote: volume,
    }))
}

impl<T: HttpTransport> BinanceAdapter<T> {
    pub fn new(transport: Arc<T>, clock: Arc<dyn Clock>) -> Self {
        BinanceAdapter { transport, clock, catalog: TtlCell::new(), intervals: TtlCell::new() }
    }

    async fn catalog(&self) -> Result<Arc<Catalog>, AdapterError> {
        if let Some(c) = self.catalog.fresh(self.clock.now_ms(), META_TTL_MS) {
            return Ok(c);
        }
        let body = http_get(&*self.transport, binance_url(BINANCE_EXCHANGE_INFO), BATCH_TIMEOUT).await?;
        let catalog = parse_catalog(&body)?;
        Ok(self.catalog.store(self.clock.now_ms(), catalog))
    }

    async fn intervals(&self) -> Result<Arc<Intervals>, AdapterError> {
        if let Some(i) = self.intervals.fresh(self.clock.now_ms(), META_TTL_MS) {
            return Ok(i);
        }
        let body = http_get(&*self.transport, binance_url(BINANCE_FUNDING_INFO), BATCH_TIMEOUT).await?;
        let intervals = parse_intervals(&body)?;
        Ok(self.intervals.store(self.clock.now_ms(), intervals))
    }

    /// A GET whose receive time is read from the injected clock right after the response arrives.
    async fn timed_get(&self, path_and_query: &str, timeout: Duration) -> (Result<String, AdapterError>, i64) {
        let r = http_get(&*self.transport, binance_url(path_and_query), timeout).await;
        (r, self.clock.now_ms())
    }

    /// Verdict from the catalog for one symbol; a catalog that could not be fetched never lists anything.
    fn base_status(catalog: &Result<Arc<Catalog>, AdapterError>, symbol: &str) -> DataStatus {
        match catalog {
            Err(_) => DataStatus::DataError,
            Ok(c) => match c.0.get(symbol) {
                Some(i) if i.tradable_usdt_perpetual => DataStatus::Listed,
                _ => DataStatus::NotListed,
            },
        }
    }

    fn finish(
        &self,
        raw: RawObservation,
        catalog: &Result<Arc<Catalog>, AdapterError>,
        intervals: &Result<Arc<Intervals>, AdapterError>,
        observed_at: i64,
    ) -> FundingObservation {
        let base = Self::base_status(catalog, &raw.symbol);
        let interval = intervals.as_ref().ok().and_then(|i| i.0.get(&raw.symbol).copied());
        let (obs, outdated) = assemble(raw, base, interval, observed_at);
        if outdated {
            self.intervals.invalidate();
        }
        obs
    }
}

impl<T: HttpTransport> ExchangeAdapter for BinanceAdapter<T> {
    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }

    async fn fetch_snapshot(&self) -> Result<Vec<FundingObservation>, AdapterError> {
        let (premium, ticker, catalog, intervals) = tokio::join!(
            self.timed_get(BINANCE_PREMIUM_INDEX, BATCH_TIMEOUT),
            self.timed_get(BINANCE_TICKER_24H, BATCH_TIMEOUT),
            self.catalog(),
            self.intervals(),
        );
        let (premium, premium_at) = premium;
        let (ticker, ticker_at) = ticker;
        let premium = parse_binance_body(&premium?)?;
        let ticker = parse_binance_body(&ticker?)?;
        let observed_at = earliest_observed_at(&[premium_at, ticker_at]).unwrap_or(premium_at);

        let mut volumes: HashMap<&str, Decimal> = HashMap::new();
        for r in ticker.as_array().ok_or_else(|| AdapterError::parse("ticker/24hr is not an array"))? {
            if let (Some(symbol), Some(volume)) = (str_field(r, "symbol"), dec_field(r, "quoteVolume")?) {
                volumes.insert(symbol, volume);
            }
        }
        let rows = premium.as_array().ok_or_else(|| AdapterError::parse("premiumIndex is not an array"))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(symbol) = str_field(row, "symbol") else { continue };
            let Some(raw) = raw_from_premium(row, volumes.get(symbol).copied())? else { continue };
            out.push(self.finish(raw, &catalog, &intervals, observed_at));
        }
        Ok(out)
    }

    async fn refetch_symbol(&self, symbol: &str) -> Result<FundingObservation, AdapterError> {
        validate_symbol(symbol)?;
        let encoded = percent_encode_value(symbol);
        let premium_q = format!("{BINANCE_PREMIUM_INDEX}?symbol={encoded}");
        let ticker_q = format!("{BINANCE_TICKER_24H}?symbol={encoded}");
        let (premium, ticker, catalog, intervals) = tokio::join!(
            self.timed_get(&premium_q, SINGLE_TIMEOUT),
            self.timed_get(&ticker_q, SINGLE_TIMEOUT),
            self.catalog(),
            self.intervals(),
        );
        let (premium, premium_at) = premium;
        let (ticker, ticker_at) = ticker;
        let premium = parse_binance_body(&premium?)?;
        let ticker = parse_binance_body(&ticker?)?;
        let observed_at = earliest_observed_at(&[premium_at, ticker_at]).unwrap_or(premium_at);
        if str_field(&premium, "symbol") != Some(symbol) {
            return Err(AdapterError::parse("premiumIndex answered for a different symbol than requested"));
        }
        if str_field(&ticker, "symbol").is_some_and(|t| t != symbol) {
            return Err(AdapterError::parse("ticker/24hr answered for a different symbol than requested"));
        }
        let volume = dec_field(&ticker, "quoteVolume")?;
        let raw = raw_from_premium(&premium, volume)?
            .ok_or_else(|| AdapterError::parse("premiumIndex row has no symbol"))?;
        Ok(self.finish(raw, &catalog, &intervals, observed_at))
    }

    async fn instrument_rules(&self, symbol: &str) -> Result<RulesLookup, AdapterError> {
        let catalog = self.catalog().await?;
        Ok(match catalog.0.get(symbol) {
            None => RulesLookup::UnknownSymbol,
            Some(i) => match &i.rules {
                Ok(r) => RulesLookup::Available(r.clone()),
                Err(reason) => RulesLookup::Unavailable { reason: reason.clone() },
            },
        })
    }

    async fn listing_status(&self, symbol: &str) -> Result<ListingStatus, AdapterError> {
        let catalog = self.catalog().await?;
        Ok(match catalog.0.get(symbol) {
            Some(i) if i.tradable_usdt_perpetual => ListingStatus::Listed,
            _ => ListingStatus::NotListed,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;

    use serde_json::Value;
    use tong_funding_core::funding::DataStatus;
    use tong_funding_core::types::Decimal;

    use super::*;
    use crate::exchange::public::adapter::testkit::{ClockedTransport, block_on, fixture, json_fixture};
    use crate::exchange::public::adapter::{BATCH_TIMEOUT, META_TTL_MS, SINGLE_TIMEOUT};
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::ManualClock;

    const NOW: i64 = 1_791_201_300_000;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn ok(rel: &str) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(fixture(rel)))
    }

    fn ok_json(v: &Value) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(v.to_string()))
    }

    /// Recorded fixture with one edit applied (the file on disk is never touched).
    fn mutated(rel: &str, f: impl FnOnce(&mut Value)) -> Value {
        let mut v = json_fixture(rel);
        f(&mut v);
        v
    }

    fn happy_fake() -> FakeTransport {
        FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"))
    }

    fn setup(fake: FakeTransport, step_ms: i64) -> (Arc<ClockedTransport>, BinanceAdapter<ClockedTransport>, ManualClock) {
        let clock = ManualClock::new(NOW);
        let transport = Arc::new(ClockedTransport::new(fake, clock.clone(), step_ms));
        let adapter = BinanceAdapter::new(Arc::clone(&transport), Arc::new(clock.clone()));
        (transport, adapter, clock)
    }

    fn snapshot(fake: FakeTransport) -> Result<Vec<FundingObservation>, AdapterError> {
        let (_, adapter, _) = setup(fake, 0);
        block_on(adapter.fetch_snapshot())
    }

    fn find<'a>(all: &'a [FundingObservation], symbol: &str) -> &'a FundingObservation {
        all.iter().find(|o| o.symbol == symbol).unwrap_or_else(|| panic!("{symbol} missing from snapshot"))
    }

    // ----- observation fields from recorded responses -----

    #[test]
    fn btcusdt_observation_matches_the_recorded_responses() {
        let all = snapshot(happy_fake()).unwrap();
        let o = find(&all, "BTCUSDT");
        assert_eq!(o.exchange, Exchange::Binance);
        assert_eq!(o.data_status, DataStatus::Listed);
        assert_eq!(o.funding_rate, d("0.00003791"));
        assert_eq!(o.mark_price, d("86007.77329710"));
        assert_eq!(o.next_funding_time, 1_791_216_000_000);
        assert_eq!(o.exchange_timestamp, 1_791_201_208_000);
        assert_eq!(o.observed_at, NOW);
        assert_eq!(o.volume_24h_quote, Some(d("10768884555.97")));
        assert_eq!(o.funding_interval_secs, Some(28_800));
    }

    #[test]
    fn intervals_come_from_funding_info_hours() {
        let all = snapshot(happy_fake()).unwrap();
        assert_eq!(find(&all, "GTCUSDT").funding_interval_secs, Some(28_800));
        assert_eq!(find(&all, "LPTUSDT").funding_interval_secs, Some(14_400));
        assert_eq!(find(&all, "ARKUSDT").funding_interval_secs, Some(3_600));
        assert_eq!(find(&all, "LPTUSDT").data_status, DataStatus::Listed);
    }

    #[test]
    fn only_trading_usdt_perpetuals_are_listed() {
        let all = snapshot(happy_fake()).unwrap();
        // SETTLING, PENDING_TRADING, delivery contract, TradFi perpetual and a USDC-margined perpetual.
        for s in ["OMGUSDT", "GAIBUSDT", "BTCUSDT_261225", "XAUUSDT", "BTCUSDC"] {
            assert_eq!(find(&all, s).data_status, DataStatus::NotListed, "{s}");
        }
        let listed: Vec<&str> = all.iter().filter(|o| o.data_status == DataStatus::Listed).map(|o| o.symbol.as_str()).collect();
        assert_eq!(listed.len(), 5, "{listed:?}");
    }

    #[test]
    fn symbol_in_market_data_but_absent_from_the_complete_catalog_is_not_listed() {
        let ex = mutated("binance/exchangeInfo.json", |v| {
            v["symbols"].as_array_mut().unwrap().retain(|s| s["symbol"] != "ETHUSDT");
        });
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok_json(&ex))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let all = snapshot(fake).unwrap();
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::NotListed);
    }

    #[test]
    fn missing_volume_row_leaves_volume_empty() {
        let tk = mutated("binance/ticker24hr.json", |v| {
            v.as_array_mut().unwrap().retain(|r| r["symbol"] != "GTCUSDT");
        });
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok_json(&tk))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let all = snapshot(fake).unwrap();
        assert_eq!(find(&all, "GTCUSDT").volume_24h_quote, None);
        assert_eq!(find(&all, "GTCUSDT").data_status, DataStatus::Listed);
    }

    // ----- interval failures: DATA_ERROR, never an 8h guess -----

    fn fake_with_funding_info(v: &Value) -> FakeTransport {
        FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok_json(v))
    }

    #[test]
    fn trading_symbol_missing_from_funding_info_is_data_error_without_a_guessed_interval() {
        let fi = mutated("binance/fundingInfo.json", |v| {
            v.as_array_mut().unwrap().retain(|r| r["symbol"] != "LPTUSDT");
        });
        let all = snapshot(fake_with_funding_info(&fi)).unwrap();
        let o = find(&all, "LPTUSDT");
        assert_eq!(o.data_status, DataStatus::DataError);
        assert_eq!(o.funding_interval_secs, None);
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::Listed, "other symbols are unaffected");
    }

    #[test]
    fn outdated_cached_interval_is_data_error_and_triggers_an_interval_refetch() {
        // BTCUSDT settles ~4.1 h after the response time; a 4 h interval cannot be right.
        let fi = mutated("binance/fundingInfo.json", |v| {
            for r in v.as_array_mut().unwrap() {
                if r["symbol"] == "BTCUSDT" {
                    r["fundingIntervalHours"] = Value::from(4);
                }
            }
        });
        let (t, adapter, _) = setup(fake_with_funding_info(&fi), 0);
        let all = block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::DataError);
        assert_eq!(t.count("/fapi/v1/fundingInfo"), 1);
        let _ = block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count("/fapi/v1/fundingInfo"), 2, "interval data is re-fetched after an inconsistency");
        assert_eq!(t.count("/fapi/v1/exchangeInfo"), 1, "the catalog is still cached");
    }

    #[test]
    fn consistent_interval_does_not_trigger_a_refetch() {
        let (t, adapter, _) = setup(happy_fake(), 0);
        block_on(adapter.fetch_snapshot()).unwrap();
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count("/fapi/v1/fundingInfo"), 1);
    }

    #[test]
    fn funding_info_failure_makes_every_listed_candidate_a_data_error() {
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", Err(AdapterError::Timeout));
        let all = snapshot(fake).unwrap();
        for s in ["BTCUSDT", "ETHUSDT", "GTCUSDT", "LPTUSDT", "ARKUSDT"] {
            let o = find(&all, s);
            assert_eq!((o.data_status, o.funding_interval_secs), (DataStatus::DataError, None), "{s}");
        }
        assert_eq!(find(&all, "OMGUSDT").data_status, DataStatus::NotListed);
    }

    // ----- catalog failure never lists everything -----

    #[test]
    fn catalog_failure_makes_every_symbol_data_error_not_listed() {
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", Err(AdapterError::Http { status: 503 }))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let all = snapshot(fake).unwrap();
        assert!(!all.is_empty());
        assert!(all.iter().all(|o| o.data_status == DataStatus::DataError));
    }

    // ----- error mapping -----

    #[test]
    fn market_request_failures_are_returned_as_adapter_errors() {
        let timeout = FakeTransport::new().on("/fapi/v1/premiumIndex", Err(AdapterError::Timeout));
        assert_eq!(snapshot(timeout), Err(AdapterError::Timeout));

        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", {
                let mut r = HttpResponse::with_status(429, "{}");
                r.headers.push(("Retry-After".into(), "3".into()));
                Ok(r)
            })
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        assert_eq!(snapshot(fake), Err(AdapterError::RateLimited { retry_after_ms: Some(3000) }));

        let http = FakeTransport::new().on("/fapi/v1/premiumIndex", Ok(HttpResponse::with_status(502, "bad gateway")));
        assert_eq!(snapshot(http), Err(AdapterError::Http { status: 502 }));
    }

    #[test]
    fn garbage_body_is_a_parse_error() {
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", Ok(HttpResponse::ok("<html>oops</html>")))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        assert!(matches!(snapshot(fake), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn code_and_msg_body_with_http_200_is_an_exchange_error() {
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", Ok(HttpResponse::ok(r#"{"code":-1003,"msg":"Too many requests; apiKey=SECRETKEY123"}"#)))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let err = snapshot(fake).unwrap_err();
        assert!(matches!(&err, AdapterError::Exchange { code, .. } if code == "-1003"), "{err:?}");
        assert!(!err.to_string().contains("SECRETKEY123"));
    }

    #[test]
    fn requests_are_unsigned_gets_with_the_batch_timeout() {
        let (t, adapter, _) = setup(happy_fake(), 0);
        block_on(adapter.fetch_snapshot()).unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 4);
        for r in &reqs {
            assert!(r.headers.is_empty(), "public client sends no key header: {r:?}");
            assert!(!r.url.contains("signature") && !r.url.contains("timestamp="));
            assert_eq!(r.timeout, BATCH_TIMEOUT);
        }
    }

    // ----- caching of metadata only -----

    #[test]
    fn batch_refresh_always_sends_fresh_market_requests_but_reuses_metadata_until_the_ttl() {
        let (t, adapter, clock) = setup(happy_fake(), 0);
        block_on(adapter.fetch_snapshot()).unwrap();
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count("/fapi/v1/premiumIndex"), 2);
        assert_eq!(t.count("/fapi/v1/ticker/24hr"), 2);
        assert_eq!(t.count("/fapi/v1/exchangeInfo"), 1);
        assert_eq!(t.count("/fapi/v1/fundingInfo"), 1);
        clock.advance(META_TTL_MS + 1);
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count("/fapi/v1/exchangeInfo"), 2);
        assert_eq!(t.count("/fapi/v1/fundingInfo"), 2);
    }

    // ----- instrument rules and listing -----

    #[test]
    fn both_lot_size_filters_are_kept_apart() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        let RulesLookup::Available(r) = block_on(adapter.instrument_rules("GTCUSDT")).unwrap() else {
            panic!("rules should be available")
        };
        assert_eq!((r.step_size, r.min_qty, r.max_qty), (d("0.1"), d("0.1"), Some(d("6000000"))));
        assert_eq!((r.market_step_size, r.market_min_qty, r.market_max_qty), (Some(d("0.1")), Some(d("0.1")), Some(d("600000"))));
        assert_eq!(r.ct_val, None);
        assert_eq!(r.lot_size().step_size, d("0.1"));
    }

    #[test]
    fn btcusdt_rules_use_lot_size() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        let RulesLookup::Available(r) = block_on(adapter.instrument_rules("BTCUSDT")).unwrap() else {
            panic!("rules should be available")
        };
        assert_eq!((r.step_size, r.min_qty, r.max_qty), (d("0.001"), d("0.001"), Some(d("1000"))));
    }

    #[test]
    fn missing_lot_size_field_means_rules_are_unavailable_not_a_default_step() {
        let ex = mutated("binance/exchangeInfo.json", |v| {
            for s in v["symbols"].as_array_mut().unwrap() {
                if s["symbol"] == "BTCUSDT" {
                    for f in s["filters"].as_array_mut().unwrap() {
                        if f["filterType"] == "LOT_SIZE" {
                            f.as_object_mut().unwrap().remove("stepSize");
                        }
                    }
                }
            }
        });
        let fake = FakeTransport::new().on("/fapi/v1/exchangeInfo", ok_json(&ex));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")).unwrap(), RulesLookup::Unavailable { .. }));
        assert_eq!(block_on(adapter.instrument_rules("NOSUCHUSDT")).unwrap(), RulesLookup::UnknownSymbol);
    }

    #[test]
    fn rules_lookup_fails_when_the_catalog_cannot_be_fetched() {
        let fake = FakeTransport::new().on("/fapi/v1/exchangeInfo", Err(AdapterError::Timeout));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.instrument_rules("BTCUSDT")), Err(AdapterError::Timeout));
        assert_eq!(block_on(adapter.listing_status("BTCUSDT")), Err(AdapterError::Timeout));
    }

    #[test]
    fn listing_status_follows_the_catalog() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        assert_eq!(block_on(adapter.listing_status("BTCUSDT")), Ok(ListingStatus::Listed));
        assert_eq!(block_on(adapter.listing_status("OMGUSDT")), Ok(ListingStatus::NotListed));
        assert_eq!(block_on(adapter.listing_status("NOSUCHUSDT")), Ok(ListingStatus::NotListed));
    }

    // ----- single-symbol refetch -----

    fn refetch_fake() -> FakeTransport {
        FakeTransport::new()
            .on("premiumIndex?symbol=BTCUSDT", ok("binance/premiumIndex_single_btcusdt.json"))
            .on("ticker/24hr?symbol=BTCUSDT", ok("binance/ticker24hr_single_btcusdt.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"))
    }

    #[test]
    fn refetch_builds_one_symbol_from_fresh_responses() {
        let (t, adapter, _) = setup(refetch_fake(), 0);
        let o = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(o.data_status, DataStatus::Listed);
        assert_eq!(o.mark_price, d("86005.07230435"));
        assert_eq!(o.exchange_timestamp, 1_791_201_214_000);
        assert_eq!(o.volume_24h_quote, Some(d("10772666718.94")));
        assert_eq!(o.funding_interval_secs, Some(28_800));
        assert_eq!(o.observed_at, NOW);
        for r in t.requests().iter().filter(|r| r.url.contains("symbol=BTCUSDT")) {
            assert_eq!(r.timeout, SINGLE_TIMEOUT);
            assert_eq!(r.timeout, Duration::from_secs(2));
        }
    }

    #[test]
    fn refetch_sends_new_requests_every_time_even_after_a_batch_snapshot() {
        let fake = refetch_fake()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"));
        let (t, adapter, clock) = setup(fake, 0);
        let batch = block_on(adapter.fetch_snapshot()).unwrap();
        let cached_observed_at = find(&batch, "BTCUSDT").observed_at;
        clock.advance(8_000);
        let first = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        clock.advance(8_000);
        let second = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(t.count("premiumIndex?symbol=BTCUSDT"), 2);
        assert_eq!(t.count("ticker/24hr?symbol=BTCUSDT"), 2);
        assert!(first.observed_at > cached_observed_at);
        assert!(second.observed_at > first.observed_at);
    }

    #[test]
    fn refetch_observed_at_is_the_earliest_response_time() {
        let (_, adapter, _) = setup(refetch_fake(), 300);
        let o = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(o.observed_at, NOW, "first response at T, second at T+300");
    }

    #[test]
    fn refetch_errors_are_returned_not_swallowed() {
        let fake = FakeTransport::new()
            .on("premiumIndex?symbol=BTCUSDT", Ok(HttpResponse::with_status(400, r#"{"code":-1121,"msg":"Invalid symbol."}"#)))
            .on("ticker/24hr?symbol=BTCUSDT", ok("binance/ticker24hr_single_btcusdt.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::Http { status: 400 }));

        let fake = FakeTransport::new()
            .on("premiumIndex?symbol=BTCUSDT", Err(AdapterError::Timeout))
            .on("ticker/24hr?symbol=BTCUSDT", ok("binance/ticker24hr_single_btcusdt.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::Timeout));
    }

    #[test]
    fn refetch_of_a_symbol_the_catalog_does_not_list_is_not_listed() {
        let omg = serde_json::json!({"symbol":"OMGUSDT","markPrice":"0.15445273","lastFundingRate":"0.00000000","nextFundingTime":1791216000000_i64,"time":1791201208000_i64});
        let fake = FakeTransport::new()
            .on("premiumIndex?symbol=OMGUSDT", ok_json(&omg))
            .on("ticker/24hr?symbol=OMGUSDT", Ok(HttpResponse::ok(r#"{"symbol":"OMGUSDT","quoteVolume":"1"}"#)))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let (_, adapter, _) = setup(fake, 0);
        let o = block_on(adapter.refetch_symbol("OMGUSDT")).unwrap();
        assert_eq!(o.data_status, DataStatus::NotListed);
    }

    // ----- round 2 -----

    #[test]
    fn status_418_is_rate_limited_with_its_retry_after() {
        let mut banned = HttpResponse::with_status(418, "{}");
        banned.headers.push(("Retry-After".into(), "60".into()));
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", Ok(banned))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        assert_eq!(snapshot(fake), Err(AdapterError::RateLimited { retry_after_ms: Some(60_000) }));
    }

    #[test]
    fn symbol_with_url_metacharacters_is_rejected_before_any_request() {
        let (t, adapter, _) = setup(refetch_fake(), 0);
        for bad in ["BTCUSDT&x=1", "BTCUSDT#@evil.com", "", "a b"] {
            assert!(matches!(block_on(adapter.refetch_symbol(bad)), Err(AdapterError::Parse(_))), "{bad:?}");
        }
        assert!(t.requests().is_empty(), "no request may be sent for an invalid symbol");
    }

    #[test]
    fn refetch_response_for_another_symbol_is_a_parse_error() {
        let fake = FakeTransport::new()
            .on("premiumIndex?symbol=ETHUSDT", ok("binance/premiumIndex_single_btcusdt.json"))
            .on("ticker/24hr?symbol=ETHUSDT", ok("binance/ticker24hr_single_btcusdt.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.refetch_symbol("ETHUSDT")), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn json_number_where_a_decimal_string_is_expected_is_a_parse_error() {
        let prem = mutated("binance/premiumIndex.json", |v| {
            v.as_array_mut().unwrap()[0]["markPrice"] = serde_json::json!(86007.5);
        });
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok_json(&prem))
            .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        assert!(matches!(snapshot(fake), Err(AdapterError::Parse(_))));
        let tk = mutated("binance/ticker24hr.json", |v| v.as_array_mut().unwrap()[0]["quoteVolume"] = serde_json::json!(1e-4));
        let fake = FakeTransport::new()
            .on("/fapi/v1/premiumIndex", ok("binance/premiumIndex.json"))
            .on("/fapi/v1/ticker/24hr", ok_json(&tk))
            .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
            .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
        assert!(matches!(snapshot(fake), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn zero_or_past_next_funding_time_on_a_trading_symbol_is_data_error() {
        for next in [0_i64, 1_791_201_208_000 - 3_600_000] {
            let prem = mutated("binance/premiumIndex.json", |v| {
                for r in v.as_array_mut().unwrap() {
                    if r["symbol"] == "BTCUSDT" {
                        r["nextFundingTime"] = Value::from(next);
                    }
                }
            });
            let fake = FakeTransport::new()
                .on("/fapi/v1/premiumIndex", ok_json(&prem))
                .on("/fapi/v1/ticker/24hr", ok("binance/ticker24hr.json"))
                .on("/fapi/v1/exchangeInfo", ok("binance/exchangeInfo.json"))
                .on("/fapi/v1/fundingInfo", ok("binance/fundingInfo.json"));
            let all = snapshot(fake).unwrap();
            assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::DataError, "next = {next}");
            assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::Listed);
        }
    }

    #[test]
    fn real_recorded_responses_are_not_rejected_by_the_string_only_rule() {
        assert!(snapshot(happy_fake()).is_ok());
        let (_, adapter, _) = setup(refetch_fake(), 0);
        assert!(block_on(adapter.refetch_symbol("BTCUSDT")).is_ok());
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")), Ok(RulesLookup::Available(_))));
    }

    #[test]
    fn binance_adapter_reports_its_exchange() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        assert_eq!(adapter.exchange(), Exchange::Binance);
    }
}

//! Bybit v5 linear public market data (spec: exchange-adapter; task 2.2).
//!
//! `tickers` carries rate, mark price, next settlement, turnover and (usually) `fundingIntervalHour`;
//! `instruments-info` carries listing status, `fundingInterval` (minutes) and `lotSizeFilter`. The
//! catalog is paged (an unpaged request silently returns only 500 of ~890 rows), so it is always
//! requested with the largest `limit` and followed until the cursor is empty. A catalog that could not
//! be fetched completely is never cached and never read as "symbol does not exist".
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tong_funding_core::funding::{DataStatus, FundingObservation, bybit_interval_secs};
use tong_funding_core::trade_cost::TopOfBook;
use tong_funding_core::types::{Decimal, Exchange};

use super::adapter::{
    BATCH_TIMEOUT, Books, ExchangeAdapter, InstrumentRules, ListingStatus, MAX_PAGES, MarketSnapshot, META_TTL_MS,
    RawObservation, RulesLookup, SINGLE_TIMEOUT, TtlCell, assemble, classify_exchange_body, dec_field, http_get, int, parse_json,
    percent_encode_value, str_field, top_of_book, validate_symbol,
};
use super::endpoints::{BYBIT_INSTRUMENTS_INFO, BYBIT_TICKERS, bybit_url};
use super::refetch::earliest_observed_at;
use crate::exchange::error::AdapterError;
use crate::exchange::transport::HttpTransport;
use crate::ports::Clock;

/// Largest `limit` the instruments endpoint accepts (verified 2026-10-05: `limit=1000` returned all 890 rows).
const INSTRUMENTS_LIMIT: u32 = 1000;

struct Instrument {
    /// `status = Trading`, `contractType = LinearPerpetual`, `quoteCoin = USDT`.
    tradable_usdt_perpetual: bool,
    /// Dated futures (`fundingInterval = 0`); never treated as a perpetual.
    linear_futures: bool,
    interval_secs: Option<i64>,
    rules: Result<InstrumentRules, String>,
}

/// Result of paging through `instruments-info`; `incomplete` is set when paging stopped early.
struct PagedCatalog {
    entries: HashMap<String, Instrument>,
    incomplete: Option<AdapterError>,
}

pub struct BybitAdapter<T: HttpTransport> {
    transport: Arc<T>,
    clock: Arc<dyn Clock>,
    catalog: TtlCell<PagedCatalog>,
}

/// HTTP 200 with `retCode != 0` is a failure.
fn parse_bybit_body(body: &str) -> Result<Value, AdapterError> {
    if let Some(limited) = classify_exchange_body(Exchange::Bybit, body) {
        return Err(limited);
    }
    let v = parse_json(body)?;
    let code = v.get("retCode").and_then(int).ok_or_else(|| AdapterError::parse("response has no retCode"))?;
    if code != 0 {
        let msg = v.get("retMsg").and_then(Value::as_str).unwrap_or("");
        return Err(AdapterError::exchange(code.to_string(), msg));
    }
    Ok(v)
}

/// Outer error: a field is a JSON number instead of a string (`Parse`). Inner error: a required
/// field is missing, so the rules are unavailable.
fn parse_rules(symbol: &str, row: &Value) -> Result<Result<InstrumentRules, String>, AdapterError> {
    let Some(lot) = row.get("lotSizeFilter") else { return Ok(Err("lotSizeFilter missing".into())) };
    let step_size = dec_field(lot, "qtyStep")?.filter(|s| *s > Decimal::ZERO);
    let min_qty = dec_field(lot, "minOrderQty")?;
    let (Some(step_size), Some(min_qty)) = (step_size, min_qty) else {
        return Ok(Err("lotSizeFilter.qtyStep or minOrderQty missing".into()));
    };
    Ok(Ok(InstrumentRules {
        exchange: Exchange::Bybit,
        symbol: symbol.to_string(),
        step_size,
        min_qty,
        max_qty: dec_field(lot, "maxOrderQty")?,
        market_step_size: None,
        market_min_qty: None,
        market_max_qty: dec_field(lot, "maxMktOrderQty")?,
        min_notional: dec_field(lot, "minNotionalValue")?,
        ct_val: None,
        ct_mult: None,
    }))
}

fn parse_instrument(symbol: &str, row: &Value) -> Result<Instrument, AdapterError> {
    let contract_type = str_field(row, "contractType");
    Ok(Instrument {
        tradable_usdt_perpetual: str_field(row, "status") == Some("Trading")
            && contract_type == Some("LinearPerpetual")
            && str_field(row, "quoteCoin") == Some("USDT"),
        linear_futures: contract_type == Some("LinearFutures"),
        interval_secs: bybit_interval_secs(row.get("fundingInterval").and_then(int)),
        rules: parse_rules(symbol, row)?,
    })
}

/// One page: its instruments and the next cursor (`None` when empty or absent).
fn parse_instruments_page(body: &str) -> Result<(Vec<(String, Instrument)>, Option<String>), AdapterError> {
    let v = parse_bybit_body(body)?;
    let result = v.get("result").ok_or_else(|| AdapterError::parse("instruments-info has no result"))?;
    let list = result.get("list").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("instruments-info has no list"))?;
    let mut rows = Vec::with_capacity(list.len());
    for r in list {
        if let Some(symbol) = str_field(r, "symbol") {
            rows.push((symbol.to_string(), parse_instrument(symbol, r)?));
        }
    }
    let cursor = result.get("nextPageCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
    Ok((rows, cursor))
}

/// Best bid / ask of one `tickers` row (`bid1Price`, `bid1Size`, `ask1Price`, `ask1Size`, base
/// coin). An empty or missing field gives `None` (no quote); a JSON number is a `Parse` error.
fn book_of(row: &Value, observed_at: i64) -> Result<Option<TopOfBook>, AdapterError> {
    Ok(top_of_book(
        dec_field(row, "bid1Price")?,
        dec_field(row, "bid1Size")?,
        dec_field(row, "ask1Price")?,
        dec_field(row, "ask1Size")?,
        observed_at,
    ))
}

impl<T: HttpTransport> BybitAdapter<T> {
    pub fn new(transport: Arc<T>, clock: Arc<dyn Clock>) -> Self {
        BybitAdapter { transport, clock, catalog: TtlCell::new() }
    }

    /// Pages through `instruments-info`. Failure of the FIRST page is returned as that error (no data);
    /// any later failure, a repeated cursor, or exceeding `MAX_PAGES` returns the pages so far with
    /// `incomplete = Some(Incomplete)`.
    async fn fetch_catalog(&self) -> Result<PagedCatalog, AdapterError> {
        let mut entries = HashMap::new();
        let mut seen_cursors: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        for page in 0..MAX_PAGES {
            let mut url = format!("{BYBIT_INSTRUMENTS_INFO}?category=linear&limit={INSTRUMENTS_LIMIT}");
            if let Some(c) = &cursor {
                url.push_str("&cursor=");
                url.push_str(&percent_encode_value(c));
            }
            let fetched = match http_get(&*self.transport, bybit_url(&url), BATCH_TIMEOUT).await {
                Ok(body) => parse_instruments_page(&body),
                Err(e) => Err(e),
            };
            let (rows, next) = match fetched {
                Ok(p) => p,
                Err(e) if page == 0 => return Err(e),
                Err(e) => {
                    let reason = format!("instruments-info page {} failed: {e}", page + 1);
                    return Ok(PagedCatalog { entries, incomplete: Some(AdapterError::incomplete(reason)) });
                }
            };
            entries.extend(rows);
            match next {
                None => return Ok(PagedCatalog { entries, incomplete: None }),
                Some(c) => {
                    if !seen_cursors.insert(c.clone()) {
                        let reason = format!("instruments-info repeated cursor after page {}", page + 1);
                        return Ok(PagedCatalog { entries, incomplete: Some(AdapterError::incomplete(reason)) });
                    }
                    cursor = Some(c);
                }
            }
        }
        let reason = format!("instruments-info still has a cursor after {MAX_PAGES} pages");
        Ok(PagedCatalog { entries, incomplete: Some(AdapterError::incomplete(reason)) })
    }

    /// Cached when complete (for `META_TTL_MS`); an incomplete catalog is returned but not cached.
    async fn catalog(&self) -> Result<Arc<PagedCatalog>, AdapterError> {
        if let Some(c) = self.catalog.fresh(self.clock.now_ms(), META_TTL_MS) {
            return Ok(c);
        }
        let paged = self.fetch_catalog().await?;
        if paged.incomplete.is_some() {
            return Ok(Arc::new(paged));
        }
        Ok(self.catalog.store(self.clock.now_ms(), paged))
    }

    async fn timed_get(&self, path_and_query: &str, timeout: Duration) -> (Result<String, AdapterError>, i64) {
        let r = http_get(&*self.transport, bybit_url(path_and_query), timeout).await;
        (r, self.clock.now_ms())
    }

    async fn snapshot(&self) -> Result<(Vec<FundingObservation>, Books), AdapterError> {
        let tickers_q = format!("{BYBIT_TICKERS}?category=linear");
        let ((tickers, tickers_at), catalog) = tokio::join!(self.timed_get(&tickers_q, BATCH_TIMEOUT), self.catalog());
        let body = parse_bybit_body(&tickers?)?;
        let observed_at = earliest_observed_at(&[tickers_at]).unwrap_or(tickers_at);
        let time = body.get("time").and_then(int);
        let rows = body
            .get("result")
            .and_then(|r| r.get("list"))
            .and_then(Value::as_array)
            .ok_or_else(|| AdapterError::parse("tickers has no result.list"))?;
        let mut out = Vec::with_capacity(rows.len());
        let mut books = Books::new();
        for row in rows {
            if let Some(obs) = self.observation(row, time, &catalog, observed_at, false)? {
                if let Some(book) = book_of(row, observed_at)? {
                    books.insert(obs.symbol.clone(), book);
                }
                out.push(obs);
            }
        }
        Ok((out, books))
    }

    fn incomplete_error(c: &PagedCatalog) -> AdapterError {
        c.incomplete.clone().unwrap_or_else(|| AdapterError::incomplete("catalog incomplete"))
    }

    /// Builds the observation of one ticker row. `explicit_request` marks a single-symbol re-fetch,
    /// where a dated future yields `DataError` instead of `NotListed`.
    fn observation(
        &self,
        row: &Value,
        response_time: Option<i64>,
        catalog: &Result<Arc<PagedCatalog>, AdapterError>,
        observed_at: i64,
        explicit_request: bool,
    ) -> Result<Option<FundingObservation>, AdapterError> {
        let Some(symbol) = str_field(row, "symbol").map(str::to_string) else { return Ok(None) };
        let (mut base, interval) = match catalog {
            Err(_) => (DataStatus::DataError, None),
            Ok(c) => match c.entries.get(&symbol) {
                Some(i) if explicit_request && i.linear_futures => (DataStatus::DataError, None),
                Some(i) if i.tradable_usdt_perpetual => (DataStatus::Listed, i.interval_secs),
                Some(_) => (DataStatus::NotListed, None),
                None if c.incomplete.is_some() => (DataStatus::DataError, None),
                None => (DataStatus::NotListed, None),
            },
        };
        // Cross-check against `tickers.fundingIntervalHour` when the row carries it.
        if base == DataStatus::Listed {
            if let (Some(hours), Some(secs)) = (row.get("fundingIntervalHour").and_then(int), interval) {
                if hours.checked_mul(3600) != Some(secs) {
                    base = DataStatus::DataError;
                }
            }
        }
        let raw = RawObservation {
            exchange: Exchange::Bybit,
            symbol,
            funding_rate: dec_field(row, "fundingRate")?,
            mark_price: dec_field(row, "markPrice")?,
            next_funding_time: row.get("nextFundingTime").and_then(int).filter(|t| *t > 0),
            exchange_timestamp: response_time,
            volume_24h_quote: dec_field(row, "turnover24h")?,
        };
        let (obs, outdated) = assemble(raw, base, interval, observed_at);
        if outdated {
            self.catalog.invalidate();
        }
        Ok(Some(obs))
    }
}

impl<T: HttpTransport> ExchangeAdapter for BybitAdapter<T> {
    fn exchange(&self) -> Exchange {
        Exchange::Bybit
    }

    async fn fetch_snapshot(&self) -> Result<Vec<FundingObservation>, AdapterError> {
        Ok(self.snapshot().await?.0)
    }

    /// Same single tickers request as `fetch_snapshot`: the quotes cost no extra request.
    async fn fetch_market(&self) -> Result<MarketSnapshot, AdapterError> {
        let (observations, books) = self.snapshot().await?;
        Ok(MarketSnapshot { observations, books: Ok(books) })
    }

    async fn refetch_symbol(&self, symbol: &str) -> Result<FundingObservation, AdapterError> {
        validate_symbol(symbol)?;
        let q = format!("{BYBIT_TICKERS}?category=linear&symbol={}", percent_encode_value(symbol));
        let ((tickers, tickers_at), catalog) = tokio::join!(self.timed_get(&q, SINGLE_TIMEOUT), self.catalog());
        let body = parse_bybit_body(&tickers?)?;
        let time = body.get("time").and_then(int);
        let row = body
            .get("result")
            .and_then(|r| r.get("list"))
            .and_then(Value::as_array)
            .and_then(|l| l.iter().find(|r| str_field(r, "symbol") == Some(symbol)))
            .ok_or_else(|| AdapterError::parse(format!("tickers returned no row for {symbol}")))?;
        self.observation(row, time, &catalog, tickers_at, true)?
            .ok_or_else(|| AdapterError::parse("ticker row has no symbol"))
    }

    async fn instrument_rules(&self, symbol: &str) -> Result<RulesLookup, AdapterError> {
        let catalog = self.catalog().await?;
        match catalog.entries.get(symbol) {
            Some(i) => Ok(match &i.rules {
                Ok(r) => RulesLookup::Available(r.clone()),
                Err(reason) => RulesLookup::Unavailable { reason: reason.clone() },
            }),
            None if catalog.incomplete.is_some() => Err(Self::incomplete_error(&catalog)),
            None => Ok(RulesLookup::UnknownSymbol),
        }
    }

    async fn listing_status(&self, symbol: &str) -> Result<ListingStatus, AdapterError> {
        let catalog = self.catalog().await?;
        match catalog.entries.get(symbol) {
            Some(i) if i.tradable_usdt_perpetual => Ok(ListingStatus::Listed),
            Some(_) => Ok(ListingStatus::NotListed),
            None if catalog.incomplete.is_some() => Err(Self::incomplete_error(&catalog)),
            None => Ok(ListingStatus::NotListed),
        }
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
    use crate::exchange::public::adapter::{BATCH_TIMEOUT, MAX_PAGES, META_TTL_MS, SINGLE_TIMEOUT};
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::ManualClock;

    const NOW: i64 = 1_791_201_300_000;
    const INSTRUMENTS: &str = "/v5/market/instruments-info";
    const TICKERS: &str = "/v5/market/tickers";
    const REAL_CURSOR: &str = "first%3D0GUSDT%26last%3DMOCAUSDT";

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn ok(rel: &str) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(fixture(rel)))
    }

    fn ok_json(v: &Value) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(v.to_string()))
    }

    fn mutated(rel: &str, f: impl FnOnce(&mut Value)) -> Value {
        let mut v = json_fixture(rel);
        f(&mut v);
        v
    }

    /// Edits the row of `symbol` in a Bybit `result.list`.
    fn edit_row(v: &mut Value, symbol: &str, f: impl FnOnce(&mut Value)) {
        let row = v["result"]["list"].as_array_mut().unwrap().iter_mut().find(|r| r["symbol"] == symbol).unwrap();
        f(row);
    }

    fn happy_fake() -> FakeTransport {
        FakeTransport::new().on(TICKERS, ok("bybit/tickers.json")).on(INSTRUMENTS, ok("bybit/instruments_complete.json"))
    }

    fn setup(fake: FakeTransport, step_ms: i64) -> (Arc<ClockedTransport>, BybitAdapter<ClockedTransport>, ManualClock) {
        let clock = ManualClock::new(NOW);
        let transport = Arc::new(ClockedTransport::new(fake, clock.clone(), step_ms));
        let adapter = BybitAdapter::new(Arc::clone(&transport), Arc::new(clock.clone()));
        (transport, adapter, clock)
    }

    fn snapshot(fake: FakeTransport) -> Result<Vec<FundingObservation>, AdapterError> {
        let (_, adapter, _) = setup(fake, 0);
        block_on(adapter.fetch_snapshot())
    }

    fn find<'a>(all: &'a [FundingObservation], symbol: &str) -> &'a FundingObservation {
        all.iter().find(|o| o.symbol == symbol).unwrap_or_else(|| panic!("{symbol} missing from snapshot"))
    }

    // ----- observation fields -----

    #[test]
    fn btcusdt_observation_matches_the_recorded_responses() {
        let all = snapshot(happy_fake()).unwrap();
        let o = find(&all, "BTCUSDT");
        assert_eq!(o.exchange, Exchange::Bybit);
        assert_eq!(o.data_status, DataStatus::Listed);
        assert_eq!(o.funding_rate, d("0.00005676"));
        assert_eq!(o.mark_price, d("86008.10"));
        assert_eq!(o.next_funding_time, 1_791_216_000_000);
        assert_eq!(o.volume_24h_quote, Some(d("4147415315.9708")));
        assert_eq!(o.exchange_timestamp, 1_791_201_236_934, "top-level `time` of the tickers response");
        assert_eq!(o.observed_at, NOW);
        assert_eq!(o.funding_interval_secs, Some(28_800));
    }

    #[test]
    fn interval_minutes_are_converted_to_seconds() {
        let all = snapshot(happy_fake()).unwrap();
        assert_eq!(find(&all, "0GUSDT").funding_interval_secs, Some(14_400), "240 minutes");
        assert_eq!(find(&all, "ARKUSDT").funding_interval_secs, Some(3_600), "60 minutes");
        assert_eq!(find(&all, "ETHUSDT").funding_interval_secs, Some(28_800), "480 minutes");
    }

    #[test]
    fn linear_futures_and_non_usdt_perpetuals_are_not_listed() {
        let all = snapshot(happy_fake()).unwrap();
        assert_eq!(find(&all, "BTCUSDT-09OCT26").data_status, DataStatus::NotListed);
        assert_eq!(find(&all, "1000BONKPERP").data_status, DataStatus::NotListed, "USDC-margined");
        assert_eq!(find(&all, "BTCUSDT-09OCT26").funding_interval_secs, None, "no interval is invented for it");
    }

    #[test]
    fn non_trading_status_is_not_listed() {
        let inst = mutated("bybit/instruments_complete.json", |v| {
            edit_row(v, "ETHUSDT", |r| r["status"] = Value::from("Settling"));
        });
        let fake = FakeTransport::new().on(TICKERS, ok("bybit/tickers.json")).on(INSTRUMENTS, ok_json(&inst));
        let all = snapshot(fake).unwrap();
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::NotListed);
    }

    #[test]
    fn perpetual_with_zero_interval_is_data_error() {
        let inst = mutated("bybit/instruments_complete.json", |v| {
            edit_row(v, "ETHUSDT", |r| r["fundingInterval"] = Value::from(0));
        });
        let fake = FakeTransport::new().on(TICKERS, ok("bybit/tickers.json")).on(INSTRUMENTS, ok_json(&inst));
        let all = snapshot(fake).unwrap();
        let o = find(&all, "ETHUSDT");
        assert_eq!((o.data_status, o.funding_interval_secs), (DataStatus::DataError, None));
    }

    #[test]
    fn symbol_in_tickers_but_absent_from_a_complete_catalog_is_not_listed() {
        let inst = mutated("bybit/instruments_complete.json", |v| {
            v["result"]["list"].as_array_mut().unwrap().retain(|r| r["symbol"] != "ETHUSDT");
        });
        let fake = FakeTransport::new().on(TICKERS, ok("bybit/tickers.json")).on(INSTRUMENTS, ok_json(&inst));
        assert_eq!(find(&snapshot(fake).unwrap(), "ETHUSDT").data_status, DataStatus::NotListed);
    }

    // ----- fundingIntervalHour cross-check -----

    fn tickers_with(symbol: &str, f: impl FnOnce(&mut Value)) -> Value {
        mutated("bybit/tickers.json", |v| edit_row(v, symbol, f))
    }

    #[test]
    fn disagreeing_ticker_interval_hour_is_data_error() {
        let t = tickers_with("BTCUSDT", |r| r["fundingIntervalHour"] = Value::from("4"));
        let fake = FakeTransport::new().on(TICKERS, ok_json(&t)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        let all = snapshot(fake).unwrap();
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::DataError);
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::Listed);
    }

    #[test]
    fn ticker_without_interval_hour_skips_the_cross_check() {
        let t = tickers_with("BTCUSDT", |r| {
            r.as_object_mut().unwrap().remove("fundingIntervalHour");
        });
        let fake = FakeTransport::new().on(TICKERS, ok_json(&t)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert_eq!(find(&snapshot(fake).unwrap(), "BTCUSDT").data_status, DataStatus::Listed);
    }

    #[test]
    fn outdated_interval_is_data_error_and_triggers_a_catalog_refetch() {
        // 4 h interval in the catalog, but settlement is ~4.1 h away. Remove the ticker hour so only the
        // consistency check can fire.
        let t = tickers_with("BTCUSDT", |r| {
            r.as_object_mut().unwrap().remove("fundingIntervalHour");
        });
        let inst = mutated("bybit/instruments_complete.json", |v| {
            edit_row(v, "BTCUSDT", |r| r["fundingInterval"] = Value::from(240));
        });
        let fake = FakeTransport::new().on(TICKERS, ok_json(&t)).on(INSTRUMENTS, ok_json(&inst));
        let (tr, adapter, _) = setup(fake, 0);
        let all = block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::DataError);
        assert_eq!(tr.count(INSTRUMENTS), 1);
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(tr.count(INSTRUMENTS), 2, "interval data re-fetched after an inconsistency");
    }

    // ----- pagination -----

    /// Real first page (real cursor) padded with synthetic rows to the real page size of 500.
    fn padded_page1() -> Value {
        mutated("bybit/instruments_page1.json", |v| {
            let list = v["result"]["list"].as_array_mut().unwrap();
            let template = list.iter().find(|r| r["symbol"] == "ETHUSDT").unwrap().clone();
            let mut n = 0;
            while list.len() < 500 {
                let mut row = template.clone();
                row["symbol"] = Value::from(format!("PAD{n:03}USDT"));
                list.push(row);
                n += 1;
            }
        })
    }

    fn page_with_cursor(symbol: &str, cursor: &str) -> Value {
        serde_json::json!({"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[{
            "symbol":symbol,"contractType":"LinearPerpetual","status":"Trading","quoteCoin":"USDT","fundingInterval":480,
            "lotSizeFilter":{"qtyStep":"0.1","minOrderQty":"0.1"}}],"nextPageCursor":cursor},"time":1})
    }

    #[test]
    fn first_page_of_500_with_a_cursor_is_followed_to_the_second_page() {
        let page1 = padded_page1();
        assert_eq!(page1["result"]["list"].as_array().unwrap().len(), 500);
        assert_eq!(page1["result"]["nextPageCursor"], REAL_CURSOR);
        let fake = FakeTransport::new()
            .on(TICKERS, ok("bybit/tickers.json"))
            .on(INSTRUMENTS, ok_json(&page1))
            .on(INSTRUMENTS, ok("bybit/instruments_page2.json"));
        let (t, adapter, _) = setup(fake, 0);
        let all = block_on(adapter.fetch_snapshot()).unwrap();
        // symbols that only exist on page 2 are listed, so both pages were merged
        assert_eq!(find(&all, "ZSUSDT").data_status, DataStatus::Listed);
        assert_eq!(find(&all, "MONUSDT").data_status, DataStatus::Listed);
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::Listed);
        assert!(matches!(block_on(adapter.instrument_rules("PAD492USDT")), Ok(RulesLookup::Available(_))), "all 500 page-1 rows kept");

        let pages: Vec<_> = t.requests().into_iter().filter(|r| r.url.contains(INSTRUMENTS)).collect();
        assert_eq!(pages.len(), 2);
        assert!(pages[0].url.contains("limit=1000") && !pages[0].url.contains("cursor="), "explicit max limit: {}", pages[0].url);
        assert!(pages[1].url.contains("cursor=first%253D0GUSDT%2526last%253DMOCAUSDT") && pages[1].url.contains("limit=1000"), "{}", pages[1].url);
    }

    #[test]
    fn complete_paging_result_is_cached_for_the_ttl() {
        let fake = FakeTransport::new()
            .on(TICKERS, ok("bybit/tickers.json"))
            .on(INSTRUMENTS, ok("bybit/instruments_page1.json"))
            .on(INSTRUMENTS, ok("bybit/instruments_page2.json"));
        let (t, adapter, clock) = setup(fake, 0);
        block_on(adapter.fetch_snapshot()).unwrap();
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count(INSTRUMENTS), 2, "two pages once, then cached");
        assert_eq!(t.count(TICKERS), 2, "tickers are never cached");
        clock.advance(META_TTL_MS + 1);
        // after the ttl the catalog is fetched again; the script repeats its last (page 2) answer for the
        // first request, which still has a valid shape, so only the request count matters here
        block_on(adapter.fetch_snapshot()).unwrap();
        assert!(t.count(INSTRUMENTS) > 2);
    }

    #[test]
    fn second_page_failure_is_incomplete_and_page_one_symbols_survive() {
        let fake = FakeTransport::new()
            .on(INSTRUMENTS, ok("bybit/instruments_page1.json"))
            .on(INSTRUMENTS, Err(AdapterError::Timeout));
        let (_, adapter, _) = setup(fake, 0);
        let paged = block_on(adapter.fetch_catalog()).unwrap();
        assert!(matches!(paged.incomplete, Some(AdapterError::Incomplete(_))), "{:?}", paged.incomplete);
        assert!(paged.entries.contains_key("BTCUSDT") && !paged.entries.contains_key("ZSUSDT"));
    }

    #[test]
    fn symbols_missing_from_an_incomplete_catalog_are_data_error_never_not_listed() {
        // every catalog attempt: page 1 answers, page 2 times out (the fake repeats only its last entry)
        let mut fake = FakeTransport::new().on(TICKERS, ok("bybit/tickers.json"));
        for _ in 0..6 {
            fake = fake.on(INSTRUMENTS, ok("bybit/instruments_page1.json")).on(INSTRUMENTS, Err(AdapterError::Timeout));
        }
        let (t, adapter, _) = setup(fake, 0);
        let all = block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::Listed);
        for s in ["ZSUSDT", "MONUSDT"] {
            assert_eq!(find(&all, s).data_status, DataStatus::DataError, "{s} is on the missing page");
        }
        assert_eq!(find(&all, "BTCUSDT-09OCT26").data_status, DataStatus::NotListed, "known from page 1");
        // lookups never answer "does not exist" from an incomplete catalog
        assert!(matches!(block_on(adapter.listing_status("ZSUSDT")), Err(AdapterError::Incomplete(_))));
        assert!(matches!(block_on(adapter.instrument_rules("ZSUSDT")), Err(AdapterError::Incomplete(_))));
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")), Ok(RulesLookup::Available(_))));
        // an incomplete catalog is not cached: the next snapshot asks again
        let before = t.count(INSTRUMENTS);
        let _ = block_on(adapter.fetch_snapshot()).unwrap();
        assert!(t.count(INSTRUMENTS) > before);
    }

    #[test]
    fn repeated_cursor_aborts_with_incomplete_instead_of_looping() {
        let fake = FakeTransport::new()
            .on(INSTRUMENTS, ok_json(&page_with_cursor("AAAUSDT", "same")))
            .on(INSTRUMENTS, ok_json(&page_with_cursor("BBBUSDT", "same")));
        let (t, adapter, _) = setup(fake, 0);
        let paged = block_on(adapter.fetch_catalog()).unwrap();
        assert!(matches!(paged.incomplete, Some(AdapterError::Incomplete(_))));
        assert_eq!(t.count(INSTRUMENTS), 2, "stopped as soon as the cursor repeated");
    }

    #[test]
    fn page_limit_aborts_with_incomplete() {
        let mut fake = FakeTransport::new();
        for i in 0..(MAX_PAGES + 5) {
            fake = fake.on(INSTRUMENTS, ok_json(&page_with_cursor(&format!("S{i}USDT"), &format!("c{i}"))));
        }
        let (t, adapter, _) = setup(fake, 0);
        let paged = block_on(adapter.fetch_catalog()).unwrap();
        assert!(matches!(paged.incomplete, Some(AdapterError::Incomplete(_))));
        assert_eq!(t.count(INSTRUMENTS), MAX_PAGES);
        assert_eq!(MAX_PAGES, 20);
    }

    #[test]
    fn first_page_failure_is_returned_as_that_error() {
        let fake = FakeTransport::new().on(INSTRUMENTS, Err(AdapterError::Timeout));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.fetch_catalog()), Err(AdapterError::Timeout)));
    }

    #[test]
    fn catalog_failure_makes_every_symbol_data_error() {
        let fake = FakeTransport::new().on(TICKERS, ok("bybit/tickers.json")).on(INSTRUMENTS, Err(AdapterError::Http { status: 503 }));
        let all = snapshot(fake).unwrap();
        assert!(!all.is_empty());
        assert!(all.iter().all(|o| o.data_status == DataStatus::DataError));
    }

    // ----- error mapping -----

    #[test]
    fn nonzero_ret_code_with_http_200_is_an_exchange_error() {
        let fake = FakeTransport::new().on(TICKERS, ok("bybit/error_invalid_symbol.json")).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        let err = snapshot(fake).unwrap_err();
        assert!(matches!(&err, AdapterError::Exchange { code, message } if code == "10001" && message.contains("symbol invalid")), "{err:?}");
    }

    #[test]
    fn ret_code_error_on_the_catalog_also_becomes_data_error() {
        let fake = FakeTransport::new().on(TICKERS, ok("bybit/tickers.json")).on(INSTRUMENTS, ok("bybit/error_invalid_symbol.json"));
        let all = snapshot(fake).unwrap();
        assert!(all.iter().all(|o| o.data_status == DataStatus::DataError));
    }

    #[test]
    fn timeout_rate_limit_http_and_garbage_are_typed() {
        let t = FakeTransport::new().on(TICKERS, Err(AdapterError::Timeout)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert_eq!(snapshot(t), Err(AdapterError::Timeout));

        let mut limited = HttpResponse::with_status(429, "");
        limited.headers.push(("Retry-After".into(), "2".into()));
        let t = FakeTransport::new().on(TICKERS, Ok(limited)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert_eq!(snapshot(t), Err(AdapterError::RateLimited { retry_after_ms: Some(2000) }));

        let t = FakeTransport::new().on(TICKERS, Ok(HttpResponse::with_status(500, "x"))).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert_eq!(snapshot(t), Err(AdapterError::Http { status: 500 }));

        let t = FakeTransport::new().on(TICKERS, Ok(HttpResponse::ok("not json"))).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert!(matches!(snapshot(t), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn requests_are_unsigned_gets_with_the_batch_timeout() {
        let (t, adapter, _) = setup(happy_fake(), 0);
        block_on(adapter.fetch_snapshot()).unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2);
        for r in &reqs {
            assert!(r.headers.is_empty(), "{r:?}");
            assert_eq!(r.timeout, BATCH_TIMEOUT);
            assert!(r.url.contains("category=linear"));
        }
    }

    // ----- rules / listing -----

    #[test]
    fn rules_come_from_lot_size_filter() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        let RulesLookup::Available(r) = block_on(adapter.instrument_rules("BTCUSDT")).unwrap() else { panic!("rules expected") };
        assert_eq!((r.step_size, r.min_qty, r.max_qty), (d("0.001"), d("0.001"), Some(d("1500.000"))));
        assert_eq!(r.market_max_qty, Some(d("150.000")));
        assert_eq!(r.min_notional, Some(d("5")));
        assert_eq!(r.ct_val, None);
    }

    #[test]
    fn missing_qty_step_means_rules_are_unavailable() {
        let inst = mutated("bybit/instruments_complete.json", |v| {
            edit_row(v, "BTCUSDT", |r| {
                r["lotSizeFilter"].as_object_mut().unwrap().remove("qtyStep");
            });
        });
        let fake = FakeTransport::new().on(INSTRUMENTS, ok_json(&inst));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")).unwrap(), RulesLookup::Unavailable { .. }));
        assert_eq!(block_on(adapter.instrument_rules("NOSUCHUSDT")).unwrap(), RulesLookup::UnknownSymbol);
    }

    #[test]
    fn listing_status_follows_the_catalog() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        assert_eq!(block_on(adapter.listing_status("BTCUSDT")), Ok(ListingStatus::Listed));
        assert_eq!(block_on(adapter.listing_status("BTCUSDT-09OCT26")), Ok(ListingStatus::NotListed));
        assert_eq!(block_on(adapter.listing_status("1000BONKPERP")), Ok(ListingStatus::NotListed));
        assert_eq!(block_on(adapter.listing_status("NOSUCHUSDT")), Ok(ListingStatus::NotListed));
    }

    // ----- single-symbol refetch -----

    fn refetch_fake() -> FakeTransport {
        FakeTransport::new()
            .on("tickers?category=linear&symbol=BTCUSDT", ok("bybit/tickers_single_btcusdt.json"))
            .on(INSTRUMENTS, ok("bybit/instruments_complete.json"))
    }

    #[test]
    fn refetch_builds_one_symbol_from_one_fresh_request() {
        let (t, adapter, _) = setup(refetch_fake(), 0);
        let o = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(o.data_status, DataStatus::Listed);
        assert_eq!(o.mark_price, d("86008.14"));
        assert_eq!(o.volume_24h_quote, Some(d("4147441805.1426")));
        assert_eq!(o.exchange_timestamp, 1_791_201_240_419);
        assert_eq!(o.funding_interval_secs, Some(28_800));
        assert_eq!(o.observed_at, NOW);
        let market: Vec<_> = t.requests().into_iter().filter(|r| r.url.contains("symbol=BTCUSDT")).collect();
        assert_eq!(market.len(), 1);
        assert_eq!(market[0].timeout, SINGLE_TIMEOUT);
        assert_eq!(SINGLE_TIMEOUT, Duration::from_secs(2));
    }

    #[test]
    fn refetch_sends_new_requests_every_time_after_a_batch_snapshot() {
        let fake = refetch_fake().on(TICKERS, ok("bybit/tickers.json"));
        let (t, adapter, clock) = setup(fake, 0);
        let batch = block_on(adapter.fetch_snapshot()).unwrap();
        let cached = find(&batch, "BTCUSDT").observed_at;
        clock.advance(8_000);
        let a = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        clock.advance(8_000);
        let b = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(t.count("tickers?category=linear&symbol=BTCUSDT"), 2);
        assert!(a.observed_at > cached && b.observed_at > a.observed_at);
    }

    #[test]
    fn refetch_of_a_linear_future_is_data_error() {
        let batch = json_fixture("bybit/tickers.json");
        let row = batch["result"]["list"].as_array().unwrap().iter().find(|r| r["symbol"] == "BTCUSDT-09OCT26").unwrap().clone();
        let body = serde_json::json!({"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[row]},"time":1_791_201_240_419_i64});
        let fake = FakeTransport::new()
            .on("symbol=BTCUSDT-09OCT26", ok_json(&body))
            .on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        let (_, adapter, _) = setup(fake, 0);
        let o = block_on(adapter.refetch_symbol("BTCUSDT-09OCT26")).unwrap();
        assert_eq!(o.data_status, DataStatus::DataError);
        assert_eq!(o.funding_interval_secs, None);
    }

    #[test]
    fn refetch_errors_are_returned() {
        let fake = FakeTransport::new()
            .on("symbol=BTCUSDT", Err(AdapterError::Timeout))
            .on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::Timeout));

        let fake = FakeTransport::new()
            .on("symbol=NOPEUSDT", ok("bybit/error_invalid_symbol.json"))
            .on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.refetch_symbol("NOPEUSDT")), Err(AdapterError::Exchange { .. })));

        let empty = serde_json::json!({"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[]},"time":1});
        let fake = FakeTransport::new().on("symbol=GONEUSDT", ok_json(&empty)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.refetch_symbol("GONEUSDT")), Err(AdapterError::Parse(_))));
    }

    // ----- round 2 -----

    #[test]
    fn rate_limit_ret_codes_with_http_200_are_rate_limited() {
        for code in [10006, 10018] {
            let body = format!(r#"{{"retCode":{code},"retMsg":"Too many visits"}}"#);
            let fake = FakeTransport::new().on(TICKERS, Ok(HttpResponse::ok(body))).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
            assert_eq!(snapshot(fake), Err(AdapterError::RateLimited { retry_after_ms: None }), "{code}");
        }
        let mut banned = HttpResponse::with_status(418, "");
        banned.headers.push(("Retry-After".into(), "9".into()));
        let fake = FakeTransport::new().on(TICKERS, Ok(banned)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert_eq!(snapshot(fake), Err(AdapterError::RateLimited { retry_after_ms: Some(9_000) }));
    }

    #[test]
    fn rate_limit_code_on_a_catalog_page_is_returned_as_rate_limited() {
        let fake = FakeTransport::new().on(INSTRUMENTS, Ok(HttpResponse::ok(r#"{"retCode":10006,"retMsg":"x"}"#)));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.fetch_catalog()), Err(AdapterError::RateLimited { retry_after_ms: None })));
    }

    #[test]
    fn symbol_with_url_metacharacters_is_rejected_before_any_request() {
        let (t, adapter, _) = setup(refetch_fake(), 0);
        for bad in ["BTCUSDT&category=inverse#@evil.com", "", "a b"] {
            assert!(matches!(block_on(adapter.refetch_symbol(bad)), Err(AdapterError::Parse(_))), "{bad:?}");
        }
        assert!(t.requests().is_empty());
    }

    #[test]
    fn cursor_is_percent_encoded_exactly_like_the_signed_client() {
        // a cursor containing %, = and & must survive one round trip through the query string
        let fake = FakeTransport::new()
            .on(INSTRUMENTS, ok_json(&page_with_cursor("AAAUSDT", "a%3Db&c=d%")))
            .on(INSTRUMENTS, ok_json(&page_with_cursor("BBBUSDT", "")));
        let (t, adapter, _) = setup(fake, 0);
        let paged = block_on(adapter.fetch_catalog()).unwrap();
        assert!(paged.incomplete.is_none());
        let second = t.requests().into_iter().filter(|r| r.url.contains(INSTRUMENTS)).nth(1).unwrap();
        assert!(second.url.ends_with("&cursor=a%253Db%26c%3Dd%25"), "{}", second.url);
    }

    #[test]
    fn json_number_where_a_decimal_string_is_expected_is_a_parse_error() {
        let t = tickers_with("BTCUSDT", |r| r["turnover24h"] = serde_json::json!(4.1e9));
        let fake = FakeTransport::new().on(TICKERS, ok_json(&t)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
        assert!(matches!(snapshot(fake), Err(AdapterError::Parse(_))));
        let inst = mutated("bybit/instruments_complete.json", |v| {
            edit_row(v, "BTCUSDT", |r| r["lotSizeFilter"]["qtyStep"] = serde_json::json!(0.001));
        });
        let fake = FakeTransport::new().on(INSTRUMENTS, ok_json(&inst));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.fetch_catalog()), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn zero_or_past_next_funding_time_on_a_trading_symbol_is_data_error() {
        for next in ["0", "1791197600000"] {
            let t = tickers_with("BTCUSDT", |r| r["nextFundingTime"] = Value::from(next));
            let fake = FakeTransport::new().on(TICKERS, ok_json(&t)).on(INSTRUMENTS, ok("bybit/instruments_complete.json"));
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
    }

    #[test]
    fn bybit_adapter_reports_its_exchange() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        assert_eq!(adapter.exchange(), Exchange::Bybit);
    }

    // ----- trade-cost-estimate: best bid / ask from the SAME tickers response -----

    #[test]
    fn tickers_give_the_best_bid_and_ask_with_no_extra_request() {
        let (t, adapter, _) = setup(happy_fake(), 0);
        let m = block_on(adapter.fetch_market()).unwrap();
        let books = m.books.expect("books available");
        let b = books["BTCUSDT"];
        assert_eq!((b.bid_price, b.bid_qty, b.ask_price, b.ask_qty), (d("86000.40"), d("1.123"), d("86000.50"), d("4.926")));
        assert_eq!(b.observed_at, NOW);
        assert_eq!(books["ETHUSDT"].ask_price, d("2713.64"));
        assert_eq!(t.count("/v5/market/tickers"), 1, "one tickers request, same as fetch_snapshot");
        let before = t.requests().len();
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.requests().len() - before, 1, "fetch_snapshot still sends exactly one request (catalog cached)");
        assert_eq!(m.observations.len(), books.len() + m.observations.iter().filter(|o| !books.contains_key(&o.symbol)).count());
    }

    #[test]
    fn empty_bid_price_makes_that_symbol_unavailable_never_mark_or_zero() {
        let body = mutated("bybit/tickers.json", |v| {
            for r in v["result"]["list"].as_array_mut().unwrap() {
                if r["symbol"] == "BTCUSDT" {
                    r["bid1Price"] = Value::from("");
                }
            }
        });
        let fake = FakeTransport::new()
            .on("/v5/market/tickers", Ok(HttpResponse::ok(body.to_string())))
            .on("/v5/market/instruments-info", ok("bybit/instruments_complete.json"));
        let (_, adapter, _) = setup(fake, 0);
        let m = block_on(adapter.fetch_market()).unwrap();
        let books = m.books.unwrap();
        assert!(!books.contains_key("BTCUSDT"));
        assert!(books.contains_key("ETHUSDT"), "other symbols keep their quotes");
        assert!(m.observations.iter().any(|o| o.symbol == "BTCUSDT"), "the funding observation is unaffected");
    }
}

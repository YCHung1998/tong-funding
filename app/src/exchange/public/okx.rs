//! OKX v5 public market data (spec: exchange-adapter; task 2.3).
//!
//! Field semantics verified on 2026-10-05 (design.md):
//! - `funding-rate.fundingTime` is the UPCOMING settlement; `nextFundingTime` is the one after it and is
//!   only used to derive the interval (`nextFundingTime - fundingTime`).
//! - `fundingRate` applies to the `fundingTime` settlement when `method = current_period`.
//! - `tickers.volCcy24h` is a COIN amount, not USDT. It is converted with `volCcy24h * last` (the latest
//!   trade price, not a 24 h average), so the result is an approximation; with no `last` the volume is
//!   left empty rather than reporting the coin amount as USDT.
//! - Symbols are exposed as `BASEUSDT` (`BTC-USDT-SWAP` is `BTCUSDT`); only `-USDT-SWAP` instruments exist
//!   for this adapter.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tong_funding_core::funding::{DataStatus, FundingObservation, okx_interval_secs};
use tong_funding_core::types::{Decimal, Exchange};

use super::adapter::{
    BATCH_TIMEOUT, ExchangeAdapter, InstrumentRules, ListingStatus, META_TTL_MS, RawObservation, RulesLookup,
    SINGLE_TIMEOUT, TtlCell, assemble, classify_exchange_body, dec_field, http_get, int, parse_json,
    percent_encode_value, str_field, validate_symbol,
};
use super::endpoints::{
    OKX_FUNDING_RATE, OKX_INSTRUMENTS, OKX_MARK_PRICE, OKX_TICKER, OKX_TICKERS, okx_url,
};
use super::refetch::earliest_observed_at;
use crate::exchange::error::AdapterError;
use crate::exchange::transport::HttpTransport;
use crate::exchange::signed::endpoints::{okx_inst_id as inst_id_of, okx_symbol as symbol_of};
use crate::ports::Clock;

struct Instrument {
    /// `state = live`, `ctType = linear` (the `-USDT-SWAP` suffix is implied by the key).
    tradable: bool,
    rules: Result<InstrumentRules, String>,
}

struct Catalog(HashMap<String, Instrument>);

pub struct OkxAdapter<T: HttpTransport> {
    transport: Arc<T>,
    clock: Arc<dyn Clock>,
    catalog: TtlCell<Catalog>,
}

/// OKX failures are `code != "0"` with HTTP 200. Returns the `data` array.
fn parse_okx_body(body: &str) -> Result<Vec<Value>, AdapterError> {
    if let Some(limited) = classify_exchange_body(Exchange::Okx, body) {
        return Err(limited);
    }
    let v = parse_json(body)?;
    let code = v.get("code").and_then(|c| c.as_str().map(str::to_string).or_else(|| int(c).map(|n| n.to_string())));
    let Some(code) = code else { return Err(AdapterError::parse("response has no code")) };
    if code != "0" {
        let msg = v.get("msg").and_then(Value::as_str).unwrap_or("");
        return Err(AdapterError::exchange(code, msg));
    }
    v.get("data")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| AdapterError::parse("response has no data array"))
}

/// Outer error: a field is a JSON number instead of a string (`Parse`). Inner error: a required
/// field is missing, so the rules are unavailable.
fn parse_rules(symbol: &str, row: &Value) -> Result<Result<InstrumentRules, String>, AdapterError> {
    let step_size = dec_field(row, "lotSz")?.filter(|s| *s > Decimal::ZERO);
    let min_qty = dec_field(row, "minSz")?;
    let ct_val = dec_field(row, "ctVal")?.filter(|v| *v > Decimal::ZERO);
    let ct_mult = dec_field(row, "ctMult")?;
    let (Some(step_size), Some(min_qty), Some(ct_val)) = (step_size, min_qty, ct_val) else {
        return Ok(Err("lotSz, minSz or ctVal missing".into()));
    };
    Ok(Ok(InstrumentRules {
        exchange: Exchange::Okx,
        symbol: symbol.to_string(),
        step_size,
        min_qty,
        max_qty: None,
        market_step_size: None,
        market_min_qty: None,
        market_max_qty: None,
        min_notional: None,
        ct_val: Some(ct_val),
        ct_mult,
    }))
}

fn parse_catalog(body: &str) -> Result<Catalog, AdapterError> {
    let mut map = HashMap::new();
    for row in parse_okx_body(body)? {
        let Some(symbol) = str_field(&row, "instId").and_then(symbol_of) else { continue };
        let tradable = str_field(&row, "state") == Some("live") && str_field(&row, "ctType") == Some("linear");
        let rules = parse_rules(&symbol, &row)?;
        map.insert(symbol, Instrument { tradable, rules });
    }
    Ok(Catalog(map))
}

/// `volCcy24h * last`; empty when either is missing, unparsable or non-positive (never the coin amount).
fn quote_volume(ticker: Option<&Value>) -> Result<Option<Decimal>, AdapterError> {
    let Some(t) = ticker else { return Ok(None) };
    let coins = dec_field(t, "volCcy24h")?;
    let last = dec_field(t, "last")?.filter(|p| *p > Decimal::ZERO);
    Ok(coins.zip(last).and_then(|(c, l)| c.checked_mul(l)))
}

fn find_row<'a>(rows: &'a [Value], inst_id: &str) -> Option<&'a Value> {
    rows.iter().find(|r| str_field(r, "instId") == Some(inst_id))
}

impl<T: HttpTransport> OkxAdapter<T> {
    pub fn new(transport: Arc<T>, clock: Arc<dyn Clock>) -> Self {
        OkxAdapter { transport, clock, catalog: TtlCell::new() }
    }

    async fn catalog(&self) -> Result<Arc<Catalog>, AdapterError> {
        if let Some(c) = self.catalog.fresh(self.clock.now_ms(), META_TTL_MS) {
            return Ok(c);
        }
        let url = okx_url(&format!("{OKX_INSTRUMENTS}?instType=SWAP"));
        let body = http_get(&*self.transport, url, BATCH_TIMEOUT).await?;
        let catalog = parse_catalog(&body)?;
        Ok(self.catalog.store(self.clock.now_ms(), catalog))
    }

    async fn timed_get(&self, path_and_query: &str, timeout: Duration) -> (Result<String, AdapterError>, i64) {
        let r = http_get(&*self.transport, okx_url(path_and_query), timeout).await;
        (r, self.clock.now_ms())
    }

    /// Builds one observation from a funding-rate row plus the mark-price and ticker rows of the same instrument.
    fn observation(
        funding: &Value,
        mark: Option<&Value>,
        ticker: Option<&Value>,
        catalog: &Result<Arc<Catalog>, AdapterError>,
        observed_at: i64,
    ) -> Result<Option<FundingObservation>, AdapterError> {
        let Some(symbol) = str_field(funding, "instId").and_then(symbol_of) else { return Ok(None) };
        let mut base = match catalog {
            Err(_) => DataStatus::DataError,
            Ok(c) => match c.0.get(&symbol) {
                Some(i) if i.tradable => DataStatus::Listed,
                _ => DataStatus::NotListed,
            },
        };
        if base == DataStatus::Listed && str_field(funding, "method") != Some("current_period") {
            base = DataStatus::DataError;
        }
        let funding_time = funding.get("fundingTime").and_then(int);
        let interval = okx_interval_secs(funding_time, funding.get("nextFundingTime").and_then(int));
        let raw = RawObservation {
            exchange: Exchange::Okx,
            symbol,
            funding_rate: dec_field(funding, "fundingRate")?,
            mark_price: match mark {
                Some(m) => dec_field(m, "markPx")?,
                None => None,
            },
            next_funding_time: funding_time,
            exchange_timestamp: funding.get("ts").and_then(int),
            volume_24h_quote: quote_volume(ticker)?,
        };
        Ok(Some(assemble(raw, base, interval, observed_at).0))
    }
}

impl<T: HttpTransport> ExchangeAdapter for OkxAdapter<T> {
    fn exchange(&self) -> Exchange {
        Exchange::Okx
    }

    async fn fetch_snapshot(&self) -> Result<Vec<FundingObservation>, AdapterError> {
        let funding_q = format!("{OKX_FUNDING_RATE}?instId=ANY");
        let mark_q = format!("{OKX_MARK_PRICE}?instType=SWAP");
        let tickers_q = format!("{OKX_TICKERS}?instType=SWAP");
        let (funding, mark, tickers, catalog) = tokio::join!(
            self.timed_get(&funding_q, BATCH_TIMEOUT),
            self.timed_get(&mark_q, BATCH_TIMEOUT),
            self.timed_get(&tickers_q, BATCH_TIMEOUT),
            self.catalog(),
        );
        let ((funding, funding_at), (mark, mark_at), (tickers, tickers_at)) = (funding, mark, tickers);
        let funding = parse_okx_body(&funding?)?;
        let mark = parse_okx_body(&mark?)?;
        let tickers = parse_okx_body(&tickers?)?;
        let observed_at = earliest_observed_at(&[funding_at, mark_at, tickers_at]).unwrap_or(funding_at);

        let by_id = |rows: &[Value]| -> HashMap<String, Value> {
            rows.iter().filter_map(|r| Some((str_field(r, "instId")?.to_string(), r.clone()))).collect()
        };
        let (mark, tickers) = (by_id(&mark), by_id(&tickers));
        let mut out = Vec::with_capacity(funding.len());
        for row in &funding {
            let Some(id) = str_field(row, "instId") else { continue };
            if let Some(obs) = Self::observation(row, mark.get(id), tickers.get(id), &catalog, observed_at)? {
                out.push(obs);
            }
        }
        Ok(out)
    }

    async fn refetch_symbol(&self, symbol: &str) -> Result<FundingObservation, AdapterError> {
        validate_symbol(symbol)?;
        let inst_id = inst_id_of(symbol).ok_or_else(|| AdapterError::parse("symbol is not a BASEUSDT symbol"))?;
        let encoded = percent_encode_value(&inst_id);
        let funding_q = format!("{OKX_FUNDING_RATE}?instId={encoded}");
        let mark_q = format!("{OKX_MARK_PRICE}?instId={encoded}");
        let ticker_q = format!("{OKX_TICKER}?instId={encoded}");
        let (funding, mark, ticker, catalog) = tokio::join!(
            self.timed_get(&funding_q, SINGLE_TIMEOUT),
            self.timed_get(&mark_q, SINGLE_TIMEOUT),
            self.timed_get(&ticker_q, SINGLE_TIMEOUT),
            self.catalog(),
        );
        let ((funding, funding_at), (mark, mark_at), (ticker, ticker_at)) = (funding, mark, ticker);
        let funding = parse_okx_body(&funding?)?;
        let mark = parse_okx_body(&mark?)?;
        let ticker = parse_okx_body(&ticker?)?;
        let observed_at = earliest_observed_at(&[funding_at, mark_at, ticker_at]).unwrap_or(funding_at);
        let missing = |what: &str| AdapterError::parse(format!("{what} returned no row for {inst_id}"));
        let funding = find_row(&funding, &inst_id).ok_or_else(|| missing("funding-rate"))?;
        let mark = find_row(&mark, &inst_id).ok_or_else(|| missing("mark-price"))?;
        let ticker = find_row(&ticker, &inst_id).ok_or_else(|| missing("ticker"))?;
        Self::observation(funding, Some(mark), Some(ticker), &catalog, observed_at)?
            .ok_or_else(|| AdapterError::parse("funding-rate row has no usable instId"))
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
            Some(i) if i.tradable => ListingStatus::Listed,
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
    const FR_ANY: &str = "funding-rate?instId=ANY";
    const MARK_SWAP: &str = "mark-price?instType=SWAP";
    const TICKERS_SWAP: &str = "market/tickers?instType=SWAP";
    const INSTRUMENTS: &str = "public/instruments?instType=SWAP";

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

    /// Edits the `data` row of one `instId`.
    fn edit_row(v: &mut Value, inst_id: &str, f: impl FnOnce(&mut Value)) {
        let row = v["data"].as_array_mut().unwrap().iter_mut().find(|r| r["instId"] == inst_id).unwrap();
        f(row);
    }

    fn fake_with(fr: Result<HttpResponse, AdapterError>, tickers: Result<HttpResponse, AdapterError>, instruments: Result<HttpResponse, AdapterError>) -> FakeTransport {
        FakeTransport::new()
            .on(FR_ANY, fr)
            .on(MARK_SWAP, ok("okx/mark_price_swap.json"))
            .on(TICKERS_SWAP, tickers)
            .on(INSTRUMENTS, instruments)
    }

    fn happy_fake() -> FakeTransport {
        fake_with(ok("okx/funding_rate_any.json"), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))
    }

    fn setup(fake: FakeTransport, step_ms: i64) -> (Arc<ClockedTransport>, OkxAdapter<ClockedTransport>, ManualClock) {
        let clock = ManualClock::new(NOW);
        let transport = Arc::new(ClockedTransport::new(fake, clock.clone(), step_ms));
        let adapter = OkxAdapter::new(Arc::clone(&transport), Arc::new(clock.clone()));
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
    fn btc_observation_matches_the_recorded_responses() {
        let all = snapshot(happy_fake()).unwrap();
        let o = find(&all, "BTCUSDT");
        assert_eq!(o.exchange, Exchange::Okx);
        assert_eq!(o.data_status, DataStatus::Listed);
        assert_eq!(o.funding_rate, d("0.0000784623357725"));
        assert_eq!(o.next_funding_time, 1_791_216_000_000, "fundingTime, not nextFundingTime");
        assert_eq!(o.funding_interval_secs, Some(28_800));
        assert_eq!(o.volume_24h_quote, Some(d("60600.3022") * d("86005.8")));
        assert_eq!(o.observed_at, NOW);
    }

    #[test]
    fn mark_price_and_exchange_timestamp_come_from_the_recorded_rows() {
        let mark = json_fixture("okx/mark_price_swap.json");
        let expected = mark["data"].as_array().unwrap().iter().find(|r| r["instId"] == "BTC-USDT-SWAP").unwrap()["markPx"].as_str().unwrap().to_string();
        let fr = json_fixture("okx/funding_rate_any.json");
        let ts: i64 = fr["data"].as_array().unwrap().iter().find(|r| r["instId"] == "BTC-USDT-SWAP").unwrap()["ts"].as_str().unwrap().parse().unwrap();
        let all = snapshot(happy_fake()).unwrap();
        let o = find(&all, "BTCUSDT");
        assert_eq!(o.mark_price, d(&expected));
        assert_eq!(o.exchange_timestamp, ts);
    }

    #[test]
    fn interval_is_the_difference_between_next_and_current_funding_time() {
        // spec example: fundingTime = 1791216000000, nextFundingTime = 1791244800000 -> 28800 s
        let fr = json_fixture("okx/funding_rate_any.json");
        let row = fr["data"].as_array().unwrap().iter().find(|r| r["instId"] == "BTC-USDT-SWAP").unwrap();
        assert_eq!((row["fundingTime"].as_str(), row["nextFundingTime"].as_str()), (Some("1791216000000"), Some("1791244800000")));
        let all = snapshot(happy_fake()).unwrap();
        assert_eq!(find(&all, "BTCUSDT").funding_interval_secs, Some(28_800));
    }

    #[test]
    fn only_usdt_swap_rows_are_returned() {
        let all = snapshot(happy_fake()).unwrap();
        let mut symbols: Vec<&str> = all.iter().map(|o| o.symbol.as_str()).collect();
        symbols.sort();
        assert_eq!(symbols, vec!["BTCUSDT", "CHIPUSDT", "ETHUSDT"], "inverse swap and FUTURES rows are ignored");
    }

    #[test]
    fn volume_is_coin_volume_times_last_price() {
        // spec: volCcy24h = 100, last = 50 -> 5000
        let t = mutated("okx/tickers_swap.json", |v| {
            edit_row(v, "BTC-USDT-SWAP", |r| {
                r["volCcy24h"] = Value::from("100");
                r["last"] = Value::from("50");
            });
        });
        let all = snapshot(fake_with(ok("okx/funding_rate_any.json"), ok_json(&t), ok("okx/instruments_swap.json"))).unwrap();
        assert_eq!(find(&all, "BTCUSDT").volume_24h_quote, Some(d("5000")));
    }

    #[test]
    fn missing_last_leaves_volume_empty_not_the_coin_amount() {
        let t = mutated("okx/tickers_swap.json", |v| {
            edit_row(v, "BTC-USDT-SWAP", |r| {
                r["volCcy24h"] = Value::from("100");
                r["last"] = Value::from("");
            });
        });
        let all = snapshot(fake_with(ok("okx/funding_rate_any.json"), ok_json(&t), ok("okx/instruments_swap.json"))).unwrap();
        let o = find(&all, "BTCUSDT");
        assert_eq!(o.volume_24h_quote, None);
        assert_eq!(o.data_status, DataStatus::Listed, "volume is optional");
    }

    // ----- method / interval / listing -----

    #[test]
    fn method_other_than_current_period_is_data_error() {
        let fr = mutated("okx/funding_rate_any.json", |v| edit_row(v, "ETH-USDT-SWAP", |r| r["method"] = Value::from("next_period")));
        let all = snapshot(fake_with(ok_json(&fr), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap();
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::DataError);
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::Listed);
    }

    #[test]
    fn missing_next_funding_time_gives_data_error_without_a_guessed_interval() {
        let fr = mutated("okx/funding_rate_any.json", |v| edit_row(v, "ETH-USDT-SWAP", |r| r["nextFundingTime"] = Value::from("")));
        let all = snapshot(fake_with(ok_json(&fr), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap();
        let o = find(&all, "ETHUSDT");
        assert_eq!((o.data_status, o.funding_interval_secs), (DataStatus::DataError, None));
    }

    #[test]
    fn non_positive_interval_gives_data_error() {
        let fr = mutated("okx/funding_rate_any.json", |v| {
            edit_row(v, "ETH-USDT-SWAP", |r| {
                let t = r["fundingTime"].clone();
                r["nextFundingTime"] = t;
            })
        });
        let all = snapshot(fake_with(ok_json(&fr), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap();
        let o = find(&all, "ETHUSDT");
        assert_eq!((o.data_status, o.funding_interval_secs), (DataStatus::DataError, None));
    }

    #[test]
    fn interval_inconsistent_with_the_settlement_distance_is_data_error() {
        // fundingTime is 7 h after the response `ts`, but the derived interval is only 4 h
        let fr = mutated("okx/funding_rate_any.json", |v| {
            edit_row(v, "ETH-USDT-SWAP", |r| {
                let ts: i64 = r["ts"].as_str().unwrap().parse().unwrap();
                let funding = ts + 7 * 3_600_000;
                r["fundingTime"] = Value::from(funding.to_string());
                r["nextFundingTime"] = Value::from((funding + 4 * 3_600_000).to_string());
            })
        });
        let all = snapshot(fake_with(ok_json(&fr), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap();
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::DataError);
    }

    #[test]
    fn non_live_instrument_is_not_listed_and_absent_one_too() {
        let inst = mutated("okx/instruments_swap.json", |v| {
            edit_row(v, "ETH-USDT-SWAP", |r| r["state"] = Value::from("suspend"));
            v["data"].as_array_mut().unwrap().retain(|r| r["instId"] != "CHIP-USDT-SWAP");
        });
        let all = snapshot(fake_with(ok("okx/funding_rate_any.json"), ok("okx/tickers_swap.json"), ok_json(&inst))).unwrap();
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::NotListed);
        assert_eq!(find(&all, "CHIPUSDT").data_status, DataStatus::NotListed);
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::Listed);
    }

    #[test]
    fn catalog_failure_makes_every_symbol_data_error() {
        let all = snapshot(fake_with(ok("okx/funding_rate_any.json"), ok("okx/tickers_swap.json"), Err(AdapterError::Http { status: 503 }))).unwrap();
        assert!(!all.is_empty());
        assert!(all.iter().all(|o| o.data_status == DataStatus::DataError));
    }

    #[test]
    fn missing_mark_price_row_is_data_error() {
        let fake = FakeTransport::new()
            .on(FR_ANY, ok("okx/funding_rate_any.json"))
            .on(MARK_SWAP, Ok(HttpResponse::ok(r#"{"code":"0","msg":"","data":[]}"#)))
            .on(TICKERS_SWAP, ok("okx/tickers_swap.json"))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"));
        let all = snapshot(fake).unwrap();
        assert!(all.iter().all(|o| o.data_status == DataStatus::DataError));
    }

    // ----- error mapping -----

    #[test]
    fn nonzero_code_with_http_200_is_an_exchange_error() {
        let body = r#"{"code":"51000","msg":"Parameter error; OK-ACCESS-KEY: SECRETKEY999","data":[]}"#;
        let err = snapshot(fake_with(Ok(HttpResponse::ok(body)), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap_err();
        assert!(matches!(&err, AdapterError::Exchange { code, .. } if code == "51000"), "{err:?}");
        assert!(!err.to_string().contains("SECRETKEY999"));
    }

    #[test]
    fn rate_limit_codes_with_http_200_are_rate_limited() {
        for code in ["50011", "50013"] {
            let body = format!(r#"{{"code":"{code}","msg":"Too Many Requests","data":[]}}"#);
            let err = snapshot(fake_with(Ok(HttpResponse::ok(body)), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap_err();
            assert_eq!(err, AdapterError::RateLimited { retry_after_ms: None }, "{code}");
        }
        let mut banned = HttpResponse::with_status(418, "");
        banned.headers.push(("Retry-After".into(), "3".into()));
        let err = snapshot(fake_with(Ok(banned), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap_err();
        assert_eq!(err, AdapterError::RateLimited { retry_after_ms: Some(3_000) });
    }

    #[test]
    fn timeout_rate_limit_http_and_garbage_are_typed() {
        let t = fake_with(Err(AdapterError::Timeout), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"));
        assert_eq!(snapshot(t), Err(AdapterError::Timeout));

        let mut limited = HttpResponse::with_status(429, "");
        limited.headers.push(("retry-after".into(), "4".into()));
        let t = fake_with(Ok(limited), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"));
        assert_eq!(snapshot(t), Err(AdapterError::RateLimited { retry_after_ms: Some(4000) }));

        let t = fake_with(Ok(HttpResponse::with_status(500, "")), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"));
        assert_eq!(snapshot(t), Err(AdapterError::Http { status: 500 }));

        let t = fake_with(Ok(HttpResponse::ok("<html>")), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"));
        assert!(matches!(snapshot(t), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn requests_are_unsigned_gets_with_the_batch_timeout_and_metadata_is_cached() {
        let (t, adapter, clock) = setup(happy_fake(), 0);
        block_on(adapter.fetch_snapshot()).unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 4);
        for r in &reqs {
            assert!(r.headers.is_empty(), "{r:?}");
            assert_eq!(r.timeout, BATCH_TIMEOUT);
        }
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count(FR_ANY), 2);
        assert_eq!(t.count(MARK_SWAP), 2);
        assert_eq!(t.count(TICKERS_SWAP), 2);
        assert_eq!(t.count(INSTRUMENTS), 1);
        clock.advance(META_TTL_MS + 1);
        block_on(adapter.fetch_snapshot()).unwrap();
        assert_eq!(t.count(INSTRUMENTS), 2);
    }

    // ----- rules / listing -----

    #[test]
    fn rules_are_in_contracts_with_ct_val() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        let RulesLookup::Available(r) = block_on(adapter.instrument_rules("BTCUSDT")).unwrap() else { panic!("rules expected") };
        assert_eq!((r.step_size, r.min_qty), (d("0.01"), d("0.01")), "lotSz / minSz in contracts");
        assert_eq!((r.ct_val, r.ct_mult), (Some(d("0.01")), Some(d("1"))));
        let RulesLookup::Available(chip) = block_on(adapter.instrument_rules("CHIPUSDT")).unwrap() else { panic!("rules expected") };
        assert_eq!((chip.step_size, chip.min_qty, chip.ct_val), (d("1"), d("1"), Some(d("100"))));
    }

    #[test]
    fn missing_lot_size_or_ct_val_means_rules_are_unavailable() {
        let inst = mutated("okx/instruments_swap.json", |v| {
            edit_row(v, "BTC-USDT-SWAP", |r| r["lotSz"] = Value::from(""));
            edit_row(v, "ETH-USDT-SWAP", |r| {
                r.as_object_mut().unwrap().remove("ctVal");
            });
        });
        let fake = FakeTransport::new().on(INSTRUMENTS, ok_json(&inst));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")).unwrap(), RulesLookup::Unavailable { .. }));
        assert!(matches!(block_on(adapter.instrument_rules("ETHUSDT")).unwrap(), RulesLookup::Unavailable { .. }));
        assert_eq!(block_on(adapter.instrument_rules("NOSUCHUSDT")).unwrap(), RulesLookup::UnknownSymbol);
    }

    #[test]
    fn listing_status_follows_the_catalog() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        assert_eq!(block_on(adapter.listing_status("BTCUSDT")), Ok(ListingStatus::Listed));
        assert_eq!(block_on(adapter.listing_status("NOSUCHUSDT")), Ok(ListingStatus::NotListed));
        let fake = FakeTransport::new().on(INSTRUMENTS, Err(AdapterError::Timeout));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.listing_status("BTCUSDT")), Err(AdapterError::Timeout));
    }

    // ----- single-symbol refetch -----

    const FR_ONE: &str = "funding-rate?instId=BTC-USDT-SWAP";
    const MARK_ONE: &str = "mark-price?instId=BTC-USDT-SWAP";
    const TICKER_ONE: &str = "market/ticker?instId=BTC-USDT-SWAP";

    fn refetch_fake() -> FakeTransport {
        FakeTransport::new()
            .on(FR_ONE, ok("okx/funding_rate_btc.json"))
            .on(MARK_ONE, ok("okx/mark_price_btc.json"))
            .on(TICKER_ONE, ok("okx/ticker_btc.json"))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"))
    }

    #[test]
    fn refetch_combines_funding_rate_mark_price_and_ticker() {
        let (t, adapter, _) = setup(refetch_fake(), 0);
        let o = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(o.data_status, DataStatus::Listed);
        assert_eq!(o.funding_rate, d("0.0000784623357725"));
        assert_eq!(o.mark_price, d("86011.4"));
        assert_eq!(o.next_funding_time, 1_791_216_000_000);
        assert_eq!(o.funding_interval_secs, Some(28_800));
        assert_eq!(o.exchange_timestamp, 1_791_201_185_968, "ts of the funding-rate response");
        assert_eq!(o.volume_24h_quote, Some(d("60601.0688") * d("86014.4")));
        for r in t.requests().iter().filter(|r| r.url.contains("instId=BTC-USDT-SWAP")) {
            assert_eq!(r.timeout, SINGLE_TIMEOUT);
            assert_eq!(r.timeout, Duration::from_secs(2));
            assert!(r.headers.is_empty());
        }
    }

    #[test]
    fn refetch_observed_at_is_the_earliest_of_its_requests() {
        let (_, adapter, _) = setup(refetch_fake(), 300);
        let o = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        assert_eq!(o.observed_at, NOW, "responses arrive at T, T+300, T+600; the earliest wins");
    }

    #[test]
    fn refetch_sends_new_requests_every_time_after_a_batch_snapshot() {
        let fake = refetch_fake()
            .on(FR_ANY, ok("okx/funding_rate_any.json"))
            .on(MARK_SWAP, ok("okx/mark_price_swap.json"))
            .on(TICKERS_SWAP, ok("okx/tickers_swap.json"));
        let (t, adapter, clock) = setup(fake, 0);
        let batch = block_on(adapter.fetch_snapshot()).unwrap();
        let cached = find(&batch, "BTCUSDT").observed_at;
        clock.advance(8_000);
        let a = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        clock.advance(8_000);
        let b = block_on(adapter.refetch_symbol("BTCUSDT")).unwrap();
        for m in [FR_ONE, MARK_ONE, TICKER_ONE] {
            assert_eq!(t.count(m), 2, "{m}");
        }
        assert!(a.observed_at > cached && b.observed_at > a.observed_at);
    }

    #[test]
    fn refetch_errors_are_returned() {
        let fake = FakeTransport::new()
            .on(FR_ONE, Ok(HttpResponse::ok(r#"{"code":"51001","msg":"Instrument ID does not exist","data":[]}"#)))
            .on(MARK_ONE, ok("okx/mark_price_btc.json"))
            .on(TICKER_ONE, ok("okx/ticker_btc.json"))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::Exchange { code, .. }) if code == "51001"));

        let fake = FakeTransport::new()
            .on(FR_ONE, Err(AdapterError::Timeout))
            .on(MARK_ONE, ok("okx/mark_price_btc.json"))
            .on(TICKER_ONE, ok("okx/ticker_btc.json"))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::Timeout));

        let fake = FakeTransport::new()
            .on(FR_ONE, Ok(HttpResponse::ok(r#"{"code":"0","msg":"","data":[]}"#)))
            .on(MARK_ONE, ok("okx/mark_price_btc.json"))
            .on(TICKER_ONE, ok("okx/ticker_btc.json"))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn refetch_without_a_last_price_has_no_volume() {
        let tick = mutated("okx/ticker_btc.json", |v| v["data"][0]["last"] = Value::from(""));
        let fake = FakeTransport::new()
            .on(FR_ONE, ok("okx/funding_rate_btc.json"))
            .on(MARK_ONE, ok("okx/mark_price_btc.json"))
            .on(TICKER_ONE, ok_json(&tick))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.refetch_symbol("BTCUSDT")).unwrap().volume_24h_quote, None);
    }

    // ----- round 2 -----

    #[test]
    fn rate_limit_code_on_a_refetch_request_is_rate_limited() {
        let fake = FakeTransport::new()
            .on(FR_ONE, Ok(HttpResponse::ok(r#"{"code":"50011","msg":"x","data":[]}"#)))
            .on(MARK_ONE, ok("okx/mark_price_btc.json"))
            .on(TICKER_ONE, ok("okx/ticker_btc.json"))
            .on(INSTRUMENTS, ok("okx/instruments_swap.json"));
        let (_, adapter, _) = setup(fake, 0);
        assert_eq!(block_on(adapter.refetch_symbol("BTCUSDT")), Err(AdapterError::RateLimited { retry_after_ms: None }));
    }

    #[test]
    fn symbol_with_url_metacharacters_is_rejected_before_any_request() {
        let (t, adapter, _) = setup(refetch_fake(), 0);
        for bad in ["BTC&x=1USDT", "BTCUSDT#@evil.com", "", "a b", "USDT"] {
            assert!(matches!(block_on(adapter.refetch_symbol(bad)), Err(AdapterError::Parse(_))), "{bad:?}");
        }
        assert!(t.requests().is_empty());
    }

    #[test]
    fn json_number_where_a_decimal_string_is_expected_is_a_parse_error() {
        let fr = mutated("okx/funding_rate_any.json", |v| edit_row(v, "BTC-USDT-SWAP", |r| r["fundingRate"] = serde_json::json!(1e-4)));
        let r = snapshot(fake_with(ok_json(&fr), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json")));
        assert!(matches!(r, Err(AdapterError::Parse(_))), "{r:?}");
        let inst = mutated("okx/instruments_swap.json", |v| edit_row(v, "BTC-USDT-SWAP", |r| r["lotSz"] = serde_json::json!(0.01)));
        let fake = FakeTransport::new().on(INSTRUMENTS, ok_json(&inst));
        let (_, adapter, _) = setup(fake, 0);
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn past_settlement_time_is_data_error_even_with_a_valid_interval() {
        let fr = mutated("okx/funding_rate_any.json", |v| {
            edit_row(v, "ETH-USDT-SWAP", |r| {
                let ts: i64 = r["ts"].as_str().unwrap().parse().unwrap();
                let past = ts - 3_600_000;
                r["fundingTime"] = Value::from(past.to_string());
                r["nextFundingTime"] = Value::from((past + 8 * 3_600_000).to_string());
            })
        });
        let all = snapshot(fake_with(ok_json(&fr), ok("okx/tickers_swap.json"), ok("okx/instruments_swap.json"))).unwrap();
        assert_eq!(find(&all, "ETHUSDT").data_status, DataStatus::DataError);
        assert_eq!(find(&all, "BTCUSDT").data_status, DataStatus::Listed);
    }

    #[test]
    fn real_recorded_responses_are_not_rejected_by_the_string_only_rule() {
        assert!(snapshot(happy_fake()).is_ok());
        let (_, adapter, _) = setup(refetch_fake(), 0);
        assert!(block_on(adapter.refetch_symbol("BTCUSDT")).is_ok());
        assert!(matches!(block_on(adapter.instrument_rules("BTCUSDT")), Ok(RulesLookup::Available(_))));
    }

    #[test]
    fn okx_adapter_reports_its_exchange() {
        let (_, adapter, _) = setup(happy_fake(), 0);
        assert_eq!(adapter.exchange(), Exchange::Okx);
    }
}

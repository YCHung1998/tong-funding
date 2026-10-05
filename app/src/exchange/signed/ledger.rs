//! Funding ledger (income / transaction-log) signed GET clients and parsers for Binance and Bybit
//! demo/testnet (change funding-pnl, spec funding-history-fetch). Read-only: only GET requests to
//! the compile-time demo/testnet hosts of `endpoints`. OKX has no ledger client (no orders there).
//!
//! UNVERIFIED (task 1.1): the parsers follow the exchanges' public documentation only. The
//! fixtures in `app/tests/fixtures/funding/` are constructed from the docs and say so in their
//! header; the sign of Binance `income`, the units and the page limits must be checked against
//! a real demo response before this is relied on.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::types::Exchange;

use super::endpoints::{BinanceHost, BybitHost};
use super::models::{dec_req, str_req};
use super::signing::{
    ClockOffsetSource, NotConnectedReason, RECV_WINDOW_MS, Resync, SIGNED_TIMEOUT, binance_signature, bybit_signature, check_status, encode_cursor,
    encode_query, is_timestamp_rejected, load_credentials, parse_json, require_offset, sanitize_error,
};
use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpResponse, HttpTransport};
use crate::ports::{Clock, SecretProvider};

/// `GET /fapi/v1/income` (USER_DATA, weight 30 per the docs).
pub const BINANCE_INCOME_PATH: &str = "/fapi/v1/income";
/// `GET /v5/account/transaction-log` (UTA).
pub const BYBIT_TRANSACTION_LOG_PATH: &str = "/v5/account/transaction-log";
/// Binance `incomeType` of funding payments.
pub const BINANCE_FUNDING_FEE: &str = "FUNDING_FEE";
/// Bybit `type` of USDT perpetual funding settlements.
pub const BYBIT_SETTLEMENT: &str = "SETTLEMENT";
/// Rows per page requested (documented maxima: Binance 1000, Bybit 50; UNVERIFIED, task 1.1).
pub const BINANCE_INCOME_LIMIT: u32 = 1000;
pub const BYBIT_TRANSACTION_LOG_LIMIT: u32 = 50;
/// Bybit `retCode` for "Too many visits" (rate limit), delivered with HTTP 200.
const BYBIT_RATE_LIMIT_RET_CODE: i64 = 10006;

/// One fetched page: the funding entries in it, how many rows the exchange returned in total
/// (before keeping only funding rows; used to tell whether a Binance page was full) and Bybit's
/// next cursor (`None` = exhausted).
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerPage {
    pub entries: Vec<FundingLedgerEntry>,
    pub rows: usize,
    pub next_cursor: Option<String>,
}

// ---- parsers ----------------------------------------------------------------------------------

/// Binance income rows (a JSON array). Only `incomeType = FUNDING_FEE` rows become entries; the
/// amount is taken as signed by the exchange (received positive: UNVERIFIED, task 1.1).
pub fn parse_binance_income(body: &Value) -> Result<LedgerPage, AdapterError> {
    let rows = body.as_array().ok_or_else(|| AdapterError::parse("income: expected a JSON array"))?;
    let mut entries = Vec::new();
    for r in rows {
        let kind = str_req(r, "incomeType")?;
        if kind != BINANCE_FUNDING_FEE {
            continue;
        }
        let time = r.get("time").and_then(Value::as_i64).ok_or_else(|| AdapterError::parse("income: missing integer field time"))?;
        entries.push(FundingLedgerEntry::new(
            Exchange::Binance,
            str_req(r, "symbol")?,
            dec_req(r, "income")?,
            str_req(r, "asset")?,
            time,
            str_req(r, "tranId")?,
            kind,
            r.clone(),
        ));
    }
    Ok(LedgerPage { entries, rows: rows.len(), next_cursor: None })
}

/// Bybit transaction-log body (`retCode` already checked). Only `type = SETTLEMENT` rows become
/// entries; `funding` is positive when received (documented; UNVERIFIED, task 1.1).
pub fn parse_bybit_transaction_log(body: &Value) -> Result<LedgerPage, AdapterError> {
    let rows = body.pointer("/result/list").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("transaction-log: missing result.list"))?;
    let mut entries = Vec::new();
    for r in rows {
        let kind = str_req(r, "type")?;
        if kind != BYBIT_SETTLEMENT {
            continue;
        }
        let time: i64 = str_req(r, "transactionTime")?
            .parse()
            .map_err(|_| AdapterError::parse("transaction-log: transactionTime is not an integer"))?;
        entries.push(FundingLedgerEntry::new(
            Exchange::Bybit,
            str_req(r, "symbol")?,
            dec_req(r, "funding")?,
            str_req(r, "currency")?,
            time,
            str_req(r, "id")?,
            kind,
            r.clone(),
        ));
    }
    let next_cursor = body.pointer("/result/nextPageCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
    Ok(LedgerPage { entries, rows: rows.len(), next_cursor })
}

// ---- Binance client ---------------------------------------------------------------------------

/// Signed GET of Binance funding income (demo/testnet host only).
pub struct BinanceLedgerClient<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn Clock>,
    offset: Arc<dyn ClockOffsetSource>,
    resync: Arc<dyn Resync>,
    host: BinanceHost,
    reason: Mutex<Option<NotConnectedReason>>,
}

impl<T: HttpTransport> BinanceLedgerClient<T> {
    pub fn new(
        transport: Arc<T>,
        secrets: Arc<dyn SecretProvider>,
        clock: Arc<dyn Clock>,
        offset: Arc<dyn ClockOffsetSource>,
        resync: Arc<dyn Resync>,
        demo_env: BinanceHost,
    ) -> Self {
        BinanceLedgerClient { transport, secrets, clock, offset, resync, host: demo_env, reason: Mutex::new(None) }
    }

    pub fn last_not_connected_reason(&self) -> Option<NotConnectedReason> {
        self.reason.lock().ok().and_then(|g| *g)
    }

    /// One page (1-based) of `FUNDING_FEE` income of `symbol` in `[start_ms, end_ms]`.
    pub async fn income_page(&self, symbol: &str, start_ms: i64, end_ms: i64, page: u32) -> Result<LedgerPage, AdapterError> {
        let params = [
            ("symbol", symbol.to_string()),
            ("incomeType", BINANCE_FUNDING_FEE.to_string()),
            ("startTime", start_ms.to_string()),
            ("endTime", end_ms.to_string()),
            ("page", page.to_string()),
            ("limit", BINANCE_INCOME_LIMIT.to_string()),
        ];
        let body = match self.attempt(&params).await {
            Err(e) if is_timestamp_rejected(&e) => {
                self.resync.resync().await.map_err(sanitize_error)?;
                self.attempt(&params).await?
            }
            other => other?,
        };
        parse_binance_income(&body)
    }

    async fn attempt(&self, params: &[(&str, String)]) -> Result<Value, AdapterError> {
        let prepared = load_credentials(self.secrets.as_ref(), Exchange::Binance, false).and_then(|c| require_offset(self.offset.as_ref()).map(|o| (c, o)));
        let (creds, offset_ms) = match prepared {
            Ok(v) => v,
            Err(reason) => {
                if let Ok(mut g) = self.reason.lock() {
                    *g = Some(reason);
                }
                return Err(reason.into());
            }
        };
        if let Ok(mut g) = self.reason.lock() {
            *g = None;
        }
        let timestamp = self.clock.now_ms().saturating_add(offset_ms);
        let mut all: Vec<(&str, String)> = params.to_vec();
        all.push(("timestamp", timestamp.to_string()));
        all.push(("recvWindow", RECV_WINDOW_MS.to_string()));
        let query = encode_query(&all);
        let signature = binance_signature(&creds.api_secret, &query)?;
        let url = format!("{}{}?{}&signature={}", self.host.base_url(), BINANCE_INCOME_PATH, query, signature);
        let request = HttpRequest::get(url, SIGNED_TIMEOUT).header("X-MBX-APIKEY", &creds.api_key);
        let response = self.transport.get(request).await.map_err(sanitize_error)?;
        binance_body(response)
    }
}

/// Binance reports failures as `{"code": -1021, "msg": "..."}` with HTTP 4xx (or 200).
fn binance_body(resp: HttpResponse) -> Result<Value, AdapterError> {
    let error_in = |v: &Value| -> Option<AdapterError> {
        let code = v.get("code")?.as_i64()?;
        let msg = v.get("msg")?.as_str()?;
        (code != 200).then(|| AdapterError::exchange(code.to_string(), msg))
    };
    if !(200..=299).contains(&resp.status) {
        if !matches!(resp.status, 429 | 418) {
            if let Some(e) = serde_json::from_str::<Value>(&resp.body).ok().as_ref().and_then(error_in) {
                return Err(e);
            }
        }
        let status = resp.status;
        return Err(check_status(resp).err().unwrap_or(AdapterError::Http { status }));
    }
    let body = parse_json(&resp.body)?;
    match error_in(&body) {
        Some(e) => Err(e),
        None => Ok(body),
    }
}

// ---- Bybit client -----------------------------------------------------------------------------

/// Signed GET of Bybit UTA transaction-log settlements (Demo Trading host only).
pub struct BybitLedgerClient<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn Clock>,
    offset: Arc<dyn ClockOffsetSource>,
    resync: Arc<dyn Resync>,
    host: BybitHost,
    reason: Mutex<Option<NotConnectedReason>>,
}

impl<T: HttpTransport> BybitLedgerClient<T> {
    pub fn new(
        transport: Arc<T>,
        secrets: Arc<dyn SecretProvider>,
        clock: Arc<dyn Clock>,
        offset: Arc<dyn ClockOffsetSource>,
        resync: Arc<dyn Resync>,
        demo_env: BybitHost,
    ) -> Self {
        BybitLedgerClient { transport, secrets, clock, offset, resync, host: demo_env, reason: Mutex::new(None) }
    }

    pub fn last_not_connected_reason(&self) -> Option<NotConnectedReason> {
        self.reason.lock().ok().and_then(|g| *g)
    }

    /// One page of `SETTLEMENT` rows (all USDT linear symbols: the endpoint has no symbol filter)
    /// in `[start_ms, end_ms]` (at most 7 days apart, per the docs).
    pub async fn transaction_log_page(&self, start_ms: i64, end_ms: i64, cursor: Option<&str>) -> Result<LedgerPage, AdapterError> {
        let mut query = encode_query(&[
            ("accountType", "UNIFIED".to_string()),
            ("category", "linear".to_string()),
            ("currency", "USDT".to_string()),
            ("type", BYBIT_SETTLEMENT.to_string()),
            ("startTime", start_ms.to_string()),
            ("endTime", end_ms.to_string()),
            ("limit", BYBIT_TRANSACTION_LOG_LIMIT.to_string()),
        ]);
        if let Some(c) = cursor {
            query.push_str("&cursor=");
            query.push_str(&encode_cursor(c));
        }
        let body = match self.attempt(&query).await {
            Err(e) if is_timestamp_rejected(&e) => {
                self.resync.resync().await.map_err(sanitize_error)?;
                self.attempt(&query).await?
            }
            other => other?,
        };
        parse_bybit_transaction_log(&body)
    }

    async fn attempt(&self, query: &str) -> Result<Value, AdapterError> {
        let prepared = load_credentials(self.secrets.as_ref(), Exchange::Bybit, false).and_then(|c| require_offset(self.offset.as_ref()).map(|o| (c, o)));
        let (creds, offset_ms) = match prepared {
            Ok(v) => v,
            Err(reason) => {
                if let Ok(mut g) = self.reason.lock() {
                    *g = Some(reason);
                }
                return Err(reason.into());
            }
        };
        if let Ok(mut g) = self.reason.lock() {
            *g = None;
        }
        let timestamp = self.clock.now_ms().saturating_add(offset_ms);
        let signature = bybit_signature(&creds.api_secret, timestamp, &creds.api_key, RECV_WINDOW_MS, query)?;
        let url = format!("{}{}?{}", self.host.base_url(), BYBIT_TRANSACTION_LOG_PATH, query);
        let request = HttpRequest::get(url, SIGNED_TIMEOUT)
            .header("X-BAPI-API-KEY", &creds.api_key)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-TIMESTAMP", &timestamp.to_string())
            .header("X-BAPI-RECV-WINDOW", &RECV_WINDOW_MS.to_string());
        let response = self.transport.get(request).await.map_err(sanitize_error)?;
        let response = check_status(response)?;
        let body = parse_json(&response.body)?;
        let code = body.get("retCode").and_then(Value::as_i64).ok_or_else(|| AdapterError::parse("missing retCode"))?;
        if code == BYBIT_RATE_LIMIT_RET_CODE {
            return Err(AdapterError::RateLimited { retry_after_ms: None });
        }
        if code != 0 {
            let msg = body.get("retMsg").and_then(Value::as_str).unwrap_or("");
            return Err(AdapterError::exchange(code.to_string(), msg));
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    use serde_json::json;
    use tong_funding_core::types::Decimal;

    use crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS;
    use crate::exchange::transport::FakeTransport;
    use crate::ports::{ManualClock, MemorySecrets, SecretName};

    const NOW: i64 = 1_791_200_000_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    fn fixture(name: &str) -> Value {
        let text = match name {
            "binance" => include_str!("../../../tests/fixtures/funding/binance_income_funding_fee.json"),
            "bybit" => include_str!("../../../tests/fixtures/funding/bybit_transaction_log_settlement.json"),
            other => panic!("{other}"),
        };
        let v: Value = serde_json::from_str(text).unwrap();
        assert!(v["_fixture_note"].as_str().unwrap().starts_with("UNVERIFIED, FROM DOCS"), "fixtures must say they are not recorded responses");
        v["response"].clone()
    }

    struct NoResync;
    impl Resync for NoResync {
        fn resync(&self) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn secrets(e: Exchange) -> Arc<dyn SecretProvider> {
        Arc::new(MemorySecrets::default().with(e, SecretName::ApiKey, "TEST_KEY_NOT_REAL").with(e, SecretName::ApiSecret, "TEST_SECRET_NOT_REAL"))
    }

    #[test]
    fn funding_parse_bybit_negative_funding_is_paid() {
        let page = parse_bybit_transaction_log(&fixture("bybit")).unwrap();
        let e = page.entries.iter().find(|e| e.symbol == "XRPUSDT").unwrap();
        assert_eq!(e.amount, d("-0.003676"));
        assert_eq!(e.exchange, Exchange::Bybit);
        assert_eq!(e.asset, "USDT");
        assert_eq!(e.settled_at_ms, 1_672_128_000_000);
        assert_eq!(e.dedupe_key, "bybit:592324_XRPUSDT_161440249321");
        assert_eq!(e.raw["funding"], json!("-0.003676"), "the whole original row is kept");
    }

    #[test]
    fn funding_parse_bybit_positive_funding_is_received() {
        let page = parse_bybit_transaction_log(&fixture("bybit")).unwrap();
        let e = page.entries.iter().find(|e| e.symbol == "BTCUSDT").unwrap();
        assert_eq!(e.amount, d("0.5"));
    }

    #[test]
    fn funding_parse_bybit_keeps_only_settlement_rows() {
        let page = parse_bybit_transaction_log(&fixture("bybit")).unwrap();
        assert_eq!(page.rows, 3);
        assert_eq!(page.entries.len(), 2, "the TRADE row is not a funding entry");
        assert!(page.entries.iter().all(|e| e.kind == "SETTLEMENT"));
        assert_eq!(page.next_cursor, None, "empty nextPageCursor = exhausted");
    }

    #[test]
    fn funding_parse_binance_tran_id_makes_the_dedupe_key() {
        let page = parse_binance_income(&fixture("binance")).unwrap();
        assert_eq!(page.rows, 3);
        assert_eq!(page.entries.len(), 2, "COMMISSION is not funding");
        let e = &page.entries[0];
        assert_eq!(e.dedupe_key, "binance:FUNDING_FEE:9689322392");
        assert_eq!((e.amount, e.settled_at_ms, e.symbol.as_str()), (d("-0.12"), 1_791_187_200_000, "BTCUSDT"));
        assert_eq!(page.entries[1].amount, d("0.035"));
    }

    #[test]
    fn funding_parse_malformed_rows_fail_instead_of_being_skipped() {
        let bad = json!([{"symbol": "BTCUSDT", "incomeType": "FUNDING_FEE", "income": "abc", "asset": "USDT", "time": 1, "tranId": 1}]);
        assert!(matches!(parse_binance_income(&bad), Err(AdapterError::Parse(_))));
        let no_id = json!({"result": {"list": [{"symbol": "X", "type": "SETTLEMENT", "funding": "1", "currency": "USDT", "transactionTime": "1"}], "nextPageCursor": ""}});
        assert!(matches!(parse_bybit_transaction_log(&no_id), Err(AdapterError::Parse(_))));
        assert!(matches!(parse_binance_income(&json!({})), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn funding_parse_binance_client_sends_one_signed_get_to_the_demo_host() {
        let body = fixture("binance").to_string();
        let t = Arc::new(FakeTransport::new().on(BINANCE_INCOME_PATH, Ok(HttpResponse::ok(body))));
        let c = BinanceLedgerClient::new(t.clone(), secrets(Exchange::Binance), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), BinanceHost::Testnet);
        let page = block_on(c.income_page("BTCUSDT", 1, 2, 1)).unwrap();
        assert_eq!(page.entries.len(), 2);
        let reqs = t.requests();
        assert_eq!(reqs.len(), 1);
        let url = &reqs[0].url;
        assert!(url.starts_with("https://testnet.binancefuture.com/fapi/v1/income?"), "{url}");
        for p in ["symbol=BTCUSDT", "incomeType=FUNDING_FEE", "startTime=1", "endTime=2", "page=1", "limit=1000", "signature="] {
            assert!(url.contains(p), "{p} missing in {url}");
        }
        assert!(ALLOWED_SIGNED_HOSTS.iter().any(|h| url.contains(h)));
    }

    #[test]
    fn funding_parse_bybit_client_sends_the_documented_filters_and_the_cursor() {
        let body = fixture("bybit").to_string();
        let t = Arc::new(FakeTransport::new().on(BYBIT_TRANSACTION_LOG_PATH, Ok(HttpResponse::ok(body))));
        let c = BybitLedgerClient::new(t.clone(), secrets(Exchange::Bybit), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), BybitHost::Demo);
        let page = block_on(c.transaction_log_page(10, 20, Some("abc"))).unwrap();
        assert_eq!(page.entries.len(), 2);
        let url = &t.requests()[0].url;
        assert!(url.starts_with("https://api-demo.bybit.com/v5/account/transaction-log?"), "{url}");
        for p in ["accountType=UNIFIED", "category=linear", "currency=USDT", "type=SETTLEMENT", "startTime=10", "endTime=20", "limit=50", "cursor=abc"] {
            assert!(url.contains(p), "{p} missing in {url}");
        }
    }

    #[test]
    fn funding_parse_without_keys_nothing_is_sent_and_the_reason_is_kept() {
        let t = Arc::new(FakeTransport::new());
        let c = BybitLedgerClient::new(t.clone(), Arc::new(MemorySecrets::default()), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), BybitHost::Demo);
        assert_eq!(block_on(c.transaction_log_page(1, 2, None)), Err(AdapterError::NotConnected));
        assert_eq!(c.last_not_connected_reason(), Some(NotConnectedReason::NoKey));
        assert!(t.requests().is_empty());
    }

    #[test]
    fn funding_parse_rate_limits_are_reported_with_retry_after() {
        let mut r = HttpResponse::with_status(429, "{}");
        r.headers.push(("Retry-After".into(), "3".into()));
        let t = Arc::new(FakeTransport::new().on(BINANCE_INCOME_PATH, Ok(r)));
        let c = BinanceLedgerClient::new(t, secrets(Exchange::Binance), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), BinanceHost::Demo);
        assert_eq!(block_on(c.income_page("BTCUSDT", 1, 2, 1)), Err(AdapterError::RateLimited { retry_after_ms: Some(3000) }));
        let t = Arc::new(FakeTransport::new().on(BYBIT_TRANSACTION_LOG_PATH, Ok(HttpResponse::ok(json!({"retCode": 10006, "retMsg": "Too many visits"}).to_string()))));
        let c = BybitLedgerClient::new(t, secrets(Exchange::Bybit), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), BybitHost::Demo);
        assert_eq!(block_on(c.transaction_log_page(1, 2, None)), Err(AdapterError::RateLimited { retry_after_ms: None }));
    }
}

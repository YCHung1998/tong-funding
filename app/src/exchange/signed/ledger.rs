//! Funding ledger (income / transaction-log) signed GET clients and parsers for Binance and Bybit
//! demo/testnet (change funding-pnl, spec funding-history-fetch). Read-only: only GET requests to
//! the compile-time demo/testnet hosts of `endpoints`. OKX (okx-funding-ledger) reads `bills-archive`
//! through `OkxSignedClient`, i.e. with the demo flag built into every request.
//!
//! UNVERIFIED (task 1.1): the parsers follow the exchanges' public documentation only. The
//! fixtures in `app/tests/fixtures/funding/` are constructed from the docs and say so in their
//! header; the sign of Binance `income`, the units and the page limits must be checked against
//! a real demo response before this is relied on.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::types::{Decimal, Exchange};

use super::endpoints::{BinanceHost, BybitHost, okx_inst_id, okx_symbol};
use super::okx::OkxSignedClient;
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
/// `GET /api/v5/account/bills-archive` (last three months; 5 requests per 2 seconds per the docs).
pub const OKX_BILLS_ARCHIVE_PATH: &str = "/api/v5/account/bills-archive";
/// OKX bill `subType`s of funding: 173 expense, 174 income (UNVERIFIED against `account/subtypes`).
pub const OKX_FUNDING_EXPENSE: &str = "173";
pub const OKX_FUNDING_INCOME: &str = "174";
/// Rows per page requested (documented maximum 100); a full page means there may be more.
pub const OKX_BILLS_LIMIT: u32 = 100;
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

/// OKX bills body (`code` already checked by the signed client; a body with a non-"0" code is
/// refused here too). Only `subType` 173 / 174 rows become entries; `balChg` is the signed change
/// (income positive). A sign that contradicts the sub type, a currency other than USDT, a missing
/// field or an instrument the system cannot name fails the WHOLE page (nothing is written from it).
/// `rows` counts every raw row; a full page gives the last row's `billId` as the next `after`.
pub fn parse_okx_bills(body: &Value) -> Result<LedgerPage, AdapterError> {
    if body.get("code").and_then(Value::as_str).is_some_and(|c| c != "0") {
        return Err(AdapterError::parse("bills: the body reports an error code"));
    }
    let rows = body.get("data").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("bills: missing data list"))?;
    let mut entries = Vec::new();
    for r in rows {
        let sub_type = str_req(r, "subType")?;
        if sub_type != OKX_FUNDING_EXPENSE && sub_type != OKX_FUNDING_INCOME {
            continue;
        }
        let amount = dec_req(r, "balChg")?;
        if (sub_type == OKX_FUNDING_EXPENSE && amount > Decimal::ZERO) || (sub_type == OKX_FUNDING_INCOME && amount < Decimal::ZERO) {
            return Err(AdapterError::parse(format!("bills: subType {sub_type} contradicts the sign of balChg {amount}")));
        }
        let asset = str_req(r, "ccy")?;
        if asset != "USDT" {
            return Err(AdapterError::parse(format!("bills: funding row in {asset}, only USDT is supported")));
        }
        let inst_id = str_req(r, "instId")?;
        let symbol = okx_symbol(&inst_id).ok_or_else(|| AdapterError::parse(format!("bills: funding row of an instrument the system cannot name ({inst_id})")))?;
        let ts: i64 = str_req(r, "ts")?.parse().map_err(|_| AdapterError::parse("bills: ts is not an integer"))?;
        entries.push(FundingLedgerEntry::new(Exchange::Okx, symbol, amount, asset, ts, str_req(r, "billId")?, sub_type, r.clone()));
    }
    let next_cursor = match rows.last() {
        Some(last) if rows.len() >= OKX_BILLS_LIMIT as usize => Some(str_req(last, "billId")?),
        _ => None,
    };
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

// ---- OKX client ---------------------------------------------------------------------------------

/// OKX funding bills per symbol and window, over the demo-flagged read client.
pub struct OkxLedgerClient<T>(Arc<OkxSignedClient<T>>);

impl<T: HttpTransport> OkxLedgerClient<T> {
    pub fn new(client: Arc<OkxSignedClient<T>>) -> Self {
        OkxLedgerClient(client)
    }

    pub fn last_not_connected_reason(&self) -> Option<NotConnectedReason> {
        self.0.last_not_connected_reason()
    }

    /// One page of funding bills of `symbol` in `[begin_ms, end_ms]` (at most 7 days, our own
    /// window rule); `after` = the previous page's last `billId` (results are newest first).
    pub async fn bills_page(&self, symbol: &str, begin_ms: i64, end_ms: i64, after: Option<&str>) -> Result<LedgerPage, AdapterError> {
        let inst_id = okx_inst_id(symbol).ok_or_else(|| AdapterError::parse("symbol is not a BASEUSDT symbol"))?;
        let mut params = vec![
            ("instType", "SWAP".to_string()),
            ("instId", inst_id),
            ("type", "8".to_string()),
            ("begin", begin_ms.to_string()),
            ("end", end_ms.to_string()),
            ("limit", OKX_BILLS_LIMIT.to_string()),
        ];
        if let Some(a) = after {
            params.push(("after", a.to_string()));
        }
        let (body, _) = self.0.get_signed(OKX_BILLS_ARCHIVE_PATH, &encode_query(&params)).await?;
        parse_okx_bills(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;

    use serde_json::json;
    use tong_funding_core::types::Decimal;

    use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, OkxHost};
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

    // ---- OKX bills-archive (okx-funding-ledger; fixtures are hand-built from the docs) ----

    fn okx_fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/funding/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["_fixture_note"].as_str().unwrap().starts_with("依文件構造、未在真實 demo 帳戶驗證"), "fixtures must say they are not recorded responses");
        v["response"].clone()
    }

    #[test]
    fn okx_parse_a_funding_expense_row_becomes_a_signed_entry_on_the_system_symbol() {
        let page = parse_okx_bills(&okx_fixture("okx_bills_funding_page1_full")).unwrap();
        let e = page.entries.iter().find(|e| e.exchange_id == "623950854533513219").unwrap();
        assert_eq!((e.exchange, e.symbol.as_str(), e.amount, e.asset.as_str()), (Exchange::Okx, "BTCUSDT", d("-0.42"), "USDT"));
        assert_eq!((e.settled_at_ms, e.kind.as_str()), (1_700_000_000_000, "173"));
        assert_eq!(e.dedupe_key, "okx:173:623950854533513219");
        assert_eq!(e.raw["balChg"], json!("-0.42"), "the whole original row is kept");
    }

    #[test]
    fn okx_parse_keeps_only_173_and_174_counts_raw_rows_and_gives_the_last_bill_id_as_cursor() {
        let page = parse_okx_bills(&okx_fixture("okx_bills_funding_page1_full")).unwrap();
        assert_eq!(page.rows, 100, "raw rows, including the trade rows");
        assert_eq!(page.entries.len(), 91, "the nine trade rows (type 2) are not funding");
        assert!(page.entries.iter().all(|e| e.kind == "173" || e.kind == "174"));
        assert_eq!(page.next_cursor.as_deref(), Some("623950854533513120"), "full page: the last billId is `after` of the next page");
        let short = parse_okx_bills(&okx_fixture("okx_bills_funding_page2_short")).unwrap();
        assert_eq!((short.rows, short.entries.len(), short.next_cursor), (12, 12, None), "fewer than the limit: exhausted");
        // income is positive
        assert!(page.entries.iter().any(|e| e.kind == "174" && e.amount > Decimal::ZERO));
    }

    #[test]
    fn okx_parse_a_sign_that_contradicts_the_subtype_fails_the_whole_page() {
        let e = parse_okx_bills(&okx_fixture("okx_bills_sign_mismatch")).unwrap_err();
        assert!(matches!(e, AdapterError::Parse(_)) && e.to_string().contains("174"), "{e}");
        // and 173 with a positive amount
        let body = json!({"code":"0","data":[{"billId":"1","type":"8","subType":"173","balChg":"0.5","ccy":"USDT","instId":"BTC-USDT-SWAP","ts":"1"}]});
        assert!(matches!(parse_okx_bills(&body), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn okx_parse_a_non_usdt_currency_or_a_missing_field_fails_instead_of_being_skipped() {
        assert!(matches!(parse_okx_bills(&okx_fixture("okx_bills_not_usdt")), Err(AdapterError::Parse(_))));
        let no_id = json!({"code":"0","data":[{"type":"8","subType":"173","balChg":"-1","ccy":"USDT","instId":"BTC-USDT-SWAP","ts":"1"}]});
        assert!(matches!(parse_okx_bills(&no_id), Err(AdapterError::Parse(_))));
        let bad_inst = json!({"code":"0","data":[{"billId":"1","type":"8","subType":"173","balChg":"-1","ccy":"USDT","instId":"BTC-USD-SWAP","ts":"1"}]});
        assert!(matches!(parse_okx_bills(&bad_inst), Err(AdapterError::Parse(_))), "a funding row of an instrument the system cannot name");
        assert!(matches!(parse_okx_bills(&json!({"code":"0"})), Err(AdapterError::Parse(_))));
    }

    fn okx_client(t: &Arc<FakeTransport>, secrets: Arc<dyn SecretProvider>) -> OkxLedgerClient<FakeTransport> {
        let signed = OkxSignedClient::new(t.clone(), secrets, Arc::new(ManualClock::new(1_607_418_537_000)), Arc::new(|| Some(715)), Arc::new(NoResync), OkxHost::Demo);
        OkxLedgerClient::new(Arc::new(signed))
    }

    fn okx_secrets() -> Arc<dyn SecretProvider> {
        Arc::new(
            MemorySecrets::default()
                .with(Exchange::Okx, SecretName::ApiKey, "TEST_KEY_NOT_REAL")
                .with(Exchange::Okx, SecretName::ApiSecret, "TEST_SECRET_NOT_REAL")
                .with(Exchange::Okx, SecretName::Passphrase, "TEST_PASS_NOT_REAL"),
        )
    }

    #[test]
    fn okx_client_sends_the_documented_filters_with_the_demo_flag_and_passes_after_on() {
        let body = okx_fixture("okx_bills_funding_page2_short").to_string();
        let t = Arc::new(FakeTransport::new().on(OKX_BILLS_ARCHIVE_PATH, Ok(HttpResponse::ok(body))));
        let c = okx_client(&t, okx_secrets());
        let page = block_on(c.bills_page("BTCUSDT", 10, 20, Some("623950854533513120"))).unwrap();
        assert_eq!(page.entries.len(), 12);
        let reqs = t.requests();
        assert_eq!(reqs.len(), 1);
        let url = &reqs[0].url;
        assert!(url.starts_with("https://openapi.okx.com/api/v5/account/bills-archive?"), "{url}");
        for p in ["instType=SWAP", "instId=BTC-USDT-SWAP", "type=8", "begin=10", "end=20", "limit=100", "after=623950854533513120"] {
            assert!(url.contains(p), "{p} missing in {url}");
        }
        let flags: Vec<&str> = reqs[0].headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case("x-simulated-trading")).map(|(_, v)| v.as_str()).collect();
        assert_eq!(flags, vec!["1"]);
        // the first page has no `after`
        let t = Arc::new(FakeTransport::new().on(OKX_BILLS_ARCHIVE_PATH, Ok(HttpResponse::ok(okx_fixture("okx_bills_funding_page2_short").to_string()))));
        block_on(okx_client(&t, okx_secrets()).bills_page("BTCUSDT", 10, 20, None)).unwrap();
        assert!(!t.requests()[0].url.contains("after="));
    }

    #[test]
    fn okx_client_a_rejected_timestamp_is_retried_once_and_rate_limits_are_reported() {
        let ok = okx_fixture("okx_bills_funding_page2_short").to_string();
        let expired = json!({"code":"50102","msg":"Timestamp request expired","data":[]}).to_string();
        let t = Arc::new(FakeTransport::new().on(OKX_BILLS_ARCHIVE_PATH, Ok(HttpResponse::ok(expired))).on(OKX_BILLS_ARCHIVE_PATH, Ok(HttpResponse::ok(ok))));
        let c = okx_client(&t, okx_secrets());
        assert!(block_on(c.bills_page("BTCUSDT", 1, 2, None)).is_ok());
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2);
        assert!(reqs.iter().all(|r| r.headers.iter().any(|(n, v)| n.eq_ignore_ascii_case("x-simulated-trading") && v == "1")), "the retry carries the flag too");
        let limited = json!({"code":"50011","msg":"Too Many Requests","data":[]}).to_string();
        let t = Arc::new(FakeTransport::new().on(OKX_BILLS_ARCHIVE_PATH, Ok(HttpResponse::ok(limited))));
        assert_eq!(block_on(okx_client(&t, okx_secrets()).bills_page("BTCUSDT", 1, 2, None)), Err(AdapterError::RateLimited { retry_after_ms: None }));
    }

    #[test]
    fn okx_client_without_a_passphrase_sends_nothing() {
        let t = Arc::new(FakeTransport::new());
        let no_pass: Arc<dyn SecretProvider> = Arc::new(MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, "k").with(Exchange::Okx, SecretName::ApiSecret, "s"));
        assert_eq!(block_on(okx_client(&t, no_pass).bills_page("BTCUSDT", 1, 2, None)), Err(AdapterError::NotConnected));
        assert!(t.requests().is_empty());
    }

    #[test]
    fn okx_client_rejects_a_symbol_that_is_not_base_usdt_before_any_request() {
        let t = Arc::new(FakeTransport::new());
        assert!(matches!(block_on(okx_client(&t, okx_secrets()).bills_page("BTCUSD", 1, 2, None)), Err(AdapterError::Parse(_))));
        assert!(t.requests().is_empty());
    }

    /// Real-machine probe (okx-funding-ledger task 3.2). `#[ignore]`: it reads the macOS Keychain and
    /// sends GETs to OKX, so only the user runs it, after a demo position crossed a settlement:
    ///   cargo test -p tong-funding okx_live_bills_probe -- --ignored --nocapture
    /// Optional: `TONG_OKX_SYMBOL=ETHUSDT`. Only `bills-archive` GETs, all carrying the demo flag.
    #[test]
    #[ignore = "reads the real Keychain and sends GETs to OKX; the user runs it (okx-funding-ledger 3.2)"]
    fn okx_live_bills_probe() {
        use crate::exchange::health::clock_sync::ClockSync;
        use crate::exchange::public::endpoints::OKX_HOST;
        use crate::exchange::reqwest_transport::ReqwestTransport;
        use crate::ports::SystemClock;
        use crate::store::secrets::BundleSecrets;
        let symbol = std::env::var("TONG_OKX_SYMBOL").unwrap_or_else(|_| "BTCUSDT".into());
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let clock = Arc::new(SystemClock);
            let public = ReqwestTransport::public_production().expect("public transport");
            let off = ClockSync::new(clock.clone()).sync_once(&public, Exchange::Okx, OKX_HOST).await.expect("OKX time").offset_ms;
            let signed = OkxSignedClient::new(
                Arc::new(ReqwestTransport::signed_demo().expect("signed transport")),
                Arc::new(BundleSecrets::system()),
                clock.clone(),
                Arc::new(move || Some(off)),
                Arc::new(NoResync),
                OkxHost::Demo,
            );
            let client = OkxLedgerClient::new(Arc::new(signed));
            let now = crate::ports::Clock::now_ms(clock.as_ref());
            let page = client.bills_page(&symbol, now - 7 * 24 * 3_600_000, now, None).await;
            match page {
                Ok(p) => {
                    println!("rows {} (funding entries {}); next cursor {:?}", p.rows, p.entries.len(), p.next_cursor);
                    for e in &p.entries {
                        println!("  {} {} {} {} (subType {}, billId {})", e.settled_at_ms, e.symbol, e.amount, e.asset, e.kind, e.exchange_id);
                    }
                }
                Err(e) => println!("bills page failed: {e}"),
            }
        });
    }
}
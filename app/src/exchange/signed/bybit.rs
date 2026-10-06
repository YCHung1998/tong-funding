//! Bybit v5 signed GET client (Demo Trading host only). Read-only: balances, positions and open
//! orders, with lists fetched across all pages. Spec: signed-read-access.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tong_funding_core::redact::redact_secrets;
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::endpoints::{
    BYBIT_BALANCE_PATH, BYBIT_OPEN_ORDERS_PATH, BYBIT_ORDERS_PAGE_LIMIT, BYBIT_POSITIONS_PAGE_LIMIT, BYBIT_POSITIONS_PATH, BybitHost, MAX_PAGES,
};
use super::models::{Balance, Completeness, Listing, OpenOrder, OrderSide, Position, PositionMode, bool_opt, dec_opt, dec_req, str_opt, str_req};
use super::signing::{
    ClockOffsetSource, NotConnectedReason, RECV_WINDOW_MS, Resync, SIGNED_TIMEOUT, bybit_signature, check_status, encode_query, load_credentials, parse_json, sanitize_error,
    encode_cursor, is_timestamp_rejected, require_offset,
};

/// Bybit `retCode` for "Too many visits" (rate limit), delivered with HTTP 200.
const BYBIT_RATE_LIMIT_RET_CODE: i64 = 10006;
use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpTransport};
use crate::ports::{Clock, SecretProvider};

/// Bybit `accountType` of `wallet-balance`. `.env.example`: UNIFIED is the default for new demo
/// accounts, CONTRACT only for accounts that predate UTA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BybitAccountType {
    Unified,
    Contract,
}

pub struct BybitSignedClient<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn Clock>,
    offset: Arc<dyn ClockOffsetSource>,
    resync: Arc<dyn Resync>,
    host: BybitHost,
    account_type: BybitAccountType,
    reason: Mutex<Option<NotConnectedReason>>,
}

impl<T: HttpTransport> BybitSignedClient<T> {
    pub const EXCHANGE: Exchange = Exchange::Bybit;

    /// `clock` + `offset` produce the calibrated exchange time used for signing and `fetched_at`;
    /// `resync` is called once when a timestamp is rejected.
    pub fn new(
        transport: Arc<T>,
        secrets: Arc<dyn SecretProvider>,
        clock: Arc<dyn Clock>,
        offset: Arc<dyn ClockOffsetSource>,
        resync: Arc<dyn Resync>,
        demo_env: BybitHost,
    ) -> Self {
        BybitSignedClient { transport, secrets, clock, offset, resync, host: demo_env, account_type: BybitAccountType::Unified, reason: Mutex::new(None) }
    }

    /// Why the most recent signed call answered `NotConnected` (`None` if it did not, or no call yet).
    pub fn last_not_connected_reason(&self) -> Option<NotConnectedReason> {
        self.reason.lock().ok().and_then(|g| *g)
    }

    pub fn with_account_type(mut self, account_type: BybitAccountType) -> Self {
        self.account_type = account_type;
        self
    }

    /// `GET /v5/account/wallet-balance`: every coin of every returned account.
    pub async fn get_balances(&self) -> Result<Vec<Balance>, AdapterError> {
        let (body, at) = self.signed_get(BYBIT_BALANCE_PATH, &encode_query(&[("accountType", self.account_type_param().to_string())])).await?;
        let mut out = Vec::new();
        for account in result_list(&body)? {
            let coins = account.get("coin").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing coin list"))?;
            for c in coins {
                out.push(parse_balance(c, at)?);
            }
        }
        Ok(out)
    }

    /// Available margin (USDT) from the same `wallet-balance` request; see [`available_margin_from`].
    pub async fn get_available_margin(&self) -> Result<Decimal, AdapterError> {
        let (body, _) = self.signed_get(BYBIT_BALANCE_PATH, &encode_query(&[("accountType", self.account_type_param().to_string())])).await?;
        available_margin_from(&body, self.account_type)
    }

    fn account_type_param(&self) -> &'static str {
        match self.account_type {
            BybitAccountType::Unified => "UNIFIED",
            BybitAccountType::Contract => "CONTRACT",
        }
    }

    /// `GET /v5/position/list` (linear, USDT-settled): non-zero positions across all pages.
    /// A failure after the first page yields `Incomplete` with the rows already fetched.
    pub async fn get_positions(&self) -> Result<Listing<Position>, AdapterError> {
        self.paged(BYBIT_POSITIONS_PATH, BYBIT_POSITIONS_PAGE_LIMIT, parse_position).await
    }

    /// `GET /v5/order/realtime` (linear, USDT-settled): open orders across all pages.
    pub async fn get_open_orders(&self) -> Result<Listing<OpenOrder>, AdapterError> {
        self.paged(BYBIT_OPEN_ORDERS_PATH, BYBIT_ORDERS_PAGE_LIMIT, |r, at| parse_order(r, at).map(Some)).await
    }

    async fn paged<R>(&self, path: &str, limit: u32, parse_row: impl Fn(&Value, i64) -> Result<Option<R>, AdapterError>) -> Result<Listing<R>, AdapterError> {
        let mut items: Vec<R> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = None;
        for page in 1..=MAX_PAGES {
            let mut query = encode_query(&[("category", "linear".to_string()), ("settleCoin", "USDT".to_string()), ("limit", limit.to_string())]);
            if let Some(c) = &cursor {
                query.push_str("&cursor=");
                query.push_str(&encode_cursor(c));
            }
            let fetched = self.signed_get(path, &query).await.and_then(|(body, at)| {
                let mut rows = Vec::new();
                for r in result_list(&body)? {
                    if let Some(x) = parse_row(r, at)? {
                        rows.push(x);
                    }
                }
                Ok((rows, next_cursor(&body)))
            });
            match fetched {
                // Nothing fetched yet: report the failure itself, never an empty listing.
                Err(e) if page == 1 => return Err(e),
                Err(e) => {
                    let reason = format!("page {page} failed after {} rows: {e}", items.len());
                    return Ok(incomplete(items, reason));
                }
                Ok((rows, next)) => {
                    items.extend(rows);
                    match next {
                        None => return Ok(Listing { items, completeness: Completeness::Complete }),
                        Some(c) if !seen.insert(c.clone()) => return Ok(incomplete(items, format!("cursor repeated at page {page}"))),
                        Some(c) => cursor = Some(c),
                    }
                }
            }
        }
        Ok(incomplete(items, format!("page cap of {MAX_PAGES} reached")))
    }

    /// One attempt; if the exchange rejects the timestamp, re-sync the clock once and send exactly
    /// one more request (a second rejection, or a failed re-sync, is returned as is).
    /// `query` is the final, already-encoded query string: the same text is signed and sent.
    async fn signed_get(&self, path: &str, query: &str) -> Result<(Value, i64), AdapterError> {
        match self.attempt(path, query).await {
            Err(e) if is_timestamp_rejected(&e) => {
                self.resync.resync().await.map_err(sanitize_error)?;
                self.attempt(path, query).await
            }
            other => other,
        }
    }

    fn set_reason(&self, reason: Option<NotConnectedReason>) {
        if let Ok(mut g) = self.reason.lock() {
            *g = reason;
        }
    }

    /// Order of checks matters: secrets, then calibrated time, and only then a request is built.
    /// Returns the whole parsed body (retCode already checked) and the exchange time (local clock +
    /// offset) at which the response arrived, using the same offset as the signature.
    async fn attempt(&self, path: &str, query: &str) -> Result<(Value, i64), AdapterError> {
        let prepared = load_credentials(self.secrets.as_ref(), Self::EXCHANGE, false).and_then(|c| require_offset(self.offset.as_ref()).map(|o| (c, o)));
        let (creds, offset_ms) = match prepared {
            Ok(v) => v,
            Err(reason) => {
                self.set_reason(Some(reason));
                return Err(reason.into());
            }
        };
        self.set_reason(None);
        let timestamp = self.clock.now_ms().saturating_add(offset_ms);
        let signature = bybit_signature(&creds.api_secret, timestamp, &creds.api_key, RECV_WINDOW_MS, query)?;
        let url = format!("{}{}?{}", self.host.base_url(), path, query);
        let request = HttpRequest::get(url, SIGNED_TIMEOUT)
            .header("X-BAPI-API-KEY", &creds.api_key)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-TIMESTAMP", &timestamp.to_string())
            .header("X-BAPI-RECV-WINDOW", &RECV_WINDOW_MS.to_string());
        let response = self.transport.get(request).await.map_err(sanitize_error)?;
        let fetched_at = self.clock.now_ms().saturating_add(offset_ms);
        let response = check_status(response)?;
        let body = parse_json(&response.body)?;
        let code = body.get("retCode").and_then(Value::as_i64).ok_or_else(|| AdapterError::parse("missing retCode"))?;
        if code == BYBIT_RATE_LIMIT_RET_CODE {
            // HTTP 200 whose body says "too many visits"; no Retry-After is given.
            return Err(AdapterError::RateLimited { retry_after_ms: None });
        }
        if code != 0 {
            let msg = body.get("retMsg").and_then(Value::as_str).unwrap_or("");
            return Err(AdapterError::exchange(code.to_string(), msg));
        }
        Ok((body, fetched_at))
    }
}

fn incomplete<R>(items: Vec<R>, reason: String) -> Listing<R> {
    Listing { items, completeness: Completeness::Incomplete { reason: redact_secrets(&reason) } }
}

fn result_list(body: &Value) -> Result<&Vec<Value>, AdapterError> {
    body.pointer("/result/list").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing result.list"))
}

fn next_cursor(body: &Value) -> Option<String> {
    body.pointer("/result/nextPageCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string)
}

fn parse_balance(c: &Value, fetched_at: i64) -> Result<Balance, AdapterError> {
    let asset = str_req(c, "coin")?;
    let amount = dec_req(c, "walletBalance")?;
    // Only a valuation the exchange provides is used (USDT is its own value); nothing is estimated.
    let usdt_value = match dec_opt(c, "usdValue")? {
        Some(v) => Some(v),
        None if asset == "USDT" => Some(amount),
        None => None,
    };
    Ok(Balance { exchange: Exchange::Bybit, asset, amount, available: dec_opt(c, "availableToWithdraw")?, usdt_value, fetched_at })
}

/// Bybit `size` is unsigned; `side` (Buy/Sell) gives the direction. Zero size (side may be "") is skipped.
fn parse_position(r: &Value, fetched_at: i64) -> Result<Option<Position>, AdapterError> {
    let size = dec_req(r, "size")?;
    if size.is_zero() {
        return Ok(None);
    }
    let (side, quantity) = match str_req(r, "side")?.as_str() {
        "Buy" => (Side::Long, size),
        "Sell" => (Side::Short, -size),
        other => return Err(AdapterError::parse(format!("unknown position side {other}"))),
    };
    let mode = match str_opt(r, "positionIdx")?.as_deref() {
        Some("0") => PositionMode::OneWay,
        Some("1" | "2") => PositionMode::Hedge,
        _ => PositionMode::Unknown,
    };
    Ok(Some(Position {
        exchange: Exchange::Bybit,
        symbol: str_req(r, "symbol")?,
        side,
        quantity,
        entry_price: dec_opt(r, "avgPrice")?,
        mark_price: dec_opt(r, "markPrice")?,
        leverage: dec_opt(r, "leverage")?,
        unrealized_pnl: dec_opt(r, "unrealisedPnl")?,
        margin: dec_opt(r, "positionIM")?,
        notional: dec_opt(r, "positionValue")?,
        mode,
        fetched_at,
    }))
}

fn parse_order(r: &Value, fetched_at: i64) -> Result<OpenOrder, AdapterError> {
    let side = match str_req(r, "side")?.as_str() {
        "Buy" => OrderSide::Buy,
        "Sell" => OrderSide::Sell,
        other => return Err(AdapterError::parse(format!("unknown order side {other}"))),
    };
    Ok(OpenOrder {
        exchange: Exchange::Bybit,
        symbol: str_req(r, "symbol")?,
        order_id: str_req(r, "orderId")?,
        side,
        order_type: str_req(r, "orderType")?,
        price: dec_opt(r, "price")?,
        quantity: dec_req(r, "qty")?,
        filled_quantity: dec_req(r, "cumExecQty")?,
        reduce_only: bool_opt(r, "reduceOnly"),
        status: str_req(r, "orderStatus")?,
        fetched_at,
    })
}

/// Available margin from a `wallet-balance` body.
/// UNIFIED: the account's `totalAvailableBalance` (Bybit's own figure for opening positions; "0" and
/// negative values are taken as reported). `availableToWithdraw` is deprecated for UNIFIED since
/// 2025-01-09 (always ""), and nothing is computed in its place: an empty value is an error.
/// CONTRACT: USDT `availableToWithdraw`, as before.
pub fn available_margin_from(body: &Value, account_type: BybitAccountType) -> Result<Decimal, AdapterError> {
    let accounts = result_list(body)?;
    match account_type {
        BybitAccountType::Unified => {
            let account = accounts
                .iter()
                .find(|a| a.get("accountType").and_then(Value::as_str) == Some("UNIFIED"))
                .ok_or_else(|| AdapterError::parse("no UNIFIED account in wallet-balance"))?;
            dec_opt(account, "totalAvailableBalance")?.ok_or_else(|| AdapterError::parse("totalAvailableBalance not reported (isolated margin is not supported)"))
        }
        BybitAccountType::Contract => {
            let usdt = accounts
                .iter()
                .filter_map(|a| a.get("coin").and_then(Value::as_array))
                .flatten()
                .find(|c| c.get("coin").and_then(Value::as_str) == Some("USDT"))
                .ok_or_else(|| AdapterError::parse("no USDT coin in wallet-balance"))?;
            dec_opt(usdt, "availableToWithdraw")?.ok_or_else(|| AdapterError::parse("USDT availableToWithdraw not reported"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tong_funding_core::types::{Decimal, Side};

    use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, MAX_PAGES};
    use crate::exchange::signed::models::{Completeness, OrderSide, PositionMode};
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::{ManualClock, MemorySecrets, SecretName};

    const KEY: &str = "TEST_KEY_NOT_REAL";
    const SECRET: &str = "TEST_SECRET_NOT_REAL";
    const NOW: i64 = 1_700_000_000_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    fn full_secrets() -> MemorySecrets {
        MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY).with(Exchange::Bybit, SecretName::ApiSecret, SECRET)
    }

    struct FakeResync {
        calls: AtomicUsize,
        result: Result<(), AdapterError>,
    }

    impl FakeResync {
        fn ok() -> Arc<Self> {
            Arc::new(FakeResync { calls: AtomicUsize::new(0), result: Ok(()) })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Resync for FakeResync {
        fn resync(&self) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(self.result.clone()))
        }
    }

    struct FailingSecrets;
    impl SecretProvider for FailingSecrets {
        fn get(&self, _e: Exchange, _n: SecretName) -> Result<Option<String>, crate::ports::SecretError> {
            Err(crate::ports::SecretError::Unavailable("keychain locked".into()))
        }
    }

    fn client_full(t: FakeTransport, secrets: Arc<dyn SecretProvider>, offset: Option<i64>, resync: Arc<FakeResync>) -> (Arc<FakeTransport>, BybitSignedClient<FakeTransport>) {
        let t = Arc::new(t);
        let c = BybitSignedClient::new(t.clone(), secrets, Arc::new(ManualClock::new(NOW)), Arc::new(move || offset), resync, BybitHost::Demo);
        (t, c)
    }

    fn client_with(t: FakeTransport, secrets: MemorySecrets, offset: Option<i64>) -> (Arc<FakeTransport>, BybitSignedClient<FakeTransport>) {
        client_full(t, Arc::new(secrets), offset, FakeResync::ok())
    }

    fn ret_code(code: i64, msg: &str) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(json!({"retCode":code,"retMsg":msg,"result":{}}).to_string()))
    }

    // ---- timestamp rejection: resync once, retry exactly once ----

    #[test]
    fn a_rejected_timestamp_is_resynced_once_and_the_request_resent_once() {
        let t = FakeTransport::new().on("/v5/position/list", ret_code(10002, "invalid request, please check your server timestamp")).on("/v5/position/list", page(vec![pos("BTCUSDT", "Buy", "1")], ""));
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), resync.clone());
        assert_eq!(block_on(c.get_positions()).unwrap().items.len(), 1);
        assert_eq!(t.requests().len(), 2);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_second_rejection_is_returned_and_there_is_no_third_request() {
        let t = FakeTransport::new().on("/v5/account/wallet-balance", ret_code(10002, "ts"));
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), resync.clone());
        assert!(matches!(block_on(c.get_balances()), Err(AdapterError::Exchange { code, .. }) if code == "10002"));
        assert_eq!(t.requests().len(), 2);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_failed_resync_returns_its_error_and_does_not_resend() {
        let t = FakeTransport::new().on("/v5/order/realtime", ret_code(10002, "ts"));
        let resync = Arc::new(FakeResync { calls: AtomicUsize::new(0), result: Err(AdapterError::Timeout) });
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), resync.clone());
        assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::Timeout);
        assert_eq!(t.requests().len(), 1);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_rejection_on_a_later_page_is_retried_once_too() {
        let t = FakeTransport::new()
            .on("/v5/order/realtime", page(vec![], "c1"))
            .on("/v5/order/realtime", ret_code(10002, "ts"))
            .on("/v5/order/realtime", page(vec![], ""));
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), resync.clone());
        assert!(block_on(c.get_open_orders()).unwrap().is_complete());
        assert_eq!((t.requests().len(), resync.calls()), (3, 1));
    }

    #[test]
    fn other_errors_do_not_trigger_a_resync() {
        let t = FakeTransport::new().on("/v5/account/wallet-balance", ret_code(10003, "invalid key"));
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), resync.clone());
        assert!(block_on(c.get_balances()).is_err());
        assert_eq!((t.requests().len(), resync.calls()), (1, 0));
    }

    // ---- rate limits ----

    #[test]
    fn retcode_10006_in_a_200_body_is_rate_limited() {
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", ret_code(10006, "Too many visits. Exceeded the API Rate Limit.")));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: None });
    }

    #[test]
    fn retry_after_is_parsed_as_whole_seconds() {
        for (value, expected) in [("3", Some(3000)), (" 12 ", Some(12_000)), ("1.5", None), ("Wed, 21 Oct 2026 07:28:00 GMT", None)] {
            let mut r = HttpResponse::with_status(429, "");
            r.headers.push(("Retry-After".into(), value.into()));
            let (_, c) = client(FakeTransport::new().on("/v5/account/wallet-balance", Ok(r)));
            assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: expected }, "Retry-After: {value}");
        }
    }

    // ---- NotConnected reasons, fetched_at ----

    #[test]
    fn the_not_connected_reason_distinguishes_every_cause() {
        let reason_for = |secrets: Arc<dyn SecretProvider>, offset: Option<i64>| {
            let (t, c) = client_full(FakeTransport::new(), secrets, offset, FakeResync::ok());
            assert_eq!(c.last_not_connected_reason(), None, "no call yet");
            assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(t.requests().len(), 0);
            c.last_not_connected_reason()
        };
        let key_only = MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY);
        assert_eq!(reason_for(Arc::new(MemorySecrets::default()), Some(0)), Some(NotConnectedReason::NoKey));
        assert_eq!(reason_for(Arc::new(key_only), Some(0)), Some(NotConnectedReason::NoSecret));
        assert_eq!(reason_for(Arc::new(FailingSecrets), Some(0)), Some(NotConnectedReason::SecretStoreError));
        assert_eq!(reason_for(Arc::new(full_secrets()), None), Some(NotConnectedReason::ClockUnsynced));
    }

    #[test]
    fn the_reason_clears_after_a_successful_call() {
        let t = Arc::new(FakeTransport::new().on("/v5/position/list", page(vec![], "")));
        let ready = Arc::new(AtomicBool::new(false));
        let r2 = ready.clone();
        let c = BybitSignedClient::new(t, Arc::new(full_secrets()), Arc::new(ManualClock::new(NOW)), Arc::new(move || r2.load(Ordering::SeqCst).then_some(0)), FakeResync::ok(), BybitHost::Demo);
        assert!(block_on(c.get_positions()).is_err());
        assert_eq!(c.last_not_connected_reason(), Some(NotConnectedReason::ClockUnsynced));
        ready.store(true, Ordering::SeqCst);
        assert!(block_on(c.get_positions()).is_ok());
        assert_eq!(c.last_not_connected_reason(), None);
    }

    #[test]
    fn fetched_at_is_exchange_time_from_the_same_offset_as_the_signature() {
        let (_, c) = client_with(FakeTransport::new().on("/v5/position/list", page(vec![pos("BTCUSDT", "Buy", "1")], "")), full_secrets(), Some(-300));
        assert_eq!(block_on(c.get_positions()).unwrap().items[0].fetched_at, NOW - 300);
    }

    fn client(t: FakeTransport) -> (Arc<FakeTransport>, BybitSignedClient<FakeTransport>) {
        client_with(t, full_secrets(), Some(1200))
    }

    fn page(rows: Vec<Value>, cursor: &str) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(json!({"retCode":0,"retMsg":"OK","result":{"list":rows,"nextPageCursor":cursor,"category":"linear"},"time":1700000000000i64}).to_string()))
    }

    fn pos(symbol: &str, side: &str, size: &str) -> Value {
        json!({"symbol":symbol,"side":side,"size":size,"avgPrice":"100.5","markPrice":"101.5","leverage":"10","unrealisedPnl":"1.25","positionIM":"10.05","positionValue":"1005","positionIdx":0})
    }

    fn host_of(url: &str) -> &str {
        url.strip_prefix("https://").unwrap().split('/').next().unwrap()
    }

    fn header<'a>(r: &'a crate::exchange::transport::HttpRequest, name: &str) -> &'a str {
        r.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or("")
    }

    // ---- signing / request shape ----

    #[test]
    fn positions_request_is_signed_per_bybit_v5() {
        let (t, c) = client(FakeTransport::new().on("/v5/position/list", page(vec![], "")));
        let r = block_on(c.get_positions()).unwrap();
        assert!(r.is_complete() && r.items.is_empty());
        let reqs = t.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url, "https://api-demo.bybit.com/v5/position/list?category=linear&settleCoin=USDT&limit=200");
        // signature precomputed with Python: hmac(secret, "1700000001200"+key+"5000"+query)
        assert_eq!(
            reqs[0].headers,
            vec![
                ("X-BAPI-API-KEY".to_string(), KEY.to_string()),
                ("X-BAPI-SIGN".to_string(), "429d142587bebbfdd9e78d247e8340e26b434d9bf329584048ace1c7ac1985b9".to_string()),
                ("X-BAPI-TIMESTAMP".to_string(), "1700000001200".to_string()),
                ("X-BAPI-RECV-WINDOW".to_string(), "5000".to_string()),
            ]
        );
        assert_eq!(reqs[0].timeout, Duration::from_secs(5));
    }

    #[test]
    fn balance_and_orders_requests_are_signed() {
        let bal = json!({"retCode":0,"retMsg":"OK","result":{"list":[{"accountType":"UNIFIED","coin":[]}]}});
        let (t, c) = client(FakeTransport::new().on("/v5/account/wallet-balance", Ok(HttpResponse::ok(bal.to_string()))).on("/v5/order/realtime", page(vec![], "")));
        block_on(c.get_balances()).unwrap();
        block_on(c.get_open_orders()).unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].url, "https://api-demo.bybit.com/v5/account/wallet-balance?accountType=UNIFIED");
        assert_eq!(header(&reqs[0], "X-BAPI-SIGN"), "592e25bf9b6e5b4142eb00e03ed39a9e0202ee3ba4a5e5b6abc49ff77fc1ffe4");
        assert_eq!(reqs[1].url, "https://api-demo.bybit.com/v5/order/realtime?category=linear&settleCoin=USDT&limit=50");
        assert_eq!(header(&reqs[1], "X-BAPI-SIGN"), "adb92f6988b4d499da9720a3b552b70d0f9db74f40b6de6b67b837c755ca1079");
        assert!(reqs.iter().all(|r| !r.url.contains(SECRET) && r.headers.iter().all(|(_, v)| !v.contains(SECRET))));
    }

    #[test]
    fn contract_account_type_can_be_selected() {
        let bal = json!({"retCode":0,"retMsg":"OK","result":{"list":[]}});
        let t = Arc::new(FakeTransport::new().on("/v5/account/wallet-balance", Ok(HttpResponse::ok(bal.to_string()))));
        let c = BybitSignedClient::new(t.clone(), Arc::new(full_secrets()), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), FakeResync::ok(), BybitHost::Demo).with_account_type(BybitAccountType::Contract);
        block_on(c.get_balances()).unwrap();
        assert!(t.requests()[0].url.ends_with("?accountType=CONTRACT"));
    }

    // ---- available margin (bybit-uta-available-margin) ----

    fn wallet(total_available: &str, coins: Value) -> Value {
        json!({"retCode":0,"retMsg":"OK","result":{"list":[{"accountType":"UNIFIED","totalAvailableBalance":total_available,"coin":coins}]}})
    }

    #[test]
    fn unified_margin_is_the_account_total_available_balance() {
        let body = wallet("8123.45", json!([{"coin":"USDT","walletBalance":"9000","availableToWithdraw":""}]));
        assert_eq!(available_margin_from(&body, BybitAccountType::Unified).unwrap(), d("8123.45"));
    }

    #[test]
    fn unified_margin_zero_and_negative_are_taken_as_reported() {
        let coins = json!([{"coin":"USDT","walletBalance":"1000","availableToWithdraw":"500"}]);
        assert_eq!(available_margin_from(&wallet("0", coins.clone()), BybitAccountType::Unified).unwrap(), d("0"));
        assert_eq!(available_margin_from(&wallet("-12.5", coins), BybitAccountType::Unified).unwrap(), d("-12.5"));
    }

    #[test]
    fn unified_margin_empty_is_an_error_naming_the_field_and_never_computed() {
        let body = wallet("", json!([{"coin":"USDT","walletBalance":"1000","totalPositionIM":"200","totalOrderIM":"50","locked":""}]));
        let e = available_margin_from(&body, BybitAccountType::Unified).unwrap_err().to_string();
        assert!(e.contains("totalAvailableBalance"), "{e}");
    }

    #[test]
    fn unified_margin_reads_only_the_unified_account() {
        let body = json!({"retCode":0,"retMsg":"OK","result":{"list":[
            {"accountType":"FUND","totalAvailableBalance":"5","coin":[]},
            {"accountType":"UNIFIED","totalAvailableBalance":"7","coin":[]}
        ]}});
        assert_eq!(available_margin_from(&body, BybitAccountType::Unified).unwrap(), d("7"));
        let none = json!({"retCode":0,"retMsg":"OK","result":{"list":[{"accountType":"FUND","totalAvailableBalance":"5","coin":[]}]}});
        assert!(available_margin_from(&none, BybitAccountType::Unified).is_err());
    }

    #[test]
    fn unified_margin_never_uses_the_deprecated_available_to_withdraw() {
        let body = wallet("", json!([{"coin":"BTC","walletBalance":"1","availableToWithdraw":"999"}]));
        assert!(available_margin_from(&body, BybitAccountType::Unified).is_err());
    }

    #[test]
    fn contract_margin_is_still_usdt_available_to_withdraw() {
        let body = json!({"retCode":0,"retMsg":"OK","result":{"list":[{"accountType":"CONTRACT","totalAvailableBalance":"","coin":[{"coin":"USDT","walletBalance":"100","availableToWithdraw":"90"}]}]}});
        assert_eq!(available_margin_from(&body, BybitAccountType::Contract).unwrap(), d("90"));
        let empty = json!({"retCode":0,"retMsg":"OK","result":{"list":[{"coin":[{"coin":"USDT","walletBalance":"100","availableToWithdraw":""}]}]}});
        assert!(available_margin_from(&empty, BybitAccountType::Contract).is_err());
    }

    #[test]
    fn get_available_margin_reads_the_unified_wallet_balance() {
        let body = wallet("8123.45", json!([{"coin":"USDT","walletBalance":"9000","availableToWithdraw":""}]));
        let (t, c) = client(FakeTransport::new().on("/v5/account/wallet-balance", Ok(HttpResponse::ok(body.to_string()))));
        assert_eq!(block_on(c.get_available_margin()).unwrap(), d("8123.45"));
        assert_eq!(t.requests()[0].url, "https://api-demo.bybit.com/v5/account/wallet-balance?accountType=UNIFIED");
    }

    // ---- normalisation ----

    #[test]
    fn balances_keep_coins_and_only_use_a_provided_usd_value() {
        let bal = json!({"retCode":0,"retMsg":"OK","result":{"list":[{"accountType":"UNIFIED","coin":[
            {"coin":"BTC","walletBalance":"0.05","availableToWithdraw":"","usdValue":""},
            {"coin":"ETH","walletBalance":"2","availableToWithdraw":"1.5","usdValue":"5000.25"},
            {"coin":"USDT","walletBalance":"100","availableToWithdraw":"90"}
        ]}]}});
        let (_, c) = client(FakeTransport::new().on("/v5/account/wallet-balance", Ok(HttpResponse::ok(bal.to_string()))));
        let b = block_on(c.get_balances()).unwrap();
        assert_eq!(b.len(), 3);
        let btc = &b[0];
        assert_eq!((btc.asset.as_str(), btc.amount, btc.usdt_value, btc.available), ("BTC", d("0.05"), None, None));
        assert_eq!((b[1].usdt_value, b[1].available), (Some(d("5000.25")), Some(d("1.5"))));
        assert_eq!((b[2].usdt_value, b[2].exchange, b[2].fetched_at), (Some(d("100")), Exchange::Bybit, NOW + 1200));
    }

    #[test]
    fn position_side_gives_the_sign_and_zero_rows_are_dropped() {
        let rows = vec![pos("BTCUSDT", "Buy", "0.019934"), pos("ETHUSDT", "Sell", "3.5"), pos("XRPUSDT", "", "0"), pos("SOLUSDT", "Buy", "0")];
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", page(rows, "")));
        let r = block_on(c.get_positions()).unwrap();
        assert_eq!(r.items.len(), 2);
        assert_eq!((r.items[0].quantity, r.items[0].side), (d("0.019934"), Side::Long));
        assert_eq!(r.items[0].quantity.to_string(), "0.019934");
        assert_eq!((r.items[1].quantity, r.items[1].side), (d("-3.5"), Side::Short));
        let p = &r.items[1];
        assert_eq!((p.entry_price, p.mark_price, p.leverage), (Some(d("100.5")), Some(d("101.5")), Some(d("10"))));
        assert_eq!((p.unrealized_pnl, p.margin, p.notional), (Some(d("1.25")), Some(d("10.05")), Some(d("1005"))));
        assert_eq!((p.mode, p.exchange, p.fetched_at), (PositionMode::OneWay, Exchange::Bybit, NOW + 1200));
    }

    #[test]
    fn hedge_index_and_unknown_mode() {
        let mut hedge = pos("BTCUSDT", "Sell", "1");
        hedge["positionIdx"] = json!(2);
        let mut unknown = pos("ETHUSDT", "Buy", "1");
        unknown.as_object_mut().unwrap().remove("positionIdx");
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", page(vec![hedge, unknown], "")));
        let r = block_on(c.get_positions()).unwrap();
        assert_eq!((r.items[0].mode, r.items[1].mode), (PositionMode::Hedge, PositionMode::Unknown));
    }

    #[test]
    fn nonzero_size_with_an_unknown_side_is_a_parse_error() {
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", page(vec![pos("BTCUSDT", "None", "1")], "")));
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn open_orders_are_normalised() {
        let row = json!({"orderId":"abc-123","symbol":"BTCUSDT","side":"Buy","orderType":"Limit","price":"60000.5","qty":"0.010","cumExecQty":"0.002","orderStatus":"PartiallyFilled","reduceOnly":false});
        let (_, c) = client(FakeTransport::new().on("/v5/order/realtime", page(vec![row], "")));
        let o = block_on(c.get_open_orders()).unwrap();
        assert!(o.is_complete());
        let o = &o.items[0];
        assert_eq!((o.order_id.as_str(), o.side, o.reduce_only), ("abc-123", OrderSide::Buy, false));
        assert_eq!((o.price, o.quantity, o.filled_quantity), (Some(d("60000.5")), d("0.010"), d("0.002")));
        assert_eq!((o.order_type.as_str(), o.status.as_str()), ("Limit", "PartiallyFilled"));
    }

    // ---- retCode ----

    #[test]
    fn http_200_with_nonzero_retcode_is_an_exchange_error() {
        let body = json!({"retCode":10003,"retMsg":"API key is invalid.","result":{},"time":1}).to_string();
        for which in ["balance", "positions", "orders"] {
            let (_, c) = client(FakeTransport::new().on("/v5/", Ok(HttpResponse::ok(body.clone()))));
            let err = match which {
                "balance" => block_on(c.get_balances()).unwrap_err(),
                "positions" => block_on(c.get_positions()).unwrap_err(),
                _ => block_on(c.get_open_orders()).unwrap_err(),
            };
            assert_eq!(err, AdapterError::Exchange { code: "10003".into(), message: "API key is invalid.".into() });
        }
    }

    #[test]
    fn missing_retcode_is_a_parse_error() {
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", Ok(HttpResponse::ok("{\"result\":{\"list\":[]}}"))));
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Parse(_))));
    }

    // ---- pagination ----

    #[test]
    fn positions_span_all_pages_and_the_cursor_is_signed() {
        let t = FakeTransport::new()
            .on("/v5/position/list", page(vec![pos("BTCUSDT", "Buy", "1")], "page_token%3D1%26"))
            .on("/v5/position/list", page(vec![pos("ETHUSDT", "Sell", "2")], ""));
        let (t, c) = client(t);
        let r = block_on(c.get_positions()).unwrap();
        assert!(r.is_complete());
        assert_eq!(r.items.iter().map(|p| p.symbol.as_str()).collect::<Vec<_>>(), ["BTCUSDT", "ETHUSDT"]);
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2);
        // the cursor is percent-encoded once more; the same string is signed (Python vector)
        assert_eq!(reqs[1].url, "https://api-demo.bybit.com/v5/position/list?category=linear&settleCoin=USDT&limit=200&cursor=page_token%253D1%2526");
        assert_eq!(header(&reqs[1], "X-BAPI-SIGN"), "810377a3198b87d398adc892c81287188f9762d35f4de7b9ae9dcd69fef2aacd");
    }

    #[test]
    fn orders_span_all_pages() {
        let t = FakeTransport::new().on("/v5/order/realtime", page(vec![], "c1")).on("/v5/order/realtime", page(vec![], ""));
        let (t, c) = client(t);
        assert!(block_on(c.get_open_orders()).unwrap().is_complete());
        assert_eq!(t.requests().len(), 2);
        assert!(t.requests()[1].url.contains("&cursor=c1"));
    }

    #[test]
    fn a_failing_second_page_yields_incomplete_with_the_rows_already_fetched() {
        for second in [
            Err(AdapterError::Timeout),
            Ok(HttpResponse::with_status(500, "oops")),
            Ok(HttpResponse::ok(json!({"retCode":10016,"retMsg":"server error","result":{}}).to_string())),
        ] {
            let t = FakeTransport::new().on("/v5/position/list", page(vec![pos("BTCUSDT", "Buy", "1")], "next")).on("/v5/position/list", second);
            let (_, c) = client(t);
            let r = block_on(c.get_positions()).unwrap();
            assert!(!r.is_complete());
            assert!(matches!(r.completeness, Completeness::Incomplete { .. }));
            assert_eq!(r.items.len(), 1);
        }
    }

    #[test]
    fn a_failing_first_page_is_an_error_not_an_empty_listing() {
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", Err(AdapterError::Timeout)));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::Timeout);
    }

    #[test]
    fn a_repeated_cursor_stops_with_incomplete() {
        let t = FakeTransport::new().on("/v5/position/list", page(vec![pos("BTCUSDT", "Buy", "1")], "same")).on("/v5/position/list", page(vec![], "same"));
        let (t, c) = client(t);
        let r = block_on(c.get_positions()).unwrap();
        assert!(!r.is_complete());
        assert_eq!(r.items.len(), 1);
        assert_eq!(t.requests().len(), 2, "must stop instead of looping");
    }

    #[test]
    fn the_page_cap_stops_with_incomplete() {
        let mut t = FakeTransport::new();
        for i in 0..(MAX_PAGES + 5) {
            t = t.on("/v5/order/realtime", page(vec![], &format!("cursor{i}")));
        }
        let (t, c) = client(t);
        let r = block_on(c.get_open_orders()).unwrap();
        assert!(!r.is_complete());
        assert_eq!(t.requests().len(), MAX_PAGES);
    }

    // ---- secrets / calibration / hosts ----

    #[test]
    fn missing_bybit_secrets_mean_not_connected_with_zero_requests() {
        let cases = [
            MemorySecrets::default(),
            MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY),
            // only Binance's secrets exist
            MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, KEY).with(Exchange::Binance, SecretName::ApiSecret, SECRET),
        ];
        for secrets in cases {
            let (t, c) = client_with(FakeTransport::new(), secrets, Some(0));
            assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(t.requests().len(), 0);
        }
    }

    #[test]
    fn missing_binance_secrets_do_not_affect_bybit() {
        // full_secrets() holds Bybit keys only
        let (t, c) = client(FakeTransport::new().on("/v5/position/list", page(vec![], "")));
        assert!(block_on(c.get_positions()).is_ok());
        assert_eq!(t.requests().len(), 1);
    }

    #[test]
    fn uncalibrated_clock_means_not_connected_with_zero_requests() {
        let (t, c) = client_with(FakeTransport::new(), full_secrets(), None);
        assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(t.requests().len(), 0);
    }

    #[test]
    fn every_signed_method_only_reaches_the_demo_host_even_with_an_env_override() {
        // SAFETY: test-only; no other test reads this variable.
        unsafe { std::env::set_var("BYBIT_BASE_URL", "https://prod.example.invalid") };
        let bal = json!({"retCode":0,"retMsg":"OK","result":{"list":[]}});
        let t = FakeTransport::new()
            .on("/v5/account/wallet-balance", Ok(HttpResponse::ok(bal.to_string())))
            .on("/v5/position/list", page(vec![], "x"))
            .on("/v5/position/list", page(vec![], ""))
            .on("/v5/order/realtime", page(vec![], ""));
        let (t, c) = client(t);
        block_on(c.get_balances()).unwrap();
        block_on(c.get_positions()).unwrap();
        block_on(c.get_open_orders()).unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 4);
        for r in reqs {
            assert_eq!(host_of(&r.url), BybitHost::Demo.host());
            assert!(ALLOWED_SIGNED_HOSTS.contains(&host_of(&r.url)));
        }
        unsafe { std::env::remove_var("BYBIT_BASE_URL") };
    }

    // ---- failures / redaction ----

    #[test]
    fn rate_limit_and_http_errors() {
        let mut limited = HttpResponse::with_status(429, "");
        limited.headers.push(("retry-after".into(), "2".into()));
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", Ok(limited)));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: Some(2000) });
        let (_, c) = client(FakeTransport::new().on("/v5/account/wallet-balance", Ok(HttpResponse::with_status(403, "forbidden"))));
        assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::Http { status: 403 });
        let (_, c) = client(FakeTransport::new().on("/v5/account/wallet-balance", Ok(HttpResponse::ok("<<nope>>"))));
        assert!(matches!(block_on(c.get_balances()), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn error_text_with_signature_headers_or_query_is_redacted() {
        let raw = AdapterError::Network("request failed: X-BAPI-SIGN: deadbeefdeadbeef X-BAPI-API-KEY: TEST_KEY_NOT_REAL url=https://api-demo.bybit.com/v5/position/list?signature=cafebabe".into());
        let (_, c) = client(FakeTransport::new().on("/v5/position/list", Err(raw)));
        let text = block_on(c.get_positions()).unwrap_err().to_string();
        for leaked in ["deadbeefdeadbeef", "TEST_KEY_NOT_REAL", "cafebabe"] {
            assert!(!text.contains(leaked), "leaked {leaked}: {text}");
        }
    }
}

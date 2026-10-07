//! OKX v5 signed ORDER requests (Demo Trading via the `x-simulated-trading: 1` header): market
//! order placement, order details, cancel, and the account-mode reading (`acctLv` / `posMode`).
//! Spec: okx-order-execution.
//!
//! The URL and the demo flag come only from `DemoEnv::Okx(OkxHost)` / `OkxHost::target()`
//! (`OrderHttpRequest::to_demo` inserts the flag itself); this file names no host and no header
//! literal. Quantities (`sz`, `accFillSz`) are CONTRACTS end to end: nothing here converts units.
//! POST bodies are JSON and the signed text is the sent text.
//!
//! UNVERIFIED until the user's real demo probe (task 4.2): every field name, the reply shapes in
//! `tests/fixtures/okx/orders`, `reduceOnly` as a JSON boolean, and the code lists in `classify`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tong_funding_core::types::Decimal;

use super::binance::ModeReading;
use super::classify::{OKX_NOT_FOUND_CODES, Reply, SubmitClass, okx_is_env_mismatch, okx_reply, okx_state, query_outcome, submit_class};
use super::endpoints::{OKX_ACCOUNT_CONFIG_PATH, OKX_CANCEL_PATH, OKX_ORDER_PATH};
use super::http::{DemoEnv, Method, OrderHttpRequest, OrderTransport};
use super::order::{ClientOrderId, OrderRef, ValidOrder};
use crate::engine::ports::{OrderSide, OrderState, OrderStatus, QueryOutcome};
use crate::exchange::error::AdapterError;
use crate::exchange::signed::endpoints::{OkxHost, okx_inst_id};
use crate::exchange::signed::models::{dec_opt, dec_req, str_opt, str_req};
use crate::exchange::signed::okx::{OkxAcctLv, OkxLatch, parse_account_mode};
use crate::exchange::signed::signing::{Credentials, okx_auth_headers, okx_signature, okx_timestamp, percent_encode};

pub const ORDER_TIMEOUT: Duration = Duration::from_secs(5);
/// Name of the header that makes OKX drop an order its server receives after the given epoch ms.
const EXP_TIME_HEADER: &str = "expTime";
/// Consecutive `51603` answers (after `expTime`) before an unknown submit counts as "never arrived".
const NOT_FOUND_CONFIRMATIONS: u8 = 2;

/// What the size guard needs about one instrument (from the public instruments, mark price and the
/// risk settings; wired in `okx-trading-enablement`).
#[derive(Debug, Clone, PartialEq)]
pub struct OkxLimits {
    /// Base coin per contract.
    pub ct_val: Decimal,
    /// Smallest order step in contracts; `sz` must be a whole multiple.
    pub lot_sz: Decimal,
    pub mark_px: Decimal,
    /// Largest notional (USDT) one leg may have.
    pub max_leg_notional: Decimal,
}

pub trait OkxLimitsSource: Send + Sync {
    /// `None` = unknown instrument or no usable data: the order is not sent.
    fn limits(&self, symbol: &str) -> Option<OkxLimits>;
}

/// `sz` (contracts) must be positive, a whole multiple of `lotSz`, and `sz x ctVal x mark` must not
/// exceed the per-leg notional cap. Every limit must be positive: anything else refuses.
pub fn check_size(sz: Decimal, l: &OkxLimits) -> Result<(), String> {
    if l.ct_val <= Decimal::ZERO || l.lot_sz <= Decimal::ZERO || l.mark_px <= Decimal::ZERO || l.max_leg_notional <= Decimal::ZERO {
        return Err("OKX size limits unusable (ctVal, lotSz, mark price and notional cap must be positive)".into());
    }
    if sz <= Decimal::ZERO {
        return Err("OKX sz must be positive".into());
    }
    if !(sz % l.lot_sz).is_zero() {
        return Err(format!("OKX sz {sz} is not a whole multiple of lotSz {}", l.lot_sz));
    }
    let notional = sz * l.ct_val * l.mark_px;
    if notional > l.max_leg_notional {
        return Err(format!("OKX notional {notional} USDT exceeds the per-leg cap {}", l.max_leg_notional));
    }
    Ok(())
}

pub struct OkxOrderClient<T> {
    transport: Arc<T>,
    creds: Arc<Credentials>,
    env: OkxHost,
    /// Set by the first account-config read that answered `code "0"` with THESE credentials: the
    /// proof that the key works as a demo key (the request carried the demo flag). Orders wait for it.
    demo_proven: AtomicBool,
    latch: Arc<OkxLatch>,
    /// `clOrdId` -> (expTime ms, consecutive 51603 after expiry) for submits left unknown or rate limited.
    pending: Mutex<HashMap<String, (i64, u8)>>,
}

fn side_param(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "buy",
        OrderSide::Sell => "sell",
    }
}

impl<T: OrderTransport> OkxOrderClient<T> {
    pub fn new(transport: Arc<T>, creds: Arc<Credentials>, demo_env: OkxHost) -> Self {
        OkxOrderClient { transport, creds, env: demo_env, demo_proven: AtomicBool::new(false), latch: OkxLatch::new(), pending: Mutex::new(HashMap::new()) }
    }

    /// Shares an `OkxLatch` with the other OKX clients: `50101` anywhere disables OKX everywhere.
    pub fn with_latch(mut self, latch: Arc<OkxLatch>) -> Self {
        self.latch = latch;
        self
    }

    pub fn latch(&self) -> &Arc<OkxLatch> {
        &self.latch
    }

    fn disabled(&self) -> Option<String> {
        self.latch.reason().map(|r| format!("OKX disabled: {r}"))
    }

    /// Sends one request; an environment mismatch (`50101`) in the reply trips the latch.
    async fn call(&self, req: OrderHttpRequest) -> Reply {
        let result = self.transport.send(req).await;
        if okx_is_env_mismatch(&result) {
            self.latch.trip("OKX 50101: API key does not match the environment (demo flag sent); OKX disabled until restart");
        }
        okx_reply(result)
    }

    /// Builds one signed request. `path_and_query` is signed exactly as sent; `body` (POST) too.
    fn build(&self, method: Method, path_and_query: &str, body: Option<String>, timestamp_ms: i64) -> Result<OrderHttpRequest, AdapterError> {
        let ts = okx_timestamp(timestamp_ms)?;
        let signature = okx_signature(&self.creds.api_secret, &ts, method.as_str(), path_and_query, body.as_deref().unwrap_or(""))?;
        let auth = okx_auth_headers(&self.creds, &ts, &signature).map_err(AdapterError::from)?;
        let req = OrderHttpRequest::to_demo(method, DemoEnv::Okx(self.env), path_and_query, ORDER_TIMEOUT).okx_auth(auth);
        Ok(match body {
            Some(text) => req.json_body(text),
            None => req,
        })
    }

    /// `POST /api/v5/trade/order`: market order, `sz` in contracts exactly as given, `tdMode`
    /// cross, explicit `reduceOnly`, no `posSide` (net mode is guaranteed by the mode gate).
    /// Carries `expTime` (signing time + the request timeout): after it OKX no longer processes it.
    pub fn submit_request(&self, order: &ValidOrder, timestamp_ms: i64) -> Result<OrderHttpRequest, AdapterError> {
        let inst_id = okx_inst_id(order.symbol()).ok_or_else(|| AdapterError::parse("symbol is not a BASEUSDT symbol"))?;
        let body = json!({
            "instId": inst_id,
            "tdMode": "cross",
            "side": side_param(order.side()),
            "ordType": "market",
            "sz": order.quantity_text(),
            "clOrdId": order.id().as_str(),
            "reduceOnly": order.reduce_only(),
        });
        let exp_time = timestamp_ms.saturating_add(ORDER_TIMEOUT.as_millis() as i64);
        self.build(Method::Post, OKX_ORDER_PATH, Some(body.to_string()), timestamp_ms).map(|r| r.header(EXP_TIME_HEADER, &exp_time.to_string()))
    }

    /// `GET /api/v5/trade/order` by `clOrdId` or `ordId`.
    pub fn query_request(&self, symbol: &str, by: &OrderRef, timestamp_ms: i64) -> Result<OrderHttpRequest, AdapterError> {
        let inst_id = okx_inst_id(symbol).ok_or_else(|| AdapterError::parse("symbol is not a BASEUSDT symbol"))?;
        let (name, value) = match by {
            OrderRef::Client(id) => ("clOrdId", id.as_str().to_string()),
            OrderRef::Exchange(id) => ("ordId", id.clone()),
        };
        let path = format!("{OKX_ORDER_PATH}?instId={}&{name}={}", percent_encode(&inst_id), percent_encode(&value));
        self.build(Method::Get, &path, None, timestamp_ms)
    }

    /// `POST /api/v5/trade/cancel-order` by `clOrdId`.
    pub fn cancel_request(&self, symbol: &str, id: &ClientOrderId, timestamp_ms: i64) -> Result<OrderHttpRequest, AdapterError> {
        let inst_id = okx_inst_id(symbol).ok_or_else(|| AdapterError::parse("symbol is not a BASEUSDT symbol"))?;
        self.build(Method::Post, OKX_CANCEL_PATH, Some(json!({ "instId": inst_id, "clOrdId": id.as_str() }).to_string()), timestamp_ms)
    }

    /// `GET /api/v5/account/config`.
    pub fn position_mode_request(&self, timestamp_ms: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.build(Method::Get, OKX_ACCOUNT_CONFIG_PATH, None, timestamp_ms)
    }

    async fn send(&self, req: Result<OrderHttpRequest, AdapterError>) -> Reply {
        if let Some(reason) = self.disabled() {
            return Reply::Unknown { reason };
        }
        match req {
            Ok(r) => self.call(r).await,
            Err(e) => Reply::Unknown { reason: format!("request not built: {e}") },
        }
    }

    /// Nothing is sent unless OKX is not latched off and the demo key has been proven.
    pub async fn submit(&self, order: &ValidOrder, timestamp_ms: i64) -> SubmitClass {
        let not_sent = |message: String| SubmitClass::Rejected { code: "not_sent".into(), message };
        if let Some(reason) = self.disabled() {
            return not_sent(reason);
        }
        if !self.demo_proven.load(Ordering::SeqCst) {
            return not_sent("OKX demo key not proven: the account-config check has not succeeded yet".into());
        }
        let req = match self.submit_request(order, timestamp_ms) {
            Ok(r) => r,
            Err(e) => return SubmitClass::Rejected { code: "local".into(), message: format!("not sent: {e}") },
        };
        let id = order.id().as_str().to_string();
        let exp_time = timestamp_ms.saturating_add(ORDER_TIMEOUT.as_millis() as i64);
        let class = submit_class(self.call(req).await, |b| parse_ack(b, &id));
        let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &class {
            // The order may still be processed until `expTime`: remember it so a 51603 is not trusted early.
            SubmitClass::Unknown { .. } | SubmitClass::RateLimited { .. } => {
                pending.insert(id, (exp_time, 0));
            }
            SubmitClass::Accepted(_) | SubmitClass::Rejected { .. } => {
                pending.remove(&id);
            }
        }
        class
    }

    /// One `GET /api/v5/trade/order`; `51603` is "not found". A rejected timestamp (`50102`) is
    /// a failure the engine's repeated lookup absorbs (no resync handle exists at this layer).
    pub async fn query(&self, symbol: &str, by: &OrderRef, timestamp: impl Fn() -> i64) -> QueryOutcome {
        let reply = self.send(self.query_request(symbol, by, timestamp())).await;
        if let (Reply::Refused { code, .. }, OrderRef::Client(id)) = (&reply, by)
            && OKX_NOT_FOUND_CODES.contains(code)
            && let Some(verdict) = self.not_found_verdict(id.as_str(), timestamp())
        {
            return verdict;
        }
        let outcome = query_outcome(reply, &OKX_NOT_FOUND_CODES, parse_order);
        if matches!(outcome, QueryOutcome::Found(_)) && let OrderRef::Client(id) = by {
            self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id.as_str());
        }
        outcome
    }

    /// `Some(Failed)` while the order, left unknown by a submit of this process, may still be
    /// processed (before `expTime`, or before the second confirmation); `None` = no such memory,
    /// the plain "not found" applies; `Some(NotFound)` once it is safe to say it never arrived.
    fn not_found_verdict(&self, id: &str, now_ms: i64) -> Option<QueryOutcome> {
        let mut pending = self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (exp_time, seen) = pending.get_mut(id)?;
        if now_ms <= *exp_time {
            return Some(QueryOutcome::Failed { reason: format!("51603 before expTime ({exp_time}): the order may still be processed; confirm later") });
        }
        *seen += 1;
        if *seen < NOT_FOUND_CONFIRMATIONS {
            return Some(QueryOutcome::Failed { reason: format!("51603 seen {seen} of {NOT_FOUND_CONFIRMATIONS} times after expTime; confirm again") });
        }
        pending.remove(id);
        Some(QueryOutcome::NotFound)
    }

    /// Cancel, then read the order back: the cancel reply carries no state, and `51400`
    /// (already filled / canceled / unknown) is decided by what the order really is.
    pub async fn cancel(&self, symbol: &str, id: &ClientOrderId, timestamp: impl Fn() -> i64) -> QueryOutcome {
        match self.send(self.cancel_request(symbol, id, timestamp())).await {
            Reply::Ok(_) | Reply::Refused { code: 51400, .. } => match self.query(symbol, &OrderRef::Client(id.clone()), timestamp).await {
                QueryOutcome::NotFound => QueryOutcome::Failed { reason: "cancel answered but the order cannot be found".into() },
                other => other,
            },
            other => query_outcome(other, &OKX_NOT_FOUND_CODES, |_| Ok(None)),
        }
    }

    /// Account mode for the executor's one-way gate: `acctLv` 2/3 with `net_mode` is one-way,
    /// `long_short_mode` is hedge, anything else (spot, portfolio margin) is an error.
    pub async fn position_mode(&self, timestamp_ms: i64) -> Result<ModeReading, String> {
        match self.send(self.position_mode_request(timestamp_ms)).await {
            Reply::Ok(b) => {
                // code "0" with these credentials and the demo flag: the key is proven (design D1)
                self.demo_proven.store(true, Ordering::SeqCst);
                let row = b.get("data").and_then(Value::as_array).and_then(|d| d.first()).ok_or("account config missing")?;
                match parse_account_mode(row) {
                    Ok(OkxAcctLv::Futures | OkxAcctLv::MultiCurrency) => Ok(ModeReading::OneWay),
                    Err(_) if str_opt(row, "posMode").ok().flatten().as_deref() == Some("long_short_mode") => Ok(ModeReading::Hedge),
                    Err(e) => Err(e.to_string()),
                }
            }
            Reply::Refused { code, message } => Err(format!("account mode refused: {code} {message}")),
            Reply::RateLimited { retry_after_ms } => Err(format!("account mode rate limited ({retry_after_ms:?} ms)")),
            Reply::Unknown { reason } => Err(format!("account mode unknown: {reason}")),
        }
    }
}

/// Place reply: `data[0].ordId`; no fill data (the engine polls).
fn parse_ack(b: &Value, client_order_id: &str) -> Result<OrderStatus, AdapterError> {
    let r = b.get("data").and_then(Value::as_array).and_then(|d| d.first()).ok_or_else(|| AdapterError::parse("missing data[0]"))?;
    Ok(OrderStatus {
        // the id we sent is the engine's id; an echo that differs is not trusted over it
        client_order_id: client_order_id.to_string(),
        exchange_order_id: Some(str_req(r, "ordId")?),
        filled_quantity: Decimal::ZERO,
        avg_price: None,
        fee: None,
        fee_asset: None,
        state: OrderState::Open,
    })
}

/// `data[0]` of order details; an empty list is "not here". OKX `fee` is negative when paid: the
/// engine's `fee` is positive when paid, so the sign flips here (spec: 手續費正負號).
fn parse_order(b: &Value) -> Result<Option<OrderStatus>, AdapterError> {
    let rows = b.get("data").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing data"))?;
    let Some(r) = rows.first() else { return Ok(None) };
    let fee = dec_opt(r, "fee")?.map(|f| -f);
    Ok(Some(OrderStatus {
        client_order_id: str_req(r, "clOrdId")?,
        exchange_order_id: Some(str_req(r, "ordId")?),
        filled_quantity: dec_req(r, "accFillSz")?,
        avg_price: dec_opt(r, "avgPx")?.filter(|p| !p.is_zero()),
        fee,
        fee_asset: str_opt(r, "feeCcy")?,
        state: okx_state(&str_req(r, "state")?)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::{IdPrefix, MAX_LEN, client_order_id};
    use crate::engine::ports::{Leg, OrderAction, OrderRequest};
    use crate::exchange::execution::http::fake::{FakeOrderTransport, Reply as R};
    use crate::exchange::reqwest_transport::HostPolicy;
    use crate::exchange::signed::signing::okx_signature;
    use tong_funding_core::types::Exchange;

    const KEY: &str = "TEST_KEY_NOT_REAL";
    const SECRET: &str = "TEST_SECRET_NOT_REAL";
    const PASS: &str = "TEST_PASS_NOT_REAL";
    /// 2020-12-08T09:08:57.715Z
    const TS: i64 = 1_607_418_537_715;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn fx(name: &str) -> String {
        let path = format!("{}/tests/fixtures/okx/orders/{name}.json", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"))
    }
    fn ok(name: &str) -> R {
        R::ok(&fx(name))
    }

    fn id() -> String {
        client_order_id(IdPrefix::Demo, "ab12", Leg::Short, OrderAction::Open, 1)
    }

    fn order(side: OrderSide, qty: &str, reduce_only: bool) -> ValidOrder {
        ValidOrder::from_request(&OrderRequest {
            client_order_id: id(),
            exchange: Exchange::Okx,
            symbol: "BTCUSDT".into(),
            side,
            quantity: qty.parse().unwrap(),
            reduce_only,
        })
        .unwrap()
    }

    fn creds() -> Arc<Credentials> {
        Arc::new(Credentials { api_key: KEY.into(), api_secret: SECRET.into(), passphrase: Some(PASS.into()) })
    }

    fn client(t: &FakeOrderTransport) -> OkxOrderClient<FakeOrderTransport> {
        OkxOrderClient::new(Arc::new(t.clone()), creds(), OkxHost::Demo)
    }

    fn header<'a>(r: &'a OrderHttpRequest, name: &str) -> Vec<&'a str> {
        r.headers().iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }

    /// Every request the fake saw must be admitted by the REAL rule (host + flag exactly once).
    fn assert_all_admitted(t: &FakeOrderTransport) {
        for r in t.requests() {
            let url = reqwest::Url::parse(r.full_url()).unwrap();
            assert_eq!(url.host_str(), Some("openapi.okx.com"), "{}", r.full_url());
            assert_eq!(header(&r, "x-simulated-trading"), vec!["1"], "{}", r.full_url());
            assert!(!r.misuses_protected_header());
            assert!(HostPolicy::SignedDemo.allows(&url, r.headers()), "{}", r.full_url());
        }
    }

    // ---- request building ----

    #[test]
    fn submit_is_a_post_with_contracts_cross_market_and_no_posside() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(OrderSide::Buy, "3", false), TS).unwrap();
        assert_eq!(r.method(), Method::Post);
        assert_eq!(r.full_url(), "https://openapi.okx.com/api/v5/trade/order");
        let text = r.body().unwrap();
        for part in ["\"instId\":\"BTC-USDT-SWAP\"", "\"sz\":\"3\"", "\"tdMode\":\"cross\"", "\"ordType\":\"market\"", "\"side\":\"buy\"", "\"reduceOnly\":false"] {
            assert!(text.contains(part), "{part} missing in {text}");
        }
        assert!(text.contains(&format!("\"clOrdId\":\"{}\"", id())));
        assert!(!text.contains("posSide"), "{text}");
        assert_eq!(header(&r, "x-simulated-trading"), vec!["1"]);
    }

    #[test]
    fn a_close_is_reduce_only_and_the_quantity_text_is_not_reformatted() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(OrderSide::Sell, "2.50", true), TS).unwrap();
        let body: Value = serde_json::from_str(r.body().unwrap()).unwrap();
        assert_eq!((body["reduceOnly"].clone(), body["side"].clone(), body["sz"].clone()), (json!(true), json!("sell"), json!("2.50")));
    }

    #[test]
    fn the_post_signature_covers_the_exact_body_and_path_that_are_sent() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(OrderSide::Buy, "3", false), TS).unwrap();
        assert_eq!(header(&r, "OK-ACCESS-TIMESTAMP"), vec!["2020-12-08T09:08:57.715Z"]);
        let expected = okx_signature(SECRET, "2020-12-08T09:08:57.715Z", "POST", "/api/v5/trade/order", r.body().unwrap()).unwrap();
        assert_eq!(header(&r, "OK-ACCESS-SIGN"), vec![expected.as_str()]);
        assert_eq!((header(&r, "OK-ACCESS-KEY"), header(&r, "OK-ACCESS-PASSPHRASE")), (vec![KEY], vec![PASS]));
        // independent check of the formula on the same prehash (Python hmac/base64 computed in signing.rs)
        assert_eq!(header(&r, "Content-Type"), vec!["application/json"]);
    }

    #[test]
    fn query_and_cancel_are_signed_over_their_query_and_body() {
        let t = FakeOrderTransport::new();
        let c = client(&t);
        let cid = ClientOrderId::parse(&id()).unwrap();
        let q = c.query_request("BTCUSDT", &OrderRef::Client(cid.clone()), TS).unwrap();
        assert_eq!(q.method(), Method::Get);
        let path = q.full_url().strip_prefix("https://openapi.okx.com").unwrap().to_string();
        assert_eq!(path, format!("/api/v5/trade/order?instId=BTC-USDT-SWAP&clOrdId={}", id()));
        let expected = okx_signature(SECRET, "2020-12-08T09:08:57.715Z", "GET", &path, "").unwrap();
        assert_eq!(header(&q, "OK-ACCESS-SIGN"), vec![expected.as_str()]);
        let q2 = c.query_request("BTCUSDT", &OrderRef::Exchange("312269865356374016".into()), TS).unwrap();
        assert!(q2.full_url().ends_with("instId=BTC-USDT-SWAP&ordId=312269865356374016"), "{}", q2.full_url());
        let x = c.cancel_request("BTCUSDT", &cid, TS).unwrap();
        assert_eq!((x.method(), x.full_url()), (Method::Post, "https://openapi.okx.com/api/v5/trade/cancel-order"));
        let body: Value = serde_json::from_str(x.body().unwrap()).unwrap();
        assert_eq!((body["instId"].clone(), body["clOrdId"].clone()), (json!("BTC-USDT-SWAP"), json!(id())));
        let m = c.position_mode_request(TS).unwrap();
        assert_eq!((m.method(), m.full_url()), (Method::Get, "https://openapi.okx.com/api/v5/account/config"));
    }

    #[test]
    fn a_symbol_that_is_not_base_usdt_is_not_built_into_a_request() {
        let t = FakeOrderTransport::new();
        let c = client(&t);
        let bad = ValidOrder::from_request(&OrderRequest { client_order_id: id(), exchange: Exchange::Okx, symbol: "BTCUSD".into(), side: OrderSide::Buy, quantity: d("1"), reduce_only: false }).unwrap();
        assert!(c.submit_request(&bad, TS).is_err());
        assert!(c.query_request("BTCUSD", &OrderRef::Exchange("1".into()), TS).is_err());
    }

    #[test]
    fn engine_client_order_ids_always_fit_the_okx_clordid_rule() {
        // OKX: case-sensitive alphanumerics up to 32 characters. The engine id layout is the guarantee
        // (design D2, replacing a runtime branch): lowercase alphanumerics, at most MAX_LEN.
        let _ = MAX_LEN; // the generic limit (36) is looser than OKX's 32: the generated ids are what must fit
        for uuid in ["", "ab12", "AB-12_cd-ef-0123456789-abcdef", "pair with spaces / ünïcode", "x".repeat(200).as_str()] {
            for (leg, action) in [(Leg::Long, OrderAction::Open), (Leg::Short, OrderAction::Close)] {
                for seq in [0, 1, 35, 60_000] {
                    let id = client_order_id(IdPrefix::Demo, uuid, leg, action, seq);
                    assert!(!id.is_empty() && id.len() <= 32, "{id}");
                    assert!(id.bytes().all(|b| b.is_ascii_alphanumeric()), "{id}");
                }
            }
        }
    }

    // ---- result classification (recorded-style fixtures) ----

    /// A client whose demo key was proven by a successful account-config read (design D1).
    async fn proven_client(t: &FakeOrderTransport) -> OkxOrderClient<FakeOrderTransport> {
        let path = format!("{}/tests/fixtures/okx/signed/account_config_futures_net.json", env!("CARGO_MANIFEST_DIR"));
        t.on(Method::Get, "/api/v5/account/config", R::ok(&std::fs::read_to_string(path).unwrap()));
        let c = client(t);
        c.position_mode(TS).await.unwrap();
        c
    }

    async fn submit_with(r: R) -> (SubmitClass, FakeOrderTransport) {
        let t = FakeOrderTransport::new();
        t.on(Method::Post, "/api/v5/trade/order", r);
        let class = proven_client(&t).await.submit(&order(OrderSide::Buy, "3", false), TS).await;
        (class, t)
    }

    #[tokio::test]
    async fn an_accepted_order_carries_the_exchange_id_and_no_fill() {
        let (class, t) = submit_with(ok("place_accepted")).await;
        match class {
            SubmitClass::Accepted(s) => {
                assert_eq!(s.exchange_order_id.as_deref(), Some("312269865356374016"));
                assert_eq!((s.state, s.filled_quantity), (OrderState::Open, Decimal::ZERO));
            }
            other => panic!("{other:?}"),
        }
        assert_all_admitted(&t);
    }

    #[tokio::test]
    async fn insufficient_balance_is_rejected_with_the_scode_and_smsg() {
        match submit_with(ok("place_rejected_51131")).await.0 {
            SubmitClass::Rejected { code, message } => assert_eq!((code.as_str(), message.as_str()), ("51131", "Insufficient balance")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn http_200_code_0_with_a_non_zero_scode_is_rejected_not_accepted() {
        assert!(matches!(submit_with(ok("place_code0_scode51121")).await.0, SubmitClass::Rejected { code, .. } if code == "51121"));
    }

    #[tokio::test]
    async fn the_timeout_code_is_unknown_and_there_is_exactly_one_post() {
        let (class, t) = submit_with(ok("place_50004")).await;
        assert!(matches!(class, SubmitClass::Unknown { .. }), "{class:?}");
        assert_eq!(t.count(Method::Post, "/api/v5/trade/order"), 1, "an unknown outcome is never resent");
    }

    #[tokio::test]
    async fn busy_and_system_error_codes_are_unknown_too() {
        for code in ["50001", "50013", "50026"] {
            let body = json!({"code": code, "msg": "x", "data": []}).to_string();
            assert!(matches!(submit_with(R::ok(&body)).await.0, SubmitClass::Unknown { .. }), "{code}");
        }
        // the same codes inside data[0].sCode
        let body = json!({"code":"1","msg":"All operations failed","data":[{"sCode":"50013","sMsg":"busy","ordId":""}]}).to_string();
        assert!(matches!(submit_with(R::ok(&body)).await.0, SubmitClass::Unknown { .. }));
    }

    #[tokio::test]
    async fn rate_limit_codes_and_http_429_are_rate_limited() {
        assert!(matches!(submit_with(ok("place_50011")).await.0, SubmitClass::RateLimited { .. }));
        let body = json!({"code":"1","msg":"","data":[{"sCode":"50061","sMsg":"sub account limit","ordId":""}]}).to_string();
        assert!(matches!(submit_with(R::ok(&body)).await.0, SubmitClass::RateLimited { .. }));
        assert!(matches!(submit_with(R::status(429, "")).await.0, SubmitClass::RateLimited { .. }));
    }

    #[tokio::test]
    async fn transport_failures_5xx_and_garbage_are_unknown() {
        for r in [R::err(AdapterError::Timeout), R::err(AdapterError::network("reset")), R::status(502, "<html>"), R::ok("not json"), R::ok(r#"{"data":[]}"#)] {
            assert!(matches!(submit_with(r.clone()).await.0, SubmitClass::Unknown { .. }), "{r:?}");
        }
    }

    #[tokio::test]
    async fn a_rejected_timestamp_on_a_submit_is_rejected_and_not_retried() {
        let (class, t) = submit_with(ok("place_50102")).await;
        assert!(matches!(class, SubmitClass::Rejected { ref code, .. } if code == "50102"), "{class:?}");
        assert_eq!(t.count(Method::Post, "/api/v5/trade/order"), 1);
    }

    // ---- query ----

    async fn query_with(name: &str) -> (QueryOutcome, FakeOrderTransport) {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, "/api/v5/trade/order", ok(name));
        let cid = ClientOrderId::parse(&id()).unwrap();
        (client(&t).query("BTCUSDT", &OrderRef::Client(cid), || TS).await, t)
    }

    #[tokio::test]
    async fn a_filled_order_reports_contracts_average_price_and_the_fee_as_a_positive_payment() {
        let (out, t) = query_with("order_filled").await;
        match out {
            QueryOutcome::Found(s) => {
                assert_eq!((s.state, s.filled_quantity, s.avg_price), (OrderState::Filled, d("3"), Some(d("60010.5"))));
                assert_eq!((s.fee, s.fee_asset.as_deref()), (Some(d("0.9")), Some("USDT")), "OKX -0.9 = paid 0.9");
                assert_eq!(s.exchange_order_id.as_deref(), Some("312269865356374016"));
            }
            other => panic!("{other:?}"),
        }
        assert_all_admitted(&t);
    }

    #[tokio::test]
    async fn states_map_to_the_engine_and_an_unknown_state_is_a_failure_not_a_guess() {
        assert!(matches!(query_with("order_live").await.0, QueryOutcome::Found(s) if s.state == OrderState::Open && s.fee_asset.is_none() && s.avg_price.is_none()));
        assert!(matches!(query_with("order_partial").await.0, QueryOutcome::Found(s) if s.state == OrderState::Open && s.filled_quantity == d("1")));
        assert!(matches!(query_with("order_canceled").await.0, QueryOutcome::Found(s) if s.state == OrderState::Cancelled));
        assert!(matches!(query_with("order_unknown_state").await.0, QueryOutcome::Failed { .. }));
    }

    #[tokio::test]
    async fn code_51603_is_not_found_and_other_refusals_are_failures() {
        assert_eq!(query_with("order_51603").await.0, QueryOutcome::NotFound);
        assert!(matches!(query_with("place_50102").await.0, QueryOutcome::Failed { .. }));
        assert!(matches!(query_with("place_50011").await.0, QueryOutcome::Failed { .. }));
    }

    // ---- cancel ----

    #[tokio::test]
    async fn an_accepted_cancel_is_followed_by_one_read_back_and_reports_its_state() {
        let t = FakeOrderTransport::new();
        t.on(Method::Post, "/api/v5/trade/cancel-order", ok("cancel_accepted"));
        t.on(Method::Get, "/api/v5/trade/order", ok("order_canceled"));
        let cid = ClientOrderId::parse(&id()).unwrap();
        let out = client(&t).cancel("BTCUSDT", &cid, || TS).await;
        assert!(matches!(out, QueryOutcome::Found(s) if s.state == OrderState::Cancelled));
        assert_eq!((t.count(Method::Post, "cancel-order"), t.count(Method::Get, "trade/order")), (1, 1));
        assert_all_admitted(&t);
    }

    #[tokio::test]
    async fn a_51400_cancel_is_decided_by_reading_the_order_back() {
        let t = FakeOrderTransport::new();
        t.on(Method::Post, "/api/v5/trade/cancel-order", ok("cancel_51400"));
        t.on(Method::Get, "/api/v5/trade/order", ok("order_filled"));
        let cid = ClientOrderId::parse(&id()).unwrap();
        let out = client(&t).cancel("BTCUSDT", &cid, || TS).await;
        assert!(matches!(&out, QueryOutcome::Found(s) if s.state == OrderState::Filled), "{out:?}");
    }

    // ---- account mode ----

    #[tokio::test]
    async fn the_position_mode_reading_follows_acctlv_and_posmode() {
        let cfg = |name: &str| {
            let t = FakeOrderTransport::new();
            let path = format!("{}/tests/fixtures/okx/signed/{name}.json", env!("CARGO_MANIFEST_DIR"));
            t.on(Method::Get, "/api/v5/account/config", R::ok(&std::fs::read_to_string(path).unwrap()));
            t
        };
        assert_eq!(client(&cfg("account_config_futures_net")).position_mode(TS).await, Ok(ModeReading::OneWay));
        assert_eq!(client(&cfg("account_config_multi_ccy")).position_mode(TS).await, Ok(ModeReading::OneWay));
        assert_eq!(client(&cfg("account_config_long_short")).position_mode(TS).await, Ok(ModeReading::Hedge));
        assert!(client(&cfg("account_config_acctlv4")).position_mode(TS).await.unwrap_err().contains("acctLv 4"));
        assert!(client(&cfg("account_config_spot")).position_mode(TS).await.is_err());
        let t = cfg("account_config_futures_net");
        client(&t).position_mode(TS).await.unwrap();
        assert_all_admitted(&t);
    }

    #[tokio::test]
    async fn a_client_without_a_passphrase_builds_nothing_and_sends_nothing() {
        let t = FakeOrderTransport::new();
        let c = OkxOrderClient::new(Arc::new(t.clone()), Arc::new(Credentials { api_key: KEY.into(), api_secret: SECRET.into(), passphrase: None }), OkxHost::Demo);
        assert!(c.submit_request(&order(OrderSide::Buy, "1", false), TS).is_err());
        assert!(matches!(c.submit(&order(OrderSide::Buy, "1", false), TS).await, SubmitClass::Rejected { .. }));
        assert!(t.requests().is_empty());
    }

    // ---- guards (okx-execution-guards) ----

    #[tokio::test]
    async fn an_unproven_client_does_not_send_an_order() {
        let t = FakeOrderTransport::new();
        t.on(Method::Post, "/api/v5/trade/order", ok("place_accepted"));
        match client(&t).submit(&order(OrderSide::Buy, "3", false), TS).await {
            SubmitClass::Rejected { code, message } => assert_eq!((code.as_str(), message.contains("not proven")), ("not_sent", true), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn a_failed_config_read_does_not_prove_the_key() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, "/api/v5/account/config", ok("place_50102"));
        t.on(Method::Post, "/api/v5/trade/order", ok("place_accepted"));
        let c = client(&t);
        assert!(c.position_mode(TS).await.is_err());
        assert!(matches!(c.submit(&order(OrderSide::Buy, "3", false), TS).await, SubmitClass::Rejected { .. }));
        assert_eq!(t.count(Method::Post, "/api/v5/trade/order"), 0);
    }

    #[tokio::test]
    async fn the_proof_and_the_order_use_the_same_credentials() {
        let t = FakeOrderTransport::new();
        t.on(Method::Post, "/api/v5/trade/order", ok("place_accepted"));
        let c = proven_client(&t).await;
        assert!(matches!(c.submit(&order(OrderSide::Buy, "3", false), TS).await, SubmitClass::Accepted(_)));
        let reqs = t.requests();
        let keys: Vec<&str> = reqs.iter().map(|r| header(r, "OK-ACCESS-KEY")[0]).collect();
        assert_eq!(keys, vec![KEY, KEY]);
        assert_eq!(header(&reqs[0], "OK-ACCESS-PASSPHRASE"), header(&reqs[1], "OK-ACCESS-PASSPHRASE"));
    }

    #[tokio::test]
    async fn a_50101_on_any_call_latches_okx_off_for_every_later_call() {
        let t = FakeOrderTransport::new();
        let c = proven_client(&t).await;
        t.on(Method::Get, "/api/v5/trade/order?", ok("place_50101"));
        t.on(Method::Post, "/api/v5/trade/order", ok("place_accepted"));
        let cid = ClientOrderId::parse(&id()).unwrap();
        assert!(matches!(c.query("BTCUSDT", &OrderRef::Client(cid.clone()), || TS).await, QueryOutcome::Failed { .. }));
        let posts = || t.count(Method::Post, "/api/v5/trade/order");
        match c.submit(&order(OrderSide::Buy, "3", false), TS).await {
            SubmitClass::Rejected { code, message } => assert_eq!((code.as_str(), message.contains("50101")), ("not_sent", true), "{message}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(posts(), 0);
        let n = t.requests().len();
        assert!(matches!(c.query("BTCUSDT", &OrderRef::Client(cid.clone()), || TS).await, QueryOutcome::Failed { reason } if reason.contains("50101")));
        assert!(matches!(c.cancel("BTCUSDT", &cid, || TS).await, QueryOutcome::Failed { .. }));
        assert!(c.position_mode(TS).await.unwrap_err().contains("50101"));
        assert_eq!(t.requests().len(), n, "latched: nothing more is sent");
    }

    #[tokio::test]
    async fn a_50101_inside_a_submit_reply_latches_too_and_is_not_an_ordinary_rejection() {
        let t = FakeOrderTransport::new();
        let c = proven_client(&t).await;
        t.on(Method::Post, "/api/v5/trade/order", ok("place_50101"));
        let class = c.submit(&order(OrderSide::Buy, "3", false), TS).await;
        assert!(matches!(class, SubmitClass::Unknown { .. }), "{class:?}: the outcome of that request is not 'clearly refused'");
        assert!(c.latch().reason().is_some());
    }

    #[tokio::test]
    async fn unlisted_rejection_codes_and_empty_data_are_unknown() {
        assert!(matches!(submit_with(ok("place_scode_unlisted")).await.0, SubmitClass::Unknown { .. }));
        assert!(matches!(submit_with(ok("place_code0_empty_data")).await.0, SubmitClass::Unknown { .. }));
        // listed codes stay rejections
        assert!(matches!(submit_with(ok("place_rejected_51131")).await.0, SubmitClass::Rejected { .. }));
    }

    #[tokio::test]
    async fn code_50013_on_a_submit_is_unknown_with_exactly_one_post() {
        let body = json!({"code":"50013","msg":"Systems are busy","data":[]}).to_string();
        let (class, t) = submit_with(R::ok(&body)).await;
        assert!(matches!(class, SubmitClass::Unknown { .. }));
        assert_eq!(t.count(Method::Post, "/api/v5/trade/order"), 1);
    }

    #[tokio::test]
    async fn a_submit_carries_exp_time_after_the_signing_time() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(OrderSide::Buy, "3", false), TS).unwrap();
        assert_eq!(header(&r, "expTime"), vec![(TS + 5_000).to_string().as_str()]);
    }

    #[tokio::test]
    async fn a_51603_after_an_unknown_submit_needs_expiry_and_two_confirmations() {
        let t = FakeOrderTransport::new();
        let c = proven_client(&t).await;
        t.on(Method::Post, "/api/v5/trade/order", ok("place_50004"));
        t.on(Method::Get, "/api/v5/trade/order?", ok("order_51603"));
        let cid = ClientOrderId::parse(&id()).unwrap();
        let by = OrderRef::Client(cid);
        assert!(matches!(c.submit(&order(OrderSide::Buy, "3", false), TS).await, SubmitClass::Unknown { .. }));
        // 50004, then 51603 right away: the order may still be processed until expTime
        assert!(matches!(c.query("BTCUSDT", &by, || TS + 100).await, QueryOutcome::Failed { .. }));
        // expired, but only one confirmation so far
        let late = TS + 5_001;
        assert!(matches!(c.query("BTCUSDT", &by, || late).await, QueryOutcome::Failed { .. }));
        assert_eq!(c.query("BTCUSDT", &by, || late + 1_000).await, QueryOutcome::NotFound, "second 51603 after expiry");
    }

    #[tokio::test]
    async fn a_51603_for_an_order_this_process_never_left_unknown_is_not_found_at_once() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, "/api/v5/trade/order?", ok("order_51603"));
        let cid = ClientOrderId::parse(&id()).unwrap();
        assert_eq!(client(&t).query("BTCUSDT", &OrderRef::Client(cid), || TS).await, QueryOutcome::NotFound);
    }

    // ---- size guard (okx-execution-guards D3) ----

    fn limits(ct_val: &str, lot_sz: &str, mark_px: &str, cap: &str) -> OkxLimits {
        OkxLimits { ct_val: d(ct_val), lot_sz: d(lot_sz), mark_px: d(mark_px), max_leg_notional: d(cap) }
    }

    #[test]
    fn a_size_must_be_a_whole_number_of_lots_and_within_the_notional_cap() {
        let btc = limits("0.01", "1", "60000", "5000");
        assert!(check_size(d("3"), &btc).is_ok(), "3 contracts = 0.03 BTC = 1800 USDT");
        assert!(check_size(d("8"), &btc).is_ok());
        assert!(check_size(d("9"), &btc).unwrap_err().contains("notional"), "9 * 0.01 * 60000 = 5400 > 5000");
        assert!(check_size(d("0.5"), &btc).unwrap_err().contains("lotSz"));
        assert!(check_size(d("2.5"), &limits("0.01", "0.5", "60000", "5000")).is_ok(), "multiples of a fractional lot");
        assert!(check_size(d("0.3"), &limits("0.01", "0.5", "60000", "5000")).is_err());
    }

    #[test]
    fn a_coin_sized_quantity_on_a_big_contract_value_instrument_is_refused() {
        // ctVal 1000 per contract: 0.5 "coins" sent as sz is not a lot multiple; 500000 would be 500 million coins
        let pepe = limits("1000", "1", "0.5", "100000");
        assert!(check_size(d("0.5"), &pepe).is_err());
        assert!(check_size(d("3"), &pepe).is_ok());
        assert!(check_size(d("500000"), &pepe).unwrap_err().contains("notional"));
    }

    #[test]
    fn missing_or_nonsensical_limits_refuse_instead_of_guessing() {
        for l in [limits("0", "1", "60000", "5000"), limits("0.01", "0", "60000", "5000"), limits("0.01", "1", "0", "5000"), limits("0.01", "1", "60000", "0"), limits("-1", "1", "60000", "5000")] {
            assert!(check_size(d("1"), &l).is_err(), "{l:?}");
        }
        assert!(check_size(d("0"), &limits("0.01", "1", "60000", "5000")).is_err());
    }
}
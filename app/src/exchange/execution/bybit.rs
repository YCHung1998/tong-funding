//! Bybit v5 signed ORDER requests (Demo Trading host only): create a linear market order with
//! `orderLinkId`, cancel and query by `orderLinkId` (or `orderId`), and the position mode of a
//! symbol (`positionIdx`). POST bodies are JSON; the signature is
//! `HMAC-SHA256(secret, timestamp + apiKey + recvWindow + body-or-query)`.
//!
//! UNVERIFIED until task 4.2: `reduceOnly` / `positionIdx` behaviour on the demo host, whether
//! `realtime` still lists a filled market order (we fall back to `history`), `cumExecFee` being in
//! USDT for USDT-settled linear contracts, and the codes listed in `classify`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::binance::ModeReading;
use super::classify::{BYBIT_LEVERAGE_UNCHANGED_CODES, BYBIT_NOT_FOUND_CODES, LeverageOutcome, Reply, SubmitClass, bybit_reply, bybit_state, leverage_outcome, query_outcome, submit_class};
use super::endpoints::{BYBIT_CANCEL_PATH, BYBIT_CREATE_PATH, BYBIT_HISTORY_PATH, BYBIT_POSITION_PATH, BYBIT_REALTIME_PATH, BYBIT_SET_LEVERAGE_PATH};
use super::http::{DemoEnv, Method, OrderHttpRequest, OrderTransport};
use super::order::{ClientOrderId, OrderRef, ValidOrder};
use crate::engine::ports::{OrderSide, OrderState, OrderStatus, QueryOutcome};
use crate::exchange::error::AdapterError;
use crate::exchange::signed::endpoints::BybitHost;
use crate::exchange::signed::models::{dec_opt, dec_req, str_opt, str_req};
use crate::exchange::signed::signing::{Credentials, RECV_WINDOW_MS, bybit_signature, encode_query};

pub const ORDER_TIMEOUT: Duration = Duration::from_secs(5);
/// Fee asset of USDT-settled linear contracts (UNVERIFIED for every account type).
pub const BYBIT_FEE_ASSET: &str = "USDT";

pub struct BybitOrderClient<T> {
    transport: Arc<T>,
    creds: Arc<Credentials>,
    env: BybitHost,
}

fn side_param(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "Buy",
        OrderSide::Sell => "Sell",
    }
}

impl<T: OrderTransport> BybitOrderClient<T> {
    pub fn new(transport: Arc<T>, creds: Arc<Credentials>, demo_env: BybitHost) -> Self {
        BybitOrderClient { transport, creds, env: demo_env }
    }

    fn sign_headers(&self, req: OrderHttpRequest, timestamp: i64, payload: &str) -> Result<OrderHttpRequest, AdapterError> {
        let signature = bybit_signature(&self.creds.api_secret, timestamp, &self.creds.api_key, RECV_WINDOW_MS, payload)?;
        Ok(req
            .header("X-BAPI-API-KEY", &self.creds.api_key)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-TIMESTAMP", &timestamp.to_string())
            .header("X-BAPI-RECV-WINDOW", &RECV_WINDOW_MS.to_string()))
    }

    fn post(&self, path: &str, body: &Value, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        let text = body.to_string(); // the exact text that is signed is the text that is sent
        let req = OrderHttpRequest::to_demo(Method::Post, DemoEnv::Bybit(self.env), path, ORDER_TIMEOUT);
        self.sign_headers(req, timestamp, &text).map(|r| r.json_body(text))
    }

    fn get(&self, path: &str, params: &[(&str, String)], timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        let query = encode_query(params);
        let req = OrderHttpRequest::to_demo(Method::Get, DemoEnv::Bybit(self.env), &format!("{path}?{query}"), ORDER_TIMEOUT);
        self.sign_headers(req, timestamp, &query)
    }

    /// `POST /v5/order/create`: linear market order, `orderLinkId` = the engine's id, explicit
    /// `reduceOnly`, `positionIdx` 0 (one-way).
    pub fn submit_request(&self, order: &ValidOrder, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        let body = json!({
            "category": "linear",
            "symbol": order.symbol(),
            "side": side_param(order.side()),
            "orderType": "Market",
            "qty": order.quantity_text(),
            "orderLinkId": order.id().as_str(),
            "reduceOnly": order.reduce_only(),
            "positionIdx": 0,
        });
        self.post(BYBIT_CREATE_PATH, &body, timestamp)
    }

    fn ref_param(by: &OrderRef) -> (&'static str, String) {
        match by {
            OrderRef::Client(id) => ("orderLinkId", id.as_str().to_string()),
            OrderRef::Exchange(id) => ("orderId", id.clone()),
        }
    }

    /// `GET /v5/order/realtime` (or `history`) for one order.
    pub fn query_request(&self, path: &str, symbol: &str, by: &OrderRef, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.get(path, &[("category", "linear".to_string()), ("symbol", symbol.to_string()), Self::ref_param(by)], timestamp)
    }

    /// `POST /v5/order/cancel` by `orderLinkId`.
    pub fn cancel_request(&self, symbol: &str, id: &ClientOrderId, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.post(BYBIT_CANCEL_PATH, &json!({ "category": "linear", "symbol": symbol, "orderLinkId": id.as_str() }), timestamp)
    }

    /// `POST /v5/position/set-leverage`: one leverage for both sides of a linear symbol.
    pub fn leverage_request(&self, symbol: &str, leverage: u32, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        let l = leverage.to_string();
        self.post(BYBIT_SET_LEVERAGE_PATH, &json!({ "category": "linear", "symbol": symbol, "buyLeverage": l, "sellLeverage": l }), timestamp)
    }

    /// `GET /v5/position/list` of one symbol.
    pub fn position_mode_request(&self, symbol: &str, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.get(BYBIT_POSITION_PATH, &[("category", "linear".to_string()), ("symbol", symbol.to_string())], timestamp)
    }

    async fn send(&self, req: Result<OrderHttpRequest, AdapterError>) -> Reply {
        match req {
            Ok(r) => bybit_reply(self.transport.send(r).await),
            Err(e) => Reply::Unknown { reason: format!("request not built: {e}") },
        }
    }

    /// Set the symbol's leverage; "leverage not modified" (110043) counts as set.
    pub async fn set_leverage(&self, symbol: &str, leverage: u32, timestamp: i64) -> LeverageOutcome {
        leverage_outcome(self.send(self.leverage_request(symbol, leverage, timestamp)).await, &BYBIT_LEVERAGE_UNCHANGED_CODES)
    }

    pub async fn submit(&self, order: &ValidOrder, timestamp: i64) -> SubmitClass {
        let req = match self.submit_request(order, timestamp) {
            Ok(r) => r,
            Err(e) => return SubmitClass::Rejected { code: "local".into(), message: format!("not sent: {e}") },
        };
        submit_class(bybit_reply(self.transport.send(req).await), |b| parse_ack(b, order.id().as_str()))
    }

    /// `realtime` first, then `history` when the order is not (or no longer) listed there.
    pub async fn query(&self, symbol: &str, by: &OrderRef, timestamp: impl Fn() -> i64) -> QueryOutcome {
        let first = query_outcome(self.send(self.query_request(BYBIT_REALTIME_PATH, symbol, by, timestamp())).await, &BYBIT_NOT_FOUND_CODES, parse_list);
        match first {
            QueryOutcome::NotFound => {
                query_outcome(self.send(self.query_request(BYBIT_HISTORY_PATH, symbol, by, timestamp())).await, &BYBIT_NOT_FOUND_CODES, parse_list)
            }
            other => other,
        }
    }

    /// Cancel, then read the order back (the cancel reply carries no status).
    pub async fn cancel(&self, symbol: &str, id: &ClientOrderId, timestamp: impl Fn() -> i64) -> QueryOutcome {
        match self.send(self.cancel_request(symbol, id, timestamp())).await {
            Reply::Ok(_) => match self.query(symbol, &OrderRef::Client(id.clone()), timestamp).await {
                QueryOutcome::NotFound => QueryOutcome::Failed { reason: "cancel accepted but the order cannot be found".into() },
                other => other,
            },
            other => query_outcome(other, &BYBIT_NOT_FOUND_CODES, |_| Ok(None)),
        }
    }

    pub async fn position_mode(&self, symbol: &str, timestamp: i64) -> Result<ModeReading, String> {
        match self.send(self.position_mode_request(symbol, timestamp)).await {
            Reply::Ok(b) => {
                let rows = b.pointer("/result/list").and_then(Value::as_array).ok_or("position list missing")?;
                let idx: Vec<String> = rows.iter().filter_map(|r| str_opt(r, "positionIdx").ok().flatten()).collect();
                match (idx.is_empty(), idx.iter().all(|i| i == "0")) {
                    (true, _) => Err("position mode unknown: no position rows for the symbol".into()),
                    (false, true) => Ok(ModeReading::OneWay),
                    (false, false) => Ok(ModeReading::Hedge),
                }
            }
            Reply::Refused { code, message } => Err(format!("position mode refused: {code} {message}")),
            Reply::RateLimited { retry_after_ms } => Err(format!("position mode rate limited ({retry_after_ms:?} ms)")),
            Reply::Unknown { reason } => Err(format!("position mode unknown: {reason}")),
        }
    }
}

/// Create reply: `result.orderId`; no fill data (the engine polls).
fn parse_ack(b: &Value, client_order_id: &str) -> Result<OrderStatus, AdapterError> {
    let r = b.get("result").ok_or_else(|| AdapterError::parse("missing result"))?;
    Ok(OrderStatus {
        client_order_id: str_opt(r, "orderLinkId")?.unwrap_or_else(|| client_order_id.to_string()),
        exchange_order_id: Some(str_req(r, "orderId")?),
        filled_quantity: Default::default(),
        avg_price: None,
        fee: None,
        fee_asset: None,
        state: OrderState::Open,
    })
}

/// `result.list[0]` of realtime / history; an empty list is "not here".
fn parse_list(b: &Value) -> Result<Option<OrderStatus>, AdapterError> {
    let rows = b.pointer("/result/list").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing result.list"))?;
    let Some(r) = rows.first() else { return Ok(None) };
    let fee = dec_opt(r, "cumExecFee")?;
    Ok(Some(OrderStatus {
        client_order_id: str_req(r, "orderLinkId")?,
        exchange_order_id: Some(str_req(r, "orderId")?),
        filled_quantity: dec_req(r, "cumExecQty")?,
        avg_price: dec_opt(r, "avgPrice")?.filter(|p| !p.is_zero()),
        fee,
        fee_asset: fee.map(|_| BYBIT_FEE_ASSET.to_string()),
        state: bybit_state(&str_req(r, "orderStatus")?)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::{IdPrefix, client_order_id};
    use crate::engine::ports::{Leg, OrderAction, OrderRequest};
    use crate::exchange::execution::http::fake::{FakeOrderTransport, Reply as R};
    use crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS;
    use tong_funding_core::types::Exchange;

    const KEY: &str = "TEST_KEY_NOT_REAL";
    const SECRET: &str = "TEST_SECRET_NOT_REAL";
    const TS: i64 = 1_700_000_001_200;

    fn id() -> String {
        client_order_id(IdPrefix::Demo, "ab12", Leg::Short, OrderAction::Open, 1)
    }

    fn order(side: OrderSide, reduce_only: bool) -> ValidOrder {
        ValidOrder::from_request(&OrderRequest {
            client_order_id: id(),
            exchange: Exchange::Bybit,
            symbol: "BTCUSDT".into(),
            side,
            quantity: "0.019".parse().unwrap(),
            reduce_only,
            intended_base_qty: None,
            leverage: None,
        })
        .unwrap()
    }

    fn client(t: &FakeOrderTransport) -> BybitOrderClient<FakeOrderTransport> {
        BybitOrderClient::new(Arc::new(t.clone()), Arc::new(Credentials { api_key: KEY.into(), api_secret: SECRET.into(), passphrase: None }), BybitHost::Demo)
    }

    /// Independent HMAC (RFC 2104 by hand over sha2), see the Binance tests for the vector check.
    fn hmac_by_hand(key: &[u8], msg: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut k = [0u8; 64];
        k[..key.len()].copy_from_slice(key);
        let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        let inner = Sha256::digest([ipad.as_slice(), msg].concat());
        Sha256::digest([opad.as_slice(), inner.as_slice()].concat()).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn create_carries_order_link_id_in_the_signed_json_body_on_the_demo_host() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(OrderSide::Sell, false), TS).unwrap();
        assert_eq!(r.method(), Method::Post);
        let parsed = reqwest::Url::parse(r.full_url()).unwrap();
        assert!(ALLOWED_SIGNED_HOSTS.contains(&parsed.host_str().unwrap()));
        assert_eq!(parsed.path(), BYBIT_CREATE_PATH);
        let body: Value = serde_json::from_str(r.body().unwrap()).unwrap();
        assert_eq!(body["orderLinkId"], json!(id()));
        assert_eq!((body["side"].as_str(), body["orderType"].as_str(), body["qty"].as_str()), (Some("Sell"), Some("Market"), Some("0.019")));
        assert_eq!(body["reduceOnly"], json!(false));
        let prehash = format!("{TS}{KEY}5000{}", r.body().unwrap());
        assert_eq!(r.header_value("X-BAPI-SIGN").unwrap(), hmac_by_hand(SECRET.as_bytes(), prehash.as_bytes()));
        assert_eq!(r.header_value("X-BAPI-API-KEY"), Some(KEY));
        assert_eq!(r.header_value("X-BAPI-TIMESTAMP"), Some("1700000001200"));
    }

    #[test]
    fn a_reduce_only_close_carries_the_flag_and_get_requests_sign_the_query() {
        let t = FakeOrderTransport::new();
        let c = client(&t);
        let r = c.submit_request(&order(OrderSide::Buy, true), TS).unwrap();
        let body: Value = serde_json::from_str(r.body().unwrap()).unwrap();
        assert_eq!((body["reduceOnly"].clone(), body["side"].clone()), (json!(true), json!("Buy")));
        let cid = ClientOrderId::parse(&id()).unwrap();
        let q = c.query_request(BYBIT_REALTIME_PATH, "BTCUSDT", &OrderRef::Client(cid.clone()), TS).unwrap();
        let query = q.full_url().split_once('?').unwrap().1.to_string();
        assert!(query.contains(&format!("orderLinkId={}", id())), "{query}");
        let prehash = format!("{TS}{KEY}5000{query}");
        assert_eq!(q.header_value("X-BAPI-SIGN").unwrap(), hmac_by_hand(SECRET.as_bytes(), prehash.as_bytes()));
        let q2 = c.query_request(BYBIT_REALTIME_PATH, "BTCUSDT", &OrderRef::Exchange("abc-1".into()), TS).unwrap();
        assert!(q2.full_url().contains("orderId=abc-1") && !q2.full_url().contains("orderLinkId"));
        let x = c.cancel_request("BTCUSDT", &cid, TS).unwrap();
        let body: Value = serde_json::from_str(x.body().unwrap()).unwrap();
        assert_eq!(body["orderLinkId"], json!(id()));
    }

    #[tokio::test]
    async fn query_falls_back_to_history_and_reads_fee_and_fill() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, BYBIT_REALTIME_PATH, R::ok(r#"{"retCode":0,"retMsg":"OK","result":{"list":[]}}"#));
        let row = format!(r#"{{"retCode":0,"retMsg":"OK","result":{{"list":[{{"orderLinkId":"{}","orderId":"b-1","cumExecQty":"0.014","avgPrice":"60001","cumExecFee":"0.47","orderStatus":"PartiallyFilledCanceled"}}]}}}}"#, id());
        t.on(Method::Get, BYBIT_HISTORY_PATH, R::ok(&row));
        let cid = ClientOrderId::parse(&id()).unwrap();
        match client(&t).query("BTCUSDT", &OrderRef::Client(cid), || TS).await {
            QueryOutcome::Found(s) => {
                assert_eq!((s.filled_quantity, s.state), ("0.014".parse().unwrap(), OrderState::Cancelled));
                assert_eq!((s.fee, s.fee_asset.as_deref()), (Some("0.47".parse().unwrap()), Some("USDT")));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn position_mode_reads_position_idx() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, BYBIT_POSITION_PATH, R::ok(r#"{"retCode":0,"result":{"list":[{"symbol":"BTCUSDT","positionIdx":0,"size":"0"}]}}"#));
        assert_eq!(client(&t).position_mode("BTCUSDT", TS).await, Ok(ModeReading::OneWay));
        t.replace(Method::Get, BYBIT_POSITION_PATH, R::ok(r#"{"retCode":0,"result":{"list":[{"positionIdx":1},{"positionIdx":2}]}}"#));
        assert_eq!(client(&t).position_mode("BTCUSDT", TS).await, Ok(ModeReading::Hedge));
        t.replace(Method::Get, BYBIT_POSITION_PATH, R::ok(r#"{"retCode":0,"result":{"list":[]}}"#));
        assert!(client(&t).position_mode("BTCUSDT", TS).await.is_err(), "no rows = unknown");
    }
}

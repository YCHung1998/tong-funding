//! Binance USDS-M Futures signed ORDER requests (demo/testnet host only): new market order with
//! `newClientOrderId`, query and cancel by `origClientOrderId` (or `orderId`), position mode
//! (`positionSide/dual`) and the fills of one order (fee). Parameters are sent in the signed query
//! string, the same text that is signed (`HMAC-SHA256(secret, query)`).
//!
//! UNVERIFIED until task 4.2: that the testnet accepts `newOrderRespType=RESULT` for MARKET orders,
//! the exact `reduceOnly` behaviour in one-way mode, the id length / charset limits, and the error
//! codes listed in `classify`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tong_funding_core::types::Decimal;

use super::classify::{BINANCE_NOT_FOUND_CODES, Reply, SubmitClass, binance_reply, binance_state, query_outcome, submit_class};
use super::endpoints::{BINANCE_ORDER_PATH, BINANCE_POSITION_MODE_PATH, BINANCE_RESP_TYPE, BINANCE_USER_TRADES_PATH};
use super::http::{DemoEnv, Method, OrderHttpRequest, OrderTransport};
use super::order::{ClientOrderId, OrderRef, ValidOrder};
use crate::engine::ports::{OrderSide, OrderStatus, QueryOutcome};
use crate::exchange::error::AdapterError;
use crate::exchange::signed::endpoints::BinanceHost;
use crate::exchange::signed::models::{dec_opt, dec_req, str_opt, str_req};
use crate::exchange::signed::signing::{Credentials, RECV_WINDOW_MS, binance_signature, encode_query};

/// Timeout of one order request (Python used 10 s; 5 s like the signed reads, UNVERIFIED).
pub const ORDER_TIMEOUT: Duration = Duration::from_secs(5);

/// Position mode as the exchange reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeReading {
    OneWay,
    Hedge,
}

pub struct BinanceOrderClient<T> {
    transport: Arc<T>,
    creds: Arc<Credentials>,
    env: BinanceHost,
}

fn side_param(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    }
}

impl<T: OrderTransport> BinanceOrderClient<T> {
    pub fn new(transport: Arc<T>, creds: Arc<Credentials>, demo_env: BinanceHost) -> Self {
        BinanceOrderClient { transport, creds, env: demo_env }
    }

    /// Signs `params` (+ `timestamp`, `recvWindow`) and builds the request.
    fn signed(&self, method: Method, path: &str, mut params: Vec<(&str, String)>, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        params.push(("timestamp", timestamp.to_string()));
        params.push(("recvWindow", RECV_WINDOW_MS.to_string()));
        let query = encode_query(&params);
        let signature = binance_signature(&self.creds.api_secret, &query)?;
        Ok(OrderHttpRequest::to_demo(method, DemoEnv::Binance(self.env), &format!("{path}?{query}&signature={signature}"), ORDER_TIMEOUT)
            .header("X-MBX-APIKEY", &self.creds.api_key))
    }

    /// `POST /fapi/v1/order` (MARKET), `newClientOrderId` = the engine's id, `reduceOnly` only on
    /// reduce-only orders.
    pub fn submit_request(&self, order: &ValidOrder, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        let mut params = vec![
            ("symbol", order.symbol().to_string()),
            ("side", side_param(order.side()).to_string()),
            ("type", "MARKET".to_string()),
            ("quantity", order.quantity_text()),
            ("newClientOrderId", order.id().as_str().to_string()),
            ("newOrderRespType", BINANCE_RESP_TYPE.to_string()),
        ];
        if order.reduce_only() {
            params.push(("reduceOnly", "true".to_string()));
        }
        self.signed(Method::Post, BINANCE_ORDER_PATH, params, timestamp)
    }

    fn ref_param(by: &OrderRef) -> (&'static str, String) {
        match by {
            OrderRef::Client(id) => ("origClientOrderId", id.as_str().to_string()),
            OrderRef::Exchange(id) => ("orderId", id.clone()),
        }
    }

    /// `GET /fapi/v1/order` by client id or exchange order id.
    pub fn query_request(&self, symbol: &str, by: &OrderRef, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.signed(Method::Get, BINANCE_ORDER_PATH, vec![("symbol", symbol.to_string()), Self::ref_param(by)], timestamp)
    }

    /// `DELETE /fapi/v1/order` by client id (the executor allows own ids only).
    pub fn cancel_request(&self, symbol: &str, id: &ClientOrderId, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        let params = vec![("symbol", symbol.to_string()), ("origClientOrderId", id.as_str().to_string())];
        self.signed(Method::Delete, BINANCE_ORDER_PATH, params, timestamp)
    }

    /// `GET /fapi/v1/positionSide/dual`.
    pub fn position_mode_request(&self, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.signed(Method::Get, BINANCE_POSITION_MODE_PATH, vec![], timestamp)
    }

    /// `GET /fapi/v1/userTrades` of one order.
    pub fn user_trades_request(&self, symbol: &str, order_id: &str, timestamp: i64) -> Result<OrderHttpRequest, AdapterError> {
        self.signed(Method::Get, BINANCE_USER_TRADES_PATH, vec![("symbol", symbol.to_string()), ("orderId", order_id.to_string())], timestamp)
    }

    async fn send(&self, req: Result<OrderHttpRequest, AdapterError>) -> Reply {
        match req {
            Ok(r) => binance_reply(self.transport.send(r).await),
            Err(e) => Reply::Unknown { reason: format!("request not built: {e}") },
        }
    }

    pub async fn submit(&self, order: &ValidOrder, timestamp: i64) -> SubmitClass {
        // A request that cannot even be built was never sent: that is a certain rejection.
        let req = match self.submit_request(order, timestamp) {
            Ok(r) => r,
            Err(e) => return SubmitClass::Rejected { code: "local".into(), message: format!("not sent: {e}") },
        };
        submit_class(binance_reply(self.transport.send(req).await), |b| parse_order(b, order.id().as_str()))
    }

    /// Order status by id; for an order with fills, its fee from `userTrades` (a failed fee lookup
    /// leaves the fee "not reported", never fails the query). `timestamp` is called per request.
    pub async fn query(&self, symbol: &str, by: &OrderRef, timestamp: impl Fn() -> i64) -> QueryOutcome {
        let fallback_id = match by {
            OrderRef::Client(id) => id.as_str().to_string(),
            OrderRef::Exchange(_) => String::new(),
        };
        let reply = self.send(self.query_request(symbol, by, timestamp())).await;
        let outcome = query_outcome(reply, &BINANCE_NOT_FOUND_CODES, |b| parse_order(b, &fallback_id).map(Some));
        match outcome {
            QueryOutcome::Found(mut status) if status.filled_quantity > Decimal::ZERO => {
                if let Some(order_id) = status.exchange_order_id.clone()
                    && let Reply::Ok(trades) = self.send(self.user_trades_request(symbol, &order_id, timestamp())).await
                    && let Some((fee, asset)) = sum_commission(&trades)
                {
                    status.fee = Some(fee);
                    status.fee_asset = Some(asset);
                }
                QueryOutcome::Found(status)
            }
            other => other,
        }
    }

    pub async fn cancel(&self, symbol: &str, id: &ClientOrderId, timestamp: i64) -> QueryOutcome {
        let reply = self.send(self.cancel_request(symbol, id, timestamp)).await;
        query_outcome(reply, &BINANCE_NOT_FOUND_CODES, |b| parse_order(b, id.as_str()).map(Some))
    }

    pub async fn position_mode(&self, timestamp: i64) -> Result<ModeReading, String> {
        match self.send(self.position_mode_request(timestamp)).await {
            Reply::Ok(b) => match b.get("dualSidePosition").and_then(Value::as_bool) {
                Some(false) => Ok(ModeReading::OneWay),
                Some(true) => Ok(ModeReading::Hedge),
                None => Err("position mode reply without dualSidePosition".into()),
            },
            Reply::Refused { code, message } => Err(format!("position mode refused: {code} {message}")),
            Reply::RateLimited { retry_after_ms } => Err(format!("position mode rate limited ({retry_after_ms:?} ms)")),
            Reply::Unknown { reason } => Err(format!("position mode unknown: {reason}")),
        }
    }
}

/// An order object (`POST`/`GET`/`DELETE /fapi/v1/order` reply).
fn parse_order(b: &Value, fallback_client_id: &str) -> Result<OrderStatus, AdapterError> {
    let state = binance_state(&str_req(b, "status")?)?;
    let avg = dec_opt(b, "avgPrice")?.filter(|p| !p.is_zero());
    Ok(OrderStatus {
        client_order_id: str_opt(b, "clientOrderId")?.unwrap_or_else(|| fallback_client_id.to_string()),
        exchange_order_id: Some(str_req(b, "orderId")?),
        filled_quantity: dec_req(b, "executedQty")?,
        avg_price: avg,
        // Not in the order object; filled in from `userTrades` by `query`.
        fee: None,
        fee_asset: None,
        state,
    })
}

/// Sum of `commission` over the trades of one order, when they share one `commissionAsset`.
fn sum_commission(trades: &Value) -> Option<(Decimal, String)> {
    let rows = trades.as_array()?;
    let mut total = Decimal::ZERO;
    let mut asset: Option<String> = None;
    for t in rows {
        let fee = dec_req(t, "commission").ok()?;
        let a = str_req(t, "commissionAsset").ok()?;
        match &asset {
            Some(x) if *x != a => return None, // mixed assets: not reported rather than wrong
            Some(_) => {}
            None => asset = Some(a),
        }
        total += fee;
    }
    asset.map(|a| (total, a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::{IdPrefix, client_order_id};
    use crate::engine::ports::{Leg, OrderAction, OrderRequest, OrderState};
    use crate::exchange::execution::http::fake::{FakeOrderTransport, Reply as R};
    use crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS;
    use tong_funding_core::types::Exchange;

    const KEY: &str = "TEST_KEY_NOT_REAL";
    const SECRET: &str = "TEST_SECRET_NOT_REAL";
    const TS: i64 = 1_700_000_001_200;

    fn creds() -> Arc<Credentials> {
        Arc::new(Credentials { api_key: KEY.into(), api_secret: SECRET.into() })
    }

    fn id() -> String {
        client_order_id(IdPrefix::Demo, "ab12", Leg::Long, OrderAction::Open, 1)
    }

    fn order(reduce_only: bool) -> ValidOrder {
        ValidOrder::from_request(&OrderRequest {
            client_order_id: id(),
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            side: if reduce_only { OrderSide::Sell } else { OrderSide::Buy },
            quantity: "0.019".parse().unwrap(),
            reduce_only,
        })
        .unwrap()
    }

    fn client(t: &FakeOrderTransport) -> BinanceOrderClient<FakeOrderTransport> {
        BinanceOrderClient::new(Arc::new(t.clone()), creds(), BinanceHost::Testnet)
    }

    fn query_of(url: &str) -> &str {
        url.split_once('?').unwrap().1
    }

    /// HMAC-SHA256 hex, computed with nothing but the standard library + sha2 (RFC 2104 by hand),
    /// independent of the `hmac` crate used in production.
    fn hmac_by_hand(key: &[u8], msg: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&Sha256::digest(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        let inner = Sha256::digest([ipad.as_slice(), msg].concat());
        let outer = Sha256::digest([opad.as_slice(), inner.as_slice()].concat());
        outer.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_hand_rolled_hmac_matches_the_published_binance_vector() {
        assert_eq!(
            hmac_by_hand(
                b"NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j",
                b"symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559"
            ),
            "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71"
        );
    }

    #[test]
    fn submit_carries_new_client_order_id_market_type_and_a_valid_signature_on_the_demo_host() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(false), TS).unwrap();
        assert_eq!(r.method(), Method::Post);
        let parsed = reqwest::Url::parse(r.full_url()).unwrap();
        assert!(ALLOWED_SIGNED_HOSTS.contains(&parsed.host_str().unwrap()));
        assert_eq!(parsed.path(), BINANCE_ORDER_PATH);
        let q = query_of(r.full_url());
        assert!(q.contains(&format!("newClientOrderId={}", id())), "{q}");
        for p in ["symbol=BTCUSDT", "side=BUY", "type=MARKET", "quantity=0.019", "timestamp=1700000001200", "recvWindow=5000"] {
            assert!(q.contains(p), "{p} missing in {q}");
        }
        assert!(!q.contains("reduceOnly"), "an opening order is not reduce-only");
        let (unsigned, sig) = q.rsplit_once("&signature=").unwrap();
        assert_eq!(sig, hmac_by_hand(SECRET.as_bytes(), unsigned.as_bytes()));
        assert_eq!(r.header_value("X-MBX-APIKEY"), Some(KEY));
    }

    #[test]
    fn a_reduce_only_close_carries_the_flag() {
        let t = FakeOrderTransport::new();
        let r = client(&t).submit_request(&order(true), TS).unwrap();
        let q = query_of(r.full_url());
        assert!(q.contains("reduceOnly=true") && q.contains("side=SELL"), "{q}");
    }

    #[test]
    fn query_cancel_and_mode_requests_are_signed_and_keyed_by_the_right_id() {
        let t = FakeOrderTransport::new();
        let c = client(&t);
        let cid = ClientOrderId::parse(&id()).unwrap();
        let q = c.query_request("BTCUSDT", &OrderRef::Client(cid.clone()), TS).unwrap();
        assert_eq!(q.method(), Method::Get);
        assert!(query_of(q.full_url()).contains(&format!("origClientOrderId={}", id())));
        let q2 = c.query_request("BTCUSDT", &OrderRef::Exchange("12345".into()), TS).unwrap();
        assert!(query_of(q2.full_url()).contains("orderId=12345") && !q2.full_url().contains("origClientOrderId"));
        let d = c.cancel_request("BTCUSDT", &cid, TS).unwrap();
        assert_eq!(d.method(), Method::Delete);
        assert!(query_of(d.full_url()).contains(&format!("origClientOrderId={}", id())));
        let m = c.position_mode_request(TS).unwrap();
        assert!(m.full_url().contains(BINANCE_POSITION_MODE_PATH));
        for r in [q, q2, d, m] {
            let (unsigned, sig) = query_of(r.full_url()).rsplit_once("&signature=").unwrap();
            assert_eq!(sig, hmac_by_hand(SECRET.as_bytes(), unsigned.as_bytes()));
        }
    }

    #[tokio::test]
    async fn submit_accepted_reply_is_read_and_query_adds_the_fee() {
        let t = FakeOrderTransport::new();
        let ack = format!(r#"{{"orderId":987,"clientOrderId":"{}","status":"FILLED","executedQty":"0.019","avgPrice":"60000.5"}}"#, id());
        t.on(Method::Post, BINANCE_ORDER_PATH, R::ok(&ack));
        t.on(Method::Get, BINANCE_ORDER_PATH, R::ok(&ack));
        t.on(Method::Get, BINANCE_USER_TRADES_PATH, R::ok(r#"[{"commission":"0.2","commissionAsset":"USDT"},{"commission":"0.256","commissionAsset":"USDT"}]"#));
        let c = client(&t);
        match c.submit(&order(false), TS).await {
            SubmitClass::Accepted(s) => {
                assert_eq!((s.client_order_id.as_str(), s.exchange_order_id.as_deref()), (id().as_str(), Some("987")));
                assert_eq!((s.filled_quantity, s.state), ("0.019".parse().unwrap(), OrderState::Filled));
            }
            other => panic!("{other:?}"),
        }
        let cid = ClientOrderId::parse(&id()).unwrap();
        match c.query("BTCUSDT", &OrderRef::Client(cid), || TS).await {
            QueryOutcome::Found(s) => assert_eq!((s.fee, s.fee_asset.as_deref()), (Some("0.456".parse().unwrap()), Some("USDT"))),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_order_is_not_found_and_a_mode_reading_is_parsed() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, BINANCE_ORDER_PATH, R::status(400, r#"{"code":-2013,"msg":"Order does not exist."}"#));
        t.on(Method::Get, BINANCE_POSITION_MODE_PATH, R::ok(r#"{"dualSidePosition":true}"#));
        let c = client(&t);
        let cid = ClientOrderId::parse(&id()).unwrap();
        assert_eq!(c.query("BTCUSDT", &OrderRef::Client(cid), || TS).await, QueryOutcome::NotFound);
        assert_eq!(c.position_mode(TS).await, Ok(ModeReading::Hedge));
    }
}

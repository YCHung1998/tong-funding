//! Binance USDS-M Futures signed GET client (demo/testnet hosts only). Read-only: balances,
//! positions and open orders. Spec: openspec/changes/exchange-readonly-adapters/specs/signed-read-access.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::endpoints::{BINANCE_BALANCE_PATH, BINANCE_LEVERAGE_BRACKET_PATH, BINANCE_OPEN_ORDERS_PATH, BINANCE_POSITIONS_PATH, BinanceHost};
use super::models::{Balance, OpenOrder, OrderSide, Position, PositionMode, bool_opt, dec_opt, dec_req, str_opt, str_req};
use super::signing::{
    ClockOffsetSource, NotConnectedReason, RECV_WINDOW_MS, Resync, SIGNED_TIMEOUT, binance_signature, check_status, encode_query, load_credentials, parse_json, sanitize_error,
    is_timestamp_rejected, require_offset,
};
use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpResponse, HttpTransport};
use crate::ports::{Clock, SecretProvider};

pub struct BinanceSignedClient<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn Clock>,
    offset: Arc<dyn ClockOffsetSource>,
    resync: Arc<dyn Resync>,
    host: BinanceHost,
    reason: Mutex<Option<NotConnectedReason>>,
}

impl<T: HttpTransport> BinanceSignedClient<T> {
    pub const EXCHANGE: Exchange = Exchange::Binance;

    /// `clock` + `offset` produce the calibrated exchange time used for signing and `fetched_at`;
    /// `resync` is called once when a timestamp is rejected; `demo_env` selects one of the
    /// compile-time demo/testnet hosts (there is no way to pass a URL).
    pub fn new(
        transport: Arc<T>,
        secrets: Arc<dyn SecretProvider>,
        clock: Arc<dyn Clock>,
        offset: Arc<dyn ClockOffsetSource>,
        resync: Arc<dyn Resync>,
        demo_env: BinanceHost,
    ) -> Self {
        BinanceSignedClient { transport, secrets, clock, offset, resync, host: demo_env, reason: Mutex::new(None) }
    }

    /// Why the most recent signed call answered `NotConnected` (`None` if it did not, or no call yet).
    pub fn last_not_connected_reason(&self) -> Option<NotConnectedReason> {
        self.reason.lock().ok().and_then(|g| *g)
    }

    /// `GET /fapi/v2/balance`: every asset the exchange lists (zero balances included).
    pub async fn get_balances(&self) -> Result<Vec<Balance>, AdapterError> {
        let (body, at) = self.signed_get(BINANCE_BALANCE_PATH).await?;
        rows(&body)?.iter().map(|r| parse_balance(r, at)).collect()
    }

    /// `GET /fapi/v2/positionRisk`: only rows whose `positionAmt` is not zero.
    pub async fn get_positions(&self) -> Result<Vec<Position>, AdapterError> {
        let (body, at) = self.signed_get(BINANCE_POSITIONS_PATH).await?;
        let mut out = Vec::new();
        for r in rows(&body)? {
            if let Some(p) = parse_position(r, at)? {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// `GET /fapi/v1/openOrders` (all symbols).
    pub async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, AdapterError> {
        let (body, at) = self.signed_get(BINANCE_OPEN_ORDERS_PATH).await?;
        rows(&body)?.iter().map(|r| parse_order(r, at)).collect()
    }

    /// One attempt; if the exchange rejects the timestamp, re-sync the clock once and send exactly
    /// one more request (a second rejection, or a failed re-sync, is returned as is).
    async fn signed_get(&self, path: &str) -> Result<(Value, i64), AdapterError> {
        self.signed_get_with(path, &[]).await
    }

    /// Same as [`Self::signed_get`] with extra query parameters (signed together with the timestamp).
    async fn signed_get_with(&self, path: &str, params: &[(&str, String)]) -> Result<(Value, i64), AdapterError> {
        match self.attempt(path, params).await {
            Err(e) if is_timestamp_rejected(&e) => {
                self.resync.resync().await.map_err(sanitize_error)?;
                self.attempt(path, params).await
            }
            other => other,
        }
    }

    /// `GET /fapi/v1/leverageBracket` of one symbol: the maximum leverage for a position of
    /// `notional` USDT (the bracket that contains it).
    pub async fn get_max_leverage(&self, symbol: &str, notional: Decimal) -> Result<Decimal, AdapterError> {
        let (body, _) = self.signed_get_with(BINANCE_LEVERAGE_BRACKET_PATH, &[("symbol", symbol.to_string())]).await?;
        leverage_cap_from_brackets(&body, symbol, notional)
    }

    fn set_reason(&self, reason: Option<NotConnectedReason>) {
        if let Ok(mut g) = self.reason.lock() {
            *g = reason;
        }
    }

    /// Order of checks matters: secrets, then calibrated time, and only then a request is built.
    /// Returns the parsed body and the exchange time (local clock + offset) at which the response
    /// arrived, using the same offset as the signature.
    async fn attempt(&self, path: &str, params: &[(&str, String)]) -> Result<(Value, i64), AdapterError> {
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
        let mut all: Vec<(&str, String)> = params.to_vec();
        all.push(("timestamp", timestamp.to_string()));
        all.push(("recvWindow", RECV_WINDOW_MS.to_string()));
        let query = encode_query(&all);
        let signature = binance_signature(&creds.api_secret, &query)?;
        let url = format!("{}{}?{}&signature={}", self.host.base_url(), path, query, signature);
        let request = HttpRequest::get(url, SIGNED_TIMEOUT).header("X-MBX-APIKEY", &creds.api_key);
        let response = self.transport.get(request).await.map_err(sanitize_error)?;
        let fetched_at = self.clock.now_ms().saturating_add(offset_ms);
        Ok((interpret(response)?, fetched_at))
    }
}

/// The leverage cap for `notional` from a `leverageBracket` body (an array of `{symbol, brackets}`
/// or one such object): the `initialLeverage` of the bracket with `notionalFloor <= notional <=
/// notionalCap`. A notional above every bracket, or a missing field, is an error (never a default).
pub fn leverage_cap_from_brackets(body: &Value, symbol: &str, notional: Decimal) -> Result<Decimal, AdapterError> {
    let entry = match body {
        Value::Array(a) => a.iter().find(|e| e.get("symbol").and_then(Value::as_str) == Some(symbol)),
        o @ Value::Object(_) if o.get("symbol").and_then(Value::as_str) == Some(symbol) => Some(o),
        _ => None,
    }
    .ok_or_else(|| AdapterError::parse(format!("no leverage brackets for {symbol}")))?;
    let brackets = entry.get("brackets").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing brackets"))?;
    for b in brackets {
        let (floor, cap) = (dec_req(b, "notionalFloor")?, dec_req(b, "notionalCap")?);
        if notional >= floor && notional <= cap {
            return dec_req(b, "initialLeverage");
        }
    }
    Err(AdapterError::parse(format!("notional {notional} is above every leverage bracket of {symbol}")))
}

/// Binance reports failures as `{"code": -1021, "msg": "..."}`, with HTTP 4xx or occasionally 200.
fn exchange_error_in(body: &Value) -> Option<AdapterError> {
    let code = body.get("code")?.as_i64()?;
    let msg = body.get("msg")?.as_str()?;
    (code != 200).then(|| AdapterError::exchange(code.to_string(), msg))
}

fn interpret(resp: HttpResponse) -> Result<Value, AdapterError> {
    if !(200..=299).contains(&resp.status) {
        if !matches!(resp.status, 429 | 418) {
            if let Some(e) = serde_json::from_str::<Value>(&resp.body).ok().as_ref().and_then(exchange_error_in) {
                return Err(e);
            }
        }
        let status = resp.status;
        return Err(check_status(resp).err().unwrap_or(AdapterError::Http { status }));
    }
    let body = parse_json(&resp.body)?;
    match exchange_error_in(&body) {
        Some(e) => Err(e),
        None => Ok(body),
    }
}

fn rows(body: &Value) -> Result<&Vec<Value>, AdapterError> {
    body.as_array().ok_or_else(|| AdapterError::parse("expected a JSON array"))
}

fn parse_balance(r: &Value, fetched_at: i64) -> Result<Balance, AdapterError> {
    let asset = str_req(r, "asset")?;
    let amount = dec_req(r, "balance")?;
    // The endpoint gives no valuation: only USDT is its own USDT value, nothing else is guessed.
    let usdt_value = (asset == "USDT").then_some(amount);
    Ok(Balance { exchange: Exchange::Binance, asset, amount, available: dec_opt(r, "availableBalance")?, usdt_value, fetched_at })
}

fn parse_position(r: &Value, fetched_at: i64) -> Result<Option<Position>, AdapterError> {
    let quantity = dec_req(r, "positionAmt")?;
    if quantity.is_zero() {
        return Ok(None);
    }
    let mode = match str_opt(r, "positionSide")?.as_deref() {
        Some("BOTH") => PositionMode::OneWay,
        Some("LONG" | "SHORT") => PositionMode::Hedge,
        _ => PositionMode::Unknown,
    };
    let isolated = str_opt(r, "marginType")?.is_some_and(|m| m.eq_ignore_ascii_case("isolated"));
    Ok(Some(Position {
        exchange: Exchange::Binance,
        symbol: str_req(r, "symbol")?,
        side: if quantity.is_sign_negative() { Side::Short } else { Side::Long },
        quantity,
        entry_price: dec_opt(r, "entryPrice")?,
        mark_price: dec_opt(r, "markPrice")?,
        leverage: dec_opt(r, "leverage")?,
        unrealized_pnl: dec_opt(r, "unRealizedProfit")?,
        margin: if isolated { dec_opt(r, "isolatedMargin")? } else { None },
        notional: dec_opt(r, "notional")?,
        mode,
        fetched_at,
    }))
}

fn parse_order(r: &Value, fetched_at: i64) -> Result<OpenOrder, AdapterError> {
    let side = match str_req(r, "side")?.as_str() {
        "BUY" => OrderSide::Buy,
        "SELL" => OrderSide::Sell,
        other => return Err(AdapterError::parse(format!("unknown order side {other}"))),
    };
    Ok(OpenOrder {
        exchange: Exchange::Binance,
        symbol: str_req(r, "symbol")?,
        order_id: str_req(r, "orderId")?,
        side,
        order_type: str_req(r, "type")?,
        price: dec_opt(r, "price")?,
        quantity: dec_req(r, "origQty")?,
        filled_quantity: dec_req(r, "executedQty")?,
        reduce_only: bool_opt(r, "reduceOnly"),
        status: str_req(r, "status")?,
        fetched_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::{Value, json};
    use tong_funding_core::types::{Decimal, Side};

    use crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS;
    use crate::exchange::signed::models::{OrderSide, PositionMode};
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
        MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, KEY).with(Exchange::Binance, SecretName::ApiSecret, SECRET)
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

    fn client_full(
        t: FakeTransport,
        secrets: Arc<dyn SecretProvider>,
        offset: Option<i64>,
        host: BinanceHost,
        resync: Arc<FakeResync>,
    ) -> (Arc<FakeTransport>, BinanceSignedClient<FakeTransport>) {
        let t = Arc::new(t);
        let c = BinanceSignedClient::new(t.clone(), secrets, Arc::new(ManualClock::new(NOW)), Arc::new(move || offset), resync, host);
        (t, c)
    }

    fn client_with(t: FakeTransport, secrets: MemorySecrets, offset: Option<i64>, host: BinanceHost) -> (Arc<FakeTransport>, BinanceSignedClient<FakeTransport>) {
        client_full(t, Arc::new(secrets), offset, host, FakeResync::ok())
    }

    fn rejection() -> Result<HttpResponse, AdapterError> {
        let body = json!({"code":-1021,"msg":"Timestamp for this request is outside of the recvWindow."}).to_string();
        Ok(HttpResponse::with_status(400, body))
    }

    // ---- timestamp rejection: resync once, retry exactly once ----

    #[test]
    fn a_rejected_timestamp_is_resynced_once_and_the_request_resent_once() {
        let t = FakeTransport::new().on("/fapi/v2/balance", rejection()).on("/fapi/v2/balance", ok_json(balance_rows()));
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), BinanceHost::Testnet, resync.clone());
        assert_eq!(block_on(c.get_balances()).unwrap().len(), 2);
        assert_eq!(t.requests().len(), 2);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_second_rejection_is_returned_and_there_is_no_third_request() {
        let t = FakeTransport::new().on("/fapi/v2/positionRisk", rejection());
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), BinanceHost::Testnet, resync.clone());
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Exchange { code, .. }) if code == "-1021"));
        assert_eq!(t.requests().len(), 2);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_failed_resync_returns_its_error_and_does_not_resend() {
        let t = FakeTransport::new().on("/fapi/v1/openOrders", rejection());
        let resync = Arc::new(FakeResync { calls: AtomicUsize::new(0), result: Err(AdapterError::Timeout) });
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), BinanceHost::Testnet, resync.clone());
        assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::Timeout);
        assert_eq!(t.requests().len(), 1);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn other_errors_do_not_trigger_a_resync() {
        let body = json!({"code":-2015,"msg":"Invalid API-key"}).to_string();
        let t = FakeTransport::new().on("/fapi/v2/balance", Ok(HttpResponse::with_status(401, body)));
        let resync = FakeResync::ok();
        let (t, c) = client_full(t, Arc::new(full_secrets()), Some(1200), BinanceHost::Testnet, resync.clone());
        assert!(block_on(c.get_balances()).is_err());
        assert_eq!((t.requests().len(), resync.calls()), (1, 0));
    }

    // ---- NotConnected reasons ----

    #[test]
    fn the_not_connected_reason_distinguishes_every_cause_and_clears_on_success() {
        let reason_for = |secrets: Arc<dyn SecretProvider>, offset: Option<i64>| {
            let (t, c) = client_full(FakeTransport::new(), secrets, offset, BinanceHost::Testnet, FakeResync::ok());
            assert_eq!(c.last_not_connected_reason(), None, "no call yet");
            assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(t.requests().len(), 0);
            c.last_not_connected_reason()
        };
        let key_only = MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, KEY);
        let secret_only = MemorySecrets::default().with(Exchange::Binance, SecretName::ApiSecret, SECRET);
        assert_eq!(reason_for(Arc::new(MemorySecrets::default()), Some(0)), Some(NotConnectedReason::NoKey));
        assert_eq!(reason_for(Arc::new(secret_only), Some(0)), Some(NotConnectedReason::NoKey));
        assert_eq!(reason_for(Arc::new(key_only), Some(0)), Some(NotConnectedReason::NoSecret));
        assert_eq!(reason_for(Arc::new(FailingSecrets), Some(0)), Some(NotConnectedReason::SecretStoreError));
        assert_eq!(reason_for(Arc::new(full_secrets()), None), Some(NotConnectedReason::ClockUnsynced));

        // a later successful call clears it
        let t = FakeTransport::new().on("/fapi/v2/balance", ok_json(balance_rows()));
        let offset = Arc::new(AtomicI64::new(-1));
        let o2 = offset.clone();
        let c = BinanceSignedClient::new(
            Arc::new(t),
            Arc::new(full_secrets()),
            Arc::new(ManualClock::new(NOW)),
            Arc::new(move || Some(o2.load(Ordering::SeqCst)).filter(|v| *v >= 0)),
            FakeResync::ok(),
            BinanceHost::Testnet,
        );
        assert!(block_on(c.get_balances()).is_err());
        assert_eq!(c.last_not_connected_reason(), Some(NotConnectedReason::ClockUnsynced));
        offset.store(0, Ordering::SeqCst);
        assert!(block_on(c.get_balances()).is_ok());
        assert_eq!(c.last_not_connected_reason(), None);
    }

    #[test]
    fn fetched_at_is_exchange_time_from_the_same_offset_as_the_signature() {
        let t = FakeTransport::new().on("/fapi/v2/balance", ok_json(balance_rows()));
        let (_, c) = client_with(t, full_secrets(), Some(-300), BinanceHost::Testnet);
        let b = block_on(c.get_balances()).unwrap();
        assert!(b.iter().all(|x| x.fetched_at == NOW - 300));
    }

    fn client(t: FakeTransport) -> (Arc<FakeTransport>, BinanceSignedClient<FakeTransport>) {
        client_with(t, full_secrets(), Some(1200), BinanceHost::Testnet)
    }

    fn ok_json(v: Value) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(v.to_string()))
    }

    fn host_of(url: &str) -> &str {
        url.strip_prefix("https://").unwrap().split('/').next().unwrap()
    }

    fn balance_rows() -> Value {
        json!([
            {"accountAlias":"x","asset":"USDT","balance":"1000.50000000","crossWalletBalance":"1000.5","crossUnPnl":"0.0","availableBalance":"900.25","maxWithdrawAmount":"900.25","marginAvailable":true,"updateTime":1700000000000i64},
            {"accountAlias":"x","asset":"BTC","balance":"0.05000000","crossWalletBalance":"0.05","crossUnPnl":"0.0","availableBalance":"0.05","maxWithdrawAmount":"0.05","marginAvailable":true,"updateTime":1700000000000i64}
        ])
    }

    fn position_row(symbol: &str, amt: &str) -> Value {
        json!({"symbol":symbol,"positionAmt":amt,"entryPrice":"2.5","markPrice":"2.6","unRealizedProfit":"0.1","liquidationPrice":"0","leverage":"5","maxNotionalValue":"1000000","marginType":"cross","isolatedMargin":"0.00000000","isAutoAddMargin":"false","positionSide":"BOTH","notional":"13.0","isolatedWallet":"0","updateTime":1700000000000i64})
    }

    // ---- request shape, signing, time ----

    #[test]
    fn balance_request_is_signed_with_calibrated_time_and_recv_window() {
        let t = FakeTransport::new().on("/fapi/v2/balance", ok_json(balance_rows()));
        let (t, c) = client(t);
        block_on(c.get_balances()).unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 1);
        // timestamp = NOW + 1200; signature precomputed with Python hmac over that exact query
        assert_eq!(
            reqs[0].url,
            "https://testnet.binancefuture.com/fapi/v2/balance?timestamp=1700000001200&recvWindow=5000&signature=5ad0e24bfec9202bf18cbc664ac9a72192a4d5363344bf19227160d8d9179401"
        );
        assert_eq!(reqs[0].headers, vec![("X-MBX-APIKEY".to_string(), KEY.to_string())]);
        assert_eq!(reqs[0].timeout, Duration::from_secs(5));
    }

    #[test]
    fn the_secret_itself_is_never_sent() {
        let t = FakeTransport::new().on("/fapi/v2/positionRisk", ok_json(json!([])));
        let (t, c) = client(t);
        block_on(c.get_positions()).unwrap();
        let r = &t.requests()[0];
        assert!(!r.url.contains(SECRET));
        assert!(r.headers.iter().all(|(_, v)| !v.contains(SECRET)));
        assert!(r.url.starts_with("https://testnet.binancefuture.com/fapi/v2/positionRisk?timestamp="));
    }

    // ---- normalisation ----

    #[test]
    fn balances_keep_each_asset_and_do_not_invent_a_usdt_value() {
        let t = FakeTransport::new().on("/fapi/v2/balance", ok_json(balance_rows()));
        let (_, c) = client(t);
        let b = block_on(c.get_balances()).unwrap();
        assert_eq!(b.len(), 2);
        let btc = b.iter().find(|x| x.asset == "BTC").unwrap();
        assert_eq!(btc.amount, d("0.05"));
        assert_eq!(btc.usdt_value, None, "BTC must not be read as 50000 or 0.05 USDT");
        assert_eq!(btc.exchange, Exchange::Binance);
        assert_eq!(btc.fetched_at, NOW + 1200, "exchange time: local clock + calibrated offset");
        let usdt = b.iter().find(|x| x.asset == "USDT").unwrap();
        assert_eq!((usdt.amount, usdt.available, usdt.usdt_value), (d("1000.5"), Some(d("900.25")), Some(d("1000.5"))));
    }

    #[test]
    fn positions_drop_zero_quantity_rows() {
        let mut rows: Vec<Value> = (0..298).map(|i| position_row(&format!("SYM{i}USDT"), "0.000")).collect();
        rows.push(position_row("GTCUSDT", "0.019934"));
        rows.push(position_row("XRPUSDT", "-12.5"));
        let t = FakeTransport::new().on("/fapi/v2/positionRisk", ok_json(Value::Array(rows)));
        let (_, c) = client(t);
        let p = block_on(c.get_positions()).unwrap();
        assert_eq!(p.len(), 2);
        let long = p.iter().find(|x| x.symbol == "GTCUSDT").unwrap();
        assert_eq!(long.quantity, d("0.019934"));
        assert_eq!(long.quantity.to_string(), "0.019934");
        assert_eq!(long.side, Side::Long);
        let short = p.iter().find(|x| x.symbol == "XRPUSDT").unwrap();
        assert_eq!((short.quantity, short.side), (d("-12.5"), Side::Short));
        assert_eq!(short.entry_price, Some(d("2.5")));
        assert_eq!(short.mark_price, Some(d("2.6")));
        assert_eq!(short.leverage, Some(d("5")));
        assert_eq!(short.unrealized_pnl, Some(d("0.1")));
        assert_eq!(short.notional, Some(d("13.0")));
        assert_eq!(short.margin, None, "cross margin: isolatedMargin is not a margin figure");
        assert_eq!(short.mode, PositionMode::OneWay);
        assert_eq!((short.exchange, short.fetched_at), (Exchange::Binance, NOW + 1200));
    }

    #[test]
    fn position_mode_and_isolated_margin() {
        let mut hedge = position_row("BTCUSDT", "0.5");
        hedge["positionSide"] = json!("LONG");
        hedge["marginType"] = json!("isolated");
        hedge["isolatedMargin"] = json!("12.34");
        let mut unknown = position_row("ETHUSDT", "1");
        unknown.as_object_mut().unwrap().remove("positionSide");
        let t = FakeTransport::new().on("/fapi/v2/positionRisk", ok_json(json!([hedge, unknown])));
        let (_, c) = client(t);
        let p = block_on(c.get_positions()).unwrap();
        assert_eq!((p[0].mode, p[0].margin), (PositionMode::Hedge, Some(d("12.34"))));
        assert_eq!(p[1].mode, PositionMode::Unknown);
    }

    #[test]
    fn open_orders_are_normalised() {
        let rows = json!([{"orderId":123456789i64,"symbol":"BTCUSDT","status":"NEW","price":"60000.10","origQty":"0.010","executedQty":"0.004","type":"LIMIT","side":"SELL","reduceOnly":true,"positionSide":"BOTH","time":1700000000000i64}]);
        let t = FakeTransport::new().on("/fapi/v1/openOrders", ok_json(rows));
        let (t, c) = client(t);
        let o = block_on(c.get_open_orders()).unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].order_id, "123456789");
        assert_eq!((o[0].side, o[0].reduce_only), (OrderSide::Sell, true));
        assert_eq!((o[0].price, o[0].quantity, o[0].filled_quantity), (Some(d("60000.10")), d("0.010"), d("0.004")));
        assert_eq!((o[0].order_type.as_str(), o[0].status.as_str()), ("LIMIT", "NEW"));
        assert!(t.requests()[0].url.contains("/fapi/v1/openOrders?timestamp="));
    }

    #[test]
    fn malformed_rows_are_parse_errors_not_silent_skips() {
        let t = FakeTransport::new().on("/fapi/v2/positionRisk", ok_json(json!([{"symbol":"BTCUSDT","positionAmt":"abc"}])));
        let (_, c) = client(t);
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Parse(_))));
        let t = FakeTransport::new().on("/fapi/v2/balance", ok_json(json!({"unexpected":"object"})));
        let (_, c) = client(t);
        assert!(matches!(block_on(c.get_balances()), Err(AdapterError::Parse(_))));
    }

    // ---- secrets / calibration: no request ----

    #[test]
    fn missing_or_failing_secrets_mean_not_connected_with_zero_requests() {
        let cases = [
            MemorySecrets::default(),
            MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, KEY),
            MemorySecrets::default().with(Exchange::Binance, SecretName::ApiSecret, SECRET),
            // only Bybit's secrets exist
            MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY).with(Exchange::Bybit, SecretName::ApiSecret, SECRET),
        ];
        for secrets in cases {
            let (t, c) = client_with(FakeTransport::new(), secrets, Some(0), BinanceHost::Testnet);
            assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::NotConnected);
            assert_eq!(t.requests().len(), 0);
        }
    }

    #[test]
    fn uncalibrated_clock_means_not_connected_with_zero_requests() {
        let (t, c) = client_with(FakeTransport::new(), full_secrets(), None, BinanceHost::Demo);
        assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(t.requests().len(), 0);
    }

    // ---- destination hosts ----

    #[test]
    fn every_signed_method_only_reaches_the_chosen_demo_host() {
        // An environment variable that tries to redirect the client must have no effect.
        // SAFETY: test-only; no other test reads this variable.
        unsafe { std::env::set_var("BINANCE_BASE_URL", "https://prod.example.invalid") };
        for host in [BinanceHost::Testnet, BinanceHost::Demo] {
            let t = FakeTransport::new()
                .on("/fapi/v2/balance", ok_json(balance_rows()))
                .on("/fapi/v2/positionRisk", ok_json(json!([])))
                .on("/fapi/v1/openOrders", ok_json(json!([])));
            let (t, c) = client_with(t, full_secrets(), Some(0), host);
            block_on(c.get_balances()).unwrap();
            block_on(c.get_positions()).unwrap();
            block_on(c.get_open_orders()).unwrap();
            let reqs = t.requests();
            assert_eq!(reqs.len(), 3);
            for r in reqs {
                assert_eq!(host_of(&r.url), host.host());
                assert!(ALLOWED_SIGNED_HOSTS.contains(&host_of(&r.url)));
                assert!(!r.url.contains("prod.example.invalid"));
            }
        }
        unsafe { std::env::remove_var("BINANCE_BASE_URL") };
    }

    // ---- failures ----

    #[test]
    fn rate_limit_statuses() {
        let mut limited = HttpResponse::with_status(429, "{}");
        limited.headers.push(("Retry-After".into(), "3".into()));
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/balance", Ok(limited)));
        assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: Some(3000) });
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/balance", Ok(HttpResponse::with_status(418, ""))));
        assert!(matches!(block_on(c.get_balances()), Err(AdapterError::RateLimited { .. })));
    }

    #[test]
    fn exchange_error_bodies_become_exchange_errors() {
        // HTTP 200 with an error object
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/balance", ok_json(json!({"code":-2015,"msg":"Invalid API-key, IP, or permissions for action."}))));
        assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::Exchange { code: "-2015".into(), message: "Invalid API-key, IP, or permissions for action.".into() });
        // HTTP 400 with an error object (timestamp outside recvWindow)
        let body = json!({"code":-1021,"msg":"Timestamp for this request is outside of the recvWindow."}).to_string();
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/positionRisk", Ok(HttpResponse::with_status(400, body))));
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Exchange { code, .. }) if code == "-1021"));
        // non-JSON 5xx
        let (_, c) = client(FakeTransport::new().on("/fapi/v1/openOrders", Ok(HttpResponse::with_status(502, "<html>"))));
        assert_eq!(block_on(c.get_open_orders()).unwrap_err(), AdapterError::Http { status: 502 });
    }

    #[test]
    fn invalid_json_and_timeouts() {
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/balance", Ok(HttpResponse::ok("<<not json>>"))));
        assert!(matches!(block_on(c.get_balances()), Err(AdapterError::Parse(_))));
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/balance", Err(AdapterError::Timeout)));
        assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::Timeout);
    }

    #[test]
    fn connection_error_text_containing_the_signed_url_is_redacted() {
        // A transport that built the variant directly (bypassing AdapterError::network).
        let raw = AdapterError::Network("error sending request for url (https://testnet.binancefuture.com/fapi/v2/balance?timestamp=1&recvWindow=5000&signature=abcdef): operation timed out".into());
        let (_, c) = client(FakeTransport::new().on("/fapi/v2/balance", Err(raw)));
        let e = block_on(c.get_balances()).unwrap_err();
        let text = e.to_string();
        assert!(!text.contains("abcdef"), "leaked: {text}");
        assert!(text.contains("signature=[REDACTED]"));
        assert!(!text.contains(KEY) && !text.contains(SECRET));
    }
}

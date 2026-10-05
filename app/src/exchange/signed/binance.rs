//! Binance USDS-M Futures signed GET client (demo/testnet hosts only). Read-only: balances,
//! positions and open orders. Spec: openspec/changes/exchange-readonly-adapters/specs/signed-read-access.

use std::sync::Arc;

use serde_json::Value;
use tong_funding_core::types::{Exchange, Side};

use super::endpoints::{BINANCE_BALANCE_PATH, BINANCE_OPEN_ORDERS_PATH, BINANCE_POSITIONS_PATH, BinanceHost};
use super::models::{Balance, OpenOrder, OrderSide, Position, PositionMode, bool_opt, dec_opt, dec_req, str_opt, str_req};
use super::signing::{
    ClockOffsetSource, RECV_WINDOW_MS, SIGNED_TIMEOUT, binance_signature, check_status, encode_query, load_credentials, parse_json, sanitize_error,
    signed_timestamp,
};
use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpResponse, HttpTransport};
use crate::ports::{Clock, SecretProvider};

pub struct BinanceSignedClient<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn Clock>,
    offset: Arc<dyn ClockOffsetSource>,
    host: BinanceHost,
}

impl<T: HttpTransport> BinanceSignedClient<T> {
    pub const EXCHANGE: Exchange = Exchange::Binance;

    /// `clock` + `offset` produce the calibrated signing time; `host` selects one of the
    /// compile-time demo/testnet hosts (there is no way to pass a URL).
    pub fn new(transport: Arc<T>, secrets: Arc<dyn SecretProvider>, clock: Arc<dyn Clock>, offset: Arc<dyn ClockOffsetSource>, demo_env: BinanceHost) -> Self {
        BinanceSignedClient { transport, secrets, clock, offset, host: demo_env }
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

    /// Order of checks matters: secrets, then calibrated time, and only then a request is built.
    /// Returns the parsed body and the local time at which the response arrived.
    async fn signed_get(&self, path: &str) -> Result<(Value, i64), AdapterError> {
        let creds = load_credentials(self.secrets.as_ref(), Self::EXCHANGE)?;
        let timestamp = signed_timestamp(self.clock.as_ref(), self.offset.as_ref())?;
        let query = encode_query(&[("timestamp", timestamp.to_string()), ("recvWindow", RECV_WINDOW_MS.to_string())]);
        let signature = binance_signature(&creds.api_secret, &query)?;
        let url = format!("{}{}?{}&signature={}", self.host.base_url(), path, query, signature);
        let request = HttpRequest::get(url, SIGNED_TIMEOUT).header("X-MBX-APIKEY", &creds.api_key);
        let response = self.transport.get(request).await.map_err(sanitize_error)?;
        let fetched_at = self.clock.now_ms();
        Ok((interpret(response)?, fetched_at))
    }
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

    fn client_with(t: FakeTransport, secrets: MemorySecrets, offset: Option<i64>, host: BinanceHost) -> (Arc<FakeTransport>, BinanceSignedClient<FakeTransport>) {
        let t = Arc::new(t);
        let c = BinanceSignedClient::new(t.clone(), Arc::new(secrets), Arc::new(ManualClock::new(NOW)), Arc::new(move || offset), host);
        (t, c)
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
        assert_eq!(btc.fetched_at, NOW);
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
        assert_eq!((short.exchange, short.fetched_at), (Exchange::Binance, NOW));
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

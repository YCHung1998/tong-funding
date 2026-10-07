//! OKX v5 signed GET client (Demo Trading via the `x-simulated-trading: 1` header). Read-only:
//! account mode, balances, available margin, positions and open orders. Spec: okx-signed-read.
//!
//! OKX demo and production share one host, so safety does not come from the host: every request is
//! built by `HttpRequest::okx_signed_get` from an `OkxTarget`, which inserts the flag itself, and
//! the real transports refuse (zero connections) an OKX request without it (`HostPolicy::allows`).
//! This file names no host and no header literal.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tong_funding_core::redact::redact_secrets;
use tong_funding_core::types::{Decimal, Exchange};

use super::endpoints::{MAX_PAGES, OkxHost, okx_symbol};
use super::models::{Balance, Completeness, Listing, OpenOrder, OrderSide, dec_opt, dec_req, str_opt, str_req};
use super::signing::{
    ClockOffsetSource, NotConnectedReason, Resync, SIGNED_TIMEOUT, check_status, is_timestamp_rejected, load_credentials, okx_auth_headers, okx_signature, okx_signed_timestamp,
    parse_json, require_offset, sanitize_error,
};
use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpTransport};
use crate::ports::{Clock, SecretProvider};

const OKX_CONFIG_PATH: &str = "/api/v5/account/config";
const OKX_BALANCE_PATH: &str = "/api/v5/account/balance";
const OKX_POSITIONS_PATH: &str = "/api/v5/account/positions";
const OKX_PENDING_ORDERS_PATH: &str = "/api/v5/trade/orders-pending";
/// Page size of `orders-pending` (documented maximum 100; UNVERIFIED against a real account).
const OKX_ORDERS_PAGE_LIMIT: usize = 100;
/// OKX codes meaning "slow down" (request too frequent, sub-account rate limit). ONE list for the
/// read and the order paths.
pub const OKX_RATE_LIMIT_CODES: [i64; 2] = [50011, 50061];
/// OKX codes meaning "the outcome is unknown" (service unavailable, endpoint timeout - "does not mean
/// the request was successful or failed" -, system busy, system error). UNVERIFIED list.
pub const OKX_UNKNOWN_CODES: [i64; 4] = [50001, 50004, 50013, 50026];
/// Code of the `AdapterError::Exchange` that reports an unsupported account mode.
pub const ACCOUNT_MODE_CODE: &str = "acct_mode_unsupported";

/// How long a confirmed account-mode reading is trusted (read gate and the executor's one-way gate).
pub const POSITION_MODE_TTL_MS: i64 = 60_000;

/// The one reason text of an environment mismatch (`50101`).
pub const ENV_MISMATCH_REASON: &str = "OKX 50101: API key does not match the environment (demo flag sent); OKX disabled until restart";

/// "OKX is disabled" latch, tripped by an environment mismatch (`50101`) on any OKX request. Once
/// tripped (first reason wins) it never resets. Production owns ONE `Arc<OkxLatch>` (the executor
/// factory, `okx_latch()`) and hands it to the order client and to the read client
/// (`OkxSignedClient::with_latch`, wired in okx-trading-enablement 3.5).
#[derive(Debug, Default)]
pub struct OkxLatch(std::sync::OnceLock<String>);

impl OkxLatch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn trip(&self, reason: &str) {
        let _ = self.0.set(reason.to_string());
    }
    pub fn reason(&self) -> Option<String> {
        self.0.get().cloned()
    }
}

/// Code of the `AdapterError::Exchange` returned while the latch is tripped.
pub const OKX_DISABLED_CODE: &str = "okx_disabled";
/// OKX "APIKey does not match current environment".
pub const ENV_MISMATCH_CODE: &str = "50101";

/// The account levels the system supports (design D4); 1 (spot) and 4 (portfolio margin) are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OkxAcctLv {
    /// `acctLv` 2: futures mode (margin in the per-currency `availEq`).
    Futures,
    /// `acctLv` 3: multi-currency margin (margin in the account-level `availEq`).
    MultiCurrency,
}

/// A non-zero `-USDT-SWAP` position. `contracts` is signed (net mode: long positive, short
/// negative) and in CONTRACTS, the unit of `AccountPosition.quantity`; converting to a base-coin
/// amount (`ctVal`) is a page concern.
#[derive(Debug, Clone, PartialEq)]
pub struct OkxPosition {
    /// System symbol (`BTCUSDT`).
    pub symbol: String,
    pub contracts: Decimal,
    pub pos_side: String,
    pub mgn_mode: String,
    pub avg_px: Option<Decimal>,
    pub mark_px: Option<Decimal>,
    pub upl: Option<Decimal>,
    pub lever: Option<Decimal>,
    pub imr: Option<Decimal>,
    pub notional_usd: Option<Decimal>,
    pub fetched_at: i64,
}

pub struct OkxSignedClient<T> {
    transport: Arc<T>,
    secrets: Arc<dyn SecretProvider>,
    clock: Arc<dyn Clock>,
    offset: Arc<dyn ClockOffsetSource>,
    resync: Arc<dyn Resync>,
    host: OkxHost,
    reason: Mutex<Option<NotConnectedReason>>,
    /// Local ms and level of the last supported reading; unsupported or failed readings are never stored.
    mode: Mutex<Option<(i64, OkxAcctLv)>>,
    latch: Arc<OkxLatch>,
}

impl<T: HttpTransport> OkxSignedClient<T> {
    pub const EXCHANGE: Exchange = Exchange::Okx;

    pub fn new(transport: Arc<T>, secrets: Arc<dyn SecretProvider>, clock: Arc<dyn Clock>, offset: Arc<dyn ClockOffsetSource>, resync: Arc<dyn Resync>, demo_env: OkxHost) -> Self {
        OkxSignedClient { transport, secrets, clock, offset, resync, host: demo_env, reason: Mutex::new(None), mode: Mutex::new(None), latch: OkxLatch::new() }
    }

    /// Shares the production `OkxLatch` (owned by the executor factory) with the order client.
    pub fn with_latch(mut self, latch: Arc<OkxLatch>) -> Self {
        self.latch = latch;
        self
    }

    /// Why the most recent signed call answered `NotConnected` (`None` if it did not, or no call yet).
    pub fn last_not_connected_reason(&self) -> Option<NotConnectedReason> {
        self.reason.lock().ok().and_then(|g| *g)
    }

    /// `GET /api/v5/account/config`: `Ok` only for `acctLv` 2 or 3 with `posMode` `net_mode`. A
    /// supported reading is reused for [`POSITION_MODE_TTL_MS`]; anything else (unsupported mode,
    /// unreadable) is an error and is not cached. The system never changes the account's settings.
    pub async fn account_mode(&self) -> Result<OkxAcctLv, AdapterError> {
        let now = self.clock.now_ms();
        if let Some((at, lv)) = self.mode.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref().copied()
            && now - at < POSITION_MODE_TTL_MS
        {
            return Ok(lv);
        }
        let (body, _) = self.signed_get(OKX_CONFIG_PATH, "").await?;
        let row = data_rows(&body)?.first().ok_or_else(|| AdapterError::parse("empty account config"))?;
        let level = parse_account_mode(row)?;
        *self.mode.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((now, level));
        Ok(level)
    }

    /// `GET /api/v5/account/balance`: every currency of the account.
    pub async fn get_balances(&self) -> Result<Vec<Balance>, AdapterError> {
        let (body, at) = self.signed_get(OKX_BALANCE_PATH, "").await?;
        let mut out = Vec::new();
        for account in data_rows(&body)? {
            let details = account.get("details").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing details list"))?;
            for c in details {
                out.push(parse_balance(c, at)?);
            }
        }
        Ok(out)
    }

    /// The exchange's own available equity (design D5): the USDT `availEq` in futures mode, the
    /// account-level `availEq` in multi-currency margin. Empty / missing is an error: nothing is
    /// computed or substituted (`availBal` and `cashBal` are different things).
    pub async fn get_available_margin(&self) -> Result<Decimal, AdapterError> {
        let level = self.account_mode().await?;
        let (body, _) = self.signed_get(OKX_BALANCE_PATH, "").await?;
        available_margin_from(&body, level)
    }

    /// `GET /api/v5/account/positions?instType=SWAP`: non-zero `-USDT-SWAP` positions, in contracts.
    /// Any isolated-margin or long/short-side row makes the whole list an error.
    pub async fn get_positions(&self) -> Result<Listing<OkxPosition>, AdapterError> {
        self.account_mode().await?;
        let (body, at) = self.signed_get(OKX_POSITIONS_PATH, "instType=SWAP").await?;
        let mut items = Vec::new();
        for row in data_rows(&body)? {
            if let Some(p) = parse_position(row, at)? {
                items.push(p);
            }
        }
        Ok(Listing { items, completeness: Completeness::Complete })
    }

    /// `GET /api/v5/trade/orders-pending?instType=SWAP` across all pages (`after` = last `ordId`).
    /// Quantities are contracts. A failure after the first page yields `Incomplete`.
    pub async fn get_open_orders(&self) -> Result<Listing<OpenOrder>, AdapterError> {
        self.account_mode().await?;
        let mut items: Vec<OpenOrder> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut after: Option<String> = None;
        for page in 1..=MAX_PAGES {
            let mut query = format!("instType=SWAP&limit={OKX_ORDERS_PAGE_LIMIT}");
            if let Some(c) = &after {
                query.push_str("&after=");
                query.push_str(c);
            }
            let fetched = self.signed_get(OKX_PENDING_ORDERS_PATH, &query).await.and_then(|(body, at)| {
                let rows = data_rows(&body)?;
                let mut parsed = Vec::new();
                for r in rows {
                    if let Some(o) = parse_order(r, at)? {
                        parsed.push(o);
                    }
                }
                // the cursor is the last RAW row, whatever was filtered out of `parsed`
                let last = if rows.len() >= OKX_ORDERS_PAGE_LIMIT { Some(str_req(&rows[rows.len() - 1], "ordId")?) } else { None };
                Ok((parsed, last))
            });
            match fetched {
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
                        Some(c) => after = Some(c),
                    }
                }
            }
        }
        Ok(incomplete(items, format!("page cap of {MAX_PAGES} reached")))
    }

    /// One attempt; if the exchange rejects the timestamp (`50102`), re-sync the clock once and send
    /// exactly one more request, rebuilt from scratch (so it carries the same demo flag by the same
    /// constructor). A second rejection, or a failed re-sync, is returned as is.
    pub(in crate::exchange) async fn signed_get(&self, path: &str, query: &str) -> Result<(Value, i64), AdapterError> {
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

    /// Order of checks: key, secret, passphrase, calibrated time; only then a request is built.
    /// `query` is the final, already-encoded query string: the same text is signed and sent.
    async fn attempt(&self, path: &str, query: &str) -> Result<(Value, i64), AdapterError> {
        if let Some(reason) = self.latch.reason() {
            return Err(AdapterError::exchange(OKX_DISABLED_CODE, format!("OKX disabled: {reason}")));
        }
        let result = self.attempt_inner(path, query).await;
        if let Err(AdapterError::Exchange { code, .. }) = &result
            && code == ENV_MISMATCH_CODE
        {
            self.latch.trip(ENV_MISMATCH_REASON);
        }
        result
    }

    async fn attempt_inner(&self, path: &str, query: &str) -> Result<(Value, i64), AdapterError> {
        let prepared = load_credentials(self.secrets.as_ref(), Self::EXCHANGE, true).and_then(|c| require_offset(self.offset.as_ref()).map(|o| (c, o)));
        let (creds, offset_ms) = match prepared {
            Ok(v) => v,
            Err(reason) => {
                self.set_reason(Some(reason));
                return Err(reason.into());
            }
        };
        let timestamp = match okx_signed_timestamp(self.clock.as_ref(), self.offset.as_ref()) {
            Ok(t) => t,
            Err(reason) => {
                self.set_reason(Some(reason));
                return Err(reason.into());
            }
        };
        self.set_reason(None);
        let request_path = if query.is_empty() { path.to_string() } else { format!("{path}?{query}") };
        let signature = okx_signature(&creds.api_secret, &timestamp, "GET", &request_path, "")?;
        let auth = okx_auth_headers(&creds, &timestamp, &signature).map_err(AdapterError::from)?;
        let request = HttpRequest::okx_signed_get(&self.host.target(), &request_path, auth, SIGNED_TIMEOUT);
        let response = self.transport.get(request).await.map_err(sanitize_error)?;
        let fetched_at = self.clock.now_ms().saturating_add(offset_ms);
        let (status, body_text) = (response.status, response.body.clone());
        // OKX reports most failures as HTTP 200 + `code`, but authentication failures may come with
        // a 4xx status and the same JSON body: keep the code (and so the 50102 retry) in that case.
        if !(200..300).contains(&status)
            && status != 429
            && let Ok(v) = parse_json(&body_text)
            && let Some(err) = code_error(&v)
        {
            return Err(err);
        }
        let response = check_status(response)?;
        let body = parse_json(&response.body)?;
        match code_error(&body) {
            Some(err) => Err(err),
            None => Ok((body, fetched_at)),
        }
    }
}

/// The supported account levels of one `account/config` row (design D4): `acctLv` 2 or 3 with
/// `net_mode`. Everything else is the "帳戶模式不支援" error naming the actual settings.
pub fn parse_account_mode(row: &Value) -> Result<OkxAcctLv, AdapterError> {
    let acct_lv = str_req(row, "acctLv")?;
    let pos_mode = str_req(row, "posMode")?;
    match (acct_lv.as_str(), pos_mode.as_str()) {
        ("2", "net_mode") => Ok(OkxAcctLv::Futures),
        ("3", "net_mode") => Ok(OkxAcctLv::MultiCurrency),
        _ => Err(AdapterError::exchange(ACCOUNT_MODE_CODE, format!("帳戶模式不支援 (acctLv {acct_lv}, posMode {pos_mode})"))),
    }
}

/// `Some(error)` unless the body says `code == "0"`; a body without a code is a parse error.
fn code_error(body: &Value) -> Option<AdapterError> {
    let code = match body.get("code") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Some(AdapterError::parse("missing code")),
    };
    if code == "0" {
        return None;
    }
    if code.parse::<i64>().is_ok_and(|n| OKX_RATE_LIMIT_CODES.contains(&n)) {
        return Some(AdapterError::RateLimited { retry_after_ms: None });
    }
    let msg = body.get("msg").and_then(Value::as_str).unwrap_or("");
    Some(AdapterError::exchange(code, msg))
}

fn incomplete<R>(items: Vec<R>, reason: String) -> Listing<R> {
    Listing { items, completeness: Completeness::Incomplete { reason: redact_secrets(&reason) } }
}

fn data_rows(body: &Value) -> Result<&Vec<Value>, AdapterError> {
    body.get("data").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing data list"))
}

fn parse_balance(c: &Value, fetched_at: i64) -> Result<Balance, AdapterError> {
    let asset = str_req(c, "ccy")?;
    let amount = dec_req(c, "cashBal")?;
    // Only a valuation the exchange provides is used (USDT is its own value); nothing is estimated.
    let usdt_value = match dec_opt(c, "eqUsd")? {
        Some(v) => Some(v),
        None if asset == "USDT" => Some(amount),
        None => None,
    };
    Ok(Balance { exchange: Exchange::Okx, asset, amount, available: dec_opt(c, "availBal")?, usdt_value, fetched_at })
}

/// Available margin from a `balance` body (design D5); no fallback to any other field.
pub fn available_margin_from(body: &Value, level: OkxAcctLv) -> Result<Decimal, AdapterError> {
    let account = data_rows(body)?.first().ok_or_else(|| AdapterError::parse("empty balance data"))?;
    match level {
        OkxAcctLv::MultiCurrency => dec_opt(account, "availEq")?.ok_or_else(|| AdapterError::parse("account-level availEq not reported")),
        OkxAcctLv::Futures => {
            let usdt = account
                .get("details")
                .and_then(Value::as_array)
                .and_then(|d| d.iter().find(|c| c.get("ccy").and_then(Value::as_str) == Some("USDT")))
                .ok_or_else(|| AdapterError::parse("no USDT entry in balance details"))?;
            dec_opt(usdt, "availEq")?.ok_or_else(|| AdapterError::parse("USDT availEq not reported"))
        }
    }
}

/// `None` for rows that are not `-USDT-SWAP` or have no position; an error for a row the system
/// cannot represent (isolated margin, long/short side): see `get_positions`.
fn parse_position(r: &Value, fetched_at: i64) -> Result<Option<OkxPosition>, AdapterError> {
    let Some(symbol) = okx_symbol(&str_req(r, "instId")?) else { return Ok(None) };
    let pos_side = str_req(r, "posSide")?;
    let mgn_mode = str_req(r, "mgnMode")?;
    if pos_side != "net" {
        return Err(AdapterError::parse(format!("{symbol}: posSide {pos_side}; the system assumes net mode")));
    }
    if mgn_mode != "cross" {
        return Err(AdapterError::parse(format!("{symbol}: mgnMode {mgn_mode} (isolated positions are not supported)")));
    }
    let contracts = dec_req(r, "pos")?;
    if contracts.is_zero() {
        return Ok(None);
    }
    Ok(Some(OkxPosition {
        symbol,
        contracts,
        pos_side,
        mgn_mode,
        avg_px: dec_opt(r, "avgPx")?,
        mark_px: dec_opt(r, "markPx")?,
        upl: dec_opt(r, "upl")?,
        lever: dec_opt(r, "lever")?,
        imr: dec_opt(r, "imr")?,
        notional_usd: dec_opt(r, "notionalUsd")?,
        fetched_at,
    }))
}

fn parse_order(r: &Value, fetched_at: i64) -> Result<Option<OpenOrder>, AdapterError> {
    let Some(symbol) = okx_symbol(&str_req(r, "instId")?) else { return Ok(None) };
    let side = match str_req(r, "side")?.as_str() {
        "buy" => OrderSide::Buy,
        "sell" => OrderSide::Sell,
        other => return Err(AdapterError::parse(format!("unknown order side {other}"))),
    };
    Ok(Some(OpenOrder {
        exchange: Exchange::Okx,
        symbol,
        order_id: str_req(r, "ordId")?,
        side,
        order_type: str_req(r, "ordType")?,
        price: dec_opt(r, "px")?,
        quantity: dec_req(r, "sz")?,
        filled_quantity: dec_opt(r, "accFillSz")?.unwrap_or(Decimal::ZERO),
        reduce_only: str_opt(r, "reduceOnly")?.as_deref() == Some("true"),
        status: str_req(r, "state")?,
        fetched_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use crate::exchange::reqwest_transport::HostPolicy;
    use crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS;
    use crate::exchange::signed::models::{Completeness, OrderSide};
    use crate::exchange::signed::signing::okx_signature;
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::{ManualClock, MemorySecrets, SecretName};

    const KEY: &str = "TEST_KEY_NOT_REAL";
    const SECRET: &str = "TEST_SECRET_NOT_REAL";
    const PASS: &str = "TEST_PASS_NOT_REAL";
    /// 2020-12-08T09:08:57.000Z; with the +715 ms offset below the signing timestamp is the one in the OKX docs.
    const NOW: i64 = 1_607_418_537_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    /// A recorded-style fixture (hand-built from the OKX docs; see the `.meta` next to each file).
    fn fx(name: &str) -> String {
        let path = format!("{}/tests/fixtures/okx/signed/{name}.json", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"))
    }

    fn ok(name: &str) -> Result<HttpResponse, AdapterError> {
        Ok(HttpResponse::ok(fx(name)))
    }

    fn full_secrets() -> MemorySecrets {
        MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, KEY).with(Exchange::Okx, SecretName::ApiSecret, SECRET).with(Exchange::Okx, SecretName::Passphrase, PASS)
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

    /// Wraps the scripted fake with the REAL admission rule of the real transports
    /// (`HostPolicy::SignedDemo.allows` + the protected-header check): a request the real transport
    /// would refuse never reaches the script. Every OKX test below therefore fails if any request
    /// lacked `x-simulated-trading: 1`.
    struct PolicyTransport {
        inner: FakeTransport,
        refused: AtomicUsize,
    }
    impl PolicyTransport {
        fn new(inner: FakeTransport) -> Self {
            PolicyTransport { inner, refused: AtomicUsize::new(0) }
        }
        fn requests(&self) -> Vec<HttpRequest> {
            self.inner.requests()
        }
        fn refused(&self) -> usize {
            self.refused.load(Ordering::SeqCst)
        }
    }
    impl HttpTransport for PolicyTransport {
        fn get(&self, req: HttpRequest) -> impl Future<Output = Result<HttpResponse, AdapterError>> + Send {
            let url = reqwest::Url::parse(&req.url).unwrap();
            let allowed = !req.misuses_protected_header() && HostPolicy::SignedDemo.allows(&url, &req.headers);
            if !allowed {
                self.refused.fetch_add(1, Ordering::SeqCst);
            }
            let fut = if allowed { Some(self.inner.get(req)) } else { None };
            async move {
                match fut {
                    Some(f) => f.await,
                    None => Err(AdapterError::network("host not allowed")),
                }
            }
        }
    }

    type TestClient = OkxSignedClient<PolicyTransport>;

    fn client_full(t: FakeTransport, secrets: Arc<dyn SecretProvider>, offset: Option<i64>, resync: Arc<FakeResync>) -> (Arc<PolicyTransport>, ManualClock, TestClient) {
        let t = Arc::new(PolicyTransport::new(t));
        let clock = ManualClock::new(NOW);
        let c = OkxSignedClient::new(t.clone(), secrets, Arc::new(clock.clone()), Arc::new(move || offset), resync, OkxHost::Demo);
        (t, clock, c)
    }

    fn client(t: FakeTransport) -> (Arc<PolicyTransport>, ManualClock, TestClient) {
        client_full(t, Arc::new(full_secrets()), Some(715), FakeResync::ok())
    }

    /// A transport with the mode check answering "futures mode, net" and the given extra routes.
    fn futures(t: FakeTransport) -> FakeTransport {
        t.on("account/config", ok("account_config_futures_net"))
    }

    fn header<'a>(r: &'a HttpRequest, name: &str) -> Vec<&'a str> {
        r.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }

    fn path_and_query(r: &HttpRequest) -> String {
        r.url.strip_prefix("https://openapi.okx.com").expect("every OKX request goes to the OKX host").to_string()
    }

    // ---- demo boundary: every request carries the flag ----

    #[test]
    fn every_okx_request_goes_to_the_okx_host_with_the_simulated_trading_flag_exactly_once() {
        let t = FakeTransport::new()
            .on("account/config", ok("account_config_futures_net"))
            .on("account/balance", ok("balance_futures"))
            .on("account/positions", ok("positions_net"))
            .on("trade/orders-pending", ok("orders_pending_page2_short"));
        let (t, clock, c) = client(t);
        block_on(c.get_balances()).unwrap();
        block_on(c.get_available_margin()).unwrap();
        block_on(c.get_positions()).unwrap();
        clock.advance(120_000); // mode cache expired: the config is fetched again
        block_on(c.get_open_orders()).unwrap();
        let reqs = t.requests();
        assert!(reqs.len() >= 5, "config, balance x2, positions, config, orders: {}", reqs.len());
        for r in &reqs {
            assert!(r.url.starts_with("https://openapi.okx.com/api/v5/"), "{}", r.url);
            assert_eq!(header(r, "x-simulated-trading"), vec!["1"], "{}", r.url);
            assert_eq!(header(r, "OK-ACCESS-KEY"), vec![KEY]);
            assert_eq!(header(r, "OK-ACCESS-PASSPHRASE"), vec![PASS]);
            assert_eq!(header(r, "OK-ACCESS-SIGN").len(), 1);
            assert!(ALLOWED_SIGNED_HOSTS.contains(&"openapi.okx.com"));
        }
        assert_eq!(t.refused(), 0, "the real admission rule accepted every request");
    }

    #[test]
    fn the_policy_wrapper_really_refuses_a_request_without_the_flag() {
        // proves the test mechanism above is not vacuous: without the flag the real rule says no
        let t = PolicyTransport::new(FakeTransport::new().on("x", ok("account_config_futures_net")));
        let bare = HttpRequest::get("https://openapi.okx.com/api/v5/account/config", SIGNED_TIMEOUT).header("X-Other", "1");
        assert_eq!(block_on(t.get(bare)), Err(AdapterError::network("host not allowed")));
        let dup = {
            let mut r = HttpRequest::get("https://openapi.okx.com/api/v5/account/config", SIGNED_TIMEOUT);
            r.headers = vec![("x-simulated-trading".into(), "1".into()), ("X-Simulated-Trading".into(), "1".into())];
            r
        };
        assert_eq!(block_on(t.get(dup)), Err(AdapterError::network("host not allowed")));
        assert_eq!(t.refused(), 2);
        assert!(t.requests().is_empty(), "nothing reached the transport");
    }

    // ---- keys ----

    #[test]
    fn a_missing_passphrase_is_not_connected_and_sends_nothing() {
        let secrets = MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, KEY).with(Exchange::Okx, SecretName::ApiSecret, SECRET);
        let (t, _, c) = client_full(FakeTransport::new(), Arc::new(secrets), Some(715), FakeResync::ok());
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::NotConnected);
        assert_eq!(c.last_not_connected_reason(), Some(NotConnectedReason::NoPassphrase));
        assert_eq!(block_on(c.get_available_margin()).unwrap_err(), AdapterError::NotConnected);
        assert!(t.requests().is_empty());
    }

    #[test]
    fn the_not_connected_reason_distinguishes_every_cause_and_nothing_is_sent() {
        let reason_for = |secrets: Arc<dyn SecretProvider>, offset: Option<i64>| {
            let (t, _, c) = client_full(FakeTransport::new(), secrets, offset, FakeResync::ok());
            assert_eq!(c.last_not_connected_reason(), None, "no call yet");
            assert_eq!(block_on(c.get_balances()).unwrap_err(), AdapterError::NotConnected);
            assert!(t.requests().is_empty());
            c.last_not_connected_reason()
        };
        let key_only = MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, KEY);
        assert_eq!(reason_for(Arc::new(MemorySecrets::default()), Some(0)), Some(NotConnectedReason::NoKey));
        assert_eq!(reason_for(Arc::new(key_only), Some(0)), Some(NotConnectedReason::NoSecret));
        assert_eq!(reason_for(Arc::new(FailingSecrets), Some(0)), Some(NotConnectedReason::SecretStoreError));
        assert_eq!(reason_for(Arc::new(full_secrets()), None), Some(NotConnectedReason::ClockUnsynced));
        let empty_pass = MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, KEY).with(Exchange::Okx, SecretName::ApiSecret, SECRET).with(Exchange::Okx, SecretName::Passphrase, "");
        assert_eq!(reason_for(Arc::new(empty_pass), Some(0)), Some(NotConnectedReason::NoPassphrase), "an empty passphrase is no passphrase");
    }

    #[test]
    fn an_error_from_the_exchange_carries_code_and_message_but_no_secret() {
        // the exchange echoes the secrets back inside its message: they must still not surface
        let msg = format!("bad passphrase {PASS} for key {KEY} secret {SECRET}");
        let body = json!({"code":"50105","msg":msg,"data":[]}).to_string();
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(HttpResponse::ok(body))));
        let e = block_on(c.get_positions()).unwrap_err();
        let text = e.to_string();
        assert!(matches!(&e, AdapterError::Exchange { code, .. } if code == "50105"), "{text}");
        for secret in [PASS, KEY, SECRET] {
            assert!(!text.contains(secret), "{secret} leaked: {text}");
        }
    }

    #[test]
    fn the_fixture_50105_is_an_exchange_error_with_its_message() {
        let (_, _, c) = client(FakeTransport::new().on("account/config", ok("error_50105")));
        let e = block_on(c.get_positions()).unwrap_err();
        assert_eq!(e, AdapterError::Exchange { code: "50105".into(), message: "Invalid OK-ACCESS-PASSPHRASE".into() });
    }

    // ---- signature and timestamp on the wire ----

    #[test]
    fn the_request_is_signed_with_the_documented_timestamp_and_formula() {
        let (t, _, c) = client(FakeTransport::new().on("account/config", ok("account_config_futures_net")).on("account/positions", ok("positions_net")));
        block_on(c.get_positions()).unwrap();
        let reqs = t.requests();
        let config = &reqs[0];
        assert_eq!(path_and_query(config), "/api/v5/account/config");
        assert_eq!(header(config, "OK-ACCESS-TIMESTAMP"), vec!["2020-12-08T09:08:57.715Z"]);
        // independently computed with Python hmac/base64 (see signing.rs)
        assert_eq!(header(config, "OK-ACCESS-SIGN"), vec!["/WuZsmVbE/RVwmhi8XsUb1SAwfcTtsmpO/2AUXz+cw8="]);
        // the signed string is the sent string: recompute from the path and query that were actually requested
        for r in &reqs {
            let sent = path_and_query(r);
            let expected = okx_signature(SECRET, header(r, "OK-ACCESS-TIMESTAMP")[0], "GET", &sent, "").unwrap();
            assert_eq!(header(r, "OK-ACCESS-SIGN"), vec![expected.as_str()], "{sent}");
        }
        assert!(path_and_query(&reqs[1]).starts_with("/api/v5/account/positions?instType=SWAP"));
    }

    #[test]
    fn a_rejected_timestamp_is_resynced_once_resent_once_and_the_resend_still_carries_the_flag() {
        let t = FakeTransport::new().on("account/config", ok("error_50102")).on("account/config", ok("account_config_futures_net")).on("account/positions", ok("positions_net"));
        let resync = FakeResync::ok();
        let (t, _, c) = client_full(t, Arc::new(full_secrets()), Some(715), resync.clone());
        assert_eq!(block_on(c.get_positions()).unwrap().items.len(), 2);
        assert_eq!(resync.calls(), 1);
        let reqs = t.requests();
        assert_eq!(reqs.iter().filter(|r| r.url.contains("account/config")).count(), 2);
        for r in &reqs {
            assert_eq!(header(r, "x-simulated-trading"), vec!["1"], "retry must rebuild through the OKX constructor: {}", r.url);
        }
        assert_eq!(t.refused(), 0);
    }

    #[test]
    fn a_401_carrying_50102_is_a_timestamp_rejection_too() {
        let t = FakeTransport::new().on("account/config", Ok(HttpResponse::with_status(401, fx("error_50102")))).on("account/config", ok("account_config_futures_net")).on("account/positions", ok("positions_net"));
        let resync = FakeResync::ok();
        let (_, _, c) = client_full(t, Arc::new(full_secrets()), Some(715), resync.clone());
        assert!(block_on(c.get_positions()).is_ok());
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_second_50102_is_returned_after_exactly_two_requests() {
        let t = FakeTransport::new().on("account/config", ok("error_50102"));
        let resync = FakeResync::ok();
        let (t, _, c) = client_full(t, Arc::new(full_secrets()), Some(715), resync.clone());
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Exchange { code, .. }) if code == "50102"));
        assert_eq!(t.requests().len(), 2);
        assert_eq!(resync.calls(), 1);
    }

    #[test]
    fn a_failed_resync_returns_its_error_and_does_not_resend() {
        let t = FakeTransport::new().on("account/config", ok("error_50102"));
        let resync = Arc::new(FakeResync { calls: AtomicUsize::new(0), result: Err(AdapterError::Timeout) });
        let (t, _, c) = client_full(t, Arc::new(full_secrets()), Some(715), resync.clone());
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::Timeout);
        assert_eq!(t.requests().len(), 1);
    }

    #[test]
    fn other_errors_do_not_trigger_a_resync() {
        let t = FakeTransport::new().on("account/config", ok("error_50105"));
        let resync = FakeResync::ok();
        let (t, _, c) = client_full(t, Arc::new(full_secrets()), Some(715), resync.clone());
        assert!(block_on(c.get_positions()).is_err());
        assert_eq!((t.requests().len(), resync.calls()), (1, 0));
    }

    // ---- rate limits and statuses ----

    #[test]
    fn code_50011_and_50061_are_rate_limited_and_50013_is_an_ordinary_exchange_error() {
        let (_, _, c) = client(FakeTransport::new().on("account/config", ok("error_50011")));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: None });
        let body = json!({"code":"50061","msg":"Sub account rate limit","data":[]}).to_string();
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(HttpResponse::ok(body))));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: None });
        let body = json!({"code":"50013","msg":"Systems are busy","data":[]}).to_string();
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(HttpResponse::ok(body))));
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Exchange { code, .. }) if code == "50013"));
    }

    #[test]
    fn http_429_is_rate_limited_and_other_statuses_without_a_code_are_http_errors() {
        let mut r = HttpResponse::with_status(429, "");
        r.headers.push(("Retry-After".into(), "4".into()));
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(r)));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::RateLimited { retry_after_ms: Some(4000) });
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(HttpResponse::with_status(502, "<html>bad gateway</html>"))));
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::Http { status: 502 });
    }

    #[test]
    fn a_non_zero_code_in_an_http_200_body_is_an_exchange_error_and_garbage_is_a_parse_error() {
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(HttpResponse::ok("not json"))));
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Parse(_))));
        let (_, _, c) = client(FakeTransport::new().on("account/config", Ok(HttpResponse::ok(r#"{"data":[]}"#))));
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Parse(_))), "no code field");
    }

    // ---- account mode gate ----

    #[test]
    fn an_unsupported_account_mode_names_the_actual_settings_and_is_never_empty_data() {
        for (fixture, expect) in [("account_config_long_short", "long_short_mode"), ("account_config_acctlv4", "acctLv 4"), ("account_config_spot", "acctLv 1")] {
            let (t, _, c) = client(FakeTransport::new().on("account/config", ok(fixture)).on("account/positions", ok("positions_net")).on("trade/orders-pending", ok("orders_pending_empty")).on("account/balance", ok("balance_futures")));
            for what in ["positions", "orders", "margin"] {
                let e = match what {
                    "positions" => block_on(c.get_positions()).map(|_| ()).unwrap_err(),
                    "orders" => block_on(c.get_open_orders()).map(|_| ()).unwrap_err(),
                    _ => block_on(c.get_available_margin()).map(|_| ()).unwrap_err(),
                };
                let text = e.to_string();
                assert!(text.contains("帳戶模式不支援") && text.contains(expect), "{fixture}/{what}: {text}");
            }
            assert!(t.requests().iter().all(|r| r.url.contains("account/config")), "only the config is ever requested for an unsupported mode: {fixture}");
            assert_eq!(t.requests().len(), 3, "an unsupported reading is not cached: each call re-reads the config");
        }
    }

    #[test]
    fn a_supported_mode_is_reused_for_sixty_seconds_then_read_again() {
        let (t, clock, c) = client(FakeTransport::new().on("account/config", ok("account_config_futures_net")).on("account/positions", ok("positions_net")));
        block_on(c.get_positions()).unwrap();
        clock.advance(59_000);
        block_on(c.get_positions()).unwrap();
        let configs = || t.requests().iter().filter(|r| r.url.contains("account/config")).count();
        assert_eq!(configs(), 1, "second call within 60 s reuses the reading");
        clock.advance(2_000);
        block_on(c.get_positions()).unwrap();
        assert_eq!(configs(), 2, "after 60 s it is read again");
    }

    #[test]
    fn an_unreadable_mode_is_an_error_and_not_cached() {
        let t = FakeTransport::new().on("account/config", Err(AdapterError::Timeout)).on("account/config", ok("account_config_futures_net")).on("account/positions", ok("positions_net"));
        let (t, _, c) = client(t);
        assert_eq!(block_on(c.get_positions()).unwrap_err(), AdapterError::Timeout);
        assert!(block_on(c.get_positions()).is_ok(), "the failure was not cached");
        assert_eq!(t.requests().iter().filter(|r| r.url.contains("account/config")).count(), 2);
    }

    #[test]
    fn the_account_mode_distinguishes_futures_from_multi_currency_margin() {
        let (_, _, c) = client(FakeTransport::new().on("account/config", ok("account_config_futures_net")));
        assert_eq!(block_on(c.account_mode()).unwrap(), OkxAcctLv::Futures);
        let (_, _, c) = client(FakeTransport::new().on("account/config", ok("account_config_multi_ccy")));
        assert_eq!(block_on(c.account_mode()).unwrap(), OkxAcctLv::MultiCurrency);
    }

    // ---- available margin ----

    #[test]
    fn futures_mode_margin_is_the_usdt_availeq_not_availbal() {
        let (_, _, c) = client(futures(FakeTransport::new().on("account/balance", ok("balance_futures"))));
        assert_eq!(block_on(c.get_available_margin()).unwrap(), d("4834.31"));
    }

    #[test]
    fn multi_currency_margin_is_the_account_level_availeq() {
        let t = FakeTransport::new().on("account/config", ok("account_config_multi_ccy")).on("account/balance", ok("balance_multi_ccy"));
        let (_, _, c) = client(t);
        assert_eq!(block_on(c.get_available_margin()).unwrap(), d("55415.62"));
    }

    #[test]
    fn an_empty_or_missing_availeq_is_an_error_naming_the_field_with_no_fallback() {
        let (_, _, c) = client(futures(FakeTransport::new().on("account/balance", ok("balance_empty_availeq"))));
        let e = block_on(c.get_available_margin()).unwrap_err();
        assert!(matches!(e, AdapterError::Parse(_)) && e.to_string().contains("availEq"), "{e}");
        let (_, _, c) = client(futures(FakeTransport::new().on("account/balance", ok("balance_no_usdt"))));
        assert!(block_on(c.get_available_margin()).unwrap_err().to_string().contains("USDT"));
        let t = FakeTransport::new().on("account/config", ok("account_config_multi_ccy")).on("account/balance", ok("balance_empty_availeq"));
        let (_, _, c) = client(t);
        assert!(block_on(c.get_available_margin()).unwrap_err().to_string().contains("availEq"));
    }

    #[test]
    fn zero_and_negative_availeq_are_taken_as_reported() {
        for (value, expected) in [("0", "0"), ("-12.5", "-12.5")] {
            let body = json!({"code":"0","msg":"","data":[{"availEq":"","details":[{"ccy":"USDT","availEq":value,"availBal":"999"}]}]}).to_string();
            let (_, _, c) = client(futures(FakeTransport::new().on("account/balance", Ok(HttpResponse::ok(body)))));
            assert_eq!(block_on(c.get_available_margin()).unwrap(), d(expected));
        }
    }

    // ---- balances ----

    #[test]
    fn balances_keep_each_currency_with_available_and_a_usdt_value_only_when_given() {
        let (_, _, c) = client(FakeTransport::new().on("account/balance", ok("balance_futures")));
        let b = block_on(c.get_balances()).unwrap();
        assert_eq!(b.len(), 2);
        let usdt = b.iter().find(|x| x.asset == "USDT").unwrap();
        assert_eq!((usdt.exchange, usdt.amount, usdt.available), (Exchange::Okx, d("4999.4"), Some(d("5000"))));
        assert_eq!(usdt.fetched_at, NOW + 715);
    }

    // ---- positions ----

    #[test]
    fn positions_are_signed_contracts_and_skip_zero_rows_and_non_usdt_swaps() {
        let (_, _, c) = client(futures(FakeTransport::new().on("account/positions", ok("positions_net"))));
        let l = block_on(c.get_positions()).unwrap();
        assert!(l.is_complete());
        let got: Vec<(String, Decimal)> = l.items.iter().map(|p| (p.symbol.clone(), p.contracts)).collect();
        assert_eq!(got, vec![("BTCUSDT".to_string(), d("-3")), ("SOLUSDT".to_string(), d("12.5"))]);
        let btc = &l.items[0];
        assert_eq!((btc.avg_px, btc.mark_px, btc.lever), (Some(d("43000.5")), Some(d("43020.3")), Some(d("5"))));
        assert_eq!((btc.mgn_mode.as_str(), btc.pos_side.as_str()), ("cross", "net"));
        assert_eq!(btc.notional_usd, Some(d("1290.6")));
    }

    #[test]
    fn an_isolated_position_makes_the_whole_list_an_error_naming_the_symbol() {
        let (_, _, c) = client(futures(FakeTransport::new().on("account/positions", ok("positions_isolated"))));
        let e = block_on(c.get_positions()).unwrap_err();
        assert!(e.to_string().contains("ETHUSDT") && e.to_string().contains("isolated"), "{e}");
    }

    #[test]
    fn a_long_or_short_posside_makes_the_whole_list_an_error() {
        let (_, _, c) = client(futures(FakeTransport::new().on("account/positions", ok("positions_long_short"))));
        let e = block_on(c.get_positions()).unwrap_err();
        assert!(e.to_string().contains("BTCUSDT") && e.to_string().contains("posSide"), "{e}");
    }

    // ---- open orders ----

    #[test]
    fn open_orders_follow_the_after_cursor_and_keep_contract_quantities() {
        let t = futures(FakeTransport::new().on("trade/orders-pending", ok("orders_pending_page1_full")).on("trade/orders-pending", ok("orders_pending_page2_short")));
        let (t, _, c) = client(t);
        let l = block_on(c.get_open_orders()).unwrap();
        assert!(l.is_complete());
        assert_eq!(l.items.len(), 107);
        let reqs: Vec<String> = t.requests().iter().filter(|r| r.url.contains("orders-pending")).map(path_and_query).collect();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0], "/api/v5/trade/orders-pending?instType=SWAP&limit=100");
        assert_eq!(reqs[1], "/api/v5/trade/orders-pending?instType=SWAP&limit=100&after=600000000000000001", "the last ordId of the previous page");
        let partial = l.items.iter().find(|o| o.status == "partially_filled").unwrap();
        assert_eq!((partial.quantity, partial.filled_quantity, partial.side), (d("2"), d("1"), OrderSide::Buy));
        assert_eq!(partial.symbol, "BTCUSDT");
        assert!(l.items.iter().any(|o| o.reduce_only));
        assert!(l.items.iter().any(|o| !o.reduce_only));
    }

    #[test]
    fn a_short_first_page_is_complete_after_one_request() {
        let (t, _, c) = client(futures(FakeTransport::new().on("trade/orders-pending", ok("orders_pending_page2_short"))));
        let l = block_on(c.get_open_orders()).unwrap();
        assert!((l.is_complete(), l.items.len()) == (true, 7));
        assert_eq!(t.requests().iter().filter(|r| r.url.contains("orders-pending")).count(), 1);
    }

    #[test]
    fn a_failing_second_page_yields_incomplete_with_the_rows_already_fetched() {
        let t = futures(FakeTransport::new().on("trade/orders-pending", ok("orders_pending_page1_full")).on("trade/orders-pending", Err(AdapterError::Timeout)));
        let (_, _, c) = client(t);
        let l = block_on(c.get_open_orders()).unwrap();
        assert_eq!(l.items.len(), 100);
        assert!(matches!(l.completeness, Completeness::Incomplete { ref reason } if reason.contains("page 2")), "{:?}", l.completeness);
    }

    #[test]
    fn a_failing_first_page_is_an_error_not_an_empty_listing() {
        let (_, _, c) = client(futures(FakeTransport::new().on("trade/orders-pending", Err(AdapterError::Timeout))));
        assert_eq!(block_on(c.get_open_orders()).map(|_| ()).unwrap_err(), AdapterError::Timeout);
    }

    #[test]
    fn a_repeated_cursor_stops_with_incomplete() {
        // the same full page forever: the second page starts at the same `after`
        let (_, _, c) = client(futures(FakeTransport::new().on("trade/orders-pending", ok("orders_pending_page1_full"))));
        let l = block_on(c.get_open_orders()).unwrap();
        assert!(matches!(l.completeness, Completeness::Incomplete { ref reason } if reason.contains("cursor")), "{:?}", l.completeness);
    }

    #[test]
    fn a_50101_trips_the_shared_latch_and_disables_the_read_client_without_further_requests() {
        let body = json!({"code":"50101","msg":"APIKey does not match current environment.","data":[]}).to_string();
        let latch = OkxLatch::new();
        let t = Arc::new(PolicyTransport::new(FakeTransport::new().on("account/config", Ok(HttpResponse::ok(body)))));
        let c = OkxSignedClient::new(t.clone(), Arc::new(full_secrets()), Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(715)), FakeResync::ok(), OkxHost::Demo).with_latch(latch.clone());
        assert!(matches!(block_on(c.get_positions()), Err(AdapterError::Exchange { code, .. }) if code == "50101"));
        assert!(latch.reason().unwrap().contains("50101"));
        let before = t.requests().len();
        let e = block_on(c.get_balances()).unwrap_err();
        assert!(matches!(&e, AdapterError::Exchange { code, .. } if code == OKX_DISABLED_CODE), "{e}");
        assert_eq!(t.requests().len(), before, "no request while latched");
    }

    /// Real-machine read probe (okx-signed-read task 4.2). `#[ignore]`: it reads the macOS Keychain
    /// and talks to OKX, so only the user runs it:
    ///   cargo test -p tong-funding okx_live_read_probe -- --ignored --nocapture
    /// Only GET requests, all carrying the demo flag; output is redacted by the error types.
    #[test]
    #[ignore = "reads the real Keychain and sends GETs to OKX; the user runs it (okx-signed-read 4.2)"]
    fn okx_live_read_probe() {
        use crate::exchange::health::clock_sync::ClockSync;
        use crate::exchange::public::endpoints::OKX_HOST;
        use crate::exchange::reqwest_transport::ReqwestTransport;
        use crate::ports::SystemClock;
        use crate::store::secrets::BundleSecrets;
        use tong_funding_core::types::Exchange as Ex;
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let clock = Arc::new(SystemClock);
            let public = ReqwestTransport::public_production().expect("public transport");
            let sync = ClockSync::new(clock.clone());
            let off = sync.sync_once(&public, Ex::Okx, OKX_HOST).await.expect("OKX time").offset_ms;
            let client = OkxSignedClient::new(
                Arc::new(ReqwestTransport::signed_demo().expect("signed transport")),
                Arc::new(BundleSecrets::system()),
                clock,
                Arc::new(move || Some(off)),
                FakeResync::ok(),
                OkxHost::Demo,
            );
            println!("account mode: {:?}", client.account_mode().await);
            println!("available margin: {:?}", client.get_available_margin().await);
            println!("balances: {:?}", client.get_balances().await);
            println!("positions (contracts): {:?}", client.get_positions().await);
            println!("open orders: {:?}", client.get_open_orders().await);
        });
    }
}

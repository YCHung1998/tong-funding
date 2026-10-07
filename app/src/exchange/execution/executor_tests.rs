//! Recorded-response tests of the demo executor, its factory and account view (tasks 1.2, 1.4,
//! 2.1 executor side). No network: every reply comes from `FakeOrderTransport` / `FakeTransport`.
#![cfg(test)]

use std::sync::{Arc, Mutex};

use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::{Decimal, Exchange};

use super::account::DemoAccountView;
use super::binance::BinanceOrderClient;
use super::bybit::BybitOrderClient;
use super::classify::SubmitClass;
use super::endpoints::*;
use super::executor::{DemoExecutor, IntentLedger, POSITION_MODE_TTL_MS};
use super::factory::DemoExecutorFactory;
use super::http::Method;
use super::http::fake::{FakeOrderTransport, Reply};
use super::order::{ClientOrderId, OrderRef};
use crate::engine::ids::{IdPrefix, client_order_id};
use crate::engine::ports::{
    AccountView, Executor, ExecutorFactory, Leg, OrderAction, OrderRequest, OrderSide, OrderState, QueryOutcome, ServerOffsets, SubmitOutcome,
};
use crate::exchange::error::AdapterError;
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, BinanceHost, BybitHost};
use crate::exchange::signed::signing::{Credentials, Resync};
use crate::exchange::transport::{FakeTransport, HttpResponse};
use crate::ports::{ManualClock, MemorySecrets, SecretError, SecretName, SecretProvider};

pub(super) const KEY: &str = "TEST_KEY_NOT_REAL";
pub(super) const SECRET: &str = "TEST_SECRET_NOT_REAL";
pub(super) const NOW: i64 = 1_700_000_000_000;
pub(super) const SYM: &str = "BTCUSDT";

pub(super) struct Offsets(pub Option<i64>);
impl ServerOffsets for Offsets {
    fn offset_ms(&self, _exchange: Exchange) -> Option<i64> {
        self.0
    }
}

/// `order_intents` stand-in: the ids it lists are "ours".
#[derive(Default)]
pub(super) struct Ledger(pub Mutex<Vec<String>>);
impl IntentLedger for Ledger {
    fn owns(&self, client_order_id: &str) -> Result<bool, String> {
        Ok(self.0.lock().unwrap().iter().any(|i| i == client_order_id))
    }
}

pub(super) fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

pub(super) fn demo_id(leg: Leg, action: OrderAction, seq: u16) -> String {
    client_order_id(IdPrefix::Demo, "pair-ab12", leg, action, seq)
}

pub(super) fn req(exchange: Exchange, id: &str, side: OrderSide, qty: &str, reduce_only: bool) -> OrderRequest {
    OrderRequest { client_order_id: id.into(), exchange, symbol: SYM.into(), side, quantity: d(qty), reduce_only, leverage: None }
}

pub(super) fn creds() -> Arc<Credentials> {
    Arc::new(Credentials { api_key: KEY.into(), api_secret: SECRET.into() })
}

/// One-way mode on both exchanges (the usual precondition).
pub(super) fn script_one_way(t: &FakeOrderTransport) {
    t.on(Method::Get, BINANCE_POSITION_MODE_PATH, Reply::ok(r#"{"dualSidePosition":false}"#));
    t.on(Method::Get, BYBIT_POSITION_PATH, Reply::ok(r#"{"retCode":0,"retMsg":"OK","result":{"list":[{"symbol":"BTCUSDT","positionIdx":0,"size":"0"}]}}"#));
}

pub(super) struct Rig {
    pub t: FakeOrderTransport,
    pub ex: Arc<DemoExecutor<FakeOrderTransport>>,
    pub clock: ManualClock,
    pub ledger: Arc<Ledger>,
}

pub(super) fn rig_with(offset: Option<i64>) -> Rig {
    let t = FakeOrderTransport::new();
    let clock = ManualClock::new(NOW);
    let ledger = Arc::new(Ledger::default());
    let limiter = Arc::new(RateLimiter::new(Arc::new(clock.clone())));
    let ex = DemoExecutor::new(
        BinanceOrderClient::new(Arc::new(t.clone()), creds(), BinanceHost::Testnet),
        BybitOrderClient::new(Arc::new(t.clone()), creds(), BybitHost::Demo),
        Arc::new(clock.clone()),
        Arc::new(Offsets(offset)),
        ledger.clone(),
        limiter,
    );
    Rig { t, ex: Arc::new(ex), clock, ledger }
}

pub(super) fn rig() -> Rig {
    let r = rig_with(Some(0));
    script_one_way(&r.t);
    r
}

pub(super) fn binance_ack(id: &str, status: &str, executed: &str) -> String {
    format!(r#"{{"orderId":555,"clientOrderId":"{id}","status":"{status}","executedQty":"{executed}","avgPrice":"60000"}}"#)
}

pub(super) fn bybit_created(id: &str) -> String {
    format!(r#"{{"retCode":0,"retMsg":"OK","result":{{"orderId":"bb-1","orderLinkId":"{id}"}}}}"#)
}

pub(super) fn bybit_row(id: &str, status: &str, cum: &str) -> String {
    format!(r#"{{"retCode":0,"retMsg":"OK","result":{{"list":[{{"orderLinkId":"{id}","orderId":"bb-1","cumExecQty":"{cum}","avgPrice":"60010","cumExecFee":"0.33","orderStatus":"{status}"}}]}}}}"#)
}

fn open_long() -> OrderRequest {
    req(Exchange::Binance, &demo_id(Leg::Long, OrderAction::Open, 0), OrderSide::Buy, "0.019", false)
}

fn open_short() -> OrderRequest {
    req(Exchange::Bybit, &demo_id(Leg::Short, OrderAction::Open, 0), OrderSide::Sell, "0.019", false)
}

// ---- 1.2 result classification ----------------------------------------------------------

#[tokio::test]
async fn timeout_connection_reset_and_unparsable_replies_are_unknown_never_rejected() {
    for (exchange, path) in [(Exchange::Binance, BINANCE_ORDER_PATH), (Exchange::Bybit, BYBIT_CREATE_PATH)] {
        for reply in [
            Reply::err(AdapterError::Timeout),
            Reply::err(AdapterError::network("connection reset by peer")),
            Reply::ok("<html>gateway</html>"),
            Reply::status(502, "bad gateway"),
            Reply::status(503, ""),
        ] {
            let r = rig();
            r.t.on(Method::Post, path, reply.clone());
            let o = if exchange == Exchange::Binance { open_long() } else { open_short() };
            let class = r.ex.submit_classified(&o).await;
            assert!(matches!(class, SubmitClass::Unknown { .. }), "{exchange:?} {reply:?} -> {class:?}");
            assert!(matches!(class.into_outcome(), SubmitOutcome::Unknown { .. }));
        }
    }
}

#[tokio::test]
async fn binance_status_unknown_codes_are_unknown_and_a_4xx_without_a_code_is_unknown() {
    for body in [r#"{"code":-1007,"msg":"Timeout waiting for response from backend server. Send status unknown; execution status unknown."}"#, "not json"] {
        let r = rig();
        r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::status(400, body));
        assert!(matches!(r.ex.submit_classified(&open_long()).await, SubmitClass::Unknown { .. }), "{body}");
    }
}

#[tokio::test]
async fn an_http_4xx_with_an_exchange_code_is_rejected_with_code_and_message() {
    let r = rig();
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::status(400, r#"{"code":-2027,"msg":"Exceeded the maximum allowable position at current leverage."}"#));
    match r.ex.submit(open_long()).await {
        SubmitOutcome::Rejected { reason } => assert!(reason.contains("-2027") && reason.contains("maximum allowable"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn bybit_http_200_with_a_nonzero_ret_code_is_rejected_with_ret_code_and_ret_msg() {
    let r = rig();
    r.t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(r#"{"retCode":110007,"retMsg":"ab not enough for new order","result":{}}"#));
    match r.ex.submit_classified(&open_short()).await {
        SubmitClass::Rejected { code, message } => assert_eq!((code.as_str(), message.as_str()), ("110007", "ab not enough for new order")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_429_with_retry_after_is_rate_limited_looked_up_by_the_engine_and_later_requests_back_off() {
    let r = rig();
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::status(429, "").with_header("Retry-After", "5"));
    let class = r.ex.submit_classified(&open_long()).await;
    assert_eq!(class, SubmitClass::RateLimited { retry_after_ms: Some(5_000) });
    assert!(matches!(class.into_outcome(), SubmitOutcome::Unknown { .. }), "never FAILED before a lookup");
    assert_eq!(r.t.count(Method::Post, BINANCE_ORDER_PATH), 1);

    // During the back-off nothing is sent: a new order is refused (certainly not placed) and a
    // lookup fails without a request (the leg's state is unchanged, not "failed").
    let second = req(Exchange::Binance, &demo_id(Leg::Long, OrderAction::Open, 1), OrderSide::Buy, "0.019", false);
    assert!(matches!(r.ex.submit_classified(&second).await, SubmitClass::Rejected { .. }));
    let q = r.ex.query(Exchange::Binance, SYM, &demo_id(Leg::Long, OrderAction::Open, 0)).await;
    assert!(matches!(&q, QueryOutcome::Failed { reason } if reason.contains("rate limited")), "{q:?}");
    assert_eq!(r.t.count(Method::Post, BINANCE_ORDER_PATH), 1);
    assert_eq!(r.t.count(Method::Get, BINANCE_ORDER_PATH), 0);
    // Bybit is not affected by Binance's back-off.
    r.t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&demo_id(Leg::Short, OrderAction::Open, 0))));
    assert!(matches!(r.ex.submit_classified(&open_short()).await, SubmitClass::Accepted(_)));

    r.clock.advance(5_001);
    r.t.on(Method::Get, BINANCE_ORDER_PATH, Reply::status(400, r#"{"code":-2013,"msg":"Order does not exist."}"#));
    assert_eq!(r.ex.query(Exchange::Binance, SYM, &demo_id(Leg::Long, OrderAction::Open, 0)).await, QueryOutcome::NotFound);
    assert_eq!(r.t.count(Method::Get, BINANCE_ORDER_PATH), 1, "after Retry-After the lookup goes out");
}

#[tokio::test]
async fn a_bybit_rate_limit_ret_code_is_rate_limited_not_rejected() {
    let r = rig();
    r.t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(r#"{"retCode":10006,"retMsg":"Too many visits!","result":{}}"#));
    assert!(matches!(r.ex.submit_classified(&open_short()).await, SubmitClass::RateLimited { .. }));
}

// ---- 1.4 executor rules -------------------------------------------------------------------

#[tokio::test]
async fn a_hedge_mode_account_or_an_unreadable_mode_sends_no_order() {
    let r = rig_with(Some(0));
    r.t.on(Method::Get, BINANCE_POSITION_MODE_PATH, Reply::ok(r#"{"dualSidePosition":true}"#));
    match r.ex.submit(open_long()).await {
        SubmitOutcome::Rejected { reason } => assert!(reason.contains("position mode mismatch"), "{reason}"),
        other => panic!("{other:?}"),
    }
    r.t.on(Method::Get, BYBIT_POSITION_PATH, Reply::err(AdapterError::Timeout));
    match r.ex.submit(open_short()).await {
        SubmitOutcome::Rejected { reason } => assert!(reason.contains("not confirmed"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.t.count(Method::Post, ""), 0, "no order request at all");
}

#[tokio::test]
async fn a_confirmed_one_way_reading_is_reused_within_the_ttl_only() {
    let r = rig();
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&demo_id(Leg::Long, OrderAction::Open, 0), "FILLED", "0.019")));
    for seq in 0..2 {
        let o = req(Exchange::Binance, &demo_id(Leg::Long, OrderAction::Open, seq), OrderSide::Buy, "0.019", false);
        let _ = r.ex.submit(o).await;
    }
    assert_eq!(r.t.count(Method::Get, BINANCE_POSITION_MODE_PATH), 1);
    r.clock.advance(POSITION_MODE_TTL_MS);
    let _ = r.ex.submit(req(Exchange::Binance, &demo_id(Leg::Long, OrderAction::Open, 2), OrderSide::Buy, "0.019", false)).await;
    assert_eq!(r.t.count(Method::Get, BINANCE_POSITION_MODE_PATH), 2, "re-read after the TTL");
}

#[tokio::test]
async fn an_uncalibrated_clock_sends_nothing() {
    let r = rig_with(None);
    script_one_way(&r.t);
    assert!(matches!(r.ex.submit(open_long()).await, SubmitOutcome::Rejected { .. }));
    assert!(matches!(r.ex.query(Exchange::Bybit, SYM, &demo_id(Leg::Short, OrderAction::Open, 0)).await, QueryOutcome::Failed { .. }));
    assert!(r.t.requests().is_empty());
}

#[tokio::test]
async fn cancelling_an_order_that_is_not_in_order_intents_is_refused_without_any_request() {
    let r = rig();
    let foreign = demo_id(Leg::Long, OrderAction::Open, 7);
    let out = r.ex.cancel(Exchange::Binance, SYM, &foreign).await;
    assert!(matches!(&out, QueryOutcome::Failed { reason } if reason.contains("not an order of this system")), "{out:?}");
    assert!(r.t.requests().is_empty());

    let own = demo_id(Leg::Long, OrderAction::Open, 0);
    r.ledger.0.lock().unwrap().push(own.clone());
    r.t.on(Method::Delete, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&own, "CANCELED", "0.005")));
    match r.ex.cancel(Exchange::Binance, SYM, &own).await {
        QueryOutcome::Found(s) => assert_eq!((s.state, s.filled_quantity), (OrderState::Cancelled, d("0.005"))),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.t.count(Method::Delete, BINANCE_ORDER_PATH), 1);
}

#[tokio::test]
async fn orders_are_looked_up_by_client_order_id_and_by_exchange_order_id() {
    let r = rig();
    let id = demo_id(Leg::Short, OrderAction::Open, 0);
    r.t.on(Method::Get, "orderLinkId=", Reply::ok(&bybit_row(&id, "Filled", "0.019")));
    r.t.on(Method::Get, "orderId=bb-1", Reply::ok(&bybit_row(&id, "Filled", "0.019")));
    let by_client = r.ex.query(Exchange::Bybit, SYM, &id).await;
    let by_exchange = r.ex.query_by(Exchange::Bybit, SYM, &OrderRef::Exchange("bb-1".into())).await;
    for out in [by_client, by_exchange] {
        match out {
            QueryOutcome::Found(s) => {
                assert_eq!((s.client_order_id.as_str(), s.state, s.filled_quantity), (id.as_str(), OrderState::Filled, d("0.019")));
                assert_eq!((s.fee, s.fee_asset.as_deref()), (Some(d("0.33")), Some("USDT")));
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn an_existing_position_never_counts_as_a_fill_and_a_failed_lookup_is_not_not_found() {
    let r = rig();
    let id = demo_id(Leg::Long, OrderAction::Open, 0);
    // The symbol carries a 1.0 position on the account, but this order filled nothing.
    r.t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&id, "NEW", "0")));
    match r.ex.query(Exchange::Binance, SYM, &id).await {
        QueryOutcome::Found(s) => assert_eq!((s.filled_quantity, s.state), (Decimal::ZERO, OrderState::Open)),
        other => panic!("{other:?}"),
    }
    r.t.replace(Method::Get, BINANCE_ORDER_PATH, Reply::err(AdapterError::Timeout));
    assert!(matches!(r.ex.query(Exchange::Binance, SYM, &id).await, QueryOutcome::Failed { .. }), "timeout = unknown, not 'not filled'");
}

#[tokio::test]
async fn okx_has_no_order_capability_and_never_sends_a_request() {
    let r = rig();
    let o = req(Exchange::Okx, &demo_id(Leg::Short, OrderAction::Open, 0), OrderSide::Sell, "1", false);
    assert!(matches!(r.ex.submit(o).await, SubmitOutcome::Rejected { reason } if reason.contains("unsupported")));
    assert!(matches!(r.ex.query(Exchange::Okx, SYM, &demo_id(Leg::Short, OrderAction::Open, 0)).await, QueryOutcome::Failed { .. }));
    assert!(matches!(r.ex.cancel(Exchange::Okx, SYM, &demo_id(Leg::Short, OrderAction::Open, 0)).await, QueryOutcome::Failed { .. }));
    assert!(r.t.requests().is_empty());
}

#[tokio::test]
async fn a_simulated_id_or_a_missing_id_is_never_sent() {
    let r = rig();
    let sim = client_order_id(IdPrefix::Sim, "pair-ab12", Leg::Long, OrderAction::Open, 0);
    assert!(matches!(r.ex.submit(req(Exchange::Binance, &sim, OrderSide::Buy, "0.01", false)).await, SubmitOutcome::Rejected { .. }));
    assert!(matches!(r.ex.submit(req(Exchange::Binance, "", OrderSide::Buy, "0.01", false)).await, SubmitOutcome::Rejected { .. }));
    assert!(r.t.requests().is_empty());
}

#[tokio::test]
async fn every_request_of_a_full_flow_goes_to_an_allowed_demo_host_and_carries_the_id() {
    let r = rig();
    let (l, s) = (demo_id(Leg::Long, OrderAction::Open, 0), demo_id(Leg::Short, OrderAction::Open, 0));
    r.ledger.0.lock().unwrap().extend([l.clone(), s.clone()]);
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&l, "NEW", "0")));
    r.t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&l, "PARTIALLY_FILLED", "0.01")));
    r.t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok("[]"));
    r.t.on(Method::Delete, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&l, "CANCELED", "0.01")));
    r.t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&s)));
    r.t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&s, "Cancelled", "0.01")));
    r.t.on(Method::Post, BYBIT_CANCEL_PATH, Reply::ok(&bybit_created(&s)));
    let _ = r.ex.submit(open_long()).await;
    let _ = r.ex.submit(open_short()).await;
    let _ = r.ex.query(Exchange::Binance, SYM, &l).await;
    let _ = r.ex.query(Exchange::Bybit, SYM, &s).await;
    let _ = r.ex.cancel(Exchange::Binance, SYM, &l).await;
    let _ = r.ex.cancel(Exchange::Bybit, SYM, &s).await;
    let reqs = r.t.requests();
    assert!(reqs.len() >= 8, "{reqs:?}");
    for q in &reqs {
        let host = reqwest::Url::parse(q.full_url()).unwrap().host_str().unwrap().to_string();
        assert!(ALLOWED_SIGNED_HOSTS.contains(&host.as_str()), "{host}");
    }
    let orders: Vec<_> = reqs.iter().filter(|q| q.method() == Method::Post && !q.full_url().contains("cancel")).collect();
    assert!(orders[0].full_url().contains(&format!("newClientOrderId={l}")));
    assert!(orders[1].body().unwrap().contains(&format!("\"orderLinkId\":\"{s}\"")));
}

// ---- factory ----------------------------------------------------------------------------

struct LockedKeychain;
impl SecretProvider for LockedKeychain {
    fn get(&self, _e: Exchange, _n: SecretName) -> Result<Option<String>, SecretError> {
        Err(SecretError::Unavailable("keychain locked".into()))
    }
}

fn factory(secrets: Arc<dyn SecretProvider>, t: &FakeOrderTransport) -> DemoExecutorFactory<FakeOrderTransport> {
    let clock = ManualClock::new(NOW);
    DemoExecutorFactory::new(
        Arc::new(t.clone()),
        secrets,
        Arc::new(clock.clone()),
        Arc::new(Offsets(Some(0))),
        Arc::new(Ledger::default()),
        Arc::new(RateLimiter::new(Arc::new(clock))),
        BinanceHost::Testnet,
    )
}

fn all_keys() -> MemorySecrets {
    MemorySecrets::default()
        .with(Exchange::Binance, SecretName::ApiKey, KEY)
        .with(Exchange::Binance, SecretName::ApiSecret, SECRET)
        .with(Exchange::Bybit, SecretName::ApiKey, KEY)
        .with(Exchange::Bybit, SecretName::ApiSecret, SECRET)
}

#[test]
fn the_factory_fails_without_usable_keys_and_never_builds_an_empty_key_executor() {
    let t = FakeOrderTransport::new();
    let no_bybit = MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, KEY).with(Exchange::Binance, SecretName::ApiSecret, SECRET);
    let cases: Vec<(&str, Arc<dyn SecretProvider>)> = vec![
        ("nothing stored", Arc::new(MemorySecrets::default())),
        ("bybit missing", Arc::new(no_bybit)),
        ("keychain locked", Arc::new(LockedKeychain)),
    ];
    for (name, secrets) in cases {
        let err = factory(secrets, &t).create(ExecutionMode::ExchangeDemo).err().unwrap_or_else(|| panic!("{name}: built"));
        assert!(err.contains("keys unavailable"), "{name}: {err}");
        assert!(!err.contains(KEY) && !err.contains(SECRET));
    }
    // `MemorySecrets` returns the first value stored, so an empty override is checked directly.
    let only_empty = MemorySecrets::default()
        .with(Exchange::Binance, SecretName::ApiKey, KEY)
        .with(Exchange::Binance, SecretName::ApiSecret, SECRET)
        .with(Exchange::Bybit, SecretName::ApiKey, KEY)
        .with(Exchange::Bybit, SecretName::ApiSecret, "");
    assert!(factory(Arc::new(only_empty), &t).create(ExecutionMode::ExchangeDemo).is_err());
    assert!(factory(Arc::new(all_keys()), &t).create(ExecutionMode::Simulation).is_err(), "never builds for SIMULATION");
    assert!(t.requests().is_empty(), "building or failing sends nothing");
}

#[test]
fn the_factory_builds_a_non_simulated_executor_with_keys() {
    let t = FakeOrderTransport::new();
    let ex = factory(Arc::new(all_keys()), &t).create(ExecutionMode::ExchangeDemo).unwrap();
    assert!(!ex.is_simulated());
}

// ---- account view -----------------------------------------------------------------------

struct NoResync;
impl Resync for NoResync {
    fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AdapterError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn account(t: FakeTransport) -> DemoAccountView<FakeTransport> {
    account_shared(Arc::new(t))
}

fn account_shared(t: Arc<FakeTransport>) -> DemoAccountView<FakeTransport> {
    let clock = Arc::new(ManualClock::new(NOW));
    let secrets: Arc<dyn SecretProvider> = Arc::new(all_keys());
    let offset = Arc::new(|| Some(0i64));
    let b = BinanceSignedClient::new(t.clone(), secrets.clone(), clock.clone(), offset.clone(), Arc::new(NoResync), BinanceHost::Testnet);
    let y = BybitSignedClient::new(t, secrets, clock, offset, Arc::new(NoResync), BybitHost::Demo);
    DemoAccountView::new(Arc::new(b), Arc::new(y))
}

#[tokio::test]
async fn account_positions_are_signed_on_both_exchanges() {
    let t = FakeTransport::new()
        .on("/fapi/v2/positionRisk", Ok(HttpResponse::ok(r#"[{"symbol":"BTCUSDT","positionAmt":"-0.019","positionSide":"BOTH"},{"symbol":"ETHUSDT","positionAmt":"0","positionSide":"BOTH"}]"#)))
        .on("/v5/position/list", Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[{"symbol":"BTCUSDT","size":"0.019","side":"Sell","positionIdx":0}],"nextPageCursor":""}}"#)));
    let a = account(t);
    let b = a.positions(Exchange::Binance).await.unwrap();
    assert!(b.complete);
    assert_eq!(b.items.len(), 1, "zero rows dropped");
    assert_eq!((b.items[0].symbol.as_str(), b.items[0].quantity), ("BTCUSDT", d("-0.019")));
    let y = a.positions(Exchange::Bybit).await.unwrap();
    assert_eq!(y.items[0].quantity, d("-0.019"), "Bybit size + side Sell -> negative");
    assert!(a.positions(Exchange::Okx).await.is_err());
}

#[tokio::test]
async fn a_hedge_mode_position_makes_the_account_list_unusable() {
    let t = FakeTransport::new().on("/fapi/v2/positionRisk", Ok(HttpResponse::ok(r#"[{"symbol":"BTCUSDT","positionAmt":"0.01","positionSide":"LONG"}]"#)));
    assert!(account(t).positions(Exchange::Binance).await.unwrap_err().contains("hedge"));
}

#[tokio::test]
async fn account_margin_is_the_available_usdt_and_missing_is_an_error() {
    let t = FakeTransport::new()
        .on("/fapi/v2/balance", Ok(HttpResponse::ok(r#"[{"asset":"USDT","balance":"1000","availableBalance":"750.5"},{"asset":"BNB","balance":"1","availableBalance":"1"}]"#)))
        .on("/v5/account/wallet-balance", Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[{"accountType":"UNIFIED","totalAvailableBalance":"480.25","coin":[{"coin":"USDT","walletBalance":"500","availableToWithdraw":""}]}]}}"#)));
    let a = account(t);
    assert_eq!(a.available_margin(Exchange::Binance).await, Ok(d("750.5")));
    // UNIFIED: `availableToWithdraw` is deprecated (""), the account's totalAvailableBalance is used.
    assert_eq!(a.available_margin(Exchange::Bybit).await, Ok(d("480.25")));
    let empty = account(FakeTransport::new().on(
        "/v5/account/wallet-balance",
        Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[{"accountType":"UNIFIED","totalAvailableBalance":"","coin":[{"coin":"USDT","walletBalance":"500","availableToWithdraw":""}]}]}}"#)),
    ));
    let e = empty.available_margin(Exchange::Bybit).await.unwrap_err();
    assert!(e.starts_with("Bybit:") && e.contains("totalAvailableBalance"), "not reported = not available (fail closed): {e}");
}

#[tokio::test]
async fn account_open_orders_keep_completeness() {
    let t = FakeTransport::new()
        .on("/fapi/v1/openOrders", Ok(HttpResponse::ok(r#"[{"symbol":"BTCUSDT","orderId":1,"side":"BUY","type":"LIMIT","price":"1","origQty":"0.02","executedQty":"0.005","status":"NEW"}]"#)));
    let o = account(t).open_orders(Exchange::Binance).await.unwrap();
    assert!(o.complete);
    assert_eq!((o.items[0].symbol.as_str(), o.items[0].remaining_quantity), ("BTCUSDT", d("0.015")));
}

#[tokio::test]
async fn query_by_client_id_parses_the_engine_id_unchanged() {
    let id = demo_id(Leg::Long, OrderAction::Close, 3);
    assert_eq!(ClientOrderId::parse(&id).unwrap().as_str(), id);
}

#[tokio::test]
async fn account_max_leverage_is_read_per_symbol_and_notional_on_both_exchanges() {
    let bracket = r#"[{"symbol":"BTCUSDT","brackets":[{"bracket":1,"initialLeverage":20,"notionalCap":10000,"notionalFloor":0},{"bracket":2,"initialLeverage":10,"notionalCap":50000,"notionalFloor":10000}]}]"#;
    let bybit = r#"{"retCode":0,"retMsg":"OK","result":{"category":"linear","list":[{"symbol":"BTCUSDT","status":"Trading","leverageFilter":{"minLeverage":"1","maxLeverage":"12.50","leverageStep":"0.01"}}]}}"#;
    let t = FakeTransport::new().on("/fapi/v1/leverageBracket", Ok(HttpResponse::ok(bracket))).on("/v5/market/instruments-info", Ok(HttpResponse::ok(bybit)));
    let t = Arc::new(t);
    let a = account_shared(t.clone());
    assert_eq!(a.max_leverage(Exchange::Binance, "BTCUSDT", d("1000")).await, Ok(d("20")));
    assert_eq!(a.max_leverage(Exchange::Binance, "BTCUSDT", d("20000")).await, Ok(d("10")), "a larger position gets the smaller cap");
    assert_eq!(a.max_leverage(Exchange::Bybit, "BTCUSDT", d("1000")).await, Ok(d("12.5")));
    assert!(a.max_leverage(Exchange::Okx, "BTCUSDT", d("1000")).await.is_err());
    let urls: Vec<String> = t.requests().iter().map(|r| r.url.clone()).collect();
    assert!(urls[0].contains("/fapi/v1/leverageBracket?symbol=BTCUSDT&timestamp=") && urls[0].contains("&signature="), "{}", urls[0]);
    assert!(urls.iter().any(|u| u.contains("/v5/market/instruments-info?category=linear&symbol=BTCUSDT")), "{urls:?}");
}

#[tokio::test]
async fn account_max_leverage_failures_are_errors_never_a_default() {
    let t = FakeTransport::new()
        .on("/fapi/v1/leverageBracket", Ok(HttpResponse::with_status(400, r#"{"code":-1121,"msg":"Invalid symbol."}"#)))
        .on("/v5/market/instruments-info", Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[]}}"#)));
    let a = account(t);
    assert!(a.max_leverage(Exchange::Binance, "NOPEUSDT", d("1000")).await.unwrap_err().starts_with("Binance:"));
    assert!(a.max_leverage(Exchange::Bybit, "NOPEUSDT", d("1000")).await.unwrap_err().starts_with("Bybit:"));
}

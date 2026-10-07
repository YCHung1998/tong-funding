//! Task 4.1: the whole engine in EXCHANGE_DEMO on recorded exchange replies. The real
//! `DemoExecutorFactory` builds the real `DemoExecutor` (Binance + Bybit request building,
//! signing, classification, rate limiter) over `FakeOrderTransport`; the real `DemoAccountView`
//! reads positions / orders / margin through the signed GET clients over `FakeTransport`. Only
//! market data, offsets and the clock are fakes. No network.
//!
//! Each case prints its event sequence (`cargo test -p tong-funding exchange_replay -- --nocapture`).
#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::sleep;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::RiskConfig;
use tong_funding_core::types::{Decimal, Exchange};

use super::account::DemoAccountView;
use super::endpoints::*;
use super::executor_tests::{KEY, Offsets, SECRET, binance_ack, bybit_created, bybit_row, d, script_one_way};
use super::factory::DemoExecutorFactory;
use super::http::Method;
use super::http::fake::{FakeOrderTransport, Reply};
use crate::engine::actor::{CONFIG_RISK, EngineDeps, EngineHandle, start};
use crate::engine::alert::{PAIR_ALERT, RecordingNotifier};
use crate::engine::command::{Command, CommandReply, NewPreparedPair};
use crate::engine::latency::{LatencyReport, ORDER_LATENCY};
use crate::engine::ports::{BoxFut, FreshQuote, MarketData, OrderRules};
use crate::engine::sim::{MarginFromAccount, SimPriceBook, SimulatedExecutor};
use crate::engine::timings::EngineTimings;
use crate::engine::transition::PAIR_TRANSITION;
use crate::exchange::error::AdapterError;
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, BinanceHost, BybitHost};
use crate::exchange::signed::signing::Resync;
use crate::exchange::transport::{FakeTransport, HttpResponse};
use crate::ports::{Clock, MemorySecrets, MonoClock, SecretName, SecretProvider};
use crate::store::db::Db;
use crate::store::db::test_support::tempdir;
use crate::store::state::{FLAG_EXECUTION_MODE, FLAG_TRIGGER_MODE};

/// Settlement time of the replayed pair.
const T: i64 = 1_800_000_000_000;
const SYM: &str = "BTCUSDT";
const PAIR: &str = "replay-pair";

/// Follows tokio's paused clock: event timestamps and latencies are the virtual times.
struct TokioClock {
    base_ms: i64,
    start: tokio::time::Instant,
}
impl Clock for TokioClock {
    fn now_ms(&self) -> i64 {
        self.base_ms + self.start.elapsed().as_millis() as i64
    }
}
impl MonoClock for TokioClock {
    fn mono_ms(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }
}

struct Market(Arc<TokioClock>);
impl MarketData for Market {
    fn refetch(&self, exchange: Exchange, symbol: &str) -> BoxFut<'_, Result<FreshQuote, String>> {
        let now = self.0.now_ms();
        let rate = if exchange == Exchange::Binance { "-0.001" } else { "0.001" };
        let funding = FundingObservation::new(exchange, symbol, d(rate), Some(28_800), T, d("100"), Some(d("10000000")), now, now, DataStatus::Listed);
        Box::pin(std::future::ready(Ok(FreshQuote { funding, price: d("100"), price_observed_at_ms: now, listed: true })))
    }
    fn order_rules(&self, _: Exchange, _: &str) -> BoxFut<'_, Result<OrderRules, String>> {
        Box::pin(std::future::ready(Ok(OrderRules { lot: LotSize { step_size: d("0.001"), min_qty: d("0.001") }, okx_ct_val: None })))
    }
}

struct NoResync;
impl Resync for NoResync {
    fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AdapterError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn keys() -> MemorySecrets {
    MemorySecrets::default()
        .with(Exchange::Binance, SecretName::ApiKey, KEY)
        .with(Exchange::Binance, SecretName::ApiSecret, SECRET)
        .with(Exchange::Bybit, SecretName::ApiKey, KEY)
        .with(Exchange::Bybit, SecretName::ApiSecret, SECRET)
}

/// An empty demo account with 10,000 USDT available on both exchanges.
fn empty_account() -> FakeTransport {
    FakeTransport::new()
        .on("/fapi/v2/positionRisk", Ok(HttpResponse::ok("[]")))
        // symbol-leverage-cap: the engine reads both legs' caps before it enters (5x fits easily).
        .on("/fapi/v1/leverageBracket", Ok(HttpResponse::ok(r#"[{"symbol":"BTCUSDT","brackets":[{"bracket":1,"initialLeverage":125,"notionalCap":50000000,"notionalFloor":0}]}]"#)))
        .on("/v5/market/instruments-info", Ok(HttpResponse::ok(r#"{"retCode":0,"retMsg":"OK","result":{"list":[{"symbol":"BTCUSDT","status":"Trading","leverageFilter":{"minLeverage":"1","maxLeverage":"100.00","leverageStep":"0.01"}}]}}"#)))
        .on("/fapi/v1/openOrders", Ok(HttpResponse::ok("[]")))
        .on("/fapi/v2/balance", Ok(HttpResponse::ok(r#"[{"asset":"USDT","balance":"10000","availableBalance":"10000"}]"#)))
        .on("/v5/position/list", Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[],"nextPageCursor":""}}"#)))
        .on("/v5/order/realtime", Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[],"nextPageCursor":""}}"#)))
        .on("/v5/account/wallet-balance", Ok(HttpResponse::ok(r#"{"retCode":0,"result":{"list":[{"accountType":"UNIFIED","totalAvailableBalance":"10000","coin":[{"coin":"USDT","walletBalance":"10000","availableToWithdraw":""}]}]}}"#)))
}

struct Replay {
    _dir: tempfile::TempDir,
    db: Db,
    orders: FakeOrderTransport,
    notifier: Arc<RecordingNotifier>,
    clock: Arc<TokioClock>,
    _h: EngineHandle,
}

fn risk() -> RiskConfig {
    RiskConfig {
        net_edge_threshold_pct: Some(d("0.05")),
        est_slippage_pct: Some(d("0.01")),
        taker_fee_pct: Exchange::ALL.into_iter().map(|e| (e, d("0.02"))).collect(),
        ..RiskConfig::default()
    }
}

/// Starts the engine in EXCHANGE_DEMO / AUTO at T−20 s with the pair added; `orders` must already
/// carry the case's scripted replies.
async fn replay(orders: FakeOrderTransport) -> Replay {
    let dir = tempdir();
    let clock = Arc::new(TokioClock { base_ms: T - 20_000, start: tokio::time::Instant::now() });
    let db = Db::open_unlocked(&dir.path().join("funding.db"), clock.clone());
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    db.config_set(CONFIG_RISK, &serde_json::to_value(risk()).unwrap(), None).unwrap();
    db.flag_set(FLAG_TRIGGER_MODE, "AUTO").unwrap();
    db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();

    script_one_way(&orders);
    // order-leverage-sync: every opening order is preceded by a leverage request on its exchange.
    orders.on(Method::Post, BINANCE_LEVERAGE_PATH, Reply::ok(r#"{"symbol":"BTCUSDT","leverage":5,"maxNotionalValue":"1000000"}"#));
    orders.on(Method::Post, BYBIT_SET_LEVERAGE_PATH, Reply::ok(r#"{"retCode":0,"retMsg":"OK","result":{}}"#));
    let secrets: Arc<dyn SecretProvider> = Arc::new(keys());
    let limiter = Arc::new(RateLimiter::new(clock.clone()));
    let factory = DemoExecutorFactory::new(
        Arc::new(orders.clone()),
        secrets.clone(),
        clock.clone(),
        Arc::new(Offsets(Some(0))),
        Arc::new(db.clone()),
        limiter,
        BinanceHost::Testnet,
    );
    let reads = Arc::new(empty_account());
    let offset = Arc::new(|| Some(0i64));
    let binance = BinanceSignedClient::new(reads.clone(), secrets.clone(), clock.clone(), offset.clone(), Arc::new(NoResync), BinanceHost::Testnet);
    let bybit = BybitSignedClient::new(reads, secrets, clock.clone(), offset, Arc::new(NoResync), BybitHost::Demo);
    let account = Arc::new(DemoAccountView::new(Arc::new(binance), Arc::new(bybit)));
    let sim = Arc::new(SimulatedExecutor::new(Arc::new(SimPriceBook::default())));
    let sim_account = Arc::new(sim.account_view(Arc::new(MarginFromAccount(account.clone()))));
    let notifier = Arc::new(RecordingNotifier::default());
    let deps = EngineDeps {
        db: db.clone(),
        clock: clock.clone(),
        timings: EngineTimings::default(),
        simulator: sim,
        factory: Arc::new(factory),
        market: Arc::new(Market(clock.clone())),
        offsets: Arc::new(Offsets(Some(0))),
        account,
        sim_account,
        reconciler: None,
        notifier: notifier.clone(),
    };
    let h = start(deps);
    let pair = NewPreparedPair {
        internal_uuid: PAIR.into(),
        pair_id: "pid-replay".into(),
        symbol: SYM.into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        settlement_ms: T,
        entry: json!({ "long_scan_price": "100", "short_scan_price": "100", "notional_usdt": "1000", "leverage": "5", "net_edge_pct": "0.07" }),
    };
    let r = tokio::time::timeout(Duration::from_secs(5), h.send(Command::AddPrepared(pair))).await.unwrap();
    assert_eq!(r, CommandReply::Accepted);
    Replay { _dir: dir, db, orders, notifier, clock, _h: h }
}

/// Lets virtual time run until the clock reads `ms`.
async fn run_until(r: &Replay, ms: i64) {
    let now = r.clock.now_ms();
    if ms > now {
        sleep(Duration::from_millis((ms - now) as u64)).await;
    }
    sleep(Duration::from_millis(5)).await;
}

/// `(ts - T, label, payload)`; a transition is labelled by its target state.
fn events(db: &Db) -> Vec<(i64, String, Value)> {
    let plain = rusqlite::Connection::open(db.path()).unwrap();
    let mut st = plain.prepare("SELECT ts_ms, event_type, payload FROM events ORDER BY id").unwrap();
    st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .unwrap()
        .map(|r| {
            let (ts, ty, p) = r.unwrap();
            let p: Value = serde_json::from_str(&p).unwrap();
            let label = if ty == PAIR_TRANSITION { p["to"].as_str().unwrap().to_string() } else { ty };
            (ts - T, label, p)
        })
        .collect()
}

fn status(db: &Db) -> String {
    db.get_pair(PAIR).unwrap().unwrap().status
}

/// Prints the case, then checks what every case must satisfy: demo hosts only, no secret in
/// any event, one latency event per submit.
fn print_and_check(name: &str, r: &Replay) -> Vec<(i64, String, Value)> {
    let evs = events(&r.db);
    println!("==== replay: {name} (event time relative to T) ====");
    for (t, label, p) in &evs {
        let brief = match label.as_str() {
            "ORDER_SUBMITTED" | "ORDER_FILL" | "ORDER_CANCEL_RESULT" | "PAIR_ALERT" | ORDER_LATENCY => p.to_string(),
            _ => p.get("detail").map(Value::to_string).unwrap_or_default(),
        };
        println!("T{t:+7} ms  {label:<22} {}", brief.chars().take(220).collect::<String>());
    }
    let requests = r.orders.requests();
    for q in &requests {
        let host = reqwest::Url::parse(q.full_url()).unwrap().host_str().unwrap().to_string();
        assert!(ALLOWED_SIGNED_HOSTS.contains(&host.as_str()), "{name}: request to {host}");
    }
    let dump = evs.iter().map(|(_, l, p)| format!("{l} {p}")).collect::<Vec<_>>().join("\n");
    assert!(!dump.contains(KEY) && !dump.contains(SECRET), "{name}: a secret reached an event");
    for q in &requests {
        for (_, v) in q.headers() {
            if v.len() >= 32 && v.chars().all(|c| c.is_ascii_hexdigit()) {
                assert!(!dump.contains(v.as_str()), "{name}: a signature reached an event");
            }
        }
        if let Some((_, sig)) = q.full_url().rsplit_once("signature=") {
            assert!(!dump.contains(sig), "{name}: a URL signature reached an event");
        }
    }
    let latency: Vec<Value> = evs.iter().filter(|(_, l, _)| l == ORDER_LATENCY).map(|(_, _, p)| p.clone()).collect();
    let submits = evs.iter().filter(|(_, l, _)| l == "ORDER_SUBMITTED").count();
    assert_eq!(latency.len(), submits, "{name}: one latency event per submit");
    let report = LatencyReport::from_events(&latency, false);
    println!("latency: submit {:?}; entry -> both accepted {:?}; excluded {}", report.submit, report.entry_to_both_accepted, report.entries_excluded);
    evs
}

fn labels(evs: &[(i64, String, Value)]) -> Vec<&str> {
    evs.iter().map(|(_, l, _)| l.as_str()).collect()
}

fn binance_id() -> String {
    crate::engine::ids::client_order_id(crate::engine::ids::IdPrefix::Demo, PAIR, crate::engine::ports::Leg::Long, crate::engine::ports::OrderAction::Open, 0)
}

fn bybit_id() -> String {
    crate::engine::ids::client_order_id(crate::engine::ids::IdPrefix::Demo, PAIR, crate::engine::ports::Leg::Short, crate::engine::ports::OrderAction::Open, 0)
}

const EMPTY_LIST: &str = r#"{"retCode":0,"retMsg":"OK","result":{"list":[]}}"#;

#[tokio::test(start_paused = true)]
async fn exchange_replay_both_filled_is_reconciled_with_latency_for_the_t5_decision() {
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")).after(180));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok(r#"[{"commission":"0.4","commissionAsset":"USDT"}]"#));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&bybit_id())).after(150));
    t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "Filled", "10")));
    let r = replay(t).await;
    run_until(&r, T - 5_000).await;
    let evs = print_and_check("both legs filled", &r);
    assert_eq!(status(&r.db), "RECONCILED", "{:?}", labels(&evs));
    let latency: Vec<Value> = evs.iter().filter(|(_, l, _)| l == ORDER_LATENCY).map(|(_, _, p)| p.clone()).collect();
    let by = |leg: &str| latency.iter().find(|p| p["leg"] == json!(leg)).unwrap().clone();
    assert_eq!((by("long")["latency_ms"].clone(), by("short")["latency_ms"].clone()), (json!(180), json!(150)));
    let report = LatencyReport::from_events(&latency, false);
    let p = report.entry_to_both_accepted.unwrap();
    assert!(p.p99 >= 180 && p.p99 < 2_500, "{p:?}");
    assert!(r.notifier.calls().is_empty());
    // Fill details with the fee reach the event log for both legs (Binance: via userTrades).
    let fills: Vec<Value> = evs.iter().filter(|(_, l, _)| l == "ORDER_FILL").map(|(_, _, p)| p.clone()).collect();
    let binance_fill = fills.iter().find(|p| p["exchange"] == json!("Binance")).expect("Binance fee looked up after the ACK");
    assert_eq!((binance_fill["fee"].clone(), binance_fill["fee_asset"].clone()), (json!("0.4"), json!("USDT")));
    let bybit_fill = fills.iter().find(|p| p["exchange"] == json!("Bybit")).unwrap();
    assert_eq!((bybit_fill["fee"].clone(), bybit_fill["filled_quantity"].clone()), (json!("0.33"), json!("10")));
}

#[tokio::test(start_paused = true)]
async fn exchange_replay_timeout_cancels_both_unfilled_orders_and_cancels_the_pair() {
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "NEW", "0")));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "NEW", "0")));
    t.on(Method::Delete, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "CANCELED", "0")));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&bybit_id())));
    t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "New", "0")));
    t.on(Method::Post, BYBIT_CANCEL_PATH, Reply::ok(&bybit_created(&bybit_id())));
    let r = replay(t).await;
    run_until(&r, T + 4_500).await;
    assert_eq!(status(&r.db), "FILL_MONITOR");
    // From the cancel on, the exchange reports both orders cancelled with no fill.
    r.orders.replace(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "CANCELED", "0")));
    r.orders.replace(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "Cancelled", "0")));
    run_until(&r, T + 7_000).await;
    let evs = print_and_check("fill timeout, nothing filled", &r);
    assert_eq!(status(&r.db), "CANCELLED", "{:?}", labels(&evs));
    assert_eq!(r.orders.count(Method::Delete, BINANCE_ORDER_PATH), 1);
    assert_eq!(r.orders.count(Method::Post, BYBIT_CANCEL_PATH), 1);
    assert_eq!(r.orders.count(Method::Post, BINANCE_ORDER_PATH) + r.orders.count(Method::Post, BYBIT_CREATE_PATH), 2, "never an extra order");
}

#[tokio::test(start_paused = true)]
async fn exchange_replay_bybit_reject_is_partial_failure_with_an_alert_and_the_long_fill_kept() {
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok("[]"));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(r#"{"retCode":110007,"retMsg":"ab not enough for new order","result":{}}"#));
    let r = replay(t).await;
    run_until(&r, T - 5_000).await;
    let evs = print_and_check("Bybit rejects the short leg", &r);
    assert_eq!(status(&r.db), "PARTIAL_FAILURE", "{:?}", labels(&evs));
    let alert = evs.iter().find(|(_, l, _)| l == PAIR_ALERT).unwrap().2.clone();
    assert_eq!(alert["reason"], json!("SUBMIT_REJECTED"));
    assert!(alert.to_string().contains("110007"), "{alert}");
    assert_eq!(r.notifier.calls().len(), 1);
    assert_eq!(r.db.get_intent(&binance_id()).unwrap().unwrap().state, "FILLED");
    assert_eq!(r.db.get_intent(&bybit_id()).unwrap().unwrap().state, "FAILED");
}

#[tokio::test(start_paused = true)]
async fn exchange_replay_partial_fill_cancels_the_rest_and_is_partial_failure() {
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok("[]"));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&bybit_id())));
    t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "PartiallyFilled", "7")));
    t.on(Method::Post, BYBIT_CANCEL_PATH, Reply::ok(&bybit_created(&bybit_id())));
    let r = replay(t).await;
    run_until(&r, T + 4_500).await;
    r.orders.replace(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "PartiallyFilledCanceled", "7")));
    run_until(&r, T + 7_000).await;
    let evs = print_and_check("short leg 70 % filled", &r);
    assert_eq!(status(&r.db), "PARTIAL_FAILURE", "{:?}", labels(&evs));
    let cancel = evs.iter().find(|(_, l, _)| l == "ORDER_CANCEL_RESULT").unwrap().2.clone();
    assert_eq!(cancel["after"]["filled_quantity"], json!("7"));
    assert_eq!(evs.iter().find(|(_, l, _)| l == PAIR_ALERT).unwrap().2["reason"], json!("FILL_TIMEOUT_ONE_LEG"));
    assert_eq!(r.orders.count(Method::Delete, ""), 0, "the filled Binance leg is not cancelled");
}

#[tokio::test(start_paused = true)]
async fn exchange_replay_a_429_is_looked_up_after_retry_after_and_reconciles() {
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::status(429, "").with_header("Retry-After", "1"));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok("[]"));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&bybit_id())));
    t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "Filled", "10")));
    let r = replay(t).await;
    run_until(&r, T - 5_000).await;
    let evs = print_and_check("Binance answers 429 to the order", &r);
    assert_eq!(status(&r.db), "RECONCILED", "{:?}", labels(&evs));
    assert!(labels(&evs).contains(&"ORDER_RESULT_UNKNOWN"), "the 429 is not a failure: looked up first");
    let gets = r.orders.requests().into_iter().filter(|q| q.method() == Method::Get && q.full_url().contains(BINANCE_ORDER_PATH)).count();
    assert!(gets >= 1, "looked up by client_order_id after the back-off");
    assert_eq!(r.orders.count(Method::Post, BINANCE_ORDER_PATH), 1, "never resubmitted");
}

#[tokio::test(start_paused = true)]
async fn exchange_replay_an_unknown_submit_is_settled_by_lookup_or_ends_unresolved() {
    // (a) The Bybit reply is lost, the lookup by orderLinkId finds the fill: RECONCILED.
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok("[]"));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::err(AdapterError::Timeout));
    t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(&bybit_row(&bybit_id(), "Filled", "10")));
    let r = replay(t).await;
    run_until(&r, T - 5_000).await;
    let evs = print_and_check("Bybit submit times out, the order exists", &r);
    assert_eq!(status(&r.db), "RECONCILED", "{:?}", labels(&evs));
    assert_eq!(r.orders.count(Method::Post, BYBIT_CREATE_PATH), 1, "never resubmitted under a new id");

    // (b) Lost reply and the order is nowhere: cancel attempt, still unknown -> UNRESOLVED + alert.
    let t = FakeOrderTransport::new();
    t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&binance_id(), "FILLED", "10")));
    t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok("[]"));
    t.on(Method::Post, BYBIT_CREATE_PATH, Reply::err(AdapterError::network("connection reset by peer")));
    t.on(Method::Get, BYBIT_REALTIME_PATH, Reply::ok(EMPTY_LIST));
    t.on(Method::Get, BYBIT_HISTORY_PATH, Reply::ok(EMPTY_LIST));
    t.on(Method::Post, BYBIT_CANCEL_PATH, Reply::ok(r#"{"retCode":110001,"retMsg":"order not exists or too late to cancel","result":{}}"#));
    let r = replay(t).await;
    run_until(&r, T + 7_000).await;
    let evs = print_and_check("Bybit submit lost, order not found", &r);
    assert_eq!(status(&r.db), "UNRESOLVED", "{:?}", labels(&evs));
    assert_eq!(evs.iter().find(|(_, l, _)| l == PAIR_ALERT).unwrap().2["reason"], json!("SUBMIT_UNKNOWN"));
    assert_eq!(r.db.get_intent(&bybit_id()).unwrap().unwrap().state, "SUBMITTED", "unknown is never FAILED");
    let _ = Decimal::ZERO;
}

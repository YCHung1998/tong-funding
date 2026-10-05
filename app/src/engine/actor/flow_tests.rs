//! End-to-end flows through the actor (tasks 2.1–2.4, 3.1, 3.2, 4.1 integration): scheduler,
//! Node 0, Node 1–5, exit, closed confirmation, modes and the kill switch. Everything runs on
//! tokio's paused time with a `ManualClock`; all ports are in-memory fakes (no network).
//!
//! Pacing: the actor ticks every (tokio) second; the test moves the injected clock by exactly one
//! second half-way between two ticks, so tick k sees `start + k * 1000` and every event timestamp
//! is a value the fake clock had.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tokio::time::sleep;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::{RiskConfig, RiskOverride, RiskOverrides};

use super::*;
use crate::engine::command::NewPreparedPair;
use crate::engine::ids::{IdPrefix, client_order_id};
use crate::engine::ports::{
    AccountOrder, AccountPosition, AccountView, BoxFut, FreshQuote, Listed, MarketData, OrderRules, OrderSide, OrderState,
    OrderStatus, QueryOutcome, ReconcileContext, ServerOffsets, StartupReconciler, SubmitOutcome,
};
use crate::engine::sim::{CountingFactory, MarginSource, SimBehavior, SimPriceBook, SimulatedExecutor};
use crate::ports::ManualClock;
use crate::store::db::test_support::tempdir;
use crate::store::state::{FLAG_TRIGGER_MODE, NewIntent};

/// Settlement `T` of the test pair (exchange time).
const T: i64 = 1_800_000_000_000;
const SYM: &str = "BTCUSDT";
const UUID: &str = "pair-0001";

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

// ---- fakes -----------------------------------------------------------------------------

/// Market data: every refetch returns a fresh quote observed "now" on the fake clock, unless
/// `frozen` (then the first quote of that leg is returned again: a source that stopped updating).
struct FakeMarket {
    clock: ManualClock,
    calls: Mutex<Vec<(Exchange, String, i64)>>,
    frozen: Mutex<bool>,
    first: Mutex<HashMap<Exchange, FreshQuote>>,
    rules: Mutex<HashMap<Exchange, OrderRules>>,
}

impl FakeMarket {
    fn new(clock: ManualClock) -> Arc<FakeMarket> {
        let lot = OrderRules { lot: LotSize { step_size: dec("0.001"), min_qty: dec("0.001") }, okx_ct_val: None };
        Arc::new(FakeMarket {
            clock,
            calls: Mutex::default(),
            frozen: Mutex::new(false),
            first: Mutex::default(),
            rules: Mutex::new(Exchange::ALL.into_iter().map(|e| (e, lot)).collect()),
        })
    }
    fn calls(&self) -> Vec<(Exchange, String, i64)> {
        self.calls.lock().unwrap().clone()
    }
    fn quote(exchange: Exchange, symbol: &str, now: i64) -> FreshQuote {
        // Long Binance pays -0.1 %, short Bybit receives +0.1 %: Net Edge qualifies.
        let rate = if exchange == Exchange::Binance { "-0.001" } else { "0.001" };
        FreshQuote {
            funding: FundingObservation::new(
                exchange,
                symbol,
                dec(rate),
                Some(28_800),
                T,
                dec("100"),
                Some(dec("10000000")),
                now,
                now,
                DataStatus::Listed,
            ),
            price: dec("100"),
            price_observed_at_ms: now,
            listed: true,
        }
    }
}

impl MarketData for FakeMarket {
    fn refetch(&self, exchange: Exchange, symbol: &str) -> BoxFut<'_, Result<FreshQuote, String>> {
        let now = self.clock.now_ms();
        self.calls.lock().unwrap().push((exchange, symbol.to_string(), now));
        let fresh = FakeMarket::quote(exchange, symbol, now);
        let mut first = self.first.lock().unwrap();
        let q = if *self.frozen.lock().unwrap() { first.entry(exchange).or_insert(fresh).clone() } else {
            first.entry(exchange).or_insert_with(|| fresh.clone());
            fresh
        };
        Box::pin(std::future::ready(Ok(q)))
    }
    fn order_rules(&self, exchange: Exchange, _symbol: &str) -> BoxFut<'_, Result<OrderRules, String>> {
        let r = self.rules.lock().unwrap().get(&exchange).copied().ok_or_else(|| "no rules".to_string());
        Box::pin(std::future::ready(r))
    }
}

struct FakeOffsets(Mutex<HashMap<Exchange, Option<i64>>>);
impl FakeOffsets {
    fn zero() -> Arc<FakeOffsets> {
        Arc::new(FakeOffsets(Mutex::new(Exchange::ALL.into_iter().map(|e| (e, Some(0))).collect())))
    }
}
impl ServerOffsets for FakeOffsets {
    fn offset_ms(&self, exchange: Exchange) -> Option<i64> {
        self.0.lock().unwrap().get(&exchange).copied().flatten()
    }
}

/// Demo-account margin (decision 6): a fixed balance, or an error.
struct FixedMargin(Result<Decimal, String>);
impl MarginSource for FixedMargin {
    fn available_margin(&self, _exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        Box::pin(std::future::ready(self.0.clone()))
    }
}

/// The order-capable executor a factory builds for EXCHANGE_DEMO (fake: fills at once, keeps
/// its own positions; no network). Shares its ledger with [`DemoAccount`].
#[derive(Default)]
struct FakeDemo {
    requests: Mutex<Vec<OrderRequest>>,
    positions: Mutex<HashMap<(Exchange, String), Decimal>>,
    orders: Mutex<HashMap<String, OrderStatus>>,
}
impl Executor for FakeDemo {
    fn is_simulated(&self) -> bool {
        false
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        self.requests.lock().unwrap().push(req.clone());
        let signed = match req.side {
            OrderSide::Buy => req.quantity,
            OrderSide::Sell => -req.quantity,
        };
        *self.positions.lock().unwrap().entry((req.exchange, req.symbol.clone())).or_default() += signed;
        let status = OrderStatus {
            client_order_id: req.client_order_id.clone(),
            exchange_order_id: Some("demo-1".into()),
            filled_quantity: req.quantity,
            avg_price: Some(dec("100")),
            state: OrderState::Filled,
        };
        self.orders.lock().unwrap().insert(req.client_order_id.clone(), status.clone());
        Box::pin(std::future::ready(SubmitOutcome::Accepted(status)))
    }
    fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
        Box::pin(std::future::ready(QueryOutcome::NotFound))
    }
    fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        let o = self.orders.lock().unwrap().get(id).cloned().map_or(QueryOutcome::NotFound, QueryOutcome::Found);
        Box::pin(std::future::ready(o))
    }
}

struct DemoAccount(Arc<FakeDemo>);
impl AccountView for DemoAccount {
    fn positions(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
        let items = (self.0.positions.lock().unwrap().iter())
            .filter(|((e, _), q)| *e == exchange && !q.is_zero())
            .map(|((e, s), q)| AccountPosition { exchange: *e, symbol: s.clone(), quantity: *q })
            .collect();
        Box::pin(std::future::ready(Ok(Listed { items, complete: true })))
    }
    fn open_orders(&self, _exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
        Box::pin(std::future::ready(Ok(Listed { items: vec![], complete: true })))
    }
    fn available_margin(&self, _exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        Box::pin(std::future::ready(Ok(dec("10000"))))
    }
}

/// A simulator stand-in that accepts every order and never fills it (for the timeout test).
#[derive(Default)]
struct NeverFills {
    submits: AtomicUsize,
    queries: AtomicUsize,
    ids: Mutex<Vec<String>>,
}
impl NeverFills {
    fn status(id: &str) -> OrderStatus {
        OrderStatus {
            client_order_id: id.into(),
            exchange_order_id: Some("sim-x".into()),
            filled_quantity: Decimal::ZERO,
            avg_price: None,
            state: OrderState::Open,
        }
    }
}
impl Executor for NeverFills {
    fn is_simulated(&self) -> bool {
        true
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        self.ids.lock().unwrap().push(req.client_order_id.clone());
        Box::pin(std::future::ready(SubmitOutcome::Accepted(NeverFills::status(&req.client_order_id))))
    }
    fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
        Box::pin(std::future::ready(QueryOutcome::NotFound))
    }
    fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(QueryOutcome::Found(NeverFills::status(id))))
    }
}

struct FakeReconciler {
    calls: AtomicUsize,
    result: Result<(), String>,
}
impl StartupReconciler for FakeReconciler {
    fn reconcile(&self, ctx: ReconcileContext) -> BoxFut<'static, Result<(), String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(ctx.executor.is_simulated(), "SIMULATION hands the simulator to the reconciler");
        Box::pin(std::future::ready(self.result.clone()))
    }
}

// ---- rig ---------------------------------------------------------------------------------

struct Rig {
    _dir: tempfile::TempDir,
    db: Db,
    clock: ManualClock,
    market: Arc<FakeMarket>,
    offsets: Arc<FakeOffsets>,
    sim: Arc<SimulatedExecutor>,
    factory: CountingFactory,
    demo: Arc<FakeDemo>,
}

struct Opts {
    start_ms: i64,
    trigger: &'static str,
    demo: bool,
    margin: Result<Decimal, String>,
    simulator: Option<Arc<dyn Executor>>,
    reconciler: Option<Arc<dyn StartupReconciler>>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts { start_ms: T - 20_000, trigger: "AUTO", demo: false, margin: Ok(dec("10000")), simulator: None, reconciler: None }
    }
}

fn risk() -> RiskConfig {
    RiskConfig {
        net_edge_threshold_pct: Some(dec("0.05")),
        est_slippage_pct: Some(dec("0.01")),
        taker_fee_pct: Exchange::ALL.into_iter().map(|e| (e, dec("0.02"))).collect(),
        ..RiskConfig::default()
    }
}

fn store_risk(db: &Db, cfg: &RiskConfig) {
    let v = serde_json::to_value(cfg).unwrap();
    let ver = db.config_get(CONFIG_RISK).unwrap().map(|e| e.version);
    db.config_set(CONFIG_RISK, &v, ver).unwrap();
}

fn store_overrides(db: &Db, ov: &RiskOverrides) {
    let v = serde_json::to_value(ov).unwrap();
    let ver = db.config_get(CONFIG_RISK_OVERRIDES).unwrap().map(|e| e.version);
    db.config_set(CONFIG_RISK_OVERRIDES, &v, ver).unwrap();
}

fn rig(o: Opts) -> (Rig, EngineDeps) {
    let dir = tempdir();
    let clock = ManualClock::new(o.start_ms);
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    let db = Db::open_unlocked(&dir.path().join("funding.db"), shared.clone());
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    store_risk(&db, &risk());
    db.flag_set(FLAG_TRIGGER_MODE, o.trigger).unwrap();
    let prices = Arc::new(SimPriceBook::default());
    for ex in Exchange::ALL {
        for s in [SYM, "ETHUSDT"] {
            prices.set(ex, s, dec("100"));
        }
    }
    let sim = Arc::new(SimulatedExecutor::new(prices));
    let sim_account: Arc<dyn AccountView> = Arc::new(sim.account_view(Arc::new(FixedMargin(o.margin.clone()))));
    let demo = Arc::new(FakeDemo::default());
    let factory = CountingFactory::returning(demo.clone());
    if o.demo {
        db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
    }
    let market = FakeMarket::new(clock.clone());
    let offsets = FakeOffsets::zero();
    let deps = EngineDeps {
        db: db.clone(),
        clock: shared,
        timings: EngineTimings::default(),
        simulator: o.simulator.unwrap_or_else(|| sim.clone()),
        factory: Arc::new(factory.clone()),
        market: market.clone(),
        offsets: offsets.clone(),
        account: Arc::new(DemoAccount(demo.clone())),
        sim_account,
        reconciler: o.reconciler,
    };
    (Rig { _dir: dir, db, clock, market, offsets, sim, factory, demo }, deps)
}

fn pair_at(uuid: &str, symbol: &str, settlement_ms: i64) -> NewPreparedPair {
    NewPreparedPair {
        internal_uuid: uuid.into(),
        pair_id: format!("pid-{uuid}"),
        symbol: symbol.into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        settlement_ms,
        entry: json!({
            "long_scan_price": "100", "short_scan_price": "100",
            "notional_usdt": "1000", "leverage": "5", "net_edge_pct": "0.07"
        }),
    }
}

async fn ask(h: &EngineHandle, c: Command) -> CommandReply {
    tokio::time::timeout(Duration::from_secs(5), h.send(c)).await.expect("engine did not reply")
}

/// Move the fake clock (and tokio's paused time) forward until it reads `until_ms`, one second per
/// actor tick; the clock moves half-way between ticks.
async fn run_until(clock: &ManualClock, until_ms: i64) {
    while clock.now_ms() < until_ms {
        sleep(Duration::from_millis(500)).await;
        clock.advance(1_000);
        sleep(Duration::from_millis(500)).await;
    }
    sleep(Duration::from_millis(10)).await;
}

/// Start the engine and add the test pair (settlement `T`) at the start time.
async fn started(o: Opts) -> (Rig, EngineHandle) {
    let (rig, deps) = rig(o);
    let h = start(deps);
    assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
    (rig, h)
}

/// `(ts_ms, label, payload)` of every event, oldest first; a transition is labelled by its target.
fn events(db: &Db) -> Vec<(i64, String, Value)> {
    let plain = rusqlite::Connection::open(db.path()).unwrap();
    let mut st = plain.prepare("SELECT ts_ms, event_type, payload FROM events ORDER BY id").unwrap();
    st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .unwrap()
        .map(|r| {
            let (ts, ty, p) = r.unwrap();
            let p: Value = serde_json::from_str(&p).unwrap();
            let label = if ty == transition::PAIR_TRANSITION { p["to"].as_str().unwrap().to_string() } else { ty };
            (ts, label, p)
        })
        .collect()
}

fn labels(db: &Db) -> Vec<String> {
    events(db).into_iter().map(|(_, l, _)| l).collect()
}

fn count(db: &Db, label: &str) -> usize {
    labels(db).iter().filter(|l| *l == label).count()
}

fn ts_of(db: &Db, label: &str) -> Vec<i64> {
    events(db).into_iter().filter(|(_, l, _)| l == label).map(|(t, _, _)| t).collect()
}

/// Read straight from the file (works on a halted store too).
fn status(db: &Db, uuid: &str) -> String {
    let plain = rusqlite::Connection::open(db.path()).unwrap();
    plain.query_row("SELECT status FROM pairs WHERE internal_uuid = ?1", [uuid], |r| r.get(0)).unwrap()
}

fn sim_id(leg: Leg, action: OrderAction) -> String {
    client_order_id(IdPrefix::Sim, UUID, leg, action, 0)
}

// ---- 2.4 / 3.1: the full SIMULATION round -----------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_full_simulation_round_from_t_minus_20_to_t_plus_20() {
    let (rig, h) = started(Opts::default()).await;
    run_until(&rig.clock, T + 20_000).await;

    println!("---- full SIMULATION round, T = {T} (event ts relative to T) ----");
    for (ts, label, p) in events(&rig.db) {
        println!("T{:+6} ms  {label:<20} {p}", ts - T);
    }
    let seq: Vec<(i64, String)> = events(&rig.db)
        .into_iter()
        .filter(|(_, l, _)| {
            ["PRE_TRADE_CHECK", "ORDER_SUBMIT", "FILL_MONITOR", "RECONCILED", "CLOSING", CLOSE_CONFIRMED, "FINALIZED"]
                .contains(&l.as_str())
        })
        .map(|(t, l, _)| (t - T, l))
        .collect();
    let expect: Vec<(i64, String)> = [
        (-10_000, "PRE_TRADE_CHECK"),
        (-10_000, "ORDER_SUBMIT"),
        (-10_000, "FILL_MONITOR"),
        (-10_000, "RECONCILED"),
        (15_000, "CLOSING"),
        (15_000, CLOSE_CONFIRMED),
        (15_000, "FINALIZED"),
    ]
    .into_iter()
    .map(|(t, l)| (t, l.to_string()))
    .collect();
    assert_eq!(seq, expect, "event sequence and fake-clock timestamps");
    assert_eq!(status(&rig.db, UUID), "FINALIZED");

    // SIMULATION never built an order-capable executor, and nothing reached the demo executor.
    assert_eq!(rig.factory.calls(), 0, "real executor factory called during SIMULATION");
    assert!(rig.demo.requests.lock().unwrap().is_empty());
    // Exactly the four orders of one round went to the simulator: two opens, two reduce-only closes.
    let sent = rig.sim.submitted();
    let ids: Vec<&str> = sent.iter().map(|r| r.client_order_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            sim_id(Leg::Long, OrderAction::Open).as_str(),
            sim_id(Leg::Short, OrderAction::Open).as_str(),
            sim_id(Leg::Long, OrderAction::Close).as_str(),
            sim_id(Leg::Short, OrderAction::Close).as_str(),
        ]
    );
    assert!(ids.iter().all(|i| i.starts_with("sim")));
    assert_eq!(sent.iter().map(|r| r.reduce_only).collect::<Vec<_>>(), vec![false, false, true, true]);
    assert_eq!(sent[0].quantity, dec("10"), "1000 USDT at 100");
    assert_eq!((sent[0].side, sent[1].side, sent[2].side, sent[3].side), (OrderSide::Buy, OrderSide::Sell, OrderSide::Sell, OrderSide::Buy));
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO);

    // Baseline at T-15 and pre-trade at T-10: two separate fetches per leg, nothing else.
    let calls = rig.market.calls();
    let times: Vec<i64> = calls.iter().map(|(_, _, t)| t - T).collect();
    assert_eq!(times, vec![-15_000, -15_000, -10_000, -10_000], "{calls:?}");

    // Every order event is marked simulated and carries a sim id.
    let orders: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == ORDER_SUBMITTED).map(|(_, _, p)| p).collect();
    assert_eq!(orders.len(), 4);
    for p in &orders {
        assert_eq!(p["simulated"], json!(true), "{p}");
        assert!(p["client_order_id"].as_str().unwrap().starts_with("sim"), "{p}");
    }
    // Every transition of the simulated pair is marked too.
    for (_, l, p) in events(&rig.db) {
        if ["PRE_TRADE_CHECK", "ORDER_SUBMIT", "FILL_MONITOR", "RECONCILED", "CLOSING", "FINALIZED"].contains(&l.as_str()) {
            assert_eq!(p["detail"]["simulated"], json!(true), "{l}: {p}");
        }
    }
    // Every intent ended FILLED (none left for a restart to reconcile).
    assert!(rig.db.list_unfinished_intents().unwrap().is_empty());
    sleep(Duration::from_millis(300)).await;
    assert_eq!(h.snapshots.borrow().pairs[0].state, PairState::Finalized);
}

/// The engine reaches no network: it holds only the injected ports. Structural proof that no
/// engine file refers to the HTTP / exchange layers (the test above then runs entirely on fakes).
#[test]
fn engine_sources_do_not_reference_the_network_layers() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine");
    let banned = [["crate::", "exchange"].concat(), ["req", "west"].concat(), ["Http", "Transport"].concat(), ["tungs", "tenite"].concat()];
    let mut stack = vec![dir];
    let mut hits = Vec::new();
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                for (n, line) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
                    if banned.iter().any(|b| line.contains(b.as_str())) {
                        hits.push(format!("{}:{}: {line}", p.display(), n + 1));
                    }
                }
            }
        }
    }
    assert!(hits.is_empty(), "{}", hits.join("\n"));
}

// ---- 3.2: trigger_mode x execution_mode ---------------------------------------------------

async fn mode_combo(trigger: &'static str, demo: bool) {
    let (rig, h) = started(Opts { trigger, demo, ..Opts::default() }).await;
    run_until(&rig.clock, T - 5_000).await;
    let auto = trigger == "AUTO";
    if auto {
        assert_eq!(status(&rig.db, UUID), "RECONCILED", "{trigger}/{demo}");
    } else {
        assert_eq!(status(&rig.db, UUID), "PREPARED", "MANUAL never enters by itself");
        assert_eq!(count(&rig.db, "PRE_TRADE_CHECK"), 0);
        assert!(rig.sim.submitted().is_empty() && rig.demo.requests.lock().unwrap().is_empty());
        assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
        run_until(&rig.clock, T - 4_000).await;
        assert_eq!(status(&rig.db, UUID), "RECONCILED", "{trigger}/{demo} after manual enter");
    }
    let demo_reqs = rig.demo.requests.lock().unwrap().clone();
    if demo {
        assert!(rig.sim.submitted().is_empty(), "EXCHANGE_DEMO orders never reach the simulator");
        assert_eq!(demo_reqs.len(), 2);
        assert!(demo_reqs.iter().all(|r| r.client_order_id.starts_with("demo")), "{demo_reqs:?}");
        assert_eq!(rig.factory.calls(), 1, "built once at startup");
    } else {
        assert!(demo_reqs.is_empty());
        assert_eq!(rig.sim.submitted().len(), 2);
        assert_eq!(rig.factory.calls(), 0);
    }
    // Both modes run the same exit path.
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED", "{trigger}/{demo}");
    let simulated = events(&rig.db).into_iter().find(|(_, l, _)| l == ORDER_SUBMITTED).unwrap().2["simulated"].clone();
    assert_eq!(simulated, json!(!demo));
}

#[tokio::test(start_paused = true)]
async fn auto_simulation() {
    mode_combo("AUTO", false).await;
}

#[tokio::test(start_paused = true)]
async fn manual_simulation() {
    mode_combo("MANUAL", false).await;
}

#[tokio::test(start_paused = true)]
async fn auto_exchange_demo() {
    mode_combo("AUTO", true).await;
}

#[tokio::test(start_paused = true)]
async fn manual_exchange_demo() {
    mode_combo("MANUAL", true).await;
}

#[tokio::test(start_paused = true)]
async fn manual_mode_does_not_auto_enter_and_a_missed_window_cancels() {
    let (rig, _h) = started(Opts { trigger: "MANUAL", ..Opts::default() }).await;
    run_until(&rig.clock, T - 1_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
    assert_eq!(count(&rig.db, "COMMAND_REFUSED"), 0);
    run_until(&rig.clock, T).await;
    assert_eq!(status(&rig.db, UUID), "CANCELLED");
    assert!(rig.sim.submitted().is_empty());
}

// ---- 3.2: kill switch ---------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn kill_switch_refuses_the_entry_once_and_never_trades() {
    let (rig, h) = started(Opts::default()).await;
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 1_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
    assert_eq!(count(&rig.db, "COMMAND_REFUSED"), 1, "one refused event, not one per tick: {:?}", labels(&rig.db));
    assert!(rig.sim.submitted().is_empty());
    run_until(&rig.clock, T).await;
    assert_eq!(status(&rig.db, UUID), "CANCELLED", "missed window while halted");
    assert_eq!(count(&rig.db, "COMMAND_REFUSED"), 1);
}

#[tokio::test(start_paused = true)]
async fn kill_switch_never_closes_but_the_scheduled_exit_still_runs() {
    let (rig, h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    run_until(&rig.clock, T + 14_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED", "the kill switch itself closes nothing");
    assert_eq!(rig.sim.submitted().len(), 2);
    run_until(&rig.clock, T + 15_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED", "auto exit is not blocked (decision 2)");
}

#[tokio::test(start_paused = true)]
async fn kill_switch_accepts_a_manual_close() {
    let (rig, h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    assert_eq!(ask(&h, Command::ManualClose { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    let sent = rig.sim.submitted();
    assert_eq!(sent.len(), 4);
    assert!(sent[2].reduce_only && sent[3].reduce_only);
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_kill_switch_stops_the_entry() {
    let (rig, h) = started(Opts::default()).await;
    crate::engine::gate::test_support::delete_kill_switch_row(&rig.db);
    run_until(&rig.clock, T - 1_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
    assert!(rig.sim.submitted().is_empty());
    assert_eq!(count(&rig.db, "PRE_TRADE_CHECK"), 0);
    // The failed read halts the store (that halt is the record; no event can be written now).
    assert!(matches!(rig.db.halt_reason(), Some(crate::store::db::HaltReason::KillSwitchReadFailed(_))));
    let blockers = h.snapshots.borrow().blockers.clone();
    assert!(matches!(blockers.as_slice(), [Blocker::KillSwitchUnreadable(_)]), "{blockers:?}");
}

// ---- 2.1 scheduling ---------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_restart_after_settlement_warns_then_cancels_without_any_order() {
    let (rig, deps) = rig(Opts { start_ms: T + 5_000, ..Opts::default() });
    // The pair was added before the restart (as a previous run would have left it).
    let env = PairEnvelope {
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        settlement_ms: T,
        simulated: true,
        scan: pair_at(UUID, SYM, T).entry,
    };
    let row = NewPair {
        internal_uuid: UUID.into(),
        pair_id: "pid".into(),
        symbol: SYM.into(),
        status: PairState::Prepared,
        entry: serde_json::to_value(env).unwrap(),
    };
    rig.db.add_pair_if_not_pending(&row).unwrap();
    let _h = start(deps);
    run_until(&rig.clock, T + 6_000).await;
    let l = labels(&rig.db);
    let warn = l.iter().position(|x| x == ENTRY_WINDOW_MISSED).expect("warning written");
    let cancel = l.iter().position(|x| x == "CANCELLED").expect("cancelled");
    assert!(warn < cancel, "{l:?}");
    assert_eq!(count(&rig.db, ENTRY_WINDOW_MISSED), 1);
    assert_eq!(status(&rig.db, UUID), "CANCELLED");
    assert!(rig.sim.submitted().is_empty(), "no Executor call");
    assert!(rig.market.calls().is_empty(), "nothing fetched for a missed pair");
}

#[tokio::test(start_paused = true)]
async fn an_unavailable_offset_blocks_the_entry_with_one_event() {
    let (rig, _h) = started(Opts::default()).await;
    rig.offsets.0.lock().unwrap().insert(Exchange::Bybit, None);
    run_until(&rig.clock, T - 1_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
    assert_eq!(count(&rig.db, ENTRY_BLOCKED), 1, "{:?}", labels(&rig.db));
    assert!(rig.sim.submitted().is_empty());
}

// ---- 2.2 Node 0 -------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_second_fetch_that_did_not_update_is_blocked() {
    let (rig, _h) = started(Opts::default()).await;
    *rig.market.frozen.lock().unwrap() = true;
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "BLOCKED");
    let blocked = events(&rig.db).into_iter().find(|(_, l, _)| l == "BLOCKED").unwrap().2;
    assert!(blocked.to_string().contains("DataFresh"), "{blocked}");
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_bybit_leverage_override_of_4_blocks_leverage_5() {
    let (rig, _h) = started(Opts::default()).await;
    let mut ov = RiskOverrides::new();
    ov.insert(Exchange::Bybit, RiskOverride { max_leverage: Some(dec("4")), ..Default::default() });
    store_overrides(&rig.db, &ov);
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "BLOCKED");
    let blocked = events(&rig.db).into_iter().find(|(_, l, _)| l == "BLOCKED").unwrap().2;
    assert!(blocked.to_string().contains("Leverage"), "{blocked}");
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_unavailable_margin_blocks() {
    let (rig, _h) = started(Opts { margin: Err("demo balance unreadable".into()), ..Opts::default() }).await;
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "BLOCKED");
    let blocked = events(&rig.db).into_iter().find(|(_, l, _)| l == "BLOCKED").unwrap().2;
    assert!(blocked.to_string().contains("Margin"), "{blocked}");
}

// ---- 2.3 Node 1–5 -------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_effective_timeout_of_7_seconds_fires_at_7_seconds() {
    let never = Arc::new(NeverFills::default());
    let (rig, _h) = started(Opts { simulator: Some(never.clone()), ..Opts::default() }).await;
    let mut ov = RiskOverrides::new();
    ov.insert(Exchange::Bybit, RiskOverride { order_timeout_seconds: Some(7), ..Default::default() });
    store_overrides(&rig.db, &ov);
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "FILL_MONITOR", "6 s after sending: still waiting");
    run_until(&rig.clock, T - 3_000).await;
    assert_eq!(status(&rig.db, UUID), "CANCELLED", "both legs unfilled at 7 s");
    assert_eq!(ts_of(&rig.db, "CANCELLED"), vec![T - 3_000], "fires at sent + 7 s, not + 15 s");
    assert_eq!(never.submits.load(Ordering::SeqCst), 2, "no order besides the original two");
    assert!(never.queries.load(Ordering::SeqCst) > 0, "fills were polled");
}

#[tokio::test(start_paused = true)]
async fn long_full_short_seventy_percent_is_partial_failure_and_nothing_else_is_sent() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Partial(dec("0.7")));
    run_until(&rig.clock, T + 4_000).await;
    assert_eq!(status(&rig.db, UUID), "FILL_MONITOR");
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    assert_eq!(ts_of(&rig.db, "PARTIAL_FAILURE"), vec![T + 5_000], "default 15 s timeout");
    run_until(&rig.clock, T + 30_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "locked: no automatic exit");
    assert_eq!(rig.sim.submitted().len(), 2, "the Executor received only the original two orders");
}

#[tokio::test(start_paused = true)]
async fn a_leg_below_min_qty_is_never_sent() {
    let (rig, _h) = started(Opts::default()).await;
    let lot = OrderRules { lot: LotSize { step_size: dec("1"), min_qty: dec("20") }, okx_ct_val: None };
    rig.market.rules.lock().unwrap().insert(Exchange::Bybit, lot);
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "CANCELLED");
    assert!(rig.sim.submitted().is_empty(), "neither leg is sent");
    let cancelled = events(&rig.db).into_iter().find(|(_, l, _)| l == "CANCELLED").unwrap().2;
    assert!(cancelled.to_string().contains("short"), "{cancelled}");
}

#[tokio::test(start_paused = true)]
async fn a_scripted_short_reject_is_partial_failure_and_keeps_the_long_leg() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("insufficient margin (scripted)".into()));
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("10"), "long leg data kept");
    let long = rig.db.get_intent(&sim_id(Leg::Long, OrderAction::Open)).unwrap().unwrap();
    assert_eq!(long.state, "FILLED");
    let short = rig.db.get_intent(&sim_id(Leg::Short, OrderAction::Open)).unwrap().unwrap();
    assert_eq!(short.state, "FAILED");
    let long_event = events(&rig.db)
        .into_iter()
        .find(|(_, l, p)| l == ORDER_SUBMITTED && p["leg"] == json!("long"))
        .unwrap()
        .2;
    assert_eq!(long_event["filled_quantity"], json!("10"), "{long_event}");
    run_until(&rig.clock, T + 30_000).await;
    assert_eq!(rig.sim.submitted().len(), 2, "no automatic repair");
}

/// OKX orders are in contracts; fills are compared in base coin (contracts x ct_val).
#[tokio::test(start_paused = true)]
async fn an_okx_leg_is_sent_in_contracts_and_compared_in_base_coin() {
    let (rig, deps) = rig(Opts::default());
    let okx = OrderRules { lot: LotSize { step_size: dec("1"), min_qty: dec("1") }, okx_ct_val: Some(dec("0.01")) };
    rig.market.rules.lock().unwrap().insert(Exchange::Okx, okx);
    let h = start(deps);
    let mut p = pair_at(UUID, SYM, T);
    p.short_exchange = Exchange::Okx;
    assert_eq!(ask(&h, Command::AddPrepared(p)).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED", "10 BTC long vs 1000 contracts x 0.01 short is balanced");
    let sent = rig.sim.submitted();
    assert_eq!((sent[0].exchange, sent[0].quantity), (Exchange::Binance, dec("10")));
    assert_eq!((sent[1].exchange, sent[1].quantity), (Exchange::Okx, dec("1000")), "OKX quantity is contracts");
    run_until(&rig.clock, T + 15_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    assert_eq!(rig.sim.submitted()[3].quantity, dec("1000"), "closed in contracts too");
}

#[tokio::test(start_paused = true)]
async fn a_hanging_submit_ends_unresolved_at_the_timeout() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Hang { executed: false });
    run_until(&rig.clock, T + 4_000).await;
    assert_eq!(status(&rig.db, UUID), "ORDER_SUBMIT", "waiting for the hanging leg");
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED");
    assert_eq!(rig.sim.submitted().len(), 2, "never resubmitted");
    let short = rig.db.get_intent(&sim_id(Leg::Short, OrderAction::Open)).unwrap().unwrap();
    assert_eq!(short.state, "SUBMITTED", "outcome unknown, not FAILED");
}

// ---- 2.4 PREPARED auto-cancel -----------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn prepared_auto_cancel_runs_while_halted_and_leaves_other_pairs_alone() {
    let (rig, deps) = rig(Opts { start_ms: T - 120_000, ..Opts::default() });
    // Halted, and Bybit no longer allowed, before the engine looks at anything.
    rig.db.set_kill_switch(true).unwrap();
    let mut cfg = risk();
    cfg.allowed_exchanges = vec![Exchange::Binance, Exchange::Okx];
    store_risk(&rig.db, &cfg);
    // A pair that already holds exposure, on another symbol.
    let env = PairEnvelope {
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        settlement_ms: T + 3_600_000,
        simulated: true,
        scan: json!({}),
    };
    let row = NewPair {
        internal_uuid: "held".into(),
        pair_id: "pid-held".into(),
        symbol: "ETHUSDT".into(),
        status: PairState::Reconciled,
        entry: serde_json::to_value(env).unwrap(),
    };
    rig.db.add_pair_if_not_pending(&row).unwrap();
    let h = start(deps);
    assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 110_000).await;
    assert_eq!(status(&rig.db, UUID), "CANCELLED");
    let cancelled = events(&rig.db).into_iter().find(|(_, l, _)| l == "CANCELLED").unwrap().2;
    assert!(cancelled.to_string().contains("ExchangeNotAllowed"), "{cancelled}");
    assert_eq!(status(&rig.db, "held"), "RECONCILED");
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn prepared_auto_cancel_is_off_in_manual_mode() {
    let (rig, _h) = started(Opts { start_ms: T - 120_000, trigger: "MANUAL", ..Opts::default() }).await;
    let mut cfg = risk();
    cfg.allowed_exchanges = vec![Exchange::Binance, Exchange::Okx];
    store_risk(&rig.db, &cfg);
    run_until(&rig.clock, T - 100_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
}

// ---- 3.2 one order path: manual orders --------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_manual_order_goes_through_the_simulator_with_an_intent() {
    let (rig, h) = started(Opts::default()).await;
    let order = |reduce_only: bool, side: OrderSide| {
        Command::ManualOrder(crate::engine::command::ManualOrder {
            exchange: Exchange::Bybit,
            symbol: "ETHUSDT".into(),
            side,
            quantity: dec("2"),
            reduce_only,
        })
    };
    assert_eq!(ask(&h, order(false, OrderSide::Sell)).await, CommandReply::Accepted);
    sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.sim.position(Exchange::Bybit, "ETHUSDT"), dec("-2"));
    // While halted only a reduce-only order passes.
    ask(&h, Command::SetKillSwitch { on: true }).await;
    assert!(matches!(ask(&h, order(false, OrderSide::Sell)).await, CommandReply::Rejected(_)));
    assert_eq!(ask(&h, order(true, OrderSide::Buy)).await, CommandReply::Accepted);
    sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.sim.position(Exchange::Bybit, "ETHUSDT"), Decimal::ZERO);
    let sent = rig.sim.submitted();
    assert_eq!(sent.len(), 2);
    for r in &sent {
        assert!(r.client_order_id.starts_with("sim"), "{r:?}");
        assert!(rig.db.get_intent(&r.client_order_id).unwrap().is_some(), "intent landed for {r:?}");
    }
    assert_ne!(sent[0].client_order_id, sent[1].client_order_id);
    assert_eq!(count(&rig.db, MANUAL_ORDER_RESULT), 2);
    assert_eq!(rig.factory.calls(), 0);
}

// ---- dedupe ------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn repeated_refusals_for_one_pair_and_cause_are_written_once() {
    let (rig, h) = started(Opts::default()).await;
    ask(&h, Command::SetKillSwitch { on: true }).await;
    for _ in 0..5 {
        assert!(matches!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Rejected(_)));
    }
    assert_eq!(count(&rig.db, "COMMAND_REFUSED"), 1);
}

// ---- startup reconciliation hook ---------------------------------------------------------

fn leave_unfinished_intent(db: &Db) {
    let i = NewIntent {
        client_order_id: client_order_id(IdPrefix::Sim, "old-pair", Leg::Long, OrderAction::Open, 0),
        pair_uuid: "old-pair".into(),
        leg: "long".into(),
        exchange: "Binance".into(),
        symbol: SYM.into(),
        side: "BUY".into(),
        quantity: "1".into(),
    };
    db.create_intent(&i).unwrap();
}

#[tokio::test(start_paused = true)]
async fn unfinished_intents_block_exposure_until_the_reconciler_succeeds() {
    for (result, lifted) in [(Ok(()), true), (Err("exchange unreachable".to_string()), false)] {
        let rec = Arc::new(FakeReconciler { calls: AtomicUsize::new(0), result: result.clone() });
        let (rig, deps) = rig(Opts { reconciler: Some(rec.clone()), ..Opts::default() });
        leave_unfinished_intent(&rig.db);
        let h = start(deps);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(rec.calls.load(Ordering::SeqCst), 1);
        let blockers = h.snapshots.borrow().blockers.clone();
        let pending = blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(_)));
        assert_eq!(pending, !lifted, "{result:?}: {blockers:?}");
        if let Err(why) = &result {
            assert!(blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(r) if r.contains(why.as_str()))));
            assert!(matches!(ask(&h, Command::ManualEnter { pair: "x".into() }).await, CommandReply::Rejected(r) if r.contains("reconciliation")));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn unfinished_intents_without_a_reconciler_stay_blocked() {
    let (rig, deps) = rig(Opts::default());
    leave_unfinished_intent(&rig.db);
    let h = start(deps);
    sleep(Duration::from_millis(300)).await;
    assert!(h.snapshots.borrow().blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(_))));
}

/// Stands in for `engine::recovery` on an interrupted simulated pair (decision 7: UNRESOLVED).
struct SimInterrupted;
impl StartupReconciler for SimInterrupted {
    fn reconcile(&self, ctx: ReconcileContext) -> BoxFut<'static, Result<(), String>> {
        let store = EventStore::new(ctx.db.clone());
        let r = transition::land_then_act(&store, UUID, PairState::FillMonitor, SystemEvent::RestartUndetermined, json!({}), |_| ());
        Box::pin(std::future::ready(r.map(|_| ()).map_err(|e| e.to_string())))
    }
}

fn seed_in_flight(db: &Db) {
    let env = PairEnvelope {
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        settlement_ms: T,
        simulated: true,
        scan: pair_at(UUID, SYM, T).entry,
    };
    let row = NewPair {
        internal_uuid: UUID.into(),
        pair_id: "pid".into(),
        symbol: SYM.into(),
        status: PairState::FillMonitor,
        entry: serde_json::to_value(env).unwrap(),
    };
    db.add_pair_if_not_pending(&row).unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_in_flight_pair_at_startup_waits_for_the_reconciler_and_its_result_is_adopted() {
    // Without a reconciler: blocked, and the scheduler never touches the pair.
    let (bare, deps) = rig(Opts::default());
    seed_in_flight(&bare.db);
    let h = start(deps);
    run_until(&bare.clock, T + 30_000).await;
    assert_eq!(status(&bare.db, UUID), "FILL_MONITOR", "no fills in memory: left to the reconciler");
    assert!(bare.sim.submitted().is_empty());
    assert!(h.snapshots.borrow().blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(_))));

    // With one: its store change is adopted and the block is lifted.
    let (rig, deps) = rig(Opts { reconciler: Some(Arc::new(SimInterrupted)), ..Opts::default() });
    seed_in_flight(&rig.db);
    let h = start(deps);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED");
    let snap = h.snapshots.borrow().clone();
    assert_eq!(snap.pairs[0].state, PairState::Unresolved);
    assert!(snap.blockers.is_empty(), "{:?}", snap.blockers);
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn no_unfinished_intents_means_no_reconciliation_block() {
    let rec = Arc::new(FakeReconciler { calls: AtomicUsize::new(0), result: Err("never".into()) });
    let (_rig, deps) = rig(Opts { reconciler: Some(rec.clone()), ..Opts::default() });
    let h = start(deps);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(rec.calls.load(Ordering::SeqCst), 0);
    assert!(h.snapshots.borrow().blockers.is_empty());
}

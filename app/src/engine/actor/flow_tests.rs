//! End-to-end flows through the actor (tasks 2.1–2.4, 3.1, 3.2, 4.1 integration): scheduler,
//! Node 0, Node 1–5, exit, closed confirmation, modes and the kill switch. Everything runs on
//! tokio's paused time with a `ManualClock`; all ports are in-memory fakes (no network).
//!
//! Pacing: the actor ticks every (tokio) second; the test moves the injected clock by exactly one
//! second half-way between two ticks, so tick k sees `start + k * 1000` and every event timestamp
//! is a value the fake clock had.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tokio::time::sleep;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::{RiskConfig, RiskOverride, RiskOverrides};

use super::*;
use crate::engine::alert::RecordingNotifier;
use crate::engine::command::NewPreparedPair;
use crate::engine::ids::{IdPrefix, client_order_id};
use crate::engine::ports::{
    AccountOrder, AccountPosition, AccountView, BoxFut, FreshQuote, Listed, MarketData, OrderRules, OrderSide, OrderState,
    OrderStatus, QueryOutcome, ReconcileContext, ServerOffsets, StartupReconciler, SubmitOutcome,
};
use crate::engine::sim::{CountingFactory, MarginSource, SimBehavior, SimPriceBook, SimulatedExecutor};
use crate::engine::recovery::{ORDER_INTENT_NOT_SENT, RECONCILE_ALERT, RecoveryReconciler, SIMULATION_INTERRUPTED};
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
            fee: None,
            fee_asset: None,
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

/// A simulator stand-in that accepts every order and never fills it (for the timeout test). A
/// cancel works like on an exchange: the order is then reported cancelled (with no fill).
#[derive(Default)]
struct NeverFills {
    submits: AtomicUsize,
    queries: AtomicUsize,
    cancels: AtomicUsize,
    ids: Mutex<Vec<String>>,
    cancelled: Mutex<BTreeSet<String>>,
}
impl NeverFills {
    fn status(id: &str) -> OrderStatus {
        OrderStatus {
            client_order_id: id.into(),
            exchange_order_id: Some("sim-x".into()),
            filled_quantity: Decimal::ZERO,
            avg_price: None,
            fee: None,
            fee_asset: None,
            state: OrderState::Open,
        }
    }
    fn current(&self, id: &str) -> OrderStatus {
        let mut s = NeverFills::status(id);
        if self.cancelled.lock().unwrap().contains(id) {
            s.state = OrderState::Cancelled;
        }
        s
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
    fn cancel(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.cancelled.lock().unwrap().insert(id.to_string());
        Box::pin(std::future::ready(QueryOutcome::Found(self.current(id))))
    }
    fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(QueryOutcome::Found(self.current(id))))
    }
}

/// Answers from a script (the last answer repeats) and records each call's scope.
struct FakeReconciler {
    calls: AtomicUsize,
    results: Mutex<Vec<Result<(), String>>>,
    scopes: Mutex<Vec<BTreeSet<String>>>,
}
impl FakeReconciler {
    fn new(results: Vec<Result<(), String>>) -> Arc<FakeReconciler> {
        Arc::new(FakeReconciler { calls: AtomicUsize::new(0), results: Mutex::new(results), scopes: Mutex::default() })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}
impl StartupReconciler for FakeReconciler {
    fn reconcile(&self, ctx: ReconcileContext) -> BoxFut<'static, Result<(), String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            ctx.executor.is_simulated(),
            ctx.execution_mode == ExecutionMode::Simulation,
            "the reconciler gets the executor of the current mode"
        );
        self.scopes.lock().unwrap().push(ctx.scope.clone());
        let mut results = self.results.lock().unwrap();
        let r = if results.len() > 1 { results.remove(0) } else { results[0].clone() };
        Box::pin(std::future::ready(r))
    }
}

/// A reconciliation that never finishes (pending forever).
struct NeverReconciles;
impl StartupReconciler for NeverReconciles {
    fn reconcile(&self, _ctx: ReconcileContext) -> BoxFut<'static, Result<(), String>> {
        Box::pin(std::future::pending())
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
    notifier: Arc<RecordingNotifier>,
}

struct Opts {
    start_ms: i64,
    trigger: &'static str,
    demo: bool,
    margin: Result<Decimal, String>,
    simulator: Option<Arc<dyn Executor>>,
    reconciler: Option<Arc<dyn StartupReconciler>>,
    /// Binance position reads that still show the pre-close position after a close order was
    /// sent (a lagging account update).
    stale_position_reads: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            start_ms: T - 20_000,
            trigger: "AUTO",
            demo: false,
            margin: Ok(dec("10000")),
            simulator: None,
            reconciler: None,
            stale_position_reads: 0,
        }
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
    let mut sim_account: Arc<dyn AccountView> = Arc::new(sim.account_view(Arc::new(FixedMargin(o.margin.clone()))));
    if o.stale_position_reads > 0 {
        sim_account = Arc::new(LaggingAccount { inner: sim_account, sim: sim.clone(), stale_left: AtomicUsize::new(o.stale_position_reads) });
    }
    let demo = Arc::new(FakeDemo::default());
    let factory = CountingFactory::returning(demo.clone());
    if o.demo {
        db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
    }
    let market = FakeMarket::new(clock.clone());
    let offsets = FakeOffsets::zero();
    let notifier = Arc::new(RecordingNotifier::default());
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
        notifier: notifier.clone(),
    };
    (Rig { _dir: dir, db, clock, market, offsets, sim, factory, demo, notifier }, deps)
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
    if demo {
        // funding-pnl 3.1: closed and confirmed flat, but FINALIZED also needs the PnL; with no
        // funding ledger fetched it waits for the retry window, then records INCOMPLETE.
        assert_eq!(status(&rig.db, UUID), "CLOSING", "{trigger}/{demo}: waiting for the PnL");
        assert_eq!(count(&rig.db, CLOSE_CONFIRMED), 1);
        assert_eq!(count(&rig.db, super::pnl_gate::PNL_PENDING), 1, "written once");
        assert_eq!(count(&rig.db, crate::funding::PAIR_PNL_COMPUTED), 0);
        run_until(&rig.clock, T + 15_000 + crate::funding::PNL_RETRY_WINDOW_MS + 2_000).await;
        assert_eq!(count(&rig.db, CLOSE_CONFIRMED), 1, "no second flat check while waiting");
        let l = labels(&rig.db);
        let pnl_at = l.iter().position(|x| x == crate::funding::PAIR_PNL_COMPUTED).expect("PnL recorded");
        let fin_at = l.iter().position(|x| x == "FINALIZED").expect("finalized");
        assert!(pnl_at < fin_at, "the PnL event lands before FINALIZED: {l:?}");
        let pnl = events(&rig.db).into_iter().find(|(_, l, _)| l == crate::funding::PAIR_PNL_COMPUTED).unwrap().2;
        assert_eq!(pnl["status"], json!("INCOMPLETE"));
        let fin = events(&rig.db).into_iter().find(|(_, l, _)| l == "FINALIZED").unwrap().2;
        assert_eq!(fin["detail"]["pnl_status"], json!("INCOMPLETE"));
    } else {
        assert_eq!(count(&rig.db, crate::funding::PAIR_PNL_COMPUTED), 0, "SIMULATION produces no PnL");
    }
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
    assert_eq!(never.cancels.load(Ordering::SeqCst), 2, "both own unfilled orders cancelled at the timeout");
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

// ---- close quantity = min(recorded fill, actual position) (decision 2026-10-05 evening) -----

/// A user's own order on the long leg's symbol (Binance), straight to the simulated ledger and
/// behind the pair's back.
async fn user_order(rig: &Rig, side: OrderSide, qty: &str, reduce_only: bool) {
    let action = if reduce_only { OrderAction::Close } else { OrderAction::Open };
    let req = OrderRequest {
        client_order_id: client_order_id(IdPrefix::Sim, "user-own-position", Leg::Long, action, 0),
        exchange: Exchange::Binance,
        symbol: SYM.into(),
        side,
        quantity: dec(qty),
        reduce_only,
    };
    let out = rig.sim.submit(req).await;
    assert!(matches!(out, SubmitOutcome::Accepted(_)), "{out:?}");
}

/// The pair's close orders that reached the simulator, per leg.
fn pair_closes(rig: &Rig) -> Vec<(Leg, Decimal)> {
    let sent = rig.sim.submitted();
    Leg::BOTH
        .into_iter()
        .filter_map(|leg| sent.iter().find(|r| r.client_order_id == sim_id(leg, OrderAction::Close)).map(|r| (leg, r.quantity)))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_extra_user_position_on_the_same_symbol_is_left_untouched() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    user_order(&rig, OrderSide::Buy, "0.05", false).await; // 10.05 long, 0.5 % off: within 1 %
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("10.05"));
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(pair_closes(&rig), vec![(Leg::Long, dec("10")), (Leg::Short, dec("10"))], "close = the pair's recorded fill");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("0.05"), "the user's own 0.05 is still there");
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO);
    assert_eq!(status(&rig.db, UUID), "FINALIZED", "the pair's part is flat; the user's part is not the pair's");
    assert_eq!(count(&rig.db, CLOSE_QUANTITY_MISMATCH), 0);
}

#[tokio::test(start_paused = true)]
async fn a_position_far_from_the_recorded_fill_is_not_closed_and_goes_to_partial_failure() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    user_order(&rig, OrderSide::Buy, "2", false).await; // 12 long vs 10 recorded: 16.7 % > 1 %
    run_until(&rig.clock, T + 16_000).await;
    assert!(pair_closes(&rig).is_empty(), "no close order at all: {:?}", pair_closes(&rig));
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("12"), "nothing touched");
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), dec("-10"), "nothing touched");
    let alerts: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == CLOSE_QUANTITY_MISMATCH).map(|(_, _, p)| p).collect();
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    let a = &alerts[0];
    assert_eq!((a["leg"].clone(), a["recorded_quantity"].clone(), a["position_quantity"].clone()), (json!("long"), json!("10"), json!("12")), "{a}");
    assert_eq!(a["tolerance_pct"], json!("1"), "{a}");
    assert_eq!(a["simulated"], json!(true), "{a}");
    let failed = events(&rig.db).into_iter().find(|(_, l, _)| l == "PARTIAL_FAILURE").unwrap().2;
    assert!(failed.to_string().contains("recorded"), "{failed}");
    assert!(rig.db.list_unfinished_intents().unwrap().is_empty(), "no close intent written");
}

#[tokio::test(start_paused = true)]
async fn a_small_shortfall_within_tolerance_closes_the_smaller_actual_position() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    user_order(&rig, OrderSide::Sell, "0.05", true).await; // 9.95 long: 0.5 % short of 10
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(pair_closes(&rig), vec![(Leg::Long, dec("9.95")), (Leg::Short, dec("10"))], "close = min(recorded, position)");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
}

#[tokio::test(start_paused = true)]
async fn an_unknown_recorded_fill_with_a_position_closes_nothing() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    rig.sim.fail_queries(Some("exchange unreachable (scripted)".into()));
    run_until(&rig.clock, T + 16_000).await;
    assert!(pair_closes(&rig).is_empty(), "never guessed: {:?}", pair_closes(&rig));
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    assert_eq!(count(&rig.db, CLOSE_QUANTITY_MISMATCH), 2, "one alert per leg");
    let a = events(&rig.db).into_iter().find(|(_, l, _)| l == CLOSE_QUANTITY_MISMATCH).unwrap().2;
    assert_eq!(a["recorded_quantity"], Value::Null, "{a}");
    assert!(a["reason"].as_str().unwrap().contains("unreachable"), "{a}");
}

// ---- fill details for funding-pnl (decision 2026-10-05 evening) ---------------------------

#[tokio::test(start_paused = true)]
async fn order_submit_carries_the_entry_snapshot_and_order_events_carry_fill_details() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    let submit = events(&rig.db).into_iter().find(|(_, l, _)| l == "ORDER_SUBMIT").unwrap().2;
    let snap = &submit["detail"]["entry_snapshot"];
    assert_eq!(snap["long"]["exchange"], json!("Binance"), "{submit}");
    assert_eq!(snap["short"]["exchange"], json!("Bybit"), "{submit}");
    assert_eq!(snap["long"]["expected_price"], json!("100"), "{submit}");
    assert_eq!(snap["short"]["expected_price"], json!("100"), "{submit}");
    assert_eq!(snap["long"]["baseline_price"], json!("100"), "{submit}");
    assert_eq!(snap["long"]["funding_rate"], json!("-0.001"), "{submit}");
    assert_eq!(snap["short"]["funding_rate"], json!("0.001"), "{submit}");
    assert_eq!(snap["notional_usdt"], json!("1000"), "{submit}");
    assert_eq!(snap["leverage"], json!("5"), "{submit}");
    assert_eq!(snap["net_edge"]["threshold_pct"], json!("0.05"), "{submit}");
    for k in ["net_edge_pct", "net_edge_usdt", "funding_income_usdt", "fee_usdt", "slippage_usdt", "safety_margin_usdt", "gross_spread"] {
        assert!(snap["net_edge"][k].is_string(), "{k} missing: {submit}");
    }
    assert_eq!(submit["detail"]["simulated"], json!(true));

    let orders: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == ORDER_SUBMITTED).map(|(_, _, p)| p).collect();
    assert_eq!(orders.len(), 2);
    for p in &orders {
        assert_eq!(p["avg_price"], json!("100"), "{p}");
        assert_eq!(p["filled_quantity"], json!("10"), "{p}");
        assert_eq!(p["fee"], json!("0"), "{p}");
        assert_eq!(p["fee_asset"], json!("USDT"), "{p}");
        assert_eq!(p["simulated"], json!(true), "{p}");
        assert!(p["exchange"].is_string() && p["symbol"] == json!(SYM), "{p}");
    }
}

/// Accepts every order unfilled; the first lookup reports it filled at 100.5 with a BNB fee.
#[derive(Default)]
struct FillsOnQuery {
    requests: Mutex<HashMap<String, OrderRequest>>,
}
impl Executor for FillsOnQuery {
    fn is_simulated(&self) -> bool {
        true
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        self.requests.lock().unwrap().insert(req.client_order_id.clone(), req.clone());
        let status = OrderStatus {
            client_order_id: req.client_order_id,
            exchange_order_id: Some("sim-q".into()),
            filled_quantity: Decimal::ZERO,
            avg_price: None,
            fee: None,
            fee_asset: None,
            state: OrderState::Open,
        };
        Box::pin(std::future::ready(SubmitOutcome::Accepted(status)))
    }
    fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
        Box::pin(std::future::ready(QueryOutcome::NotFound))
    }
    fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        let out = self.requests.lock().unwrap().get(id).map_or(QueryOutcome::NotFound, |r| {
            QueryOutcome::Found(OrderStatus {
                client_order_id: id.into(),
                exchange_order_id: Some("sim-q".into()),
                filled_quantity: r.quantity,
                avg_price: Some(dec("100.5")),
                fee: Some(dec("0.0123")),
                fee_asset: Some("BNB".into()),
                state: OrderState::Filled,
            })
        });
        Box::pin(std::future::ready(out))
    }
}

#[tokio::test(start_paused = true)]
async fn a_fill_reported_by_a_later_query_is_recorded_with_its_details() {
    let exec = Arc::new(FillsOnQuery::default());
    let (rig, _h) = started(Opts { simulator: Some(exec.clone()), ..Opts::default() }).await;
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    let fills: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == ORDER_FILL).map(|(_, _, p)| p).collect();
    assert_eq!(fills.len(), 2, "one per leg, written once: {fills:?}");
    let mut legs: Vec<String> = fills.iter().map(|p| p["leg"].as_str().unwrap().to_string()).collect();
    legs.sort();
    assert_eq!(legs, vec!["long", "short"]);
    for p in &fills {
        assert_eq!(p["action"], json!("open"), "{p}");
        assert_eq!(p["state"], json!("Filled"), "{p}");
        assert_eq!(p["filled_quantity"], json!("10"), "{p}");
        assert_eq!(p["avg_price"], json!("100.5"), "{p}");
        assert_eq!(p["fee"], json!("0.0123"), "{p}");
        assert_eq!(p["fee_asset"], json!("BNB"), "{p}");
        assert_eq!(p["simulated"], json!(true), "{p}");
        assert!(p["client_order_id"].as_str().unwrap().starts_with("sim"), "{p}");
    }
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
        let rec = FakeReconciler::new(vec![result.clone()]);
        let (rig, deps) = rig(Opts { demo: true, reconciler: Some(rec.clone()), ..Opts::default() });
        leave_unfinished_intent(&rig.db);
        let h = start(deps);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(rec.calls(), 1);
        assert_eq!(*rec.scopes.lock().unwrap(), vec![BTreeSet::from(["old-pair".to_string()])], "startup scope");
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
    let rec = FakeReconciler::new(vec![Err("never".into())]);
    let (_rig, deps) = rig(Opts { reconciler: Some(rec.clone()), ..Opts::default() });
    let h = start(deps);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(rec.calls(), 0);
    assert!(h.snapshots.borrow().blockers.is_empty());
}

// ---- 4.2 wave 3: decision 8, retries, startup scope ----------------------------------------

/// A demo pair left in FILL_MONITOR by the previous run (not simulated).
fn seed_demo_in_flight(db: &Db, uuid: &str) {
    let env = PairEnvelope {
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        settlement_ms: T - 3_600_000,
        simulated: false,
        scan: pair_at(uuid, "ETHUSDT", T).entry,
    };
    let row = NewPair {
        internal_uuid: uuid.into(),
        pair_id: format!("pid-{uuid}"),
        symbol: "ETHUSDT".into(),
        status: PairState::FillMonitor,
        entry: serde_json::to_value(env).unwrap(),
    };
    db.add_pair_if_not_pending(&row).unwrap();
}

// decision 8: unfinished demo reconciliation refuses exposure only in EXCHANGE_DEMO.
#[tokio::test(start_paused = true)]
async fn pending_demo_reconciliation_refuses_entries_only_in_exchange_demo() {
    for demo in [true, false] {
        let rec = FakeReconciler::new(vec![Err("demo keys unavailable".into())]);
        let (rig, deps) = rig(Opts { demo, trigger: "MANUAL", reconciler: Some(rec.clone()), ..Opts::default() });
        seed_demo_in_flight(&rig.db, "demo-stuck");
        let h = start(deps);
        assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
        sleep(Duration::from_millis(300)).await;
        let blockers = h.snapshots.borrow().blockers.clone();
        assert!(
            blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(r) if r.contains("demo keys unavailable"))),
            "demo={demo}: the blocker stays visible in the Snapshot: {blockers:?}"
        );
        let reply = ask(&h, Command::ManualEnter { pair: UUID.into() }).await;
        if demo {
            assert!(matches!(&reply, CommandReply::Rejected(r) if r.contains("reconciliation")), "{reply:?}");
            assert_eq!(status(&rig.db, UUID), "PREPARED");
        } else {
            assert_eq!(reply, CommandReply::Accepted, "SIMULATION entries stay allowed");
            assert_ne!(status(&rig.db, UUID), "PREPARED");
        }
        assert_eq!(status(&rig.db, "demo-stuck"), "FILL_MONITOR", "the unreconciled pair is left alone");
    }
}

#[tokio::test(start_paused = true)]
async fn an_unreconciled_demo_pair_still_takes_a_max_concurrent_pairs_slot() {
    let rec = FakeReconciler::new(vec![Err("demo keys unavailable".into())]);
    let (rig, deps) = rig(Opts { reconciler: Some(rec), ..Opts::default() });
    store_risk(&rig.db, &RiskConfig { max_concurrent_pairs: 1, ..risk() });
    seed_demo_in_flight(&rig.db, "demo-stuck");
    let h = start(deps);
    assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "BLOCKED", "{:?}", labels(&rig.db));
    let blocked = events(&rig.db).into_iter().find(|(_, l, _)| l == "BLOCKED").unwrap().2;
    assert!(blocked.to_string().contains("RiskLimits"), "{blocked}");
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_pending_reconciliation_is_retried_every_30_seconds_until_it_succeeds() {
    let rec = FakeReconciler::new(vec![Err("exchange unreachable".into()), Ok(())]);
    let start_ms = T - 600_000; // well before the entry window of the pair added below
    let (rig, deps) = rig(Opts { demo: true, trigger: "MANUAL", start_ms, reconciler: Some(rec.clone()), ..Opts::default() });
    leave_unfinished_intent(&rig.db);
    let t0 = rig.clock.now_ms();
    let h = start(deps);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(rec.calls(), 1);
    let pending = |h: &EngineHandle| h.snapshots.borrow().blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(_)));
    assert!(pending(&h));
    // A pair added after startup is live: it is never in the reconciler's scope.
    assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
    run_until(&rig.clock, t0 + RECONCILE_RETRY_MS - 1_000).await;
    assert_eq!(rec.calls(), 1, "not before 30 s");
    assert!(pending(&h));
    run_until(&rig.clock, t0 + RECONCILE_RETRY_MS + 1_000).await;
    assert_eq!(rec.calls(), 2);
    assert!(!pending(&h), "{:?}", h.snapshots.borrow().blockers);
    assert_eq!(rec.scopes.lock().unwrap()[1], BTreeSet::from(["old-pair".to_string()]));
    run_until(&rig.clock, t0 + 4 * RECONCILE_RETRY_MS).await;
    assert_eq!(rec.calls(), 2, "no run once it succeeded");
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
}

fn seed_sim_reconciled(db: &Db) {
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
        status: PairState::Reconciled,
        entry: serde_json::to_value(env).unwrap(),
    };
    db.add_pair_if_not_pending(&row).unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_simulated_reconciled_pair_at_startup_is_never_auto_exited_and_goes_to_unresolved() {
    // While its reconciliation has not finished, the scheduler does not touch it (past T+15).
    let (bare, deps) = rig(Opts { start_ms: T + 20_000, reconciler: Some(Arc::new(NeverReconciles)), ..Opts::default() });
    seed_sim_reconciled(&bare.db);
    let h = start(deps);
    run_until(&bare.clock, T + 40_000).await;
    assert_eq!(status(&bare.db, UUID), "RECONCILED", "{:?}", labels(&bare.db));
    assert!(bare.sim.submitted().is_empty(), "no close order for a position that no longer exists");
    assert!(h.snapshots.borrow().blockers.iter().any(|b| matches!(b, Blocker::ReconciliationPending(_))));

    // With the real reconciler: UNRESOLVED (decision 7), and the block is lifted.
    let (rig, mut deps) = rig(Opts { start_ms: T + 20_000, ..Opts::default() });
    deps.reconciler = Some(Arc::new(RecoveryReconciler::new(rig.market.clone(), Arc::new(rig.clock.clone()))));
    seed_sim_reconciled(&rig.db);
    let h = start(deps);
    run_until(&rig.clock, T + 25_000).await;
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED");
    assert_eq!(count(&rig.db, SIMULATION_INTERRUPTED), 1);
    assert!(h.snapshots.borrow().blockers.is_empty(), "{:?}", h.snapshots.borrow().blockers);
    assert!(rig.sim.submitted().is_empty());
}

// ---- 4.2 crash points (crash-recovery spec "崩潰測試涵蓋兩個關鍵時點") ------------------------
//
// Each test runs the full actor on a real database file in its own tokio runtime ("process 1"),
// stops it at a kill point by dropping that runtime (every task dies mid-flight), then starts a
// fresh actor with `RecoveryReconciler` on the same file ("process 2").
//
// Test-only fault injection: the exchange's `submit` never returns. Kill point (a) "intent
// written, executor not called": the call dies before it reaches the exchange (the exchange never
// learns the id). Kill point (b) "executor called, result not written back": the exchange fills
// the order, but the reply never comes back, so the intent stays SUBMITTED.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OnSubmit {
    /// Kill point (a): dies before the exchange sees the order.
    DieBefore,
    /// Kill point (b): the exchange fills it, the reply is lost.
    DieAfter,
    /// Fill and reply (process 2; nothing may be submitted there anyway).
    Fill,
}

/// An exchange (or, with `simulated`, the simulator) that outlives the program: orders, positions
/// and call counters survive the restart.
struct CrashExchange {
    simulated: bool,
    on_submit: Mutex<HashMap<Exchange, OnSubmit>>,
    orders: Mutex<HashMap<String, OrderStatus>>,
    positions: Mutex<HashMap<(Exchange, String), Decimal>>,
    submits: AtomicUsize,
    queries: AtomicUsize,
}

impl CrashExchange {
    fn new(simulated: bool, long: OnSubmit, short: OnSubmit) -> Arc<CrashExchange> {
        Arc::new(CrashExchange {
            simulated,
            on_submit: Mutex::new([(Exchange::Binance, long), (Exchange::Bybit, short)].into()),
            orders: Mutex::default(),
            positions: Mutex::default(),
            submits: AtomicUsize::new(0),
            queries: AtomicUsize::new(0),
        })
    }
    fn submits(&self) -> usize {
        self.submits.load(Ordering::SeqCst)
    }
    fn queries(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }
    fn fill(&self, req: &OrderRequest) -> OrderStatus {
        let signed = match req.side {
            OrderSide::Buy => req.quantity,
            OrderSide::Sell => -req.quantity,
        };
        *self.positions.lock().unwrap().entry((req.exchange, req.symbol.clone())).or_default() += signed;
        let status = OrderStatus {
            client_order_id: req.client_order_id.clone(),
            exchange_order_id: Some(format!("x-{}", req.client_order_id)),
            filled_quantity: req.quantity,
            avg_price: Some(dec("100")),
            fee: None,
            fee_asset: None,
            state: OrderState::Filled,
        };
        self.orders.lock().unwrap().insert(req.client_order_id.clone(), status.clone());
        status
    }
}

impl Executor for CrashExchange {
    fn is_simulated(&self) -> bool {
        self.simulated
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        let on = self.on_submit.lock().unwrap().get(&req.exchange).copied().unwrap_or(OnSubmit::Fill);
        match on {
            OnSubmit::DieBefore => Box::pin(std::future::pending()),
            OnSubmit::DieAfter => {
                self.fill(&req);
                Box::pin(std::future::pending())
            }
            OnSubmit::Fill => {
                let st = self.fill(&req);
                Box::pin(std::future::ready(SubmitOutcome::Accepted(st)))
            }
        }
    }
    fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
        panic!("nothing may cancel here");
    }
    fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let o = self.orders.lock().unwrap().get(id).cloned().map_or(QueryOutcome::NotFound, QueryOutcome::Found);
        Box::pin(std::future::ready(o))
    }
}

impl AccountView for CrashExchange {
    fn positions(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
        let items = (self.positions.lock().unwrap().iter())
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

/// One "process": its own paused runtime; dropping it kills every task mid-flight.
fn process<F: Future<Output = ()>>(f: F) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().start_paused(true).build().unwrap();
    rt.block_on(f);
    drop(rt);
}

/// Deps on the database file at `path`; `exchange` is the demo exchange (EXCHANGE_DEMO) or the
/// simulator (SIMULATION). Returns the db handle and the factory (to count its calls).
fn crash_deps(path: &std::path::Path, clock: &ManualClock, exchange: &Arc<CrashExchange>, demo: bool) -> (Db, CountingFactory, EngineDeps) {
    let shared: Arc<dyn Clock> = Arc::new(clock.clone());
    let db = Db::open_unlocked(path, shared.clone());
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    let factory = CountingFactory::returning(exchange.clone());
    let simulator: Arc<dyn Executor> = if demo { Arc::new(NeverFills::default()) } else { exchange.clone() };
    let market = FakeMarket::new(clock.clone());
    let deps = EngineDeps {
        db: db.clone(),
        clock: shared.clone(),
        timings: EngineTimings::default(),
        simulator,
        factory: Arc::new(factory.clone()),
        market: market.clone(),
        offsets: FakeOffsets::zero(),
        account: exchange.clone(),
        sim_account: exchange.clone(),
        reconciler: Some(Arc::new(RecoveryReconciler::new(market, shared))),
        notifier: Arc::new(RecordingNotifier::default()),
    };
    (db, factory, deps)
}

/// What the restart found.
struct AfterRestart {
    db: Db,
    submits_after_restart: usize,
    queries_after_restart: usize,
    factory_calls: usize,
    blockers: Vec<Blocker>,
}

/// Process 1 runs to the kill point (both entry orders landed, submits never return); process 2
/// restarts on the same file and reconciles.
fn crash_and_restart(demo: bool, long: OnSubmit, short: OnSubmit) -> (tempfile::TempDir, AfterRestart, Arc<CrashExchange>) {
    let dir = tempdir();
    let path = dir.path().join("funding.db");
    let exchange = CrashExchange::new(!demo, long, short);
    let clock = ManualClock::new(T - 12_000);

    process(async {
        let (db, _f, deps) = crash_deps(&path, &clock, &exchange, demo);
        store_risk(&db, &risk());
        db.flag_set(FLAG_TRIGGER_MODE, "AUTO").unwrap();
        if demo {
            db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        }
        let h = start(deps);
        assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
        run_until(&clock, T - 9_000).await;
        // At the kill point: both intents landed (SUBMITTED), both submits called, no result.
        assert_eq!(status(&db, UUID), "ORDER_SUBMIT", "{:?}", labels(&db));
        let intents = db.list_unfinished_intents().unwrap();
        assert_eq!(intents.len(), 2, "{intents:?}");
        assert!(intents.iter().all(|i| i.state == "SUBMITTED"), "{intents:?}");
        assert_eq!(exchange.submits(), 2);
    }); // killed

    let (submits_before, queries_before) = (exchange.submits(), exchange.queries());
    exchange.on_submit.lock().unwrap().clear(); // process 2 would fill normally (it must not submit)
    let mut out = None;
    process(async {
        let (db, factory, deps) = crash_deps(&path, &clock, &exchange, demo);
        let h = start(deps);
        run_until(&clock, T - 6_000).await;
        let blockers = h.snapshots.borrow().blockers.clone();
        out = Some(AfterRestart {
            db,
            submits_after_restart: exchange.submits() - submits_before,
            queries_after_restart: exchange.queries() - queries_before,
            factory_calls: factory.calls(),
            blockers,
        });
    });
    (dir, out.unwrap(), exchange)
}

fn intent_states(db: &Db) -> Vec<String> {
    let plain = rusqlite::Connection::open(db.path()).unwrap();
    let mut st = plain.prepare("SELECT state FROM order_intents ORDER BY client_order_id").unwrap();
    st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
}

#[test]
fn demo_kill_point_a_intent_written_but_never_sent_is_cancelled_without_resubmitting() {
    let (_dir, r, _x) = crash_and_restart(true, OnSubmit::DieBefore, OnSubmit::DieBefore);
    assert_eq!(r.submits_after_restart, 0, "no order submitted again");
    assert_eq!(r.queries_after_restart, 2, "both intents queried by client_order_id");
    assert_eq!(status(&r.db, UUID), "CANCELLED", "{:?}", labels(&r.db));
    assert_eq!(intent_states(&r.db), vec!["CANCELLED", "CANCELLED"], "marked not sent");
    assert_eq!(count(&r.db, ORDER_INTENT_NOT_SENT), 2);
    assert_eq!(count(&r.db, RECONCILE_ALERT), 0, "no alert: nothing was exposed");
    assert!(r.blockers.is_empty(), "{:?}", r.blockers);
}

#[test]
fn demo_kill_point_b_both_orders_filled_but_unrecorded_continue_to_reconciled_without_resubmitting() {
    let (_dir, r, _x) = crash_and_restart(true, OnSubmit::DieAfter, OnSubmit::DieAfter);
    assert_eq!(r.submits_after_restart, 0, "no order submitted again");
    assert_eq!(intent_states(&r.db), vec!["FILLED", "FILLED"]);
    assert_eq!(status(&r.db, UUID), "RECONCILED", "{:?}", labels(&r.db));
    let path: Vec<String> = labels(&r.db).into_iter().filter(|l| ["FILL_MONITOR", "RECONCILED"].contains(&l.as_str())).collect();
    assert_eq!(path, vec!["FILL_MONITOR", "RECONCILED"], "FILL_MONITOR, then the normal fill decision");
    assert_eq!(count(&r.db, RECONCILE_ALERT), 0);
    assert!(r.blockers.is_empty(), "{:?}", r.blockers);
}

#[test]
fn demo_kill_point_b_on_one_leg_only_is_a_partial_failure_with_an_alert() {
    let (_dir, r, x) = crash_and_restart(true, OnSubmit::DieAfter, OnSubmit::DieBefore);
    assert_eq!(r.submits_after_restart, 0, "no order submitted again");
    assert_eq!(status(&r.db, UUID), "PARTIAL_FAILURE", "{:?}", labels(&r.db));
    assert_eq!(count(&r.db, RECONCILE_ALERT), 1, "the alert is raised");
    assert!(intent_states(&r.db).contains(&"FILLED".to_string()), "the long leg's data is kept");
    assert_eq!(x.positions.lock().unwrap().get(&(Exchange::Binance, SYM.to_string())).copied(), Some(dec("10")), "nothing closed");
    assert!(r.blockers.is_empty(), "{:?}", r.blockers);
}

#[test]
fn simulation_kill_points_a_and_b_end_unresolved_without_any_exchange_request() {
    for on in [OnSubmit::DieBefore, OnSubmit::DieAfter] {
        let (_dir, r, _x) = crash_and_restart(false, on, on);
        assert_eq!(r.submits_after_restart, 0, "{on:?}: no order submitted again");
        assert_eq!(r.queries_after_restart, 0, "{on:?}: simulated intents are never queried");
        assert_eq!(r.factory_calls, 0, "{on:?}: no order-capable executor was even built");
        assert_eq!(status(&r.db, UUID), "UNRESOLVED", "{on:?}: {:?}", labels(&r.db));
        assert_eq!(count(&r.db, SIMULATION_INTERRUPTED), 1, "{on:?}");
        assert!(r.blockers.is_empty(), "{on:?}: {:?}", r.blockers);
    }
}

// ---- funding-pnl 3.1: a manual "confirmed closed" needs the PnL too -------------------------

/// A pair already locked in PARTIAL_FAILURE when the engine starts (not in flight: no reconciler).
async fn started_locked(simulated: bool) -> (Rig, EngineHandle) {
    let (rig, deps) = rig(Opts { demo: !simulated, ..Opts::default() });
    let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T, simulated, scan: pair_at(UUID, SYM, T).entry };
    let p = crate::store::state::NewPair {
        internal_uuid: UUID.into(),
        pair_id: format!("pid-{UUID}"),
        symbol: SYM.into(),
        status: PairState::Prepared,
        entry: serde_json::to_value(env).unwrap(),
    };
    rig.db.add_pair_if_not_pending(&p).unwrap();
    rig.db.set_pair_status(UUID, PairState::PartialFailure).unwrap();
    let h = start(deps);
    (rig, h)
}

#[tokio::test(start_paused = true)]
async fn a_manual_confirm_on_a_demo_pair_records_the_pnl_before_finalized() {
    let (rig, h) = started_locked(false).await;
    // Merge with exchange-demo-execution: the user's flag is not trusted, the system re-queries
    // both legs. A leg that still holds a position refuses the confirmation (and writes no PnL).
    rig.demo.positions.lock().unwrap().insert((Exchange::Binance, SYM.to_string()), dec("10"));
    let refused = ask(&h, Command::ConfirmClosed { pair: UUID.into(), verified_flat: true }).await;
    assert!(matches!(refused, CommandReply::Rejected(_)), "{refused:?}");
    assert_eq!(count(&rig.db, crate::funding::PAIR_PNL_COMPUTED), 0, "a refused confirmation writes no PnL");
    rig.demo.positions.lock().unwrap().clear();
    // Flat on the re-query: accepted even with `verified_flat: false` (the flag is ignored).
    assert_eq!(ask(&h, Command::ConfirmClosed { pair: UUID.into(), verified_flat: false }).await, CommandReply::Accepted);
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    let l = labels(&rig.db);
    let pnl_at = l.iter().position(|x| x == crate::funding::PAIR_PNL_COMPUTED).expect("PnL recorded");
    assert!(pnl_at < l.iter().position(|x| x == "FINALIZED").unwrap(), "{l:?}");
}

#[tokio::test(start_paused = true)]
async fn a_manual_confirm_on_a_simulated_pair_finalizes_without_any_pnl() {
    let (rig, h) = started_locked(true).await;
    assert_eq!(ask(&h, Command::ConfirmClosed { pair: UUID.into(), verified_flat: true }).await, CommandReply::Accepted);
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    assert_eq!(count(&rig.db, crate::funding::PAIR_PNL_COMPUTED), 0, "SIMULATION produces no PnL");
}

// ---- exchange-demo-execution: timeout cancel, alerts, manual exits, close ------------------

/// An account whose Binance position reads lag: after a close order was sent, the first
/// `stale_left` reads still show the position as it was before the close.
struct LaggingAccount {
    inner: Arc<dyn AccountView>,
    sim: Arc<SimulatedExecutor>,
    stale_left: AtomicUsize,
}
impl AccountView for LaggingAccount {
    fn positions(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
        Box::pin(async move {
            let mut l = self.inner.positions(exchange).await?;
            let closing = self.sim.submitted().iter().any(|r| r.reduce_only && r.exchange == exchange);
            if exchange == Exchange::Binance && closing && self.stale_left.load(Ordering::SeqCst) > 0 {
                self.stale_left.fetch_sub(1, Ordering::SeqCst);
                l.items.retain(|p| p.symbol != SYM);
                l.items.push(AccountPosition { exchange, symbol: SYM.into(), quantity: dec("10") });
            }
            Ok(l)
        })
    }
    fn open_orders(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
        self.inner.open_orders(exchange)
    }
    fn available_margin(&self, exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        self.inner.available_margin(exchange)
    }
}

/// Wraps a simulator and counts submit / cancel calls.
struct Counting {
    inner: Arc<SimulatedExecutor>,
    submits: AtomicUsize,
    cancels: AtomicUsize,
}
impl Counting {
    fn new() -> Arc<Counting> {
        let p = Arc::new(SimPriceBook::default());
        p.set(Exchange::Binance, SYM, dec("100"));
        p.set(Exchange::Bybit, SYM, dec("100"));
        Arc::new(Counting { inner: Arc::new(SimulatedExecutor::new(p)), submits: AtomicUsize::new(0), cancels: AtomicUsize::new(0) })
    }
    fn counts(&self) -> (usize, usize) {
        (self.submits.load(Ordering::SeqCst), self.cancels.load(Ordering::SeqCst))
    }
}
impl Executor for Counting {
    fn is_simulated(&self) -> bool {
        true
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        self.inner.submit(req)
    }
    fn cancel(&self, e: Exchange, s: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.inner.cancel(e, s, id)
    }
    fn query(&self, e: Exchange, s: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.inner.query(e, s, id)
    }
}

/// Accepts every order with no fill; what a cancel does is scripted.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CancelScript {
    /// The cancel loses the race: "unknown order" error, and the order is in fact fully filled.
    RacesAFill,
    /// The cancel times out and every lookup after it fails.
    Unconfirmable,
}
struct TimeoutExchange {
    script: CancelScript,
    submits: AtomicUsize,
    cancels: AtomicUsize,
    orders: Mutex<HashMap<String, (Decimal, OrderState, Decimal)>>,
}
impl TimeoutExchange {
    fn new(script: CancelScript) -> Arc<TimeoutExchange> {
        Arc::new(TimeoutExchange { script, submits: AtomicUsize::new(0), cancels: AtomicUsize::new(0), orders: Mutex::default() })
    }
    fn status(id: &str, state: OrderState, filled: Decimal) -> OrderStatus {
        OrderStatus { client_order_id: id.into(), exchange_order_id: Some("x-1".into()), filled_quantity: filled, avg_price: None, fee: None, fee_asset: None, state }
    }
}
impl Executor for TimeoutExchange {
    fn is_simulated(&self) -> bool {
        true
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        self.orders.lock().unwrap().insert(req.client_order_id.clone(), (req.quantity, OrderState::Open, Decimal::ZERO));
        Box::pin(std::future::ready(SubmitOutcome::Accepted(TimeoutExchange::status(&req.client_order_id, OrderState::Open, Decimal::ZERO))))
    }
    fn cancel(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        let mut orders = self.orders.lock().unwrap();
        let out = match (self.script, orders.get_mut(id)) {
            (CancelScript::RacesAFill, Some(o)) => {
                *o = (o.0, OrderState::Filled, o.0);
                QueryOutcome::Failed { reason: "-2011: Unknown order sent (already filled)".into() }
            }
            (CancelScript::Unconfirmable, Some(o)) => {
                o.1 = OrderState::Cancelled; // never visible: every later lookup fails
                QueryOutcome::Failed { reason: "request timed out".into() }
            }
            (_, None) => QueryOutcome::NotFound,
        };
        Box::pin(std::future::ready(out))
    }
    fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        let o = self.orders.lock().unwrap().get(id).copied();
        let out = match (self.script, o) {
            (CancelScript::Unconfirmable, Some((_, OrderState::Cancelled, _))) => QueryOutcome::Failed { reason: "request timed out".into() },
            (_, Some((_, state, filled))) => QueryOutcome::Found(TimeoutExchange::status(id, state, filled)),
            (_, None) => QueryOutcome::NotFound,
        };
        Box::pin(std::future::ready(out))
    }
}

fn payloads(db: &Db, label: &str) -> Vec<Value> {
    events(db).into_iter().filter(|(_, l, _)| l == label).map(|(_, _, p)| p).collect()
}

// -- 2.2 fill timeout

#[tokio::test(start_paused = true)]
async fn fills_complete_before_the_timeout_send_no_cancel() {
    let counting = Counting::new();
    let (rig, _h) = started(Opts { simulator: Some(counting.clone()), ..Opts::default() }).await;
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(counting.counts(), (2, 0), "two orders, no cancel");
    assert_eq!(count(&rig.db, ORDER_CANCEL_RESULT), 0);
}

#[tokio::test(start_paused = true)]
async fn at_the_timeout_the_unfilled_rest_of_a_seventy_percent_leg_is_cancelled_and_only_two_orders_exist() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Partial(dec("0.7")));
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    let cancels = payloads(&rig.db, ORDER_CANCEL_RESULT);
    assert_eq!(cancels.len(), 1, "only the short leg (the long one is filled): {cancels:?}");
    assert_eq!(cancels[0]["client_order_id"], json!(sim_id(Leg::Short, OrderAction::Open)));
    assert_eq!((cancels[0]["after"]["state"].clone(), cancels[0]["after"]["filled_quantity"].clone()), (json!("Cancelled"), json!("7")));
    assert_eq!(rig.sim.submitted().len(), 2, "no top-up, no sell-off: still exactly two order requests");
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), dec("-7"), "positions untouched by the timeout handling");
    let pf = events(&rig.db).into_iter().find(|(_, l, _)| l == "PARTIAL_FAILURE").unwrap();
    assert!(pf.2.to_string().contains("Terminal"), "decided on the final (post-cancel) fill: {}", pf.2);
}

#[tokio::test(start_paused = true)]
async fn a_cancel_that_races_a_fill_counts_the_leg_as_filled() {
    let x = TimeoutExchange::new(CancelScript::RacesAFill);
    let (rig, _h) = started(Opts { simulator: Some(x.clone()), ..Opts::default() }).await;
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(x.cancels.load(Ordering::SeqCst), 2);
    assert_eq!(status(&rig.db, UUID), "RECONCILED", "both legs turned out filled: {:?}", labels(&rig.db));
    assert_eq!(x.submits.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn a_cancel_whose_result_cannot_be_confirmed_is_unresolved_with_an_alert() {
    let x = TimeoutExchange::new(CancelScript::Unconfirmable);
    let (rig, h) = started(Opts { simulator: Some(x.clone()), ..Opts::default() }).await;
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED", "{:?}", labels(&rig.db));
    let alert = payloads(&rig.db, PAIR_ALERT);
    assert_eq!(alert.len(), 1);
    assert_eq!(alert[0]["reason"], json!("FILL_UNCONFIRMED"));
    sleep(Duration::from_millis(300)).await;
    assert_eq!(h.snapshots.borrow().alerts.len(), 1);
}

// -- 2.1 fill confirmation by order

#[tokio::test(start_paused = true)]
async fn a_failing_lookup_keeps_polling_and_never_counts_as_unfilled() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Unknown { executed: true });
    rig.sim.fail_queries(Some("rate limited (retry after Some(1000) ms)".into()));
    run_until(&rig.clock, T - 6_000).await;
    assert_eq!(status(&rig.db, UUID), "FILL_MONITOR", "an unknown leg is not 'not filled'");
    rig.sim.fail_queries(None);
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED", "{:?}", labels(&rig.db));
    assert_eq!(rig.sim.submitted().len(), 2);
}

// -- 3.1 alerts

#[tokio::test(start_paused = true)]
async fn a_rejected_leg_raises_one_alert_on_all_three_channels_and_keeps_the_filled_leg() {
    let (rig, h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("insufficient margin (scripted)".into()));
    run_until(&rig.clock, T - 8_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    let alerts = payloads(&rig.db, PAIR_ALERT);
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert_eq!(alerts[0]["reason"], json!("SUBMIT_REJECTED"));
    let long = alerts[0]["legs"]["open"].as_array().unwrap().iter().find(|l| l["leg"] == json!("long")).unwrap().clone();
    assert_eq!((long["filled_quantity"].clone(), long["state"].clone()), (json!("10"), json!("Filled")), "{long}");
    let calls = rig.notifier.calls();
    assert_eq!(calls.len(), 1, "notified once");
    assert_eq!((calls[0].state, calls[0].reason), (PairState::PartialFailure, AlertReason::SubmitRejected));
    sleep(Duration::from_millis(300)).await;
    let snap = h.snapshots.borrow().clone();
    assert_eq!(snap.alerts.len(), 1);
    assert_eq!((snap.alerts[0].pair.as_str(), snap.alerts[0].reason.as_deref()), (UUID, Some("SUBMIT_REJECTED")));
    assert_eq!(count(&rig.db, alert::ALERT_NOTIFIED), 1);
}

#[tokio::test(start_paused = true)]
async fn an_hour_in_partial_failure_sends_and_cancels_nothing_and_the_alert_stays() {
    let counting = Counting::new();
    counting.inner.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted".into()));
    let (rig, h) = started(Opts { simulator: Some(counting.clone()), ..Opts::default() }).await;
    run_until(&rig.clock, T - 8_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    let before = counting.counts();
    run_until(&rig.clock, T - 8_000 + 3_600_000).await;
    assert_eq!(counting.counts(), before, "no automatic order or cancel while the alert stands");
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    assert_eq!(h.snapshots.borrow().alerts.len(), 1, "time does not clear the alert");
    assert_eq!(rig.notifier.calls().len(), 1, "and does not notify again");
}

/// Deps for a second process on the same database (in-memory state, including the simulated
/// ledger, is gone).
fn restarted(rig: &Rig, notifier: Arc<RecordingNotifier>) -> EngineDeps {
    let shared: Arc<dyn Clock> = Arc::new(rig.clock.clone());
    let sim = Arc::new(SimulatedExecutor::new(Arc::new(SimPriceBook::default())));
    let sim_account: Arc<dyn AccountView> = Arc::new(sim.account_view(Arc::new(FixedMargin(Ok(dec("10000"))))));
    EngineDeps {
        db: rig.db.clone(),
        clock: shared,
        timings: EngineTimings::default(),
        simulator: sim,
        factory: Arc::new(rig.factory.clone()),
        market: rig.market.clone(),
        offsets: rig.offsets.clone(),
        account: Arc::new(DemoAccount(rig.demo.clone())),
        sim_account,
        reconciler: None,
        notifier,
    }
}

#[tokio::test(start_paused = true)]
async fn after_a_restart_the_alert_is_in_the_first_snapshot_and_is_not_notified_again() {
    let (rig, h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted".into()));
    run_until(&rig.clock, T - 8_000).await;
    assert_eq!(rig.notifier.calls().len(), 1);
    drop(h); // process 1 ends
    sleep(Duration::from_millis(50)).await;

    let second = Arc::new(RecordingNotifier::default());
    let h2 = start(restarted(&rig, second.clone()));
    let first = h2.snapshots.borrow().clone();
    assert_eq!(first.alerts.len(), 1, "the very first snapshot carries the alert");
    assert_eq!(first.alerts[0].state, PairState::PartialFailure);
    run_until(&rig.clock, T + 30_000).await;
    assert!(second.calls().is_empty(), "no second notification for the same entry");
    assert_eq!(count(&rig.db, PAIR_ALERT), 1, "no second alert event either");
}

#[tokio::test(start_paused = true)]
async fn a_failing_notifier_writes_one_event_and_changes_nothing_else() {
    let (rig, h) = started(Opts::default()).await;
    *rig.notifier.fail_with.lock().unwrap() = Some("osascript not available".into());
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted".into()));
    run_until(&rig.clock, T - 8_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    let failed = payloads(&rig.db, alert::ALERT_NOTIFY_FAILED);
    assert_eq!(failed.len(), 1);
    assert!(failed[0]["error"].as_str().unwrap().contains("osascript"));
    assert_eq!(count(&rig.db, PAIR_ALERT), 1);
    sleep(Duration::from_millis(300)).await;
    assert_eq!(h.snapshots.borrow().alerts.len(), 1, "the banner is unaffected");
    run_until(&rig.clock, T + 10_000).await;
    assert_eq!(rig.notifier.calls().len(), 1, "a failed notification is not retried in a loop");
}

#[tokio::test(start_paused = true)]
async fn alert_events_are_counted_by_reason() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted".into()));
    run_until(&rig.clock, T - 8_000).await;
    let counts = alert::count_by_reason(&rig.db, 0, i64::MAX).unwrap();
    assert_eq!(counts["SUBMIT_REJECTED"], 1);
    assert_eq!(counts.values().sum::<i64>(), count(&rig.db, PAIR_ALERT) as i64, "every alert in exactly one class");
}

// -- 3.2 manual exits

#[tokio::test(start_paused = true)]
async fn confirm_closed_is_refused_while_a_leg_still_holds_a_position_and_accepted_once_flat() {
    let (rig, h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted".into()));
    run_until(&rig.clock, T - 8_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");

    // The user says "verified flat", but the long leg still holds 10: refused with the position.
    let r = ask(&h, Command::ConfirmClosed { pair: UUID.into(), verified_flat: true }).await;
    assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("long position 10")), "{r:?}");
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    sleep(Duration::from_millis(300)).await;
    assert_eq!(h.snapshots.borrow().alerts.len(), 1, "alert stays");
    assert_eq!(payloads(&rig.db, MANUAL_CONFIRM_RESULT)[0]["accepted"], json!(false));

    // The user flattens the long leg by hand (reduce-only, same executor), then confirms again.
    user_order(&rig, OrderSide::Sell, "10", true).await;
    let r = ask(&h, Command::ConfirmClosed { pair: UUID.into(), verified_flat: false }).await;
    assert_eq!(r, CommandReply::Accepted, "accepted on the system's re-query, not the user's flag");
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    sleep(Duration::from_millis(300)).await;
    assert!(h.snapshots.borrow().alerts.is_empty(), "the alert disappears with the manual exit");
    let results = payloads(&rig.db, MANUAL_CONFIRM_RESULT);
    assert_eq!(results.last().unwrap()["accepted"], json!(true));
}

// -- 3.3 close

#[tokio::test(start_paused = true)]
async fn a_lagging_position_update_is_confirmed_on_the_second_read() {
    let (rig, _h) = started(Opts { stale_position_reads: 1, ..Opts::default() }).await;
    run_until(&rig.clock, T + 20_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED", "{:?}", labels(&rig.db));
    let closed = ts_of(&rig.db, "FINALIZED")[0];
    assert!(closed > T + 15_000, "the first flat check saw the stale position and was retried");
    assert_eq!(count(&rig.db, "PARTIAL_FAILURE"), 0);
}

#[tokio::test(start_paused = true)]
async fn an_open_order_left_on_the_symbol_blocks_finalized_and_ends_in_partial_failure() {
    let (rig, _h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Long, OrderAction::Close), SimBehavior::Partial(dec("0.5")));
    run_until(&rig.clock, T + 25_000).await;
    assert_ne!(status(&rig.db, UUID), "FINALIZED");
    run_until(&rig.clock, T + 31_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "{:?}", labels(&rig.db));
    assert_eq!(count(&rig.db, "FINALIZED"), 0);
}

/// User decision 2026-10-05 evening (close = min(recorded, actual); a difference beyond
/// `max_leg_imbalance_pct` goes to the user) overrides the pair-close scenario "one leg
/// liquidated -> close only the other leg": a leg at 0 against a recorded 10 is a 100 %
/// difference, so NOTHING is closed and the pair goes to PARTIAL_FAILURE with the mismatch alert.
#[tokio::test(start_paused = true)]
async fn a_liquidated_leg_is_a_quantity_mismatch_and_nothing_is_closed_per_the_user_decision() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 5_000).await;
    user_order(&rig, OrderSide::Sell, "10", true).await; // the long leg is gone (e.g. liquidated)
    run_until(&rig.clock, T + 16_000).await;
    assert!(pair_closes(&rig).is_empty(), "{:?}", pair_closes(&rig));
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    assert_eq!(count(&rig.db, CLOSE_QUANTITY_MISMATCH), 1);
    assert_eq!(payloads(&rig.db, PAIR_ALERT)[0]["reason"], json!("CLOSE_LEG_FAILED"));
}

#[tokio::test(start_paused = true)]
async fn a_rejected_close_leg_is_partial_failure_and_a_manual_close_handles_only_the_remaining_leg() {
    let (rig, h) = started(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Close), SimBehavior::Reject("110090 risk limit (scripted)".into()));
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "{:?}", labels(&rig.db));
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO, "the long leg closed");
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), dec("-10"));
    let sent_before = rig.sim.submitted().len();
    run_until(&rig.clock, T + 40_000).await;
    assert_eq!(rig.sim.submitted().len(), sent_before, "no automatic retry");

    assert_eq!(ask(&h, Command::ManualClose { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T + 42_000).await;
    let new: Vec<OrderRequest> = rig.sim.submitted().into_iter().skip(sent_before).collect();
    assert_eq!(new.len(), 1, "only the remaining (short) leg: {new:?}");
    assert_eq!((new[0].exchange, new[0].side, new[0].quantity, new[0].reduce_only), (Exchange::Bybit, OrderSide::Buy, dec("10"), true));
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
}

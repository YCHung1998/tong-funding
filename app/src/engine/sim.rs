//! `SimulatedExecutor` (design D6, D7; execution-modes spec): the only `Executor` that exists
//! during SIMULATION. It has no client field and this module never refers to the crate's
//! exchange module (a source-scan test below enforces that), so it cannot place an order.
//!
//! Outcomes are scripted per order (by `client_order_id` prefix or by exchange + symbol); the
//! default is a full fill at the injected latest price, with no fees and no slippage (not a claim
//! about reality). A simulated ledger of positions and open orders backs `query`, `cancel` and
//! [`SimAccountView`]. It lives in memory only: after a restart simulated pairs go to UNRESOLVED
//! (decision 7), they are never "recovered" from this ledger.
//!
//! Simulated marker: `is_simulated() == true`, every accepted `client_order_id` carries the `sim`
//! prefix (other ids are rejected), and every simulated exchange order id starts with `sim-`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tong_funding_core::types::{Decimal, Exchange, Price};

use super::ids::IdPrefix;
use super::ports::{
    AccountOrder, AccountPosition, AccountView, BoxFut, Executor, Listed, OrderRequest, OrderSide, OrderState, OrderStatus,
    QueryOutcome, SubmitOutcome,
};

/// Latest price used for simulated fills. `None` = no price -> the order is rejected.
pub trait SimPrices: Send + Sync {
    fn latest(&self, exchange: Exchange, symbol: &str) -> Option<Price>;
}

/// A settable price table (tests, and the actor can feed it from the market `watch`).
#[derive(Default)]
pub struct SimPriceBook(Mutex<HashMap<(Exchange, String), Price>>);

impl SimPriceBook {
    pub fn set(&self, exchange: Exchange, symbol: &str, price: Price) {
        lock(&self.0).insert((exchange, symbol.to_string()), price);
    }
}

impl SimPrices for SimPriceBook {
    fn latest(&self, exchange: Exchange, symbol: &str) -> Option<Price> {
        lock(&self.0).get(&(exchange, symbol.to_string())).copied()
    }
}

/// Where SIMULATION reads available margin from. In production this is the demo account's real
/// balance (decision 6, via [`MarginFromAccount`]); an `Err` must reach the Margin check unchanged.
pub trait MarginSource: Send + Sync {
    fn available_margin(&self, exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>>;
}

/// Margin from a read-only `AccountView` (the demo account); only `available_margin` is used.
pub struct MarginFromAccount(pub Arc<dyn AccountView>);

impl MarginSource for MarginFromAccount {
    fn available_margin(&self, exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        self.0.available_margin(exchange)
    }
}

/// What the simulation does with one submitted order.
#[derive(Debug, Clone, PartialEq)]
pub enum SimBehavior {
    /// Fill completely at the latest price (default).
    Fill,
    /// Fill this fraction (0 < f ≤ 1) at the latest price; the rest stays an open order.
    Partial(Decimal),
    /// Reject with this reason; nothing is recorded, so a later `query` says `NotFound` (a
    /// rejected order never exists on an exchange either).
    Reject(String),
    /// `submit` returns `SubmitOutcome::Unknown`. `executed` = the order was in fact filled (a
    /// later `query` finds it) or never arrived (`query` says `NotFound`).
    Unknown { executed: bool },
    /// `submit` never resolves; `executed` as for `Unknown`.
    Hang { executed: bool },
}

#[derive(Debug, Clone)]
struct SimOrder {
    req: OrderRequest,
    status: OrderStatus,
}

#[derive(Debug, Default)]
struct SimLedger {
    /// Signed: long positive, short negative. Zero entries are removed.
    positions: HashMap<(Exchange, String), Decimal>,
    orders: HashMap<String, SimOrder>,
    next_order_no: u64,
}

impl SimLedger {
    /// Fill `fraction` of `req` at the latest price and record the order. Reduce-only orders are
    /// clipped to the position and rejected if they would open, add to or flip it.
    fn fill(&mut self, req: &OrderRequest, fraction: Decimal, prices: &dyn SimPrices) -> Result<OrderStatus, String> {
        let Some(price) = prices.latest(req.exchange, &req.symbol) else {
            return Err(format!("no simulated price for {} {}", req.exchange.name(), req.symbol));
        };
        let key = (req.exchange, req.symbol.clone());
        let pos = self.positions.get(&key).copied().unwrap_or(Decimal::ZERO);
        let quantity = if req.reduce_only {
            let reduces = match req.side {
                OrderSide::Buy => pos < Decimal::ZERO,
                OrderSide::Sell => pos > Decimal::ZERO,
            };
            if !reduces {
                return Err("reduce-only order would open or increase a position (simulated)".into());
            }
            req.quantity.min(pos.abs())
        } else {
            req.quantity
        };
        if quantity <= Decimal::ZERO {
            return Err("quantity must be positive (simulated)".into());
        }
        let filled = quantity * fraction;
        let signed = match req.side {
            OrderSide::Buy => filled,
            OrderSide::Sell => -filled,
        };
        let new_pos = pos + signed;
        if new_pos.is_zero() {
            self.positions.remove(&key);
        } else {
            self.positions.insert(key, new_pos);
        }
        self.next_order_no += 1;
        let status = OrderStatus {
            client_order_id: req.client_order_id.clone(),
            exchange_order_id: Some(format!("sim-{}", self.next_order_no)),
            filled_quantity: filled,
            avg_price: Some(price),
            state: if filled == quantity { OrderState::Filled } else { OrderState::Open },
        };
        let mut recorded = req.clone();
        recorded.quantity = quantity;
        self.orders.insert(req.client_order_id.clone(), SimOrder { req: recorded, status: status.clone() });
        Ok(status)
    }
}

struct Script {
    default: SimBehavior,
    by_prefix: Vec<(String, SimBehavior)>,
    by_symbol: HashMap<(Exchange, String), SimBehavior>,
    query_failure: Option<String>,
}

pub struct SimulatedExecutor {
    prices: Arc<dyn SimPrices>,
    ledger: Arc<Mutex<SimLedger>>,
    script: Mutex<Script>,
    submitted: Mutex<Vec<OrderRequest>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SimulatedExecutor {
    pub fn new(prices: Arc<dyn SimPrices>) -> Self {
        SimulatedExecutor {
            prices,
            ledger: Arc::default(),
            script: Mutex::new(Script {
                default: SimBehavior::Fill,
                by_prefix: Vec::new(),
                by_symbol: HashMap::new(),
                query_failure: None,
            }),
            submitted: Mutex::default(),
        }
    }

    /// Orders whose `client_order_id` starts with `prefix` get `behavior`. Prefix rules win over
    /// symbol rules; among prefixes the longest match wins. Re-scripting a prefix replaces it.
    pub fn script_prefix(&self, prefix: &str, behavior: SimBehavior) {
        let mut s = lock(&self.script);
        s.by_prefix.retain(|(p, _)| p != prefix);
        s.by_prefix.push((prefix.to_string(), behavior));
    }

    pub fn script_symbol(&self, exchange: Exchange, symbol: &str, behavior: SimBehavior) {
        lock(&self.script).by_symbol.insert((exchange, symbol.to_string()), behavior);
    }

    pub fn set_default(&self, behavior: SimBehavior) {
        lock(&self.script).default = behavior;
    }

    /// `Some(reason)`: every `query` / `cancel` fails (exchange unreachable); `None` restores.
    pub fn fail_queries(&self, reason: Option<String>) {
        lock(&self.script).query_failure = reason;
    }

    /// Simulated signed position.
    pub fn position(&self, exchange: Exchange, symbol: &str) -> Decimal {
        lock(&self.ledger).positions.get(&(exchange, symbol.to_string())).copied().unwrap_or(Decimal::ZERO)
    }

    fn behavior_for(&self, req: &OrderRequest) -> SimBehavior {
        let s = lock(&self.script);
        let by_prefix = s
            .by_prefix
            .iter()
            .filter(|(p, _)| req.client_order_id.starts_with(p.as_str()))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, b)| b.clone());
        by_prefix
            .or_else(|| s.by_symbol.get(&(req.exchange, req.symbol.clone())).cloned())
            .unwrap_or_else(|| s.default.clone())
    }

    /// Find an order by id on (exchange, symbol), like an exchange lookup, and let `f` change it.
    fn lookup(&self, exchange: Exchange, symbol: &str, client_order_id: &str, f: impl FnOnce(&mut SimOrder)) -> QueryOutcome {
        if let Some(reason) = lock(&self.script).query_failure.clone() {
            return QueryOutcome::Failed { reason };
        }
        let mut ledger = lock(&self.ledger);
        match ledger.orders.get_mut(client_order_id) {
            Some(order) if order.req.exchange == exchange && order.req.symbol == symbol => {
                f(order);
                QueryOutcome::Found(order.status.clone())
            }
            _ => QueryOutcome::NotFound,
        }
    }

    /// Decide the outcome and apply it to the ledger, synchronously (nothing here waits).
    /// Returns the outcome and whether `submit` must hang instead of returning it.
    fn execute(&self, req: OrderRequest) -> (SubmitOutcome, bool) {
        lock(&self.submitted).push(req.clone());
        let reject = |reason: &str| (SubmitOutcome::Rejected { reason: reason.to_string() }, false);
        if IdPrefix::of(&req.client_order_id) != Some(IdPrefix::Sim) {
            return reject("simulated executor only accepts sim-prefixed client_order_ids");
        }
        let behavior = self.behavior_for(&req);
        let mut ledger = lock(&self.ledger);
        if ledger.orders.contains_key(&req.client_order_id) {
            return reject("duplicate client_order_id (simulated)");
        }
        let (fraction, unknown, hang) = match behavior {
            SimBehavior::Fill => (Some(Decimal::ONE), false, false),
            SimBehavior::Partial(f) => {
                if f <= Decimal::ZERO || f > Decimal::ONE {
                    return reject("invalid simulated fill fraction");
                }
                (Some(f), false, false)
            }
            SimBehavior::Reject(reason) => return reject(&reason),
            SimBehavior::Unknown { executed } => (executed.then_some(Decimal::ONE), true, false),
            SimBehavior::Hang { executed } => (executed.then_some(Decimal::ONE), true, true),
        };
        let unknown_outcome = || SubmitOutcome::Unknown { reason: "simulated unknown result".into() };
        let Some(fraction) = fraction else {
            // Never reached the (simulated) exchange: nothing to record, a query finds nothing.
            return (unknown_outcome(), hang);
        };
        // With an unknown outcome the caller learns what happened only through `query`
        // (a fill is recorded; a rejection, e.g. no price, leaves nothing to find).
        match (ledger.fill(&req, fraction, self.prices.as_ref()), unknown) {
            (_, true) => (unknown_outcome(), hang),
            (Ok(status), false) => (SubmitOutcome::Accepted(status), false),
            (Err(reason), false) => reject(&reason),
        }
    }

    /// Every request `submit` received, in order (including rejected ones).
    pub fn submitted(&self) -> Vec<OrderRequest> {
        lock(&self.submitted).clone()
    }

    /// A read-only view over this executor's ledger; margin comes from `margin`.
    pub fn account_view(&self, margin: Arc<dyn MarginSource>) -> SimAccountView {
        SimAccountView { ledger: self.ledger.clone(), margin }
    }
}

impl Executor for SimulatedExecutor {
    fn is_simulated(&self) -> bool {
        true
    }

    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        let (outcome, hang) = self.execute(req);
        if hang {
            Box::pin(std::future::pending())
        } else {
            Box::pin(std::future::ready(outcome))
        }
    }

    fn cancel(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome> {
        let outcome = self.lookup(exchange, symbol, client_order_id, |order| {
            if order.status.state == OrderState::Open {
                order.status.state = OrderState::Cancelled;
            }
        });
        Box::pin(std::future::ready(outcome))
    }

    fn query(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome> {
        let outcome = self.lookup(exchange, symbol, client_order_id, |_| {});
        Box::pin(std::future::ready(outcome))
    }
}

/// `AccountView` for SIMULATION: positions and open orders from the simulated ledger,
/// `available_margin` from the injected source (decision 6), errors passed through unchanged.
pub struct SimAccountView {
    ledger: Arc<Mutex<SimLedger>>,
    margin: Arc<dyn MarginSource>,
}

impl AccountView for SimAccountView {
    fn positions(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
        let ledger = lock(&self.ledger);
        let mut items: Vec<AccountPosition> = ledger
            .positions
            .iter()
            .filter(|((ex, _), q)| *ex == exchange && !q.is_zero())
            .map(|((ex, symbol), q)| AccountPosition { exchange: *ex, symbol: symbol.clone(), quantity: *q })
            .collect();
        items.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        Box::pin(std::future::ready(Ok(Listed { items, complete: true })))
    }

    fn open_orders(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
        let ledger = lock(&self.ledger);
        let mut items: Vec<AccountOrder> = ledger
            .orders
            .values()
            .filter(|o| o.req.exchange == exchange && o.status.state == OrderState::Open)
            .map(|o| AccountOrder {
                exchange: o.req.exchange,
                symbol: o.req.symbol.clone(),
                client_order_id: Some(o.req.client_order_id.clone()),
                remaining_quantity: o.req.quantity - o.status.filled_quantity,
            })
            .collect();
        items.sort_by(|a, b| a.client_order_id.cmp(&b.client_order_id));
        Box::pin(std::future::ready(Ok(Listed { items, complete: true })))
    }

    fn available_margin(&self, exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
        self.margin.available_margin(exchange)
    }
}

/// Test helper: an `ExecutorFactory` that counts `create` calls, to prove the order-capable
/// factory is called 0 times during SIMULATION (design D6). Clones share the counter.
#[cfg(test)]
#[derive(Clone)]
pub struct CountingFactory {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    result: Option<Arc<dyn Executor>>,
}

#[cfg(test)]
impl CountingFactory {
    /// Every `create` fails (e.g. keys unavailable).
    pub fn failing() -> Self {
        CountingFactory { calls: Arc::default(), result: None }
    }

    /// Every `create` hands out `executor`.
    pub fn returning(executor: Arc<dyn Executor>) -> Self {
        CountingFactory { calls: Arc::default(), result: Some(executor) }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
impl super::ports::ExecutorFactory for CountingFactory {
    fn create(&self, _mode: tong_funding_core::risk::ExecutionMode) -> Result<Arc<dyn Executor>, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.result.clone().ok_or_else(|| "counting factory: no executor (keys unavailable)".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::client_order_id;
    use crate::engine::ports::{ExecutorFactory, Leg, OrderAction};
    use futures_util::FutureExt;
    use tong_funding_core::risk::ExecutionMode;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn sim() -> (SimulatedExecutor, Arc<SimPriceBook>) {
        let prices = Arc::new(SimPriceBook::default());
        prices.set(Exchange::Binance, "BTCUSDT", d("60000"));
        prices.set(Exchange::Bybit, "BTCUSDT", d("60010"));
        (SimulatedExecutor::new(prices.clone()), prices)
    }

    fn id(leg: Leg, action: OrderAction, seq: u16) -> String {
        client_order_id(IdPrefix::Sim, "pair-1", leg, action, seq)
    }

    fn req(id: &str, exchange: Exchange, side: OrderSide, qty: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: id.to_string(),
            exchange,
            symbol: "BTCUSDT".into(),
            side,
            quantity: d(qty),
            reduce_only: false,
        }
    }

    /// Resolve a future that must already be ready (the simulation never waits on anything).
    fn ready<T>(f: BoxFut<'_, T>) -> T {
        f.now_or_never().expect("simulated call must resolve immediately")
    }

    fn accepted(o: SubmitOutcome) -> OrderStatus {
        match o {
            SubmitOutcome::Accepted(s) => s,
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    struct FixedMargin(Result<Decimal, String>);
    impl MarginSource for FixedMargin {
        fn available_margin(&self, _exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
            let r = self.0.clone();
            Box::pin(async move { r })
        }
    }

    #[test]
    fn default_is_a_full_fill_at_the_injected_price_and_is_marked_simulated() {
        let (ex, _) = sim();
        assert!(ex.is_simulated());
        let cid = id(Leg::Long, OrderAction::Open, 0);
        let st = accepted(ready(ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "0.019"))));
        assert_eq!(st.state, OrderState::Filled);
        assert_eq!(st.filled_quantity, d("0.019"));
        assert_eq!(st.avg_price, Some(d("60000")));
        assert_eq!(st.client_order_id, cid);
        assert!(st.exchange_order_id.as_deref().is_some_and(|e| e.starts_with("sim-")), "{st:?}");
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), d("0.019"));
        assert_eq!(ready(ex.query(Exchange::Binance, "BTCUSDT", &cid)), QueryOutcome::Found(st));
    }

    #[test]
    fn a_sell_opens_a_negative_position_and_closing_flattens_it() {
        let (ex, _) = sim();
        let open = id(Leg::Short, OrderAction::Open, 0);
        accepted(ready(ex.submit(req(&open, Exchange::Bybit, OrderSide::Sell, "0.5"))));
        assert_eq!(ex.position(Exchange::Bybit, "BTCUSDT"), d("-0.5"));
        let close = id(Leg::Short, OrderAction::Close, 0);
        let mut r = req(&close, Exchange::Bybit, OrderSide::Buy, "0.5");
        r.reduce_only = true;
        accepted(ready(ex.submit(r)));
        assert_eq!(ex.position(Exchange::Bybit, "BTCUSDT"), Decimal::ZERO);
    }

    #[test]
    fn partial_fill_leaves_an_open_order_that_cancel_closes() {
        let (ex, _) = sim();
        ex.script_symbol(Exchange::Binance, "BTCUSDT", SimBehavior::Partial(d("0.7")));
        let cid = id(Leg::Long, OrderAction::Open, 0);
        let st = accepted(ready(ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "1.0"))));
        assert_eq!((st.state, st.filled_quantity), (OrderState::Open, d("0.7")));
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), d("0.7"));
        let view = ex.account_view(Arc::new(FixedMargin(Ok(d("100")))));
        let oo = ready(view.open_orders(Exchange::Binance)).unwrap();
        assert!(oo.complete);
        assert_eq!(oo.items.len(), 1);
        assert_eq!(oo.items[0].client_order_id.as_deref(), Some(cid.as_str()));
        assert_eq!(oo.items[0].remaining_quantity, d("0.3"));
        match ready(ex.cancel(Exchange::Binance, "BTCUSDT", &cid)) {
            QueryOutcome::Found(s) => assert_eq!((s.state, s.filled_quantity), (OrderState::Cancelled, d("0.7"))),
            other => panic!("{other:?}"),
        }
        assert!(ready(view.open_orders(Exchange::Binance)).unwrap().items.is_empty());
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), d("0.7"), "cancel keeps the filled part");
        assert_eq!(ready(ex.cancel(Exchange::Binance, "BTCUSDT", "simlo0000nope")), QueryOutcome::NotFound);
    }

    #[test]
    fn invalid_partial_fraction_is_rejected_not_filled() {
        let (ex, _) = sim();
        for f in ["0", "-0.5", "1.5"] {
            ex.set_default(SimBehavior::Partial(d(f)));
            let cid = client_order_id(IdPrefix::Sim, f, Leg::Long, OrderAction::Open, 0);
            assert!(matches!(ready(ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "1"))), SubmitOutcome::Rejected { .. }), "{f}");
        }
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), Decimal::ZERO);
    }

    #[test]
    fn scripted_reject_by_prefix_on_one_leg_while_the_other_fills() {
        let (ex, _) = sim();
        let short = id(Leg::Short, OrderAction::Open, 0);
        let long = id(Leg::Long, OrderAction::Open, 0);
        ex.script_prefix(&short, SimBehavior::Reject("insufficient margin (simulated)".into()));
        let r = ready(ex.submit(req(&short, Exchange::Bybit, OrderSide::Sell, "0.1")));
        assert_eq!(r, SubmitOutcome::Rejected { reason: "insufficient margin (simulated)".into() });
        let st = accepted(ready(ex.submit(req(&long, Exchange::Binance, OrderSide::Buy, "0.1"))));
        assert_eq!(st.state, OrderState::Filled);
        assert_eq!(ex.position(Exchange::Bybit, "BTCUSDT"), Decimal::ZERO);
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), d("0.1"));
    }

    #[test]
    fn prefix_rules_beat_symbol_rules_and_the_longest_prefix_wins() {
        let (ex, _) = sim();
        ex.script_symbol(Exchange::Binance, "BTCUSDT", SimBehavior::Reject("symbol".into()));
        ex.script_prefix("sim", SimBehavior::Reject("short prefix".into()));
        ex.script_prefix("simlo", SimBehavior::Fill);
        let st = accepted(ready(ex.submit(req(&id(Leg::Long, OrderAction::Open, 0), Exchange::Binance, OrderSide::Buy, "1"))));
        assert_eq!(st.state, OrderState::Filled);
        let r = ready(ex.submit(req(&id(Leg::Short, OrderAction::Open, 0), Exchange::Binance, OrderSide::Sell, "1")));
        assert_eq!(r, SubmitOutcome::Rejected { reason: "short prefix".into() });
    }

    #[test]
    fn unknown_result_then_query_by_the_same_id_tells_what_happened() {
        let (ex, _) = sim();
        let filled = id(Leg::Long, OrderAction::Open, 0);
        let lost = id(Leg::Short, OrderAction::Open, 0);
        ex.script_prefix(&filled, SimBehavior::Unknown { executed: true });
        ex.script_prefix(&lost, SimBehavior::Unknown { executed: false });
        assert!(matches!(ready(ex.submit(req(&filled, Exchange::Binance, OrderSide::Buy, "1"))), SubmitOutcome::Unknown { .. }));
        assert!(matches!(ready(ex.submit(req(&lost, Exchange::Bybit, OrderSide::Sell, "1"))), SubmitOutcome::Unknown { .. }));
        match ready(ex.query(Exchange::Binance, "BTCUSDT", &filled)) {
            QueryOutcome::Found(s) => assert_eq!((s.state, s.filled_quantity), (OrderState::Filled, d("1"))),
            other => panic!("{other:?}"),
        }
        assert_eq!(ready(ex.query(Exchange::Bybit, "BTCUSDT", &lost)), QueryOutcome::NotFound);
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), d("1"));
        assert_eq!(ex.position(Exchange::Bybit, "BTCUSDT"), Decimal::ZERO);
    }

    #[test]
    fn hang_never_resolves_but_the_ledger_reflects_executed() {
        let (ex, _) = sim();
        let cid = id(Leg::Long, OrderAction::Open, 0);
        ex.set_default(SimBehavior::Hang { executed: true });
        let mut fut = ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "2"));
        for _ in 0..3 {
            assert!((&mut fut).now_or_never().is_none(), "a hanging submit must stay pending");
        }
        assert!(matches!(ready(ex.query(Exchange::Binance, "BTCUSDT", &cid)), QueryOutcome::Found(_)));
        let other = id(Leg::Long, OrderAction::Open, 1);
        ex.set_default(SimBehavior::Hang { executed: false });
        assert!(ex.submit(req(&other, Exchange::Binance, OrderSide::Buy, "2")).now_or_never().is_none());
        assert_eq!(ready(ex.query(Exchange::Binance, "BTCUSDT", &other)), QueryOutcome::NotFound);
    }

    #[test]
    fn no_price_means_rejected_and_no_position() {
        let (ex, _) = sim();
        let mut r = req(&id(Leg::Long, OrderAction::Open, 0), Exchange::Okx, OrderSide::Buy, "1");
        r.symbol = "ETHUSDT".into();
        assert!(matches!(ready(ex.submit(r)), SubmitOutcome::Rejected { .. }));
        assert_eq!(ex.position(Exchange::Okx, "ETHUSDT"), Decimal::ZERO);
    }

    #[test]
    fn the_latest_price_is_read_at_submit_time() {
        let (ex, prices) = sim();
        prices.set(Exchange::Binance, "BTCUSDT", d("61000.5"));
        let st = accepted(ready(ex.submit(req(&id(Leg::Long, OrderAction::Open, 0), Exchange::Binance, OrderSide::Buy, "1"))));
        assert_eq!(st.avg_price, Some(d("61000.5")));
    }

    #[test]
    fn non_sim_ids_and_duplicate_ids_are_rejected_without_touching_the_ledger() {
        let (ex, _) = sim();
        let demo = client_order_id(IdPrefix::Demo, "pair-1", Leg::Long, OrderAction::Open, 0);
        for bad in [demo.as_str(), "", "my-order", "SIMlo0000"] {
            let r = ready(ex.submit(req(bad, Exchange::Binance, OrderSide::Buy, "1")));
            assert!(matches!(r, SubmitOutcome::Rejected { .. }), "{bad:?}: {r:?}");
        }
        let cid = id(Leg::Long, OrderAction::Open, 0);
        let first = accepted(ready(ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "1"))));
        let again = ready(ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "5")));
        assert!(matches!(again, SubmitOutcome::Rejected { .. }), "{again:?}");
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), d("1"));
        assert_eq!(ready(ex.query(Exchange::Binance, "BTCUSDT", &cid)), QueryOutcome::Found(first));
        assert_eq!(ex.submitted().len(), 6, "every call is logged, rejected ones included");
    }

    #[test]
    fn reduce_only_never_opens_or_flips_a_position() {
        let (ex, _) = sim();
        let mut r = req(&id(Leg::Long, OrderAction::Close, 0), Exchange::Binance, OrderSide::Sell, "1");
        r.reduce_only = true;
        assert!(matches!(ready(ex.submit(r)), SubmitOutcome::Rejected { .. }), "flat: nothing to reduce");
        accepted(ready(ex.submit(req(&id(Leg::Long, OrderAction::Open, 0), Exchange::Binance, OrderSide::Buy, "0.4"))));
        let mut add = req(&id(Leg::Long, OrderAction::Close, 1), Exchange::Binance, OrderSide::Buy, "1");
        add.reduce_only = true;
        assert!(matches!(ready(ex.submit(add)), SubmitOutcome::Rejected { .. }), "same direction would add");
        let mut over = req(&id(Leg::Long, OrderAction::Close, 2), Exchange::Binance, OrderSide::Sell, "1");
        over.reduce_only = true;
        let st = accepted(ready(ex.submit(over)));
        assert_eq!((st.state, st.filled_quantity), (OrderState::Filled, d("0.4")), "clipped to the position");
        assert_eq!(ex.position(Exchange::Binance, "BTCUSDT"), Decimal::ZERO);
    }

    #[test]
    fn query_failure_is_reported_not_hidden() {
        let (ex, _) = sim();
        let cid = id(Leg::Long, OrderAction::Open, 0);
        accepted(ready(ex.submit(req(&cid, Exchange::Binance, OrderSide::Buy, "1"))));
        ex.fail_queries(Some("unreachable".into()));
        assert_eq!(ready(ex.query(Exchange::Binance, "BTCUSDT", &cid)), QueryOutcome::Failed { reason: "unreachable".into() });
        assert_eq!(ready(ex.cancel(Exchange::Binance, "BTCUSDT", &cid)), QueryOutcome::Failed { reason: "unreachable".into() });
        ex.fail_queries(None);
        assert!(matches!(ready(ex.query(Exchange::Binance, "BTCUSDT", &cid)), QueryOutcome::Found(_)));
    }

    #[test]
    fn account_view_reads_the_ledger_and_delegates_margin() {
        let (ex, _) = sim();
        accepted(ready(ex.submit(req(&id(Leg::Long, OrderAction::Open, 0), Exchange::Binance, OrderSide::Buy, "0.3"))));
        accepted(ready(ex.submit(req(&id(Leg::Short, OrderAction::Open, 0), Exchange::Bybit, OrderSide::Sell, "0.3"))));
        let view = ex.account_view(Arc::new(FixedMargin(Ok(d("1234.5")))));
        let bin = ready(view.positions(Exchange::Binance)).unwrap();
        assert!(bin.complete);
        assert_eq!(bin.items, vec![AccountPosition { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), quantity: d("0.3") }]);
        let byb = ready(view.positions(Exchange::Bybit)).unwrap();
        assert_eq!(byb.items, vec![AccountPosition { exchange: Exchange::Bybit, symbol: "BTCUSDT".into(), quantity: d("-0.3") }]);
        assert!(ready(view.positions(Exchange::Okx)).unwrap().items.is_empty());
        assert!(ready(view.open_orders(Exchange::Binance)).unwrap().items.is_empty());
        assert_eq!(ready(view.available_margin(Exchange::Bybit)), Ok(d("1234.5")));
    }

    #[test]
    fn margin_error_propagates_and_is_never_replaced_by_a_default() {
        let (ex, _) = sim();
        let view = ex.account_view(Arc::new(FixedMargin(Err("demo balance unavailable".into()))));
        assert_eq!(ready(view.available_margin(Exchange::Binance)), Err("demo balance unavailable".into()));
    }

    #[test]
    fn margin_from_account_uses_only_available_margin_of_the_wrapped_view() {
        let (ex, _) = sim();
        let inner: Arc<dyn AccountView> = Arc::new(ex.account_view(Arc::new(FixedMargin(Ok(d("7"))))));
        let src = MarginFromAccount(inner);
        assert_eq!(ready(src.available_margin(Exchange::Okx)), Ok(d("7")));
    }

    #[test]
    fn counting_factory_counts_every_call() {
        let f = CountingFactory::failing();
        let g = f.clone();
        assert_eq!(f.calls(), 0);
        assert!(g.create(ExecutionMode::ExchangeDemo).is_err());
        assert_eq!(f.calls(), 1, "clones share the counter");
        let (ex, _) = sim();
        let h = CountingFactory::returning(Arc::new(ex));
        assert!(h.create(ExecutionMode::ExchangeDemo).unwrap().is_simulated());
        assert_eq!(h.calls(), 1);
    }

    /// Structural guarantee (design D6, execution-modes spec "依賴關係證明"): this module never
    /// refers to the crate's exchange module. The needles are split with `concat!` so this test
    /// does not match itself.
    #[test]
    fn sim_module_does_not_reference_the_exchange_module() {
        let src = include_str!("sim.rs");
        for needle in [concat!("crate", "::", "exchange"), concat!("exchange", "::"), concat!("super::super", "::exchange")] {
            assert!(!src.contains(needle), "sim.rs must not reference {needle:?}");
        }
        // Grouped imports such as `use crate::{x, exchange}`: no `use` item may name it at all.
        let word = concat!("exch", "ange");
        let mut offset = 0;
        let mut checked = 0;
        for line in src.split_inclusive('\n') {
            let t = line.trim_start();
            if t.starts_with("use ") || t.starts_with("pub use ") || t.starts_with("pub(crate) use ") {
                let start = offset + (line.len() - t.len());
                let end = src[start..].find(';').map_or(src.len(), |e| start + e);
                let stmt = &src[start..end];
                let names_it = stmt.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|w| w == word);
                assert!(!names_it, "sim.rs imports {word}: {stmt}");
                checked += 1;
            }
            offset += line.len();
        }
        assert!(checked >= 3, "the scan must actually see the imports ({checked})");
    }
}

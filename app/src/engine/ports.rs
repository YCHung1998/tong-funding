//! What the engine needs from the outside world, as traits it owns (design D8). Real
//! implementations adapt `exchange` (read-only GETs) and, later, `exchange-demo-execution`
//! (orders); tests use fakes. Nothing here can place an order except an `Executor`, and the only
//! `Executor` that exists during SIMULATION is `sim::SimulatedExecutor` (design D6).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tong_funding_core::funding::FundingObservation;
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::{Decimal, Exchange, Price, Side};

/// A boxed, sendable future: the engine stores executors as `Arc<dyn Executor>`.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Which leg of a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Leg {
    Long,
    Short,
}

impl Leg {
    pub const BOTH: [Leg; 2] = [Leg::Long, Leg::Short];
    pub fn as_str(self) -> &'static str {
        match self {
            Leg::Long => "long",
            Leg::Short => "short",
        }
    }
}

/// Opening or closing order of a leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderAction {
    Open,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    /// Side of the order that opens (`Open`) or closes (`Close`) a position on `side`.
    pub fn for_leg(side: Side, action: OrderAction) -> OrderSide {
        match (side, action) {
            (Side::Long, OrderAction::Open) | (Side::Short, OrderAction::Close) => OrderSide::Buy,
            (Side::Short, OrderAction::Open) | (Side::Long, OrderAction::Close) => OrderSide::Sell,
        }
    }
}

/// One market order. `quantity` is in the exchange's ORDER UNIT, already floored by
/// `core::quantity`: base coin on Binance / Bybit, CONTRACTS on OKX (one contract is `ct_val` of
/// the base coin). The engine converts to base coin (`fill::SizedLeg::base_qty`, `fill::LegFill`)
/// only to compare legs across exchanges; it never sends a base-coin amount to OKX.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderRequest {
    pub client_order_id: String,
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: Decimal,
    pub reduce_only: bool,
}

/// What the exchange (or the simulation) says about one order.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderStatus {
    pub client_order_id: String,
    pub exchange_order_id: Option<String>,
    /// Same unit as `OrderRequest::quantity` (contracts on OKX).
    pub filled_quantity: Decimal,
    pub avg_price: Option<Price>,
    pub state: OrderState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderState {
    /// Accepted, not (fully) filled yet.
    Open,
    Filled,
    /// Cancelled or expired; `filled_quantity` may be partial.
    Cancelled,
    Rejected,
}

/// Result of a submit call. `Unknown` (timeout, disconnect, unparsable reply) must never be
/// treated as a failure or retried under a new id (crash-recovery spec).
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    Accepted(OrderStatus),
    Rejected { reason: String },
    Unknown { reason: String },
}

/// Result of looking an order up by `client_order_id`.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryOutcome {
    Found(OrderStatus),
    NotFound,
    Failed { reason: String },
}

/// Order lifecycle only: submit, cancel, look up by `client_order_id` (design D8).
pub trait Executor: Send + Sync {
    /// `true` only for `sim::SimulatedExecutor`; events from it are tagged as simulated.
    fn is_simulated(&self) -> bool;
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome>;
    fn cancel(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome>;
    fn query(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome>;
}

/// Builds the order-capable executor when switching to EXCHANGE_DEMO; never called during
/// SIMULATION (design D6). Fails (e.g. keys unavailable) -> stay in SIMULATION.
pub trait ExecutorFactory: Send + Sync {
    fn create(&self, mode: ExecutionMode) -> Result<Arc<dyn Executor>, String>;
}

/// A position on one symbol: signed quantity, long positive / short negative, in the exchange's
/// order unit (contracts on OKX), like `OrderRequest::quantity`.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountPosition {
    pub exchange: Exchange,
    pub symbol: String,
    pub quantity: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountOrder {
    pub exchange: Exchange,
    pub symbol: String,
    pub client_order_id: Option<String>,
    pub remaining_quantity: Decimal,
}

/// A list that says whether it is complete; an incomplete list must not be read as "nothing".
#[derive(Debug, Clone, PartialEq)]
pub struct Listed<T> {
    pub items: Vec<T>,
    pub complete: bool,
}

/// Read-only account state (design D8). During SIMULATION positions and orders come from the
/// simulated ledger; `available_margin` always comes from the demo account (decision 6) and
/// `Err` means "not available" -> the Margin check fails (fail closed).
pub trait AccountView: Send + Sync {
    fn positions(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>>;
    fn open_orders(&self, exchange: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>>;
    fn available_margin(&self, exchange: Exchange) -> BoxFut<'_, Result<Decimal, String>>;
}

/// A freshly fetched (never cached) market snapshot of one leg.
#[derive(Debug, Clone, PartialEq)]
pub struct FreshQuote {
    pub funding: FundingObservation,
    pub price: Price,
    /// Local receive time of the price (ms).
    pub price_observed_at_ms: i64,
    pub listed: bool,
}

/// Order-size rules of one instrument, in the exchange's order unit (contracts on OKX).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderRules {
    /// Market-order lot filter.
    pub lot: LotSize,
    /// OKX contract value (base coin per contract); `None` elsewhere. Required on OKX.
    pub okx_ct_val: Option<Decimal>,
}

/// Single-symbol re-fetch for the baseline and pre-trade prices (adapters `refetch`).
pub trait MarketData: Send + Sync {
    fn refetch(&self, exchange: Exchange, symbol: &str) -> BoxFut<'_, Result<FreshQuote, String>>;
    /// Lot filter and OKX contract value for Node 1 sizing (adapters' instrument rules).
    /// `Err` = unknown: the submission is aborted before anything is sent.
    fn order_rules(&self, exchange: Exchange, symbol: &str) -> BoxFut<'_, Result<OrderRules, String>>;
}

/// `serverTime` offsets (exchange minus local, ms). `None` = not calibrated -> no entry (fail closed).
pub trait ServerOffsets: Send + Sync {
    fn offset_ms(&self, exchange: Exchange) -> Option<i64>;
}

/// Startup reconciliation hook (crash-recovery spec; implemented by `engine::recovery`). When the
/// store holds unfinished order intents, or pairs that were in flight at shutdown
/// (PRE_TRADE_CHECK, ORDER_SUBMIT, FILL_MONITOR, CLOSING), the actor sets
/// `reconciliation_pending` before it handles anything and calls [`StartupReconciler::reconcile`]
/// once from a spawned task. Until that returns `Ok`, every `opens_exposure()` command is refused
/// and the scheduler leaves the in-flight pairs alone (it has no fills for them in memory).
///
/// Contract: read-only towards exchanges (query by `client_order_id`, positions, open orders; never
/// submit, cancel or close); every pair change is landed with `transition::land_then_act` on
/// `ctx.db`. `Ok` = done: the actor reloads the pairs from the store and lifts the block.
/// `Err(reason)` = not finished (exchange unreachable, keys unavailable, ...): the block stays and
/// `reason` is shown (`Blocker::ReconciliationPending`).
pub trait StartupReconciler: Send + Sync {
    fn reconcile(&self, ctx: ReconcileContext) -> BoxFut<'static, Result<(), String>>;
}

/// What the actor hands to the reconciler: the store, and the executor and account view of the
/// current `execution_mode` (the simulator and the simulated ledger in SIMULATION).
#[derive(Clone)]
pub struct ReconcileContext {
    pub db: crate::store::db::Db,
    pub executor: Arc<dyn Executor>,
    pub account: Arc<dyn AccountView>,
    pub execution_mode: ExecutionMode,
}

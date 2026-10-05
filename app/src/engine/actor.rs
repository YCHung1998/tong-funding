//! The engine actor (design D1–D3; tasks 1.2, 2.1–2.4, 3.2). One tokio task owns every piece of
//! mutable trading state. Inputs: `Command`s (bounded mpsc, each may carry a oneshot for the
//! reply), internal `Event`s from spawned I/O tasks, market prices on a `watch` (latest value only)
//! and the scheduler tick. Output: rate-limited `Snapshot`s on a `watch`. The actor never awaits
//! exchange I/O; it spawns it and handles the result when the `Event` comes back.
//!
//! Every command, the scheduler's own `Tick` and whatever the scheduler triggers go through
//! [`Actor::dispatch`], the one place where the gate (`opens_exposure` vs kill switch / halted
//! store / pending reconciliation) is applied.
//!
//! Flow of one pair (every state change is landed in the store before its action runs):
//! - tick: `schedule::decide` with the injected clock and `ServerOffsets` → baseline re-fetch
//!   (T−15 by default), `EntryTrigger` (AUTO only, through the gate), one `ENTRY_BLOCKED` event per
//!   cause, `ENTRY_WINDOW_MISSED` warning then core `Cancel`, `AutoExit` at T+15 (not gated: it
//!   only reduces exposure). PREPARED auto-cancel (AUTO only, runs while halted) on the baseline
//!   data and on a slow periodic re-fetch before the baseline time.
//! - entry: PREPARED →(StartCheck) PRE_TRADE_CHECK → spawned pre-trade re-fetch, margins, order
//!   rules and foreign-exposure read → Node 0 with `effective_for_pair` → BLOCKED or
//!   →(CheckPassed) ORDER_SUBMIT → Node 1 sizing → one intent-first submit per leg on the executor
//!   of the current mode → FILL_MONITOR / PARTIAL_FAILURE / CANCELLED → fills polled by
//!   `Executor::query` each tick → `fill::fill_decision` with the EFFECTIVE `order_timeout_seconds`
//!   → RECONCILED / IMBALANCED / CANCELLED / PARTIAL_FAILURE / UNRESOLVED. At the timeout the
//!   pair's own unfinished orders are cancelled and looked up again first (the final fill
//!   decides). Never an extra order. Every submit writes `ORDER_LATENCY` (both legs in parallel).
//! - exit: RECONCILED (or a locked state, by hand) → CLOSING → positions read → one reduce-only
//!   close per non-flat leg (intent first) → flat check (positions 0, no open orders; retried
//!   until the close timeout) → `CLOSE_CONFIRMED` event → FINALIZED; a rejected close or a
//!   non-flat result at the timeout → PARTIAL_FAILURE.
//! - alerts: entering PARTIAL_FAILURE / IMBALANCED / UNRESOLVED writes one `PAIR_ALERT` and
//!   notifies once (`alert`); the banner (`Snapshot::alerts`) is derived from the pair states.
//!   "Confirm closed" is accepted only after a re-query shows both legs flat.
//!
//! A panic inside the actor ends the task: every later `send` gets `Rejected("engine stopped")`
//! and the snapshot stops updating. There is no automatic restart (design Risks; unverified).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, sleep_until};
use tong_funding_core::pair::{Event as PairEvent, ManualEvent, PairState, SystemEvent};
use tong_funding_core::risk::{
    EffectiveConfig, ExecutionMode, RiskConfig, RiskOverrides, TriggerMode, effective_for_pair, parse_overrides,
};
use tong_funding_core::types::{Decimal, Exchange, Side};

use super::alert::{self, AlertNotice, AlertReason, Notifier, PAIR_ALERT};
use super::command::{
    Alert, Blocker, CONTRACT_SETTINGS_UPDATED, Command, CommandReply, Event, FlatReport, ManualOrder, NewPreparedPair, Notice, PairUuid,
    PairView, RISK_CONFIG_UPDATED, Snapshot,
};
use super::latency::{self, ORDER_LATENCY};
use super::fill::{self, AutoCancel, FillDecision, LegFill, LegSizing, PreparedRecheck, SubmitPlan};
use super::gate::{self, ModeSwitch};
use super::ids::{self, IdPrefix, client_order_id};
use super::intent::{self, IntentError};
use super::node0::{self, EntrySnapshot, Node0Context, Node0Leg, Node0Verdict};
use super::ports::{
    AccountView, Executor, ExecutorFactory, FreshQuote, Leg, MarketData, OrderAction, OrderRequest, OrderRules, OrderSide,
    OrderState, OrderStatus, QueryOutcome, ReconcileContext, ServerOffsets, StartupReconciler, SubmitOutcome,
};
use super::schedule::{self, ClockReading, PairTiming, ScheduleAction};
pub use super::schedule::ENTRY_WINDOW_MISSED;
use super::timings::{EngineTimings, MissedWindowPolicy};
use super::transition::{self, TransitionError};
use crate::ports::Clock;
use crate::store::db::{Db, HaltReason};
use crate::store::events::EventStore;
use crate::store::state::{AddPairOutcome, ConfigChange, FLAG_EXECUTION_MODE, IntentState, NewPair};

#[cfg(test)]
mod flow_tests;

/// Command queue bound: a UI that floods commands waits (backpressure) instead of growing memory.
pub const COMMAND_CAPACITY: usize = 64;
/// Bound of the internal queue from spawned I/O tasks back to the actor.
pub const EVENT_CAPACITY: usize = 256;

/// Written when the gate refuses an exposure-opening command (once per pair, command and cause).
pub const COMMAND_REFUSED: &str = "COMMAND_REFUSED";
/// Written with the new pair row, in the same transaction.
pub const PAIR_PREPARED: &str = "PAIR_PREPARED";
/// The entry time has come but entry is not allowed (once per pair and cause).
pub const ENTRY_BLOCKED: &str = "ENTRY_BLOCKED";
/// The baseline re-fetch failed for a leg; Node 0 falls back to the scan price (core rule).
pub const BASELINE_FETCH_FAILED: &str = "BASELINE_FETCH_FAILED";
/// One per submit result (open or close); carries `simulated`.
pub const ORDER_SUBMITTED: &str = "ORDER_SUBMITTED";
/// A later lookup reported a changed fill of a pair's order (cumulative quantity, average price,
/// fee and fee asset); carries `simulated`.
pub const ORDER_FILL: &str = "ORDER_FILL";
/// Alert: a leg's actual position differs from the pair's recorded fill by more than the
/// effective `max_leg_imbalance_pct` (or the recorded fill is unknown); nothing is closed.
pub const CLOSE_QUANTITY_MISMATCH: &str = "CLOSE_QUANTITY_MISMATCH";
/// Both legs verified flat (positions 0, no open order); written right before FINALIZED.
pub const CLOSE_CONFIRMED: &str = "CLOSE_CONFIRMED";
/// Result of a manual order (not part of a pair).
pub const MANUAL_ORDER_RESULT: &str = "MANUAL_ORDER_RESULT";
pub const CONFIG_UPDATED: &str = "CONFIG_UPDATED";
/// Startup reconciliation finished (or failed).
pub const RECONCILIATION_RESULT: &str = "RECONCILIATION_RESULT";
/// Fill timeout: what cancelling an own unfilled order and looking it up again said.
pub const ORDER_CANCEL_RESULT: &str = "ORDER_CANCEL_RESULT";
/// Result of "confirm closed" (accepted after the system re-query, or refused with positions).
pub const MANUAL_CONFIRM_RESULT: &str = "MANUAL_CONFIRM_RESULT";
/// Result of a manual cancel (manual order page): the executor's answer, unchanged.
pub const MANUAL_CANCEL_RESULT: &str = "MANUAL_CANCEL_RESULT";

/// `config` key of the global `RiskConfig` JSON. Missing = defaults (incomplete: Node 0 blocks).
pub const CONFIG_RISK: &str = "risk";
/// `config` key of the per-exchange overrides (`{"Bybit": {...}}`). Missing = none.
pub const CONFIG_RISK_OVERRIDES: &str = "risk_overrides";
/// `config` key of the contract template `{"notional_usdt": "...", "leverage": "..."}` (per leg).
/// Missing = the Python defaults (1000 / 5), applied by the UI view-model.
pub const CONFIG_CONTRACT_TEMPLATE: &str = "contract_template";

/// PREPARED auto-cancel re-fetch period (AUTO only). It runs only while the pair is more than one
/// period before its baseline time; from the baseline on, the baseline data and Node 0 decide.
pub const PREPARED_RECHECK_MS: i64 = 30_000;

/// While startup reconciliation is pending, it is run again this often (injected clock). It is
/// idempotent: settled intents are not queried again and landed transitions are not repeated.
pub const RECONCILE_RETRY_MS: i64 = 30_000;

/// Latest price per (exchange, symbol); feeders update it with `send_modify`.
pub type MarketPrices = BTreeMap<(Exchange, String), Decimal>;

/// A command plus where to send the reply (if the sender wants one).
#[derive(Debug)]
pub struct CommandMsg {
    pub command: Command,
    pub reply: Option<oneshot::Sender<CommandReply>>,
}

/// What the engine needs at start.
pub struct EngineDeps {
    pub db: Db,
    pub clock: Arc<dyn Clock>,
    pub timings: EngineTimings,
    /// The SIMULATION executor (`sim::SimulatedExecutor` in production).
    pub simulator: Arc<dyn Executor>,
    /// Builds the order-capable executor; called only when switching to EXCHANGE_DEMO.
    pub factory: Arc<dyn ExecutorFactory>,
    /// Single-symbol re-fetch (baseline, pre-trade, PREPARED re-check) and order rules.
    pub market: Arc<dyn MarketData>,
    /// `serverTime` offsets per exchange (`None` = not calibrated: no entry).
    pub offsets: Arc<dyn ServerOffsets>,
    /// Read-only exchange account (positions, open orders, margin) for EXCHANGE_DEMO pairs.
    pub account: Arc<dyn AccountView>,
    /// Account view over the simulated ledger (`SimulatedExecutor::account_view`) for SIMULATION
    /// pairs; its margin is the demo account's real balance (decision 6).
    pub sim_account: Arc<dyn AccountView>,
    /// Startup reconciliation (`engine::recovery`); `None` = unfinished intents keep exposure
    /// blocked (fail closed).
    pub reconciler: Option<Arc<dyn StartupReconciler>>,
    /// System notification channel for alerts (`alert::LogNotifier` until the macOS one exists).
    pub notifier: Arc<dyn Notifier>,
}

/// The public surface: send commands, read snapshots, feed prices. Nothing here exposes mutable
/// engine state.
pub struct EngineHandle {
    pub commands: mpsc::Sender<CommandMsg>,
    pub snapshots: watch::Receiver<Snapshot>,
    pub market: watch::Sender<MarketPrices>,
}

impl EngineHandle {
    /// Send `command` and wait for its reply. A stopped engine answers `Rejected`.
    pub async fn send(&self, command: Command) -> CommandReply {
        let (tx, rx) = oneshot::channel();
        if self.commands.send(CommandMsg { command, reply: Some(tx) }).await.is_err() {
            return CommandReply::Rejected("engine stopped".into());
        }
        rx.await.unwrap_or_else(|_| CommandReply::Rejected("engine stopped before replying".into()))
    }
}

/// Start the actor on the current tokio runtime.
pub fn start(deps: EngineDeps) -> EngineHandle {
    let (actor, handle) = Actor::new(deps);
    tokio::spawn(actor.run());
    handle
}

/// What the engine keeps in `pairs.entry_json` next to the scan snapshot, so pairs can be
/// rebuilt after a restart.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PairEnvelope {
    pub long_exchange: Exchange,
    pub short_exchange: Exchange,
    pub settlement_ms: i64,
    pub simulated: bool,
    pub scan: Value,
}

const fn idx(leg: Leg) -> usize {
    match leg {
        Leg::Long => 0,
        Leg::Short => 1,
    }
}

const fn side_of(leg: Leg) -> Side {
    match leg {
        Leg::Long => Side::Long,
        Leg::Short => Side::Short,
    }
}

fn dstr(d: Decimal) -> String {
    d.normalize().to_string()
}

/// States in which orders may be in flight; after a restart they belong to the reconciler.
const fn in_flight(state: PairState) -> bool {
    match state {
        PairState::PreTradeCheck | PairState::OrderSubmit | PairState::FillMonitor | PairState::Closing => true,
        PairState::Prepared
        | PairState::Blocked
        | PairState::Reconciled
        | PairState::Imbalanced
        | PairState::Finalized
        | PairState::Cancelled
        | PairState::PartialFailure
        | PairState::Unresolved => false,
    }
}

/// What startup hands to the reconciler: in-flight pairs, and RECONCILED simulated pairs (their
/// simulated position died with the process; decision 7).
const fn needs_reconciliation(view: &PairView) -> bool {
    in_flight(view.state) || (view.simulated && matches!(view.state, PairState::Reconciled))
}

/// One order of a leg and what is known about it.
#[derive(Debug, Clone)]
struct LegOrder {
    req: OrderRequest,
    /// Base coin per order unit (OKX contract value, else 1): turns order units into the base-coin
    /// quantities `fill::LegFill` compares across exchanges.
    unit_base: Decimal,
    /// `None` while the submit call has not returned.
    outcome: Option<SubmitOutcome>,
    /// Latest status (from an accepted submit or a query by `client_order_id`).
    status: Option<OrderStatus>,
    query_inflight: bool,
    /// Fill-timeout cancel of this (own, not completely filled) order.
    cancel: CancelStep,
    /// After the timeout cancel, the final state could not be confirmed (lookup failed, or the
    /// order was still open): the leg counts as unknown (-> UNRESOLVED), never guessed.
    final_unknown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelStep {
    NotNeeded,
    InFlight,
    Done,
}

impl LegOrder {
    fn new(req: OrderRequest, unit_base: Decimal) -> LegOrder {
        LegOrder { req, unit_base, outcome: None, status: None, query_inflight: false, cancel: CancelStep::NotNeeded, final_unknown: false }
    }

    fn fill(&self) -> LegFill {
        let requested = self.req.quantity * self.unit_base;
        if self.final_unknown {
            return LegFill::Unknown;
        }
        match (&self.status, &self.outcome) {
            (Some(s), _) => {
                let filled = s.filled_quantity * self.unit_base;
                match s.state {
                    OrderState::Open | OrderState::Filled => LegFill::Known { requested, filled },
                    OrderState::Cancelled | OrderState::Rejected => LegFill::Terminal { requested, filled },
                }
            }
            (None, Some(SubmitOutcome::Rejected { .. })) => LegFill::Terminal { requested, filled: Decimal::ZERO },
            (None, Some(SubmitOutcome::Accepted(_) | SubmitOutcome::Unknown { .. }) | None) => LegFill::Unknown,
        }
    }

    /// The submit itself was refused (nothing exists on the exchange).
    fn submit_rejected(&self) -> Option<&str> {
        match &self.outcome {
            Some(SubmitOutcome::Rejected { reason }) => Some(reason),
            Some(SubmitOutcome::Accepted(_) | SubmitOutcome::Unknown { .. }) | None => None,
        }
    }

    /// The order can no longer change (filled, cancelled or rejected).
    fn is_over(&self) -> bool {
        match (&self.status, &self.outcome) {
            (Some(s), _) => s.state != OrderState::Open,
            (None, Some(SubmitOutcome::Rejected { .. })) => true,
            (None, Some(SubmitOutcome::Accepted(_) | SubmitOutcome::Unknown { .. }) | None) => false,
        }
    }

    /// Worth a lookup by `client_order_id` (open, or result unknown / still pending).
    fn needs_query(&self) -> bool {
        !self.query_inflight && !self.is_over() && self.cancel == CancelStep::NotNeeded
    }
}

/// The orders of one submission (entry or close) of a pair.
#[derive(Debug, Clone)]
struct OrderSet {
    sent_at_ms: i64,
    /// The pair's effective settings at sending (timeout, imbalance tolerance).
    eff: EffectiveConfig,
    legs: [Option<LegOrder>; 2],
    /// The fill-timeout cancels were started (at most once per set).
    cancel_started: bool,
}

impl OrderSet {
    fn find_mut(&mut self, client_order_id: &str) -> Option<&mut LegOrder> {
        self.legs.iter_mut().flatten().find(|o| o.req.client_order_id == client_order_id)
    }
    fn timed_out(&self, now_ms: i64) -> bool {
        now_ms >= fill::timeout_at_ms(&self.eff, self.sent_at_ms)
    }
    fn leg_fill(&self, leg: Leg) -> LegFill {
        self.legs[idx(leg)].as_ref().map_or(LegFill::Unknown, LegOrder::fill)
    }
}

#[derive(Debug, Clone)]
struct EntryContext {
    rules: [Result<OrderRules, String>; 2],
    foreign: [Result<bool, String>; 2],
}

type QuotePair = (Result<FreshQuote, String>, Result<FreshQuote, String>);

/// In-memory progress of one pair. Lost on restart (orders in flight then belong to the
/// reconciler; see `orphan`).
#[derive(Debug, Clone, Default)]
struct Flow {
    /// Scan snapshot from `entry_json` (`node0::EntrySnapshot`).
    scan: Value,
    /// Loaded in an in-flight state at startup without in-memory orders: the scheduler leaves it
    /// alone until reconciliation (or a manual close) takes over.
    orphan: bool,
    baseline_requested: bool,
    baseline: Option<QuotePair>,
    pretrade: Option<QuotePair>,
    margins: Option<(Result<Decimal, String>, Result<Decimal, String>)>,
    context: Option<EntryContext>,
    recheck_inflight: bool,
    last_recheck_ms: Option<i64>,
    open: Option<OrderSet>,
    close: Option<OrderSet>,
    /// Signed position per leg that is expected to remain after the close: exposure on the same
    /// symbol that is not the pair's (left untouched). The flat check compares against it.
    close_residual: [Decimal; 2],
    flat_inflight: bool,
    /// When the entry / the close was triggered (injected clock): the start of the latency the
    /// T-5 decision depends on (`ORDER_LATENCY.triggered_at`).
    open_triggered_ms: Option<i64>,
    close_triggered_ms: Option<i64>,
}

impl Flow {
    fn new(scan: Value, orphan: bool) -> Flow {
        Flow { scan, orphan, ..Flow::default() }
    }
    fn set_mut(&mut self, action: OrderAction) -> Option<&mut OrderSet> {
        match action {
            OrderAction::Open => self.open.as_mut(),
            OrderAction::Close => self.close.as_mut(),
        }
    }
}

struct Actor {
    db: Db,
    events: EventStore,
    clock: Arc<dyn Clock>,
    timings: EngineTimings,
    simulator: Arc<dyn Executor>,
    factory: Arc<dyn ExecutorFactory>,
    market: Arc<dyn MarketData>,
    offsets: Arc<dyn ServerOffsets>,
    account: Arc<dyn AccountView>,
    sim_account: Arc<dyn AccountView>,
    reconciler: Option<Arc<dyn StartupReconciler>>,
    /// The executor of the current `execution_mode`: `simulator` in SIMULATION, the factory-built
    /// one in EXCHANGE_DEMO (dropped when switching back).
    executor: Arc<dyn Executor>,
    trigger_mode: TriggerMode,
    execution_mode: ExecutionMode,
    pairs: BTreeMap<PairUuid, PairView>,
    flows: BTreeMap<PairUuid, Flow>,
    /// `(pair, cause)` already recorded (COMMAND_REFUSED / ENTRY_BLOCKED are written once).
    reported: BTreeSet<(PairUuid, String)>,
    /// Disambiguates manual order ids created at the same clock millisecond.
    manual_seq: u64,
    /// Set at startup while unfinished intents / in-flight pairs await reconciliation; refuses
    /// exposure meanwhile (in EXCHANGE_DEMO only, decision 8; always shown in the Snapshot).
    reconciliation_pending: Option<String>,
    /// Pairs (and manual `pair_uuid`s) found in flight at startup that the reconciler has not
    /// decided yet. The reconciler touches only these; the scheduler leaves them alone.
    reconcile_scope: BTreeSet<PairUuid>,
    /// Shown in every Snapshot until they no longer apply (e.g. the startup mode fallback, until
    /// EXCHANGE_DEMO is switched on again).
    notices: Vec<Notice>,
    reconcile_inflight: bool,
    last_reconcile_ms: Option<i64>,
    notifier: Arc<dyn Notifier>,
    /// Reason of the current alert entry per pair (shown with the banner).
    alert_reasons: BTreeMap<PairUuid, String>,
    /// `(pair, transition event id)` whose notification is in flight or done in this run.
    notify_started: BTreeSet<(PairUuid, i64)>,
    /// "Confirm closed" re-queries in flight, with the replies waiting for them.
    pending_confirms: BTreeMap<PairUuid, Vec<oneshot::Sender<CommandReply>>>,
    cmd_rx: mpsc::Receiver<CommandMsg>,
    event_tx: mpsc::Sender<Event>,
    event_rx: mpsc::Receiver<Event>,
    market_rx: watch::Receiver<MarketPrices>,
    market_open: bool,
    snapshot_tx: watch::Sender<Snapshot>,
    /// State changed since the last pushed Snapshot.
    dirty: bool,
    last_push: Option<Instant>,
}

impl Actor {
    fn new(deps: EngineDeps) -> (Actor, EngineHandle) {
        let EngineDeps { db, clock, timings, simulator, factory, market, offsets, account, sim_account, reconciler, notifier } = deps;
        let events = EventStore::new(db.clone());
        let (trigger_mode, mut execution_mode, warnings) = gate::load_modes(&db);
        for w in warnings {
            let _ = events.append("MODE_LOAD_WARNING", None, json!({ "warning": w }));
        }
        let mut executor = simulator.clone();
        let mut notices = Vec::new();
        if execution_mode == ExecutionMode::ExchangeDemo {
            match factory.create(ExecutionMode::ExchangeDemo) {
                Ok(e) => executor = e,
                Err(reason) => {
                    // Fail closed: no order-capable executor -> SIMULATION, persisted, recorded and
                    // told to the user (decision 2026-10-05 evening).
                    execution_mode = ExecutionMode::Simulation;
                    notices.push(Notice {
                        code: gate::EXECUTION_MODE_FALLBACK.to_string(),
                        message: format!(
                            "儲存的模式是 EXCHANGE_DEMO，但無法建立 demo 執行器（{reason}），已退回 SIMULATION。\
                             修好金鑰後可切回 EXCHANGE_DEMO（只剩 demo 配對時允許）。"
                        ),
                    });
                    let _ = db.set_flag_with_event(
                        FLAG_EXECUTION_MODE,
                        gate::execution_mode_str(ExecutionMode::Simulation),
                        gate::EXECUTION_MODE_FALLBACK,
                        &json!({ "from": "EXCHANGE_DEMO", "to": "SIMULATION", "reason": reason }),
                    );
                }
            }
        }
        let mut pairs = BTreeMap::new();
        let mut flows = BTreeMap::new();
        for (view, scan) in load_open_pairs(&db) {
            flows.insert(view.internal_uuid.clone(), Flow::new(scan, needs_reconciliation(&view)));
            pairs.insert(view.internal_uuid.clone(), view);
        }
        let (reconcile_scope, reconciliation_pending) = startup_reconciliation(&db, &pairs);

        let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
        let (market_tx, market_rx) = watch::channel(MarketPrices::new());
        let mut actor = Actor {
            db,
            events,
            clock,
            timings,
            simulator,
            factory,
            market,
            offsets,
            account,
            sim_account,
            reconciler,
            executor,
            trigger_mode,
            execution_mode,
            pairs,
            flows,
            reported: BTreeSet::new(),
            manual_seq: 0,
            reconciliation_pending,
            reconcile_scope,
            notices,
            reconcile_inflight: false,
            last_reconcile_ms: None,
            notifier,
            alert_reasons: BTreeMap::new(),
            notify_started: BTreeSet::new(),
            pending_confirms: BTreeMap::new(),
            cmd_rx,
            event_tx,
            event_rx,
            market_rx,
            market_open: true,
            snapshot_tx: watch::channel(dummy_snapshot()).0,
            dirty: false,
            last_push: None,
        };
        // Pairs already in an alert state at startup: banner from their state; PAIR_ALERT /
        // notification only for an entry that has none yet (never again after a restart).
        actor.sync_alerts();
        let (snapshot_tx, snapshots) = watch::channel(actor.snapshot());
        actor.snapshot_tx = snapshot_tx;
        (actor, EngineHandle { commands: cmd_tx, snapshots, market: market_tx })
    }

    async fn run(mut self) {
        self.start_reconciliation();
        let mut tick = tokio::time::interval(Duration::from_millis(self.timings.tick_ms.max(1) as u64));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let push_at = self.next_push_at();
            tokio::select! {
                msg = self.cmd_rx.recv() => match msg {
                    Some(m) => self.on_command(m),
                    None => break, // every sender is gone: the engine stops
                },
                Some(ev) = self.event_rx.recv() => self.on_event(ev),
                _ = tick.tick() => {
                    let _ = self.dispatch(Command::Tick);
                }
                changed = self.market_rx.changed(), if self.market_open => match changed {
                    Ok(()) => self.dirty = true,
                    Err(_) => self.market_open = false,
                },
                _ = sleep_until(push_at.unwrap_or_else(tokio::time::Instant::now)), if push_at.is_some() => {}
            }
            self.maybe_publish();
        }
    }

    // ---- startup reconciliation hook ----

    /// Spawns one reconciliation run when startup left something to reconcile (and none is
    /// running). Called at start and, while still pending, every `RECONCILE_RETRY_MS`.
    fn start_reconciliation(&mut self) {
        let Some(reason) = self.reconciliation_pending.clone() else { return };
        if self.reconcile_inflight {
            return;
        }
        let Some(reconciler) = self.reconciler.clone() else {
            self.reconciliation_pending = Some(format!("{reason}; no reconciler configured"));
            return;
        };
        let simulated = self.execution_mode == ExecutionMode::Simulation;
        let ctx = ReconcileContext {
            db: self.db.clone(),
            executor: self.executor.clone(),
            account: self.account_for(simulated),
            execution_mode: self.execution_mode,
            scope: self.reconcile_scope.clone(),
        };
        self.reconcile_inflight = true;
        self.last_reconcile_ms = Some(self.clock.now_ms());
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let result = reconciler.reconcile(ctx).await;
            let _ = tx.send(Event::ReconciliationDone { result }).await;
        });
    }

    /// Retry a pending reconciliation (tick).
    fn maybe_retry_reconciliation(&mut self, now: i64) {
        if self.reconciliation_pending.is_some()
            && self.reconciler.is_some()
            && !self.reconcile_inflight
            && self.last_reconcile_ms.is_none_or(|t| now - t >= RECONCILE_RETRY_MS)
        {
            self.start_reconciliation();
        }
    }

    /// Adopt what the reconciler landed for the pairs in scope. A pair it decided (its state
    /// changed, or the whole run succeeded) leaves the scope and is the actor's again; the rest
    /// stay hands-off for the next run.
    fn on_reconciliation_done(&mut self, result: Result<(), String>) {
        self.reconcile_inflight = false;
        match &result {
            Ok(()) => self.note(RECONCILIATION_RESULT, None, json!({ "ok": true })),
            Err(reason) => self.note(RECONCILIATION_RESULT, None, json!({ "ok": false, "reason": reason })),
        }
        let scope: Vec<PairUuid> = self.reconcile_scope.iter().cloned().collect();
        let mut still: Vec<PairUuid> = Vec::new();
        for id in scope {
            let before = self.pairs.get(&id).map(|v| v.state);
            let row = match self.db.get_pair(&id) {
                Ok(Some(row)) => row,
                // No pair row (manual orders): done when the run succeeded.
                Ok(None) => {
                    if result.is_ok() {
                        self.reconcile_scope.remove(&id);
                    }
                    continue;
                }
                Err(_) => continue, // kept; the store error shows elsewhere
            };
            let Some((view, scan)) = view_of_row(&self.db, row) else { continue };
            let decided = (result.is_ok() || before != Some(view.state)) && !needs_reconciliation(&view);
            let flow = self.flows.entry(id.clone()).or_default();
            flow.scan = scan;
            flow.orphan = !decided;
            if decided {
                self.reconcile_scope.remove(&id);
            } else if result.is_ok() {
                still.push(id.clone());
            }
            self.pairs.insert(id, view);
        }
        self.sync_alerts();
        self.reconciliation_pending = match result {
            Ok(()) if still.is_empty() => None,
            Ok(()) => Some(format!("reconciler finished but pair(s) still in flight: {}", still.join(", "))),
            Err(reason) => Some(format!("reconciliation did not complete: {reason}")),
        };
    }

    fn on_command(&mut self, msg: CommandMsg) {
        if let Command::ConfirmClosed { pair, .. } = &msg.command {
            // Answered when the re-query of both legs is back (never on the user's word).
            self.dirty = true;
            let pair = pair.clone();
            match self.start_confirm(&pair) {
                Ok(()) => {
                    if let Some(tx) = msg.reply {
                        self.pending_confirms.entry(pair).or_default().push(tx);
                    }
                }
                Err(reply) => {
                    if let Some(tx) = msg.reply {
                        let _ = tx.send(reply);
                    }
                }
            }
            return;
        }
        let reply = self.dispatch(msg.command);
        if let Some(tx) = msg.reply {
            let _ = tx.send(reply);
        }
    }

    /// The single entry for every command: gate first, then one handler per variant.
    fn dispatch(&mut self, cmd: Command) -> CommandReply {
        self.dirty = true;
        if cmd.opens_exposure() {
            let blockers = gate::current_blockers(&self.db, self.reconciliation_pending.as_deref());
            let refusing = gate::refusing(&blockers, self.execution_mode);
            let name = command_name(&cmd);
            let pair = pair_of(&cmd).map(str::to_string);
            if let Err(why) = gate::admit(&cmd, &refusing) {
                let payload = json!({ "command": name, "reason": why });
                // Best effort: on a halted store this cannot be written (the halt is the record).
                match pair {
                    Some(p) => self.report_once(&p, format!("{COMMAND_REFUSED}:{name}:{why}"), COMMAND_REFUSED, payload),
                    None => self.note(COMMAND_REFUSED, None, payload),
                }
                return CommandReply::Rejected(why);
            }
            if let Some(p) = pair {
                // Admitted again: a later refusal is news and is recorded again.
                self.reported.retain(|(rp, k)| !(*rp == p && k.starts_with(COMMAND_REFUSED)));
            }
        }
        match cmd {
            Command::Tick => self.on_tick(),
            Command::AddPrepared(p) => self.add_prepared(p),
            Command::EntryTrigger { pair } => self.start_entry(&pair, "scheduler"),
            Command::ManualEnter { pair } => self.start_entry(&pair, "user"),
            Command::EnterSelected { pairs } => self.enter_selected(pairs),
            Command::ManualOrder(o) => self.manual_order(o),
            Command::ManualCancel { exchange, symbol, client_order_id } => self.manual_cancel(exchange, symbol, client_order_id),
            Command::AutoExit { pair } => self.begin_close(&pair, SystemEvent::ScheduledClose.into(), "scheduler"),
            Command::ManualExit { pair } => self.begin_close(&pair, ManualEvent::RequestClose.into(), "user exit"),
            Command::ManualClose { pair } => self.begin_close(&pair, ManualEvent::RequestClose.into(), "user close"),
            Command::CancelPrepared { pair, reason } => {
                self.transition(&pair, ManualEvent::Cancel, json!({ "reason": reason }))
            }
            // The user's `verified_flat` is not trusted: the system re-queries both legs.
            Command::ConfirmClosed { pair, .. } => match self.start_confirm(&pair) {
                Ok(()) => CommandReply::Accepted,
                Err(reply) => reply,
            },
            Command::SetTriggerMode(m) => self.set_trigger_mode(m),
            Command::SetExecutionMode(m) => self.set_execution_mode(m),
            Command::UpdateConfig { key, value } => self.update_config(&key, value),
            Command::SaveRiskSettings { risk, overrides } => self.save_risk_settings(risk, overrides),
            Command::SaveContractTemplate { notional_usdt, leverage } => self.save_contract_template(notional_usdt, leverage),
            Command::SetKillSwitch { on } => match self.db.set_kill_switch(on) {
                Ok(()) => CommandReply::Accepted,
                Err(e) => CommandReply::Rejected(format!("cannot save kill switch: {e}")),
            },
        }
    }

    // ---- scheduler ----

    fn on_tick(&mut self) -> CommandReply {
        let now = self.clock.now_ms();
        self.maybe_retry_reconciliation(now);
        let ids: Vec<PairUuid> = self.pairs.keys().cloned().collect();
        for id in ids {
            let Some(view) = self.pairs.get(&id).cloned() else { continue };
            let orphan = self.flows.get(&id).is_some_and(|f| f.orphan);
            match view.state {
                PairState::Prepared => self.tick_prepared(&view, now),
                PairState::Reconciled if !orphan => self.tick_reconciled(&view, now),
                PairState::OrderSubmit if !orphan => self.check_submitted(&id, now),
                PairState::FillMonitor if !orphan => {
                    self.poll(&id, OrderAction::Open);
                    self.check_fills(&id, now);
                }
                PairState::Closing if !orphan => {
                    self.poll(&id, OrderAction::Close);
                    self.check_close(&id, now);
                }
                PairState::OrderSubmit
                | PairState::FillMonitor
                | PairState::Closing
                | PairState::Reconciled
                | PairState::PreTradeCheck
                | PairState::Blocked
                | PairState::Imbalanced
                | PairState::Finalized
                | PairState::Cancelled
                | PairState::PartialFailure
                | PairState::Unresolved => {}
            }
        }
        CommandReply::Accepted
    }

    fn reading(&self, view: &PairView, now: i64) -> ClockReading {
        ClockReading {
            local_now_ms: now,
            long: (view.long_exchange, self.offsets.offset_ms(view.long_exchange)),
            short: (view.short_exchange, self.offsets.offset_ms(view.short_exchange)),
        }
    }

    fn tick_prepared(&mut self, view: &PairView, now: i64) {
        let id = view.internal_uuid.clone();
        let baseline_requested = self.flows.get(&id).is_some_and(|f| f.baseline_requested);
        let timing = PairTiming { state: view.state, settlement_ms: view.settlement_ms, baseline_requested };
        match schedule::decide(&timing, &self.timings, &self.reading(view, now)) {
            ScheduleAction::Nothing | ScheduleAction::Exit { .. } => {}
            ScheduleAction::FetchBaseline => {
                self.flows.entry(id.clone()).or_default().baseline_requested = true;
                let (market, tx) = (self.market.clone(), self.event_tx.clone());
                let (le, se, sym, pair) = (view.long_exchange, view.short_exchange, view.symbol.clone(), id.clone());
                tokio::spawn(async move {
                    let (long, short) = tokio::join!(market.refetch(le, &sym), market.refetch(se, &sym));
                    let _ = tx.send(Event::BaselineFetched { pair, long, short }).await;
                });
            }
            ScheduleAction::Enter => match self.trigger_mode {
                TriggerMode::Auto => {
                    let _ = self.dispatch(Command::EntryTrigger { pair: id.clone() });
                }
                TriggerMode::Manual => {}
            },
            ScheduleAction::EntryBlocked(block) => {
                let payload = json!({ "reason": format!("{block:?}"), "settlement_ms": view.settlement_ms });
                self.report_once(&id, format!("{ENTRY_BLOCKED}:{block:?}"), ENTRY_BLOCKED, payload);
            }
            ScheduleAction::EntryWindowMissed { policy } => match policy {
                MissedWindowPolicy::WarnThenCancel => {
                    let payload = json!({ "settlement_ms": view.settlement_ms, "now_ms": now, "policy": "WarnThenCancel" });
                    // The warning comes first; without it nothing is cancelled (retried next tick).
                    if self.events.append(ENTRY_WINDOW_MISSED, Some(&id), payload).is_ok() {
                        let _ = self.land(&id, SystemEvent::Cancel, json!({ "reason": ENTRY_WINDOW_MISSED }));
                    }
                }
            },
        }
        self.maybe_recheck(&id, now);
    }

    /// PREPARED auto-cancel re-fetch (AUTO only; not gated by the kill switch).
    fn maybe_recheck(&mut self, id: &str, now: i64) {
        let Some(view) = self.pairs.get(id).cloned() else { return };
        if view.state != PairState::Prepared || self.trigger_mode != TriggerMode::Auto {
            return;
        }
        if now >= self.timings.base_price_at(view.settlement_ms) - PREPARED_RECHECK_MS {
            return;
        }
        let flow = self.flows.entry(id.to_string()).or_default();
        if flow.recheck_inflight || flow.last_recheck_ms.is_some_and(|t| now - t < PREPARED_RECHECK_MS) {
            return;
        }
        flow.recheck_inflight = true;
        flow.last_recheck_ms = Some(now);
        let (market, tx) = (self.market.clone(), self.event_tx.clone());
        let (le, se, sym, pair) = (view.long_exchange, view.short_exchange, view.symbol.clone(), id.to_string());
        tokio::spawn(async move {
            let (long, short) = tokio::join!(market.refetch(le, &sym), market.refetch(se, &sym));
            let _ = tx.send(Event::RecheckFetched { pair, long, short }).await;
        });
    }

    /// Evaluate the PREPARED auto-cancel on fresh quotes; cancel (land core `Cancel`) if worsened.
    fn auto_cancel_check(&mut self, id: &str, long: &FreshQuote, short: &FreshQuote) {
        let Some(view) = self.pairs.get(id).cloned() else { return };
        if self.trigger_mode != TriggerMode::Auto {
            return;
        }
        let Some(scan) = self.flows.get(id).map(|f| f.scan.clone()) else { return };
        let Ok(entry) = EntrySnapshot::from_json(&scan) else { return }; // Node 0 blocks it later
        // Unreadable settings count as incomplete: PREPARED holds no exposure, fail closed.
        let (risk, overrides) = self.load_risk().unwrap_or_else(|_| (RiskConfig::default(), RiskOverrides::new()));
        let eff = effective_for_pair(&risk, &overrides, view.long_exchange, view.short_exchange);
        let recheck = PreparedRecheck {
            long: &long.funding,
            short: &short.funding,
            notional: entry.notional_usdt,
            effective: &eff,
            allowed_exchanges: &risk.allowed_exchanges,
        };
        match fill::prepared_auto_cancel(view.state, &recheck) {
            AutoCancel::NoAction => {}
            AutoCancel::Cancel { reasons } => {
                let detail = json!({ "reason": "PREPARED_AUTO_CANCEL", "reasons": format!("{reasons:?}") });
                let _ = self.land(id, SystemEvent::Cancel, detail);
            }
        }
    }

    fn tick_reconciled(&mut self, view: &PairView, now: i64) {
        let timing = PairTiming { state: view.state, settlement_ms: view.settlement_ms, baseline_requested: true };
        match schedule::decide(&timing, &self.timings, &self.reading(view, now)) {
            // Exit in either trigger mode (late beats never); not gated (reduces exposure only).
            ScheduleAction::Exit { .. } => {
                let _ = self.dispatch(Command::AutoExit { pair: view.internal_uuid.clone() });
            }
            ScheduleAction::Nothing
            | ScheduleAction::FetchBaseline
            | ScheduleAction::Enter
            | ScheduleAction::EntryBlocked(_)
            | ScheduleAction::EntryWindowMissed { .. } => {}
        }
    }

    // ---- entry: PREPARED -> PRE_TRADE_CHECK -> Node 0 -> ORDER_SUBMIT ----

    /// A pair's orders must go to the executor of the mode it was created in: a demo pair is never
    /// traded through the simulator (e.g. after a startup fallback to SIMULATION) and vice versa.
    fn executor_mismatch(&self, view: &PairView) -> Option<String> {
        match (view.simulated, self.executor.is_simulated()) {
            (true, true) | (false, false) => None,
            (false, true) => Some("pair belongs to EXCHANGE_DEMO but the demo executor is not available (未連線); switch to EXCHANGE_DEMO first".into()),
            (true, false) => Some("pair belongs to SIMULATION but the engine is in EXCHANGE_DEMO; switch back to SIMULATION first".into()),
        }
    }

    fn start_entry(&mut self, pair: &str, source: &str) -> CommandReply {
        let Some(view) = self.pairs.get(pair).cloned() else {
            return CommandReply::Rejected(format!("unknown pair {pair}"));
        };
        if view.state != PairState::Prepared {
            return CommandReply::Rejected(format!("pair is {}, not PREPARED", view.state));
        }
        if let Some(why) = self.executor_mismatch(&view) {
            return CommandReply::Rejected(why);
        }
        // Never enter without calibrated exchange time, never at or after settlement (D9).
        match self.reading(&view, self.clock.now_ms()).exchange_now_range() {
            None => return CommandReply::Rejected("serverTime offset unavailable: no entry (fail closed)".into()),
            Some((_, latest)) if latest >= view.settlement_ms => {
                return CommandReply::Rejected("entry window closed: settlement time reached".into());
            }
            Some(_) => {}
        }
        if let Err(e) = self.land(pair, SystemEvent::StartCheck, json!({ "source": source })) {
            return CommandReply::Rejected(e);
        }
        let now = self.clock.now_ms();
        let flow = self.flows.entry(pair.to_string()).or_default();
        flow.open_triggered_ms = Some(now);
        flow.pretrade = None;
        flow.margins = None;
        flow.context = None;
        let (market, account, tx) = (self.market.clone(), self.account_for(view.simulated), self.event_tx.clone());
        let (le, se, sym, pair) = (view.long_exchange, view.short_exchange, view.symbol.clone(), pair.to_string());
        tokio::spawn(async move {
            let (long, short) = tokio::join!(market.refetch(le, &sym), market.refetch(se, &sym));
            let _ = tx.send(Event::PretradeFetched { pair: pair.clone(), long, short }).await;
            let (long, short) = tokio::join!(account.available_margin(le), account.available_margin(se));
            let _ = tx.send(Event::MarginFetched { pair: pair.clone(), long, short }).await;
            let (long_rules, short_rules) = tokio::join!(market.order_rules(le, &sym), market.order_rules(se, &sym));
            let (long_foreign, short_foreign) =
                tokio::join!(foreign_exposure(account.as_ref(), le, &sym), foreign_exposure(account.as_ref(), se, &sym));
            let _ = tx.send(Event::EntryContextFetched { pair, long_rules, short_rules, long_foreign, short_foreign }).await;
        });
        CommandReply::Accepted
    }

    /// Runs Node 0 once the pre-trade quotes, margins and context are all in.
    fn maybe_run_node0(&mut self, pair: &str) {
        if self.pairs.get(pair).map(|v| v.state) != Some(PairState::PreTradeCheck) {
            return;
        }
        let Some(flow) = self.flows.get_mut(pair) else { return };
        if flow.pretrade.is_none() || flow.margins.is_none() || flow.context.is_none() {
            return;
        }
        let (Some(pretrade), Some(margins), Some(context)) = (flow.pretrade.take(), flow.margins.take(), flow.context.take())
        else {
            return;
        };
        let (scan, baseline) = (flow.scan.clone(), flow.baseline.clone());
        self.run_node0(pair, pretrade, margins, context, scan, baseline);
    }

    fn run_node0(
        &mut self,
        pair: &str,
        pretrade: QuotePair,
        margins: (Result<Decimal, String>, Result<Decimal, String>),
        context: EntryContext,
        scan: Value,
        baseline: Option<QuotePair>,
    ) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        let fail = |this: &mut Actor, detail: Value| {
            let _ = this.land(pair, SystemEvent::CheckFailed, detail);
        };
        let (risk, overrides) = match self.load_risk() {
            Ok(x) => x,
            Err(why) => return fail(self, json!({ "block": format!("settings unreadable: {why}") })),
        };
        let eff = effective_for_pair(&risk, &overrides, view.long_exchange, view.short_exchange);
        let entry = match EntrySnapshot::from_json(&scan) {
            Ok(e) => e,
            Err(why) => return fail(self, json!({ "block": format!("InvalidEntry: {why}") })),
        };
        let (pre_l, pre_s) = match pretrade {
            (Ok(l), Ok(s)) => (l, s),
            (l, s) => {
                let why = [l.err(), s.err()].into_iter().flatten().collect::<Vec<_>>().join("; ");
                return fail(self, json!({ "block": format!("pre-trade fetch failed: {why}") }));
            }
        };
        let (base_l, base_s) = match &baseline {
            Some((l, s)) => (l.as_ref().ok(), s.as_ref().ok()),
            None => (None, None),
        };
        let mut notes = Vec::new();
        let foreign = |r: &Result<bool, String>, leg: Leg, notes: &mut Vec<String>| match r {
            Ok(b) => *b,
            Err(e) => {
                notes.push(format!("{}: account unreadable, counted as foreign exposure: {e}", leg.as_str()));
                true
            }
        };
        let long_foreign = foreign(&context.foreign[0], Leg::Long, &mut notes);
        let short_foreign = foreign(&context.foreign[1], Leg::Short, &mut notes);
        let others = self.pairs.iter().filter(|(k, _)| k.as_str() != pair).map(|(_, v)| v.state);
        let open_pair_count = node0::count_open_pairs(others);
        let ctx = Node0Context {
            now_ms: self.clock.now_ms(),
            symbol: &view.symbol,
            entry: &entry,
            effective: &eff,
            max_concurrent_pairs: risk.max_concurrent_pairs,
            allowed_exchanges: &risk.allowed_exchanges,
            open_pair_count,
        };
        let long = Node0Leg {
            exchange: view.long_exchange,
            baseline: base_l,
            pretrade: &pre_l,
            available_margin: &margins.0,
            has_foreign_exposure: long_foreign,
        };
        let short = Node0Leg {
            exchange: view.short_exchange,
            baseline: base_s,
            pretrade: &pre_s,
            available_margin: &margins.1,
            has_foreign_exposure: short_foreign,
        };
        match node0::run(&ctx, &long, &short) {
            Node0Verdict::Block(block) => {
                // Every failed check by name, for the staged orders page (never only the first).
                let failed_checks: Vec<String> = match &block {
                    node0::Node0Block::Checks { failed, .. } => failed.iter().map(|c| format!("{c:?}")).collect(),
                    node0::Node0Block::ConfigIncomplete { .. } | node0::Node0Block::InvalidEntry { .. } | node0::Node0Block::DataMismatch { .. } => Vec::new(),
                };
                fail(self, json!({ "block": format!("{block:?}"), "notes": notes, "failed_checks": failed_checks }))
            }
            Node0Verdict::Pass => {
                // Lands in the same transaction as ORDER_SUBMIT (funding-pnl's entry record).
                let snapshot = entry_snapshot_json(&view, &entry, &eff, [&pre_l, &pre_s], [base_l, base_s], &scan);
                let detail = json!({
                    "checks": "pass",
                    "baseline_used": base_l.is_some() && base_s.is_some(),
                    "notes": notes,
                    "entry_snapshot": snapshot,
                });
                if self.land(pair, SystemEvent::CheckPassed, detail) == Ok(PairState::OrderSubmit) {
                    self.submit_entry(&view, &entry, eff, [pre_l.price, pre_s.price], context.rules);
                }
            }
        }
    }

    /// Node 1 + the two intent-first submits. Any sizing failure sends nothing (wave-1 decision).
    fn submit_entry(
        &mut self,
        view: &PairView,
        entry: &EntrySnapshot,
        eff: EffectiveConfig,
        prices: [Decimal; 2],
        rules: [Result<OrderRules, String>; 2],
    ) {
        let pair = view.internal_uuid.as_str();
        let [long_rules, short_rules] = rules;
        let (long_rules, short_rules) = match (long_rules, short_rules) {
            (Ok(l), Ok(s)) => (l, s),
            (l, s) => {
                let why: Vec<String> = [(Leg::Long, l.err()), (Leg::Short, s.err())]
                    .into_iter()
                    .filter_map(|(leg, e)| e.map(|e| format!("{}: order rules unavailable: {e}", leg.as_str())))
                    .collect();
                let _ = self.land(pair, SubmitPlan::ABORT_EVENT, json!({ "reason": "nothing sent", "failed": why }));
                return;
            }
        };
        let sizing = |exchange: Exchange, price: Decimal, r: &OrderRules| LegSizing {
            exchange,
            notional: entry.notional_usdt,
            price,
            lot: r.lot,
            okx_ct_val: r.okx_ct_val,
        };
        let plan = fill::plan_submit(
            &sizing(view.long_exchange, prices[0], &long_rules),
            &sizing(view.short_exchange, prices[1], &short_rules),
        );
        let (long, short) = match plan {
            SubmitPlan::Abort { failed } => {
                let why: Vec<String> = failed.iter().map(|(leg, e)| format!("{}: {e:?}", leg.as_str())).collect();
                let _ = self.land(pair, SubmitPlan::ABORT_EVENT, json!({ "reason": "nothing sent", "failed": why }));
                return;
            }
            SubmitPlan::Send { long, short } => (long, short),
        };
        let prefix = self.id_prefix();
        let unit = |ex: Exchange, r: &OrderRules| match ex {
            Exchange::Okx => r.okx_ct_val.unwrap_or(Decimal::ONE),
            Exchange::Binance | Exchange::Bybit => Decimal::ONE,
        };
        let mut legs: [Option<LegOrder>; 2] = [None, None];
        for (leg, sized, exchange, r) in
            [(Leg::Long, long, view.long_exchange, long_rules), (Leg::Short, short, view.short_exchange, short_rules)]
        {
            let req = OrderRequest {
                client_order_id: self.next_client_order_id(prefix, pair, leg, OrderAction::Open),
                exchange,
                symbol: view.symbol.clone(),
                side: OrderSide::for_leg(side_of(leg), OrderAction::Open),
                quantity: sized.order_qty.value(),
                reduce_only: false,
            };
            legs[idx(leg)] = Some(LegOrder::new(req, unit(exchange, &r)));
        }
        let set = OrderSet { sent_at_ms: self.clock.now_ms(), eff, legs, cancel_started: false };
        self.send_set(pair, OrderAction::Open, set);
    }

    /// Remember the set, then spawn one intent-first submit per leg.
    fn send_set(&mut self, pair: &str, action: OrderAction, set: OrderSet) {
        let reqs: Vec<(Leg, OrderRequest)> =
            Leg::BOTH.into_iter().filter_map(|leg| set.legs[idx(leg)].as_ref().map(|o| (leg, o.req.clone()))).collect();
        let flow = self.flows.entry(pair.to_string()).or_default();
        let triggered = match action {
            OrderAction::Open => flow.open_triggered_ms,
            OrderAction::Close => flow.close_triggered_ms,
        };
        match action {
            OrderAction::Open => flow.open = Some(set),
            OrderAction::Close => flow.close = Some(set),
        }
        // Both legs are spawned before either can answer: they run in parallel (D5).
        for (leg, req) in reqs {
            self.spawn_submit(pair.to_string(), leg, action, req, triggered);
        }
    }

    /// ORDER_SUBMIT: both submit results in (a still-hanging submit counts as "result unknown" once
    /// the effective timeout has passed) → FILL_MONITOR / PARTIAL_FAILURE / CANCELLED.
    fn check_submitted(&mut self, pair: &str, now: i64) {
        if self.pairs.get(pair).map(|v| v.state) != Some(PairState::OrderSubmit) {
            return;
        }
        let Some(set) = self.flows.get(pair).and_then(|f| f.open.clone()) else { return };
        let timed_out = set.timed_out(now);
        let mut rejected = Vec::new();
        for leg in Leg::BOTH {
            let Some(o) = &set.legs[idx(leg)] else { continue };
            match (&o.outcome, timed_out) {
                (None, false) => return, // keep waiting for this submit
                (Some(SubmitOutcome::Rejected { reason }), _) => rejected.push(format!("{}: {reason}", leg.as_str())),
                (None, true) | (Some(SubmitOutcome::Accepted(_) | SubmitOutcome::Unknown { .. }), _) => {}
            }
        }
        let event = match rejected.len() {
            0 => SystemEvent::BothLegsSubmitted,
            1 => SystemEvent::OneLegSubmitFailed,
            _ => SystemEvent::BothSubmitsFailed,
        };
        let pending: Vec<&str> = Leg::BOTH
            .into_iter()
            .filter(|l| set.legs[idx(*l)].as_ref().is_some_and(|o| o.outcome.is_none()))
            .map(Leg::as_str)
            .collect();
        let detail = json!({ "rejected": rejected, "no_reply_by_timeout": pending });
        if self.land(pair, event, detail) == Ok(PairState::FillMonitor) {
            self.check_fills(pair, now);
        }
    }

    /// FILL_MONITOR: `fill::fill_decision` with the effective settings. Never sends an order. At
    /// the effective timeout, the pair's own orders that are not over (open, or result unknown)
    /// are cancelled and looked up again first (fill-confirmation spec); the decision is then
    /// taken on those FINAL fills. A cancel or final lookup that cannot be confirmed leaves the
    /// leg unknown -> UNRESOLVED.
    fn check_fills(&mut self, pair: &str, now: i64) {
        if self.pairs.get(pair).map(|v| v.state) != Some(PairState::FillMonitor) {
            return;
        }
        let Some(set) = self.flows.get(pair).and_then(|f| f.open.clone()) else { return };
        if set.legs.iter().flatten().any(|o| o.cancel == CancelStep::InFlight) {
            return; // waiting for the final lookups after the timeout cancel
        }
        let (long, short) = (set.leg_fill(Leg::Long), set.leg_fill(Leg::Short));
        match fill::fill_decision(&set.eff, set.sent_at_ms, now, long, short) {
            FillDecision::Wait => {}
            FillDecision::Transition(event) => {
                let at_timeout =
                    matches!(event, SystemEvent::TimeoutNoFills | SystemEvent::TimeoutPartialFill | SystemEvent::TimeoutUndetermined);
                if at_timeout && !set.cancel_started && self.start_timeout_cancels(pair) {
                    return;
                }
                let mut detail = json!({ "long": format!("{long:?}"), "short": format!("{short:?}"), "sent_at_ms": set.sent_at_ms });
                if event == SystemEvent::TimeoutUndetermined {
                    // A leg whose submit never gave a usable answer: "result unknown"; otherwise the
                    // fill confirmation itself could not be completed.
                    let submit_unknown = set.legs.iter().flatten().any(|o| {
                        o.status.is_none() && matches!(o.outcome, None | Some(SubmitOutcome::Unknown { .. }))
                    });
                    let reason = if submit_unknown { AlertReason::SubmitUnknown } else { AlertReason::FillUnconfirmed };
                    detail["alert_reason"] = json!(reason.as_str());
                }
                let _ = self.land(pair, event, detail);
            }
        }
    }

    /// Cancel (own ids only, never a position) every opening order of the pair that is not over,
    /// then look each up again. Returns false when there is nothing to cancel.
    fn start_timeout_cancels(&mut self, pair: &str) -> bool {
        let Some(set) = self.flows.get_mut(pair).and_then(|f| f.open.as_mut()) else { return false };
        set.cancel_started = true;
        let mut todo = Vec::new();
        for o in set.legs.iter_mut().flatten() {
            if !o.is_over() {
                o.cancel = CancelStep::InFlight;
                todo.push(o.req.clone());
            }
        }
        for req in &todo {
            let (executor, db, tx, pair) = (self.executor.clone(), self.db.clone(), self.event_tx.clone(), pair.to_string());
            let req = req.clone();
            tokio::spawn(async move {
                let cancel = executor.cancel(req.exchange, &req.symbol, &req.client_order_id).await;
                let after = executor.query(req.exchange, &req.symbol, &req.client_order_id).await;
                if let QueryOutcome::Found(status) = &after
                    && let Ok(Some(row)) = db.get_intent(&req.client_order_id)
                    && IntentState::parse(&row.state) != Some(intent_state_of(status))
                {
                    let _ = intent::record_query_outcome(&db, &req.client_order_id, &after, executor.is_simulated());
                }
                let _ = tx.send(Event::CancelChecked { pair, client_order_id: req.client_order_id, cancel, after }).await;
            });
        }
        !todo.is_empty()
    }

    fn on_cancel_checked(&mut self, pair: &str, client_order_id: &str, cancel: QueryOutcome, after: QueryOutcome) {
        let simulated = self.pairs.get(pair).is_some_and(|v| v.simulated);
        let Some(o) = self.flows.get_mut(pair).and_then(|f| f.open.as_mut()).and_then(|s| s.find_mut(client_order_id)) else { return };
        o.cancel = CancelStep::Done;
        o.query_inflight = false;
        let verdict = match &after {
            QueryOutcome::Found(s) if s.state != OrderState::Open => {
                o.status = Some(s.clone());
                "final"
            }
            QueryOutcome::Found(s) => {
                o.status = Some(s.clone());
                o.final_unknown = true;
                "still open after cancel: unknown"
            }
            QueryOutcome::NotFound | QueryOutcome::Failed { .. } => {
                o.final_unknown = true;
                "final state not confirmed: unknown"
            }
        };
        let payload = json!({
            "client_order_id": client_order_id,
            "exchange": o.req.exchange.name(),
            "symbol": o.req.symbol,
            "cancel": query_json(&cancel),
            "after": query_json(&after),
            "verdict": verdict,
            "simulated": simulated,
        });
        self.note(ORDER_CANCEL_RESULT, Some(pair), payload);
        let now = self.clock.now_ms();
        self.check_fills(pair, now);
    }

    /// Spawn a lookup by `client_order_id` for every leg of `action`'s set that needs one.
    fn poll(&mut self, pair: &str, action: OrderAction) {
        let Some(flow) = self.flows.get_mut(pair) else { return };
        let Some(set) = flow.set_mut(action) else { return };
        let mut todo = Vec::new();
        for o in set.legs.iter_mut().flatten() {
            if o.needs_query() {
                o.query_inflight = true;
                todo.push(o.req.clone());
            }
        }
        for req in todo {
            self.spawn_lookup(pair, req);
        }
    }

    /// One lookup by `client_order_id`; the result comes back as `Event::Queried`.
    fn spawn_lookup(&self, pair: &str, req: OrderRequest) {
        let (executor, db, tx, pair) = (self.executor.clone(), self.db.clone(), self.event_tx.clone(), pair.to_string());
        tokio::spawn(async move {
            let outcome = executor.query(req.exchange, &req.symbol, &req.client_order_id).await;
            // Keep the intent in step with what the exchange reports (state changes only).
            if let QueryOutcome::Found(status) = &outcome
                && let Ok(Some(row)) = db.get_intent(&req.client_order_id)
                && IntentState::parse(&row.state) != Some(intent_state_of(status))
            {
                let _ = intent::record_query_outcome(&db, &req.client_order_id, &outcome, executor.is_simulated());
            }
            let _ = tx.send(Event::Queried { pair: Some(pair), client_order_id: req.client_order_id, outcome }).await;
        });
    }

    // ---- exit: -> CLOSING -> close orders -> flat check -> FINALIZED ----

    fn begin_close(&mut self, pair: &str, event: PairEvent, source: &str) -> CommandReply {
        if let Some(why) = self.pairs.get(pair).and_then(|v| self.executor_mismatch(v)) {
            return CommandReply::Rejected(why);
        }
        if let Err(e) = self.land(pair, event, json!({ "source": source })) {
            return CommandReply::Rejected(e);
        }
        let Some(view) = self.pairs.get(pair).cloned() else { return CommandReply::Accepted };
        let now = self.clock.now_ms();
        let flow = self.flows.entry(pair.to_string()).or_default();
        flow.close_triggered_ms = Some(now);
        flow.orphan = false;
        flow.close = None;
        flow.close_residual = [Decimal::ZERO; 2];
        flow.flat_inflight = false;
        // The pair's own orders per leg: open fills minus earlier close fills is what it holds.
        let mut orders: [Vec<PairOrder>; 2] = [Vec::new(), Vec::new()];
        match self.db.list_intents_for_pair(pair) {
            Ok(rows) => {
                for row in rows {
                    if let Some((leg, action)) = ids::leg_action_of(&row.client_order_id) {
                        let rejected = IntentState::parse(&row.state) == Some(IntentState::Failed);
                        orders[idx(leg)].push(PairOrder { client_order_id: row.client_order_id, action, rejected });
                    }
                }
            }
            Err(e) => {
                let _ = self.land(pair, SystemEvent::CloseFailed, json!({ "reason": format!("order intents unreadable: {e}") }));
                return CommandReply::Accepted;
            }
        }
        let (account, executor, tx) = (self.account_for(view.simulated), self.executor.clone(), self.event_tx.clone());
        let (le, se, sym, pair) = (view.long_exchange, view.short_exchange, view.symbol.clone(), pair.to_string());
        let [long_orders, short_orders] = orders;
        tokio::spawn(async move {
            let (long, short) =
                tokio::join!(signed_position(account.as_ref(), le, &sym), signed_position(account.as_ref(), se, &sym));
            let (long_recorded, short_recorded) = tokio::join!(
                recorded_fill(executor.as_ref(), le, &sym, &long_orders),
                recorded_fill(executor.as_ref(), se, &sym, &short_orders)
            );
            let _ = tx.send(Event::ClosePositionsFetched { pair, long, short, long_recorded, short_recorded }).await;
        });
        CommandReply::Accepted
    }

    /// Close quantity per leg = min(the pair's recorded fill, |actual position|) (decision
    /// 2026-10-05 evening). A leg whose position differs from the recorded fill by more than the
    /// effective `max_leg_imbalance_pct` (relative to the larger, base coin; equal passes), or
    /// whose recorded fill is unknown while a position exists, is not the pair's alone: nothing is
    /// sent on either leg, an alert with both quantities is written and the pair lands
    /// `CloseFailed` → PARTIAL_FAILURE for the user. Exposure that is not the pair's is never
    /// touched; the flat check then expects exactly that remainder (`Flow::close_residual`).
    fn on_close_positions(&mut self, pair: &str, positions: [Result<Decimal, String>; 2], recorded: [Result<Decimal, String>; 2]) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        if view.state != PairState::Closing {
            return;
        }
        // Closing is never held back by settings: unreadable settings use the defaults.
        let (risk, overrides) = self.load_risk().unwrap_or_else(|_| (RiskConfig::default(), RiskOverrides::new()));
        let eff = effective_for_pair(&risk, &overrides, view.long_exchange, view.short_exchange);
        let tolerance = eff.max_leg_imbalance_pct;
        let [long_pos, short_pos] = positions;
        let [long_rec, short_rec] = recorded;
        let mut plan: [(Decimal, Decimal); 2] = [(Decimal::ZERO, Decimal::ZERO); 2]; // (close qty, residual)
        let mut mismatches: Vec<Value> = Vec::new();
        for (leg, pos, rec, exchange) in
            [(Leg::Long, long_pos, long_rec, view.long_exchange), (Leg::Short, short_pos, short_rec, view.short_exchange)]
        {
            let pos = match pos {
                Ok(p) => p,
                Err(e) => {
                    let _ = self.land(pair, SystemEvent::CloseFailed, json!({ "reason": format!("{}: position unreadable: {e}", leg.as_str()) }));
                    return;
                }
            };
            let expected_sign_ok = match leg {
                Leg::Long => pos >= Decimal::ZERO,
                Leg::Short => pos <= Decimal::ZERO,
            };
            if !expected_sign_ok {
                let reason = format!("{}: position {pos} is on the wrong side; left for the user", leg.as_str());
                let _ = self.land(pair, SystemEvent::CloseFailed, json!({ "reason": reason }));
                return;
            }
            let actual = pos.abs();
            let alert = |recorded: Option<Decimal>, diff_pct: Option<Decimal>, reason: String| {
                json!({
                    "leg": leg.as_str(),
                    "exchange": exchange.name(),
                    "symbol": view.symbol,
                    "recorded_quantity": recorded.map(dstr),
                    "position_quantity": dstr(actual),
                    "diff_pct": diff_pct.map(dstr),
                    "tolerance_pct": dstr(tolerance),
                    "reason": reason,
                    "simulated": view.simulated,
                })
            };
            // Both quantities are in the leg's order unit, so the relative difference is the same
            // as in base coin (contracts x ct_val on OKX).
            let close_qty = match rec {
                // Nothing held and nothing known (e.g. a simulated pair after a restart: its
                // ledger is gone): nothing to close.
                Err(_) if actual.is_zero() => Decimal::ZERO,
                Err(e) => {
                    mismatches.push(alert(None, None, format!("recorded fill unknown ({e}); the position may not be the pair's")));
                    continue;
                }
                // Nothing filled and nothing held (e.g. the rejected leg of a PARTIAL_FAILURE).
                Ok(recorded) if recorded.is_zero() && actual.is_zero() => Decimal::ZERO,
                Ok(recorded) => {
                    // Larger of the two is > 0 here.
                    let diff_pct = (actual - recorded).abs() * Decimal::ONE_HUNDRED / actual.max(recorded);
                    if diff_pct > tolerance {
                        mismatches.push(alert(Some(recorded), Some(diff_pct), "position differs from the pair's recorded fill".into()));
                        continue;
                    }
                    actual.min(recorded)
                }
            };
            let residual = match leg {
                Leg::Long => pos - close_qty,
                Leg::Short => pos + close_qty,
            };
            plan[idx(leg)] = (close_qty, residual);
        }
        if !mismatches.is_empty() {
            for a in &mismatches {
                self.note(CLOSE_QUANTITY_MISMATCH, Some(pair), a.clone());
            }
            let detail = json!({ "reason": "position differs from the pair's recorded fill; nothing closed, left for the user", "legs": mismatches });
            let _ = self.land(pair, SystemEvent::CloseFailed, detail);
            return;
        }
        let prefix = self.id_prefix();
        let mut legs: [Option<LegOrder>; 2] = [None, None];
        for (leg, exchange) in [(Leg::Long, view.long_exchange), (Leg::Short, view.short_exchange)] {
            let (close_qty, _) = plan[idx(leg)];
            if close_qty.is_zero() {
                continue;
            }
            let req = OrderRequest {
                client_order_id: self.next_client_order_id(prefix, pair, leg, OrderAction::Close),
                exchange,
                symbol: view.symbol.clone(),
                side: OrderSide::for_leg(side_of(leg), OrderAction::Close),
                quantity: close_qty,
                reduce_only: true,
            };
            legs[idx(leg)] = Some(LegOrder::new(req, Decimal::ONE));
        }
        self.flows.entry(pair.to_string()).or_default().close_residual = [plan[0].1, plan[1].1];
        let set = OrderSet { sent_at_ms: self.clock.now_ms(), eff, legs, cancel_started: false };
        self.send_set(pair, OrderAction::Close, set);
        self.check_close(pair, self.clock.now_ms());
    }

    /// CLOSING: a rejected close → PARTIAL_FAILURE; all closes over (or timed out) → flat check.
    fn check_close(&mut self, pair: &str, now: i64) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        if view.state != PairState::Closing {
            return;
        }
        let Some(flow) = self.flows.get(pair) else { return };
        let Some(set) = flow.close.clone() else { return };
        let rejected: Vec<String> = Leg::BOTH
            .into_iter()
            .filter_map(|l| set.legs[idx(l)].as_ref().and_then(|o| o.submit_rejected().map(|r| format!("{}: {r}", l.as_str()))))
            .collect();
        if !rejected.is_empty() {
            let _ = self.land(pair, SystemEvent::CloseFailed, json!({ "reason": "close order rejected", "rejected": rejected }));
            return;
        }
        let all_over = set.legs.iter().flatten().all(LegOrder::is_over);
        if !(all_over || set.timed_out(now)) || flow.flat_inflight {
            return;
        }
        let flow = self.flows.entry(pair.to_string()).or_default();
        flow.flat_inflight = true;
        let residual = flow.close_residual;
        let (account, tx) = (self.account_for(view.simulated), self.event_tx.clone());
        let legs = [(view.long_exchange, view.symbol.clone(), residual[0]), (view.short_exchange, view.symbol.clone(), residual[1])];
        let pair = pair.to_string();
        tokio::spawn(async move {
            let flat = is_flat(account.as_ref(), &legs).await;
            let _ = tx.send(Event::FlatChecked { pair, flat }).await;
        });
    }

    fn on_flat_checked(&mut self, pair: &str, flat: Result<bool, String>) {
        let Some(flow) = self.flows.get_mut(pair) else { return };
        flow.flat_inflight = false;
        let timed_out = flow.close.as_ref().is_none_or(|s| s.timed_out(self.clock.now_ms()));
        if self.pairs.get(pair).map(|v| v.state) != Some(PairState::Closing) {
            return;
        }
        match flat {
            Ok(true) => {
                let residual = self.flows.get(pair).map_or([Decimal::ZERO; 2], |f| f.close_residual);
                let mut payload = json!({ "verified_flat": true, "simulated": self.pairs.get(pair).is_some_and(|v| v.simulated) });
                if residual.iter().any(|r| !r.is_zero()) {
                    // Exposure on the same symbol that was not the pair's and was left alone.
                    payload["left_untouched"] = json!({ "long": dstr(residual[0]), "short": dstr(residual[1]) });
                }
                // The confirmation is recorded before FINALIZED; without it nothing is finalized.
                if self.events.append(CLOSE_CONFIRMED, Some(pair), payload).is_ok() {
                    let _ = self.land(pair, SystemEvent::ClosedConfirmed { verified_flat: true }, json!({ "source": "flat check" }));
                }
            }
            // Position updates may lag: re-checked on the next tick until the close timeout.
            Ok(false) if timed_out => {
                let _ = self.land(pair, SystemEvent::CloseFailed, json!({ "reason": "not flat after closing (position or open order left)" }));
            }
            Ok(false) => {}
            Err(e) if timed_out => {
                let _ = self.land(pair, SystemEvent::CloseFailed, json!({ "reason": format!("flat check failed: {e}") }));
            }
            Err(_) => {} // retried on the next tick until the timeout
        }
    }

    // ---- manual orders: same executor, same intent path, not part of a pair ----

    fn manual_order(&mut self, o: ManualOrder) -> CommandReply {
        let prefix = self.id_prefix();
        let leg = match o.side {
            OrderSide::Buy => Leg::Long,
            OrderSide::Sell => Leg::Short,
        };
        let action = if o.reduce_only { OrderAction::Close } else { OrderAction::Open };
        let now = self.clock.now_ms();
        let (key, id) = loop {
            self.manual_seq += 1;
            let key = format!("manual-{now}-{}", self.manual_seq);
            let id = client_order_id(prefix, &key, leg, action, 0);
            match self.db.get_intent(&id) {
                Ok(None) => break (key, id),
                Ok(Some(_)) => continue,
                Err(e) => return CommandReply::Rejected(format!("cannot check order id: {e}")),
            }
        };
        let req = OrderRequest {
            client_order_id: id.clone(),
            exchange: o.exchange,
            symbol: o.symbol,
            side: o.side,
            quantity: o.quantity,
            reduce_only: o.reduce_only,
        };
        let (db, executor, tx) = (self.db.clone(), self.executor.clone(), self.event_tx.clone());
        tokio::spawn(async move {
            let outcome = match intent::submit_with_intent(&db, executor.as_ref(), &key, leg, req).await {
                Ok(report) => report.outcome,
                Err(IntentError::NotLanded(e)) => SubmitOutcome::Rejected { reason: format!("not sent: {e}") },
                Err(IntentError::ResultNotRecorded(e)) => SubmitOutcome::Unknown { reason: format!("result not recorded: {e}") },
            };
            let _ = tx.send(Event::ManualSubmitted { client_order_id: id, outcome }).await;
        });
        CommandReply::Accepted
    }

    /// Manual cancel by `client_order_id` on the current executor; never gated (only reduces
    /// exposure). The answer is recorded as is (`MANUAL_CANCEL_RESULT`), never shown as success
    /// unless the executor said so.
    fn manual_cancel(&mut self, exchange: Exchange, symbol: String, client_order_id: String) -> CommandReply {
        if client_order_id.trim().is_empty() {
            return CommandReply::Rejected("order id is empty".into());
        }
        let (executor, tx) = (self.executor.clone(), self.event_tx.clone());
        tokio::spawn(async move {
            let outcome = executor.cancel(exchange, &symbol, &client_order_id).await;
            let _ = tx.send(Event::ManualCancelled { exchange, symbol, client_order_id, outcome }).await;
        });
        CommandReply::Accepted
    }

    /// One-click submit (ui-trading-pages 1.2): each pair goes through `start_entry`, whose
    /// land-then-act PREPARED → PRE_TRADE_CHECK can succeed only once per pair, so a pair the
    /// AUTO scheduler already took is refused without any order. Accepted only when every pair
    /// was; otherwise the reply names each refused pair (the others still proceed).
    fn enter_selected(&mut self, pairs: Vec<PairUuid>) -> CommandReply {
        if pairs.is_empty() {
            return CommandReply::Rejected("no pair selected".into());
        }
        let mut refused = Vec::new();
        for p in &pairs {
            match self.start_entry(p, "user one-click") {
                CommandReply::Accepted => {}
                CommandReply::AlreadyPending => refused.push(format!("{p}: already pending")),
                CommandReply::Rejected(why) => refused.push(format!("{p}: {why}")),
            }
        }
        if refused.is_empty() {
            CommandReply::Accepted
        } else {
            CommandReply::Rejected(format!("{}/{} entered; refused: {}", pairs.len() - refused.len(), pairs.len(), refused.join("; ")))
        }
    }

    // ---- events ----

    /// Results of spawned I/O. Only here may they change state.
    fn on_event(&mut self, ev: Event) {
        self.dirty = true;
        match ev {
            Event::BaselineFetched { pair, long, short } => {
                for (leg, r) in [(Leg::Long, &long), (Leg::Short, &short)] {
                    if let Err(e) = r {
                        self.note(BASELINE_FETCH_FAILED, Some(&pair), json!({ "leg": leg.as_str(), "error": e }));
                    }
                }
                if let (Ok(l), Ok(s)) = (&long, &short)
                    && self.pairs.get(&pair).map(|v| v.state) == Some(PairState::Prepared)
                {
                    let (l, s) = (l.clone(), s.clone());
                    self.auto_cancel_check(&pair, &l, &s);
                }
                self.flows.entry(pair).or_default().baseline = Some((long, short));
            }
            Event::RecheckFetched { pair, long, short } => {
                self.flows.entry(pair.clone()).or_default().recheck_inflight = false;
                if let (Ok(l), Ok(s)) = (long, short)
                    && self.pairs.get(&pair).map(|v| v.state) == Some(PairState::Prepared)
                {
                    self.auto_cancel_check(&pair, &l, &s);
                }
            }
            Event::PretradeFetched { pair, long, short } => {
                self.flows.entry(pair.clone()).or_default().pretrade = Some((long, short));
                self.maybe_run_node0(&pair);
            }
            Event::MarginFetched { pair, long, short } => {
                self.flows.entry(pair.clone()).or_default().margins = Some((long, short));
                self.maybe_run_node0(&pair);
            }
            Event::EntryContextFetched { pair, long_rules, short_rules, long_foreign, short_foreign } => {
                let ctx = EntryContext { rules: [long_rules, short_rules], foreign: [long_foreign, short_foreign] };
                self.flows.entry(pair.clone()).or_default().context = Some(ctx);
                self.maybe_run_node0(&pair);
            }
            Event::Submitted { pair, leg, action, client_order_id, outcome } => {
                self.on_submitted(&pair, leg, action, &client_order_id, outcome);
            }
            Event::Queried { pair: Some(pair), client_order_id, outcome } => {
                let now = self.clock.now_ms();
                let simulated = self.pairs.get(&pair).is_some_and(|v| v.simulated);
                let Some(flow) = self.flows.get_mut(&pair) else { return };
                let mut action = None;
                let mut fill_event = None;
                for (a, set) in [(OrderAction::Open, flow.open.as_mut()), (OrderAction::Close, flow.close.as_mut())] {
                    if let Some(o) = set.and_then(|s| s.find_mut(&client_order_id)) {
                        o.query_inflight = false;
                        if o.cancel != CancelStep::NotNeeded {
                            break; // the timeout cancel's own lookup decides this leg
                        }
                        if let QueryOutcome::Found(status) = &outcome {
                            if fill_changed(o.status.as_ref(), status) {
                                let mut p = status_json(status);
                                p["client_order_id"] = json!(client_order_id);
                                p["exchange"] = json!(o.req.exchange.name());
                                p["symbol"] = json!(o.req.symbol);
                                p["requested_quantity"] = json!(dstr(o.req.quantity));
                                p["action"] = json!(match a {
                                    OrderAction::Open => "open",
                                    OrderAction::Close => "close",
                                });
                                p["simulated"] = json!(simulated);
                                fill_event = Some(p);
                            }
                            o.status = Some(status.clone());
                        }
                        action = Some(a);
                        break;
                    }
                }
                if let Some(mut p) = fill_event {
                    if let Some((leg, _)) = ids::leg_action_of(&client_order_id) {
                        p["leg"] = json!(leg.as_str());
                    }
                    self.note(ORDER_FILL, Some(&pair), p);
                }
                match action {
                    Some(OrderAction::Open) => self.check_fills(&pair, now),
                    Some(OrderAction::Close) => self.check_close(&pair, now),
                    None => {}
                }
            }
            Event::Queried { pair: None, .. } => {} // manual orders are not polled
            Event::ManualSubmitted { client_order_id, outcome } => {
                let mut payload = outcome_json(&outcome);
                payload["client_order_id"] = json!(client_order_id);
                payload["simulated"] = json!(self.executor.is_simulated());
                self.note(MANUAL_ORDER_RESULT, None, payload);
            }
            Event::ManualCancelled { exchange, symbol, client_order_id, outcome } => {
                let mut payload = query_json(&outcome);
                payload["exchange"] = json!(exchange.name());
                payload["symbol"] = json!(symbol);
                payload["client_order_id"] = json!(client_order_id);
                payload["simulated"] = json!(self.executor.is_simulated());
                self.note(MANUAL_CANCEL_RESULT, None, payload);
            }
            Event::ClosePositionsFetched { pair, long, short, long_recorded, short_recorded } => {
                self.on_close_positions(&pair, [long, short], [long_recorded, short_recorded]);
            }
            Event::FlatChecked { pair, flat } => self.on_flat_checked(&pair, flat),
            Event::ReconciliationDone { result } => self.on_reconciliation_done(result),
            Event::CancelChecked { pair, client_order_id, cancel, after } => self.on_cancel_checked(&pair, &client_order_id, cancel, after),
            Event::ConfirmChecked { pair, result } => self.on_confirm_checked(&pair, result),
        }
    }

    fn on_submitted(&mut self, pair: &str, leg: Leg, action: OrderAction, client_order_id: &str, outcome: SubmitOutcome) {
        let simulated = self.pairs.get(pair).is_some_and(|v| v.simulated);
        let mut payload = outcome_json(&outcome);
        payload["client_order_id"] = json!(client_order_id);
        payload["leg"] = json!(leg.as_str());
        payload["action"] = json!(match action {
            OrderAction::Open => "open",
            OrderAction::Close => "close",
        });
        payload["simulated"] = json!(simulated);
        if let Some(v) = self.pairs.get(pair) {
            let exchange = match leg {
                Leg::Long => v.long_exchange,
                Leg::Short => v.short_exchange,
            };
            payload["exchange"] = json!(exchange.name());
            payload["symbol"] = json!(v.symbol);
        }
        self.note(ORDER_SUBMITTED, Some(pair), payload);
        let Some(o) = self.flows.get_mut(pair).and_then(|f| f.set_mut(action)).and_then(|s| s.find_mut(client_order_id)) else {
            return; // not an order this pair is waiting for
        };
        let mut fee_lookup = None;
        if let SubmitOutcome::Accepted(status) = &outcome {
            o.status = Some(status.clone());
            // Filled in the ACK but without its fee (Binance reports fees per trade): one lookup
            // fetches it for the fill details (`ORDER_FILL`), whatever the pair does next.
            if status.state == OrderState::Filled && status.fee.is_none() && status.filled_quantity > Decimal::ZERO {
                o.query_inflight = true;
                fee_lookup = Some(o.req.clone());
            }
        }
        o.outcome = Some(outcome);
        if let Some(req) = fee_lookup {
            self.spawn_lookup(pair, req);
        }
        let now = self.clock.now_ms();
        match (self.pairs.get(pair).map(|v| v.state), action) {
            (Some(PairState::OrderSubmit), OrderAction::Open) => self.check_submitted(pair, now),
            (Some(PairState::FillMonitor), OrderAction::Open) => self.check_fills(pair, now),
            (Some(PairState::Closing), OrderAction::Close) => self.check_close(pair, now),
            _ => {}
        }
    }

    // ---- pairs, modes, config ----

    fn add_prepared(&mut self, p: NewPreparedPair) -> CommandReply {
        let simulated = self.execution_mode == ExecutionMode::Simulation;
        let env = PairEnvelope {
            long_exchange: p.long_exchange,
            short_exchange: p.short_exchange,
            settlement_ms: p.settlement_ms,
            simulated,
            scan: p.entry,
        };
        let entry = match serde_json::to_value(&env) {
            Ok(v) => v,
            Err(e) => return CommandReply::Rejected(format!("cannot encode pair: {e}")),
        };
        let row = NewPair {
            internal_uuid: p.internal_uuid.clone(),
            pair_id: p.pair_id.clone(),
            symbol: p.symbol.clone(),
            status: PairState::Prepared,
            entry,
        };
        let payload = json!({ "symbol": p.symbol, "settlement_ms": p.settlement_ms, "simulated": simulated });
        // Atomic in the store (unique index) and written together with its event.
        match self.db.add_pair_with_event(&row, PAIR_PREPARED, &payload) {
            Ok(AddPairOutcome::Added) => {
                let view = PairView {
                    internal_uuid: p.internal_uuid.clone(),
                    pair_id: p.pair_id,
                    symbol: p.symbol,
                    long_exchange: env.long_exchange,
                    short_exchange: env.short_exchange,
                    state: PairState::Prepared,
                    settlement_ms: env.settlement_ms,
                    simulated,
                };
                self.flows.insert(p.internal_uuid.clone(), Flow::new(env.scan, false));
                self.pairs.insert(p.internal_uuid, view);
                CommandReply::Accepted
            }
            Ok(AddPairOutcome::AlreadyPending) => CommandReply::AlreadyPending,
            Err(e) => CommandReply::Rejected(format!("cannot add pair: {e}")),
        }
    }

    /// Land a pair transition (store first, `simulated` marker added to the detail), then update
    /// memory. `Err` carries the reason for the user.
    fn land(&mut self, pair: &str, event: impl Into<PairEvent>, detail: Value) -> Result<PairState, String> {
        let Some((from, simulated)) = self.pairs.get(pair).map(|v| (v.state, v.simulated)) else {
            return Err(format!("unknown pair {pair}"));
        };
        let mut detail = detail;
        if let Value::Object(m) = &mut detail {
            m.insert("simulated".into(), json!(simulated));
        }
        match transition::land_then_act(&self.events, pair, from, event, detail, |_| ()) {
            Ok((to, ())) => {
                if let Some(v) = self.pairs.get_mut(pair) {
                    v.state = to;
                }
                // The actor (a user command) moved it: it is no longer the reconciler's.
                self.reconcile_scope.remove(pair);
                self.dirty = true;
                self.sync_alert_for(pair);
                Ok(to)
            }
            Err(e @ TransitionError::Stale { .. }) => {
                // Memory disagreed with the store: adopt the stored state, act on nothing.
                if let Ok(Some(row)) = self.db.get_pair(pair)
                    && let (Ok(s), Some(v)) = (row.status.parse::<PairState>(), self.pairs.get_mut(pair))
                {
                    v.state = s;
                }
                Err(e.to_string())
            }
            Err(e @ (TransitionError::Illegal(_) | TransitionError::StoreFailed(_))) => Err(e.to_string()),
        }
    }

    fn transition(&mut self, pair: &str, event: impl Into<PairEvent>, detail: Value) -> CommandReply {
        match self.land(pair, event, detail) {
            Ok(_) => CommandReply::Accepted,
            Err(e) => CommandReply::Rejected(e),
        }
    }

    fn set_trigger_mode(&mut self, target: TriggerMode) -> CommandReply {
        if target == self.trigger_mode {
            return CommandReply::Accepted;
        }
        match gate::set_trigger_mode(&self.db, self.trigger_mode, target) {
            Ok(()) => {
                self.trigger_mode = target;
                CommandReply::Accepted
            }
            Err(e) => CommandReply::Rejected(e),
        }
    }

    /// Allowed iff every open pair belongs to `target` (`PairView::simulated`); see
    /// `gate::switch_execution_mode`.
    fn set_execution_mode(&mut self, target: ExecutionMode) -> CommandReply {
        let target_simulated = target == ExecutionMode::Simulation;
        let other_mode_open =
            self.pairs.values().filter(|v| transition::is_open(v.state) && v.simulated != target_simulated).count();
        match gate::switch_execution_mode(&self.db, self.factory.as_ref(), self.execution_mode, target, other_mode_open) {
            ModeSwitch::Unchanged => CommandReply::Accepted,
            ModeSwitch::Switched { mode, executor } => {
                self.execution_mode = mode;
                self.executor = executor.unwrap_or_else(|| self.simulator.clone());
                match mode {
                    ExecutionMode::ExchangeDemo => {
                        // The fallback no longer applies; a pending reconciliation of the demo
                        // pairs can now use the demo executor (no need to wait for the retry).
                        self.notices.retain(|n| n.code != gate::EXECUTION_MODE_FALLBACK);
                        if self.reconciler.is_some() {
                            self.start_reconciliation();
                        }
                    }
                    ExecutionMode::Simulation => {}
                }
                CommandReply::Accepted
            }
            ModeSwitch::Refused(why) => CommandReply::Rejected(why),
        }
    }

    /// Validate with core (`RiskConfig::from_json` / `parse_overrides`) before storing.
    fn update_config(&mut self, key: &str, value: Value) -> CommandReply {
        let valid = match key {
            CONFIG_RISK => RiskConfig::from_json(&value.to_string()).map(|_| ()).map_err(|e| e.to_string()),
            CONFIG_RISK_OVERRIDES => parse_overrides(&value).map(|_| ()).map_err(|e| e.to_string()),
            other => Err(format!("unknown config key {other:?}")),
        };
        if let Err(why) = valid {
            return CommandReply::Rejected(why);
        }
        let version = match self.db.config_get(key) {
            Ok(e) => e.map(|e| e.version),
            Err(e) => return CommandReply::Rejected(format!("cannot read {key}: {e}")),
        };
        match self.db.config_set(key, &value, version) {
            Ok(v) => {
                self.note(CONFIG_UPDATED, None, json!({ "key": key, "version": v }));
                CommandReply::Accepted
            }
            Err(e) => CommandReply::Rejected(format!("cannot save {key}: {e}")),
        }
    }

    /// Risk page save: both values validated by core first, then stored with one
    /// `RISK_CONFIG_UPDATED` (before / after) in a single transaction. Nothing changes on failure.
    fn save_risk_settings(&mut self, risk: Value, overrides: Value) -> CommandReply {
        if let Err(e) = RiskConfig::from_json(&risk.to_string()) {
            return CommandReply::Rejected(format!("risk: {e}"));
        }
        if let Err(e) = parse_overrides(&overrides) {
            return CommandReply::Rejected(format!("risk_overrides: {e}"));
        }
        let (old_risk, old_ov) = match (self.db.config_get(CONFIG_RISK), self.db.config_get(CONFIG_RISK_OVERRIDES)) {
            (Ok(r), Ok(o)) => (r, o),
            (Err(e), _) | (_, Err(e)) => return CommandReply::Rejected(format!("cannot read settings: {e}")),
        };
        let payload = json!({
            "before": { "risk": old_risk.as_ref().map(|e| e.value.clone()), "overrides": old_ov.as_ref().map(|e| e.value.clone()) },
            "after": { "risk": risk, "overrides": overrides },
        });
        let changes = [
            ConfigChange { key: CONFIG_RISK.into(), value: risk, expected_version: old_risk.map(|e| e.version) },
            ConfigChange { key: CONFIG_RISK_OVERRIDES.into(), value: overrides, expected_version: old_ov.map(|e| e.version) },
        ];
        match self.db.config_set_many_with_event(&changes, RISK_CONFIG_UPDATED, &payload) {
            Ok(_) => CommandReply::Accepted,
            Err(e) => CommandReply::Rejected(format!("cannot save risk settings: {e}")),
        }
    }

    /// Contract page save: per-leg notional and leverage must be > 0; stored with
    /// `CONTRACT_SETTINGS_UPDATED` (before / after). Existing pairs are not touched.
    fn save_contract_template(&mut self, notional_usdt: Decimal, leverage: Decimal) -> CommandReply {
        for (name, v) in [("notional_usdt", notional_usdt), ("leverage", leverage)] {
            if v <= Decimal::ZERO {
                return CommandReply::Rejected(format!("{name} must be > 0"));
            }
        }
        let old = match self.db.config_get(CONFIG_CONTRACT_TEMPLATE) {
            Ok(e) => e,
            Err(e) => return CommandReply::Rejected(format!("cannot read {CONFIG_CONTRACT_TEMPLATE}: {e}")),
        };
        let value = json!({ "notional_usdt": dstr(notional_usdt), "leverage": dstr(leverage) });
        let payload = json!({ "before": old.as_ref().map(|e| e.value.clone()), "after": value });
        let change = ConfigChange { key: CONFIG_CONTRACT_TEMPLATE.into(), value, expected_version: old.map(|e| e.version) };
        match self.db.config_set_many_with_event(&[change], CONTRACT_SETTINGS_UPDATED, &payload) {
            Ok(_) => CommandReply::Accepted,
            Err(e) => CommandReply::Rejected(format!("cannot save contract template: {e}")),
        }
    }

    /// Stored global settings and overrides (validated by core). Missing keys = defaults / none.
    fn load_risk(&self) -> Result<(RiskConfig, RiskOverrides), String> {
        load_risk_config(&self.db)
    }

    // ---- order plumbing ----

    fn id_prefix(&self) -> IdPrefix {
        if self.executor.is_simulated() { IdPrefix::Sim } else { IdPrefix::Demo }
    }

    /// Positions / open orders of a pair come from the simulated ledger for simulated pairs.
    fn account_for(&self, simulated: bool) -> Arc<dyn AccountView> {
        if simulated { self.sim_account.clone() } else { self.account.clone() }
    }

    /// The first sequence number whose id has no intent yet (a new number only for a genuinely
    /// new order; an order with an unknown result is never re-sent under a new id).
    fn next_client_order_id(&self, prefix: IdPrefix, pair: &str, leg: Leg, action: OrderAction) -> String {
        let mut seq: u16 = 0;
        loop {
            let id = client_order_id(prefix, pair, leg, action, seq);
            match self.db.get_intent(&id) {
                Ok(Some(_)) if seq < u16::MAX => seq += 1,
                // Free, or the store cannot tell: landing the intent will then fail (duplicate
                // or halted) and nothing is sent.
                Ok(Some(_) | None) | Err(_) => return id,
            }
        }
    }

    /// The single order path: lands the intent (INTENDED, SUBMITTED) and submits `req` on the
    /// current executor in a spawned task; the result comes back as `Event::Submitted` (and, for
    /// an unknown result, the follow-up query under the same id as `Event::Queried`). Callers
    /// must have landed the pair transition first (land then act).
    ///
    /// Every submit writes one `ORDER_LATENCY` event (request sent / ACK on the injected clock,
    /// `triggered_at` of the entry or close) from the spawned task.
    fn spawn_submit(&self, pair: PairUuid, leg: Leg, action: OrderAction, req: OrderRequest, triggered_at: Option<i64>) {
        let (db, executor, tx, clock, events) = (self.db.clone(), self.executor.clone(), self.event_tx.clone(), self.clock.clone(), self.events.clone());
        tokio::spawn(async move {
            let client_order_id = req.client_order_id.clone();
            let exchange = req.exchange;
            let result = intent::submit_with_intent_clocked(&db, executor.as_ref(), Some(clock.as_ref()), &pair, leg, req).await;
            if let Ok(report) = &result
                && let Some(t) = report.timing
            {
                let class = match &report.outcome {
                    SubmitOutcome::Accepted(_) => "accepted",
                    SubmitOutcome::Rejected { .. } => "rejected",
                    SubmitOutcome::Unknown { .. } => "unknown",
                };
                let action_str = match action {
                    OrderAction::Open => "open",
                    OrderAction::Close => "close",
                };
                let payload = latency::latency_payload(
                    &pair,
                    leg.as_str(),
                    action_str,
                    exchange.name(),
                    &client_order_id,
                    t.request_sent_at_ms,
                    t.ack_at_ms,
                    class,
                    triggered_at,
                    executor.is_simulated(),
                );
                let _ = events.append(ORDER_LATENCY, Some(&pair), payload);
            }
            let (outcome, query) = match result {
                Ok(report) => (report.outcome, report.query),
                // The executor was not called: nothing exists on the exchange.
                Err(IntentError::NotLanded(e)) => (SubmitOutcome::Rejected { reason: format!("not sent: {e}") }, None),
                // The executor was called; what happened is unknown to us.
                Err(IntentError::ResultNotRecorded(e)) => {
                    (SubmitOutcome::Unknown { reason: format!("result not recorded: {e}") }, None)
                }
            };
            let _ = tx.send(Event::Submitted { pair: pair.clone(), leg, action, client_order_id: client_order_id.clone(), outcome }).await;
            if let Some(outcome) = query {
                let _ = tx.send(Event::Queried { pair: Some(pair), client_order_id, outcome }).await;
            }
        });
    }

    /// Best-effort event (a halted store cannot write; the halt is the record).
    fn note(&self, event_type: &str, pair: Option<&str>, payload: Value) {
        let _ = self.events.append(event_type, pair, payload);
    }

    /// Write `event_type` once per `(pair, key)`.
    fn report_once(&mut self, pair: &str, key: String, event_type: &str, payload: Value) {
        if self.reported.insert((pair.to_string(), key)) {
            self.note(event_type, Some(pair), payload);
        }
    }

    // ---- snapshots ----

    fn interval(&self) -> Duration {
        Duration::from_millis(self.timings.snapshot_min_interval_ms.max(0) as u64)
    }

    /// When the pending (dirty) Snapshot may be pushed; `None` when nothing changed.
    fn next_push_at(&self) -> Option<Instant> {
        self.dirty.then(|| self.last_push.map_or_else(tokio::time::Instant::now, |t| t + self.interval()))
    }

    fn maybe_publish(&mut self) {
        if self.dirty && self.last_push.is_none_or(|t| tokio::time::Instant::now() >= t + self.interval()) {
            self.snapshot_tx.send_replace(self.snapshot());
            self.last_push = Some(tokio::time::Instant::now());
            self.dirty = false;
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            now_ms: self.clock.now_ms(),
            trigger_mode: self.trigger_mode,
            execution_mode: self.execution_mode,
            pairs: self.pairs.values().cloned().collect(),
            blockers: gate::current_blockers(&self.db, self.reconciliation_pending.as_deref()),
            prices: self.market_rx.borrow().iter().map(|((e, s), p)| (*e, s.clone(), *p)).collect(),
            notices: self.notices.clone(),
            alerts: self
                .pairs
                .values()
                .filter(|v| alert::is_alert_state(v.state))
                .map(|v| Alert {
                    pair: v.internal_uuid.clone(),
                    pair_id: v.pair_id.clone(),
                    symbol: v.symbol.clone(),
                    state: v.state,
                    simulated: v.simulated,
                    reason: self.alert_reasons.get(&v.internal_uuid).cloned(),
                })
                .collect(),
        }
    }

    // ---- alerts (partial-failure-alerting) ----

    fn sync_alerts(&mut self) {
        let ids: Vec<PairUuid> = self.pairs.keys().cloned().collect();
        for id in ids {
            self.sync_alert_for(&id);
        }
    }

    /// For a pair in an alert state: write the entry's `PAIR_ALERT` once and start its one
    /// notification once (both keyed by the transition event that entered the state, so a restart
    /// does neither again). Any other state: forget the reason (the banner is derived anyway).
    fn sync_alert_for(&mut self, pair: &str) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        if !alert::is_alert_state(view.state) {
            self.alert_reasons.remove(pair);
            return;
        }
        let entry = match alert::entry_of(&self.db, pair, view.state) {
            Ok(Some(e)) => e,
            Ok(None) | Err(_) => return, // no entry event (e.g. seeded state): banner only
        };
        let reason = match entry.alerted {
            Some(r) => r,
            None => {
                let r = AlertReason::of_transition(&entry.transition);
                let payload = json!({
                    "reason": r.as_str(),
                    "state": view.state.as_str(),
                    "from": entry.transition.get("from").cloned().unwrap_or(Value::Null),
                    "transition_id": entry.transition_id,
                    "pair_id": view.pair_id,
                    "symbol": view.symbol,
                    "long_exchange": view.long_exchange.name(),
                    "short_exchange": view.short_exchange.name(),
                    "detail": entry.transition.get("detail").cloned().unwrap_or(Value::Null),
                    "legs": self.legs_json(pair),
                    "simulated": view.simulated,
                });
                if self.events.append(PAIR_ALERT, Some(pair), payload).is_err() {
                    return; // halted store: the halt is the record; the banner still shows
                }
                r
            }
        };
        self.alert_reasons.insert(pair.to_string(), reason.as_str().to_string());
        if entry.notified || !self.notify_started.insert((pair.to_string(), entry.transition_id)) {
            return;
        }
        let notice = AlertNotice {
            pair: pair.to_string(),
            pair_id: view.pair_id.clone(),
            symbol: view.symbol.clone(),
            state: view.state,
            reason,
            simulated: view.simulated,
        };
        let (notifier, events, transition_id) = (self.notifier.clone(), self.events.clone(), entry.transition_id);
        // Off the actor: a notifier may block; its failure changes nothing but one event.
        tokio::task::spawn_blocking(move || {
            let payload = |extra: Value| {
                let mut p = json!({ "state": notice.state.as_str(), "reason": notice.reason.as_str(), "transition_id": transition_id });
                if let (Value::Object(m), Value::Object(x)) = (&mut p, extra) {
                    m.extend(x);
                }
                p
            };
            match notifier.notify(&notice) {
                Ok(()) => {
                    let _ = events.append(alert::ALERT_NOTIFIED, Some(&notice.pair), payload(json!({})));
                }
                Err(e) => {
                    let _ = events.append(alert::ALERT_NOTIFY_FAILED, Some(&notice.pair), payload(json!({ "error": e })));
                }
            }
        });
    }

    /// Both legs' orders and fills as known in memory (kept in the alert event: the filled leg's
    /// data is never dropped).
    fn legs_json(&self, pair: &str) -> Value {
        let Some(flow) = self.flows.get(pair) else { return Value::Null };
        let set_json = |set: &Option<OrderSet>| -> Value {
            let Some(set) = set else { return Value::Null };
            let legs: Vec<Value> = Leg::BOTH
                .into_iter()
                .filter_map(|leg| {
                    set.legs[idx(leg)].as_ref().map(|o| {
                        let mut v = match &o.status {
                            Some(s) => status_json(s),
                            None => json!({}),
                        };
                        v["leg"] = json!(leg.as_str());
                        v["client_order_id"] = json!(o.req.client_order_id);
                        v["exchange"] = json!(o.req.exchange.name());
                        v["requested_quantity"] = json!(dstr(o.req.quantity));
                        v["submit"] = match &o.outcome {
                            Some(out) => outcome_json(out),
                            None => json!({ "result": "no reply" }),
                        };
                        v["final_unknown"] = json!(o.final_unknown);
                        v
                    })
                })
                .collect();
            json!(legs)
        };
        json!({ "open": set_json(&flow.open), "close": set_json(&flow.close) })
    }

    // ---- confirm closed (manual exit 2 of 2) ----

    /// Starts the re-query of both legs for "confirm closed". Only for pairs in an alert state.
    fn start_confirm(&mut self, pair: &str) -> Result<(), CommandReply> {
        let Some(view) = self.pairs.get(pair).cloned() else {
            return Err(CommandReply::Rejected(format!("unknown pair {pair}")));
        };
        if !alert::is_alert_state(view.state) {
            return Err(CommandReply::Rejected(format!(
                "pair is {}: confirm closed applies to PARTIAL_FAILURE / IMBALANCED / UNRESOLVED",
                view.state
            )));
        }
        if self.pending_confirms.contains_key(pair) {
            return Ok(()); // one re-query answers every waiting confirm
        }
        self.pending_confirms.insert(pair.to_string(), Vec::new());
        let (account, tx) = (self.account_for(view.simulated), self.event_tx.clone());
        let legs = [(view.long_exchange, view.symbol.clone()), (view.short_exchange, view.symbol.clone())];
        let pair = pair.to_string();
        tokio::spawn(async move {
            let result = flat_report(account.as_ref(), &legs).await;
            let _ = tx.send(Event::ConfirmChecked { pair, result }).await;
        });
        Ok(())
    }

    fn on_confirm_checked(&mut self, pair: &str, result: Result<FlatReport, String>) {
        let waiting = self.pending_confirms.remove(pair).unwrap_or_default();
        let simulated = self.pairs.get(pair).is_some_and(|v| v.simulated);
        let reply = match result {
            Ok(r) if r.is_flat() => {
                let detail = json!({ "source": "user", "verified_by": "system re-query of positions and open orders" });
                match self.land(pair, ManualEvent::ConfirmClosed { verified_flat: true }, detail) {
                    Ok(_) => {
                        self.note(MANUAL_CONFIRM_RESULT, Some(pair), json!({ "accepted": true, "simulated": simulated }));
                        CommandReply::Accepted
                    }
                    Err(e) => CommandReply::Rejected(e),
                }
            }
            Ok(r) => {
                let payload = json!({
                    "accepted": false,
                    "positions": { "long": dstr(r.positions[0]), "short": dstr(r.positions[1]) },
                    "open_orders": { "long": r.open_orders[0], "short": r.open_orders[1] },
                    "simulated": simulated,
                });
                self.note(MANUAL_CONFIRM_RESULT, Some(pair), payload);
                CommandReply::Rejected(format!(
                    "not flat: long position {}, short position {}, open orders long {} / short {}",
                    r.positions[0], r.positions[1], r.open_orders[0], r.open_orders[1]
                ))
            }
            Err(e) => {
                self.note(MANUAL_CONFIRM_RESULT, Some(pair), json!({ "accepted": false, "error": e, "simulated": simulated }));
                CommandReply::Rejected(format!("cannot verify that the pair is flat: {e}"))
            }
        };
        for tx in waiting {
            let _ = tx.send(reply.clone());
        }
    }
}

/// A lookup reports something new about an order's fill (quantity, state, price or fee).
fn fill_changed(before: Option<&OrderStatus>, now: &OrderStatus) -> bool {
    before.is_none_or(|b| {
        (b.filled_quantity, b.state, b.avg_price, b.fee, &b.fee_asset)
            != (now.filled_quantity, now.state, now.avg_price, now.fee, &now.fee_asset)
    })
}

/// The intent state an order status settles to (mirrors `intent::settled_state`).
fn intent_state_of(status: &OrderStatus) -> IntentState {
    match status.state {
        OrderState::Open => IntentState::Acknowledged,
        OrderState::Filled => IntentState::Filled,
        OrderState::Cancelled => IntentState::Cancelled,
        OrderState::Rejected => IntentState::Failed,
    }
}

/// What Node 0 and Node 1 used for an entry, written with the ORDER_SUBMIT transition: per leg the
/// expected (pre-trade) price that sized the order, the baseline and scan prices and the funding
/// data; the Net Edge recomputed from the same pre-trade funding data with its threshold; notional
/// and leverage. Decimals are strings.
fn entry_snapshot_json(
    view: &PairView,
    entry: &EntrySnapshot,
    eff: &EffectiveConfig,
    pretrade: [&FreshQuote; 2],
    baseline: [Option<&FreshQuote>; 2],
    scan: &Value,
) -> Value {
    let leg = |exchange: Exchange, pre: &FreshQuote, base: Option<&FreshQuote>, scan_price: Decimal| {
        json!({
            "exchange": exchange.name(),
            "expected_price": dstr(pre.price),
            "price_observed_at_ms": pre.price_observed_at_ms,
            "baseline_price": base.map(|b| dstr(b.price)),
            "scan_price": dstr(scan_price),
            "funding_rate": dstr(pre.funding.funding_rate),
            "funding_interval_secs": pre.funding.funding_interval_secs,
            "next_funding_time": pre.funding.next_funding_time,
            "mark_price": dstr(pre.funding.mark_price),
        })
    };
    let net_edge = match node0::net_edge_with_threshold(&pretrade[0].funding, &pretrade[1].funding, entry.notional_usdt, eff) {
        Ok((e, threshold)) => json!({
            "net_edge_pct": dstr(e.net_edge_pct),
            "net_edge_usdt": dstr(e.net_edge_usdt),
            "funding_income_usdt": dstr(e.funding_income_usdt),
            "fee_usdt": dstr(e.fee_usdt),
            "slippage_usdt": dstr(e.slippage_usdt),
            "safety_margin_usdt": dstr(e.safety_margin_usdt),
            "gross_spread": dstr(e.gross_spread),
            "threshold_pct": dstr(threshold),
        }),
        Err(why) => json!({ "unavailable": why }),
    };
    json!({
        "long": leg(view.long_exchange, pretrade[0], baseline[0], entry.long_scan_price),
        "short": leg(view.short_exchange, pretrade[1], baseline[1], entry.short_scan_price),
        "notional_usdt": dstr(entry.notional_usdt),
        "leverage": dstr(entry.leverage),
        "net_edge": net_edge,
        "scan_net_edge_pct": scan.get("net_edge_pct").cloned(),
    })
}

/// Fill fields of an order status (cumulative for the order at the time of the report; the
/// latest ORDER_SUBMITTED / ORDER_FILL of a `client_order_id` is its fill). Quantities are in the
/// exchange order unit (contracts on OKX).
fn status_json(s: &OrderStatus) -> Value {
    json!({
        "state": format!("{:?}", s.state),
        "filled_quantity": dstr(s.filled_quantity),
        "avg_price": s.avg_price.map(dstr),
        "fee": s.fee.map(dstr),
        "fee_asset": s.fee_asset,
        "exchange_order_id": s.exchange_order_id,
    })
}

fn outcome_json(outcome: &SubmitOutcome) -> Value {
    match outcome {
        SubmitOutcome::Accepted(s) => {
            let mut v = status_json(s);
            v["outcome"] = json!("accepted");
            v
        }
        SubmitOutcome::Rejected { reason } => json!({ "outcome": "rejected", "reason": reason }),
        SubmitOutcome::Unknown { reason } => json!({ "outcome": "unknown", "reason": reason }),
    }
}

/// A position or open order on `symbol` that is not this pair's (the pair has none before entry).
/// An incomplete list is not "nothing" (fail closed).
async fn foreign_exposure(account: &dyn AccountView, exchange: Exchange, symbol: &str) -> Result<bool, String> {
    let positions = account.positions(exchange).await?;
    let orders = account.open_orders(exchange).await?;
    if !positions.complete || !orders.complete {
        return Err(format!("{} account list incomplete", exchange.name()));
    }
    Ok(positions.items.iter().any(|p| p.symbol == symbol && !p.quantity.is_zero())
        || orders.items.iter().any(|o| o.symbol == symbol))
}

/// Signed position on `symbol` (exchange order unit).
async fn signed_position(account: &dyn AccountView, exchange: Exchange, symbol: &str) -> Result<Decimal, String> {
    let positions = account.positions(exchange).await?;
    if !positions.complete {
        return Err(format!("{} positions list incomplete", exchange.name()));
    }
    Ok(positions.items.iter().filter(|p| p.symbol == symbol).map(|p| p.quantity).sum())
}

/// One of a pair's orders on a leg, from its order intent.
#[derive(Debug, Clone)]
struct PairOrder {
    client_order_id: String,
    action: OrderAction,
    /// Intent FAILED: the order never existed on the exchange.
    rejected: bool,
}

/// The pair's recorded fill on one leg (exchange order unit): the `filled_quantity` of its open
/// orders minus that of its earlier close orders (a retried close after a partial one), each
/// looked up by `client_order_id` on the pair's executor, so it also works after a restart. A
/// rejected intent counts 0 without a lookup. An order that cannot be found or looked up makes
/// the leg unknown (`Err`): never guessed.
async fn recorded_fill(executor: &dyn Executor, exchange: Exchange, symbol: &str, orders: &[PairOrder]) -> Result<Decimal, String> {
    let mut total = Decimal::ZERO;
    for o in orders.iter().filter(|o| !o.rejected) {
        let filled = match executor.query(exchange, symbol, &o.client_order_id).await {
            QueryOutcome::Found(status) => status.filled_quantity,
            QueryOutcome::NotFound => return Err(format!("order {} not found", o.client_order_id)),
            QueryOutcome::Failed { reason } => return Err(format!("order {} lookup failed: {reason}", o.client_order_id)),
        };
        match o.action {
            OrderAction::Open => total += filled,
            OrderAction::Close => total -= filled,
        }
    }
    Ok(total.max(Decimal::ZERO))
}

/// Closed confirmation: every leg's position equals what is expected to remain (the exposure
/// that is not the pair's; normally 0) and no open order is left on its symbol.
async fn is_flat(account: &dyn AccountView, legs: &[(Exchange, String, Decimal); 2]) -> Result<bool, String> {
    for (exchange, symbol, residual) in legs {
        if signed_position(account, *exchange, symbol).await? != *residual {
            return Ok(false);
        }
        let orders = account.open_orders(*exchange).await?;
        if !orders.complete {
            return Err(format!("{} open orders list incomplete", exchange.name()));
        }
        if orders.items.iter().any(|o| &o.symbol == symbol) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Global risk settings and per-exchange overrides from `config` (missing = defaults / none).
pub(crate) fn load_risk_config(db: &Db) -> Result<(RiskConfig, RiskOverrides), String> {
    let risk = match db.config_get(CONFIG_RISK) {
        Ok(None) => RiskConfig::default(),
        Ok(Some(e)) => RiskConfig::from_json(&e.value.to_string()).map_err(|e| format!("{CONFIG_RISK}: {e}"))?,
        Err(e) => return Err(format!("{CONFIG_RISK}: {e}")),
    };
    let overrides = match db.config_get(CONFIG_RISK_OVERRIDES) {
        Ok(None) => RiskOverrides::new(),
        Ok(Some(e)) => parse_overrides(&e.value).map_err(|e| format!("{CONFIG_RISK_OVERRIDES}: {e}"))?,
        Err(e) => return Err(format!("{CONFIG_RISK_OVERRIDES}: {e}")),
    };
    Ok((risk, overrides))
}

/// What startup must reconcile before exposure is allowed: the pairs that need it (in flight,
/// or RECONCILED simulated) and the `pair_uuid` of every unfinished intent, plus the reason
/// shown while it is pending. A failed intent listing counts as "must" (fail closed).
fn startup_reconciliation(db: &Db, pairs: &BTreeMap<PairUuid, PairView>) -> (BTreeSet<PairUuid>, Option<String>) {
    let mut scope: BTreeSet<PairUuid> = pairs.values().filter(|v| needs_reconciliation(v)).map(|v| v.internal_uuid.clone()).collect();
    let flying = scope.len();
    let intents = match db.list_unfinished_intents() {
        Ok(v) => v,
        Err(e) => return (scope, Some(format!("cannot list unfinished order intents: {e}"))),
    };
    scope.extend(intents.iter().map(|i| i.pair_uuid.clone()));
    let reason = (!scope.is_empty())
        .then(|| format!("{} unfinished order intent(s), {flying} pair(s) in flight at startup", intents.len()));
    (scope, reason)
}

fn dummy_snapshot() -> Snapshot {
    Snapshot {
        now_ms: 0,
        trigger_mode: TriggerMode::Manual,
        execution_mode: ExecutionMode::Simulation,
        pairs: Vec::new(),
        blockers: Vec::<Blocker>::new(),
        prices: Vec::new(),
        notices: Vec::new(),
        alerts: Vec::new(),
    }
}

fn query_json(o: &QueryOutcome) -> Value {
    match o {
        QueryOutcome::Found(s) => {
            let mut v = status_json(s);
            v["result"] = json!("found");
            v
        }
        QueryOutcome::NotFound => json!({ "result": "not found" }),
        QueryOutcome::Failed { reason } => json!({ "result": "failed", "reason": reason }),
    }
}

/// Positions and open-order counts of both legs' symbol (complete lists only).
async fn flat_report(account: &dyn AccountView, legs: &[(Exchange, String); 2]) -> Result<FlatReport, String> {
    let mut positions = [Decimal::ZERO; 2];
    let mut open_orders = [0usize; 2];
    for (i, (exchange, symbol)) in legs.iter().enumerate() {
        positions[i] = signed_position(account, *exchange, symbol).await?;
        let orders = account.open_orders(*exchange).await?;
        if !orders.complete {
            return Err(format!("{} open orders list incomplete", exchange.name()));
        }
        open_orders[i] = orders.items.iter().filter(|o| &o.symbol == symbol).count();
    }
    Ok(FlatReport { positions, open_orders })
}

/// Open pairs from the store with their scan snapshot (they count for mode switching and limits
/// after a restart). An open pair row that cannot be read halts the store: an unknown pair may
/// hold exposure (fail closed).
fn load_open_pairs(db: &Db) -> Vec<(PairView, Value)> {
    let Ok(rows) = db.list_pairs() else { return Vec::new() };
    rows.into_iter()
        .filter(|row| row.status.parse::<PairState>().map_or(true, transition::is_open))
        .filter_map(|row| view_of_row(db, row))
        .collect()
}

/// A pair row as the actor sees it, with its scan snapshot. An unreadable state or entry halts
/// the store (an unknown pair may hold exposure; fail closed).
fn view_of_row(db: &Db, row: crate::store::state::PairRow) -> Option<(PairView, Value)> {
    let state = match row.status.parse::<PairState>() {
        Ok(s) => s,
        Err(e) => {
            db.halt(HaltReason::ConfigReadFailed(format!("pair {}: {e}", row.internal_uuid)));
            return None;
        }
    };
    match serde_json::from_value::<PairEnvelope>(row.entry) {
        Ok(env) => Some((
            PairView {
                internal_uuid: row.internal_uuid,
                pair_id: row.pair_id,
                symbol: row.symbol,
                long_exchange: env.long_exchange,
                short_exchange: env.short_exchange,
                state,
                settlement_ms: env.settlement_ms,
                simulated: env.simulated,
            },
            env.scan,
        )),
        Err(e) => {
            db.halt(HaltReason::ConfigReadFailed(format!("pair {} entry unreadable: {e}", row.internal_uuid)));
            None
        }
    }
}

/// Short label for events (no payload data).
fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Tick => "Tick",
        Command::AddPrepared(_) => "AddPrepared",
        Command::EntryTrigger { .. } => "EntryTrigger",
        Command::ManualEnter { .. } => "ManualEnter",
        Command::EnterSelected { .. } => "EnterSelected",
        Command::ManualOrder(_) => "ManualOrder",
        Command::ManualCancel { .. } => "ManualCancel",
        Command::AutoExit { .. } => "AutoExit",
        Command::ManualExit { .. } => "ManualExit",
        Command::ManualClose { .. } => "ManualClose",
        Command::ConfirmClosed { .. } => "ConfirmClosed",
        Command::CancelPrepared { .. } => "CancelPrepared",
        Command::SetTriggerMode(_) => "SetTriggerMode",
        Command::SetExecutionMode(_) => "SetExecutionMode",
        Command::UpdateConfig { .. } => "UpdateConfig",
        Command::SaveRiskSettings { .. } => "SaveRiskSettings",
        Command::SaveContractTemplate { .. } => "SaveContractTemplate",
        Command::SetKillSwitch { .. } => "SetKillSwitch",
    }
}

fn pair_of(c: &Command) -> Option<&str> {
    match c {
        Command::EntryTrigger { pair }
        | Command::ManualEnter { pair }
        | Command::AutoExit { pair }
        | Command::ManualExit { pair }
        | Command::ManualClose { pair }
        | Command::ConfirmClosed { pair, .. }
        | Command::CancelPrepared { pair, .. } => Some(pair),
        Command::AddPrepared(p) => Some(&p.internal_uuid),
        Command::Tick
        | Command::ManualOrder(_)
        | Command::EnterSelected { .. }
        | Command::ManualCancel { .. }
        | Command::SetTriggerMode(_)
        | Command::SetExecutionMode(_)
        | Command::UpdateConfig { .. }
        | Command::SaveRiskSettings { .. }
        | Command::SaveContractTemplate { .. }
        | Command::SetKillSwitch { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::command::{Blocker, NewPreparedPair};
    use crate::engine::gate::test_support::{CountingFactory, NullExecutor, delete_kill_switch_row};
    use crate::engine::ports::{
        AccountOrder, AccountPosition, BoxFut, FreshQuote, Leg, Listed, OrderAction, OrderRequest, OrderRules, OrderSide,
    };
    use crate::engine::transition::{ILLEGAL_TRANSITION, PAIR_TRANSITION};
    use crate::store::db::test_support::tempdir;
    use crate::store::state::{FLAG_EXECUTION_MODE, FLAG_TRIGGER_MODE, NewPair};
    use serde_json::json;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::time::{Instant, sleep};
    use tong_funding_core::pair::PairState;
    use tong_funding_core::risk::{ExecutionMode, TriggerMode};

    /// Test clock that follows tokio's (paused) time, so ticks and timestamps agree.
    struct TokioClock {
        base_ms: i64,
        start: Instant,
    }
    impl Clock for TokioClock {
        fn now_ms(&self) -> i64 {
            self.base_ms + self.start.elapsed().as_millis() as i64
        }
    }

    const T0: i64 = 1_700_000_000_000;

    /// Ports that know nothing: no market data, no offsets, no account (these tests do not trade).
    struct NoPorts;
    impl MarketData for NoPorts {
        fn refetch(&self, _: Exchange, _: &str) -> BoxFut<'_, Result<FreshQuote, String>> {
            Box::pin(std::future::ready(Err("no market data in this test".into())))
        }
        fn order_rules(&self, _: Exchange, _: &str) -> BoxFut<'_, Result<OrderRules, String>> {
            Box::pin(std::future::ready(Err("no rules in this test".into())))
        }
    }
    impl ServerOffsets for NoPorts {
        fn offset_ms(&self, _: Exchange) -> Option<i64> {
            None
        }
    }
    impl AccountView for NoPorts {
        fn positions(&self, _: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
            Box::pin(std::future::ready(Err("no account in this test".into())))
        }
        fn open_orders(&self, _: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
            Box::pin(std::future::ready(Err("no account in this test".into())))
        }
        fn available_margin(&self, _: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
            Box::pin(std::future::ready(Err("no account in this test".into())))
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        db: Db,
        sim: Arc<NullExecutor>,
        factory: Arc<CountingFactory>,
    }

    fn rig_with(factory: Arc<CountingFactory>) -> (Rig, EngineDeps) {
        let dir = tempdir();
        let clock: Arc<dyn Clock> = Arc::new(TokioClock { base_ms: T0, start: tokio::time::Instant::now() });
        // Unlocked: the instance lock is keyed by inode, and an actor task of a test that just
        // finished may still hold a handle on a file whose inode a new tempdir reuses.
        let db = Db::open_unlocked(&dir.path().join("funding.db"), clock.clone());
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        let sim = NullExecutor::sim();
        let deps = EngineDeps {
            db: db.clone(),
            clock,
            timings: EngineTimings::default(),
            simulator: sim.clone(),
            factory: factory.clone(),
            market: Arc::new(NoPorts),
            offsets: Arc::new(NoPorts),
            account: Arc::new(NoPorts),
            sim_account: Arc::new(NoPorts),
            reconciler: None,
            notifier: Arc::new(crate::engine::alert::RecordingNotifier::default()),
        };
        (Rig { _dir: dir, db, sim, factory }, deps)
    }

    fn rig() -> (Rig, EngineDeps) {
        rig_with(CountingFactory::ok())
    }

    async fn ask(h: &EngineHandle, c: Command) -> CommandReply {
        tokio::time::timeout(Duration::from_secs(5), h.send(c)).await.expect("engine did not reply")
    }

    fn new_pair(uuid: &str, symbol: &str) -> NewPreparedPair {
        NewPreparedPair {
            internal_uuid: uuid.into(),
            pair_id: format!("pid-{uuid}"),
            symbol: symbol.into(),
            long_exchange: Exchange::Binance,
            short_exchange: Exchange::Bybit,
            settlement_ms: T0 + 3_600_000,
            entry: json!({"edge": "0.001"}),
        }
    }

    /// A pair already in the store (as after a restart) in `state`, settling in an hour (so no
    /// scheduled exit happens during these tests).
    fn seed(db: &Db, uuid: &str, symbol: &str, state: PairState) {
        let env = PairEnvelope {
            long_exchange: Exchange::Binance,
            short_exchange: Exchange::Bybit,
            settlement_ms: T0 + 3_600_000,
            simulated: true,
            scan: json!({}),
        };
        let p = NewPair {
            internal_uuid: uuid.into(),
            pair_id: format!("pid-{uuid}"),
            symbol: symbol.into(),
            status: state,
            entry: serde_json::to_value(env).unwrap(),
        };
        db.add_pair_if_not_pending(&p).unwrap();
    }

    fn status(db: &Db, uuid: &str) -> String {
        db.get_pair(uuid).unwrap().unwrap().status
    }

    fn count_events(db: &Db, ty: &str) -> i64 {
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        plain.query_row("SELECT COUNT(*) FROM events WHERE event_type = ?1", [ty], |r| r.get(0)).unwrap()
    }

    fn break_event_inserts(db: &Db) {
        db.with_raw_conn_for_tests(|c| {
            Ok(c.execute_batch("CREATE TRIGGER inject_fail BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected failure'); END")?)
        })
        .unwrap();
    }

    fn refused(r: &CommandReply, needle: &str) -> bool {
        matches!(r, CommandReply::Rejected(why) if why.starts_with("refused") && why.contains(needle))
    }

    // ---- 1.2 actor loop ----------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_submit_that_never_resolves_does_not_stop_ticks_or_commands() {
        let (rig, deps) = rig();
        let (actor, h) = Actor::new(deps);
        let req = OrderRequest {
            client_order_id: "sim-hang".into(),
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            side: OrderSide::Buy,
            quantity: Decimal::new(1, 3),
            reduce_only: false,
        };
        actor.spawn_submit("u-hang".into(), Leg::Long, OrderAction::Open, req, None);
        tokio::spawn(actor.run());
        sleep(Duration::from_millis(10)).await;
        assert_eq!(rig.sim.submits.load(Ordering::SeqCst), 1, "the hanging submit is in flight");

        let mut snaps = h.snapshots.clone();
        let mut seen_now = Vec::new();
        for i in 0..30 {
            sleep(Duration::from_secs(1)).await;
            seen_now.push(snaps.borrow_and_update().now_ms);
            let r = ask(&h, Command::AddPrepared(new_pair(&format!("u{i}"), &format!("SYM{i}")))).await;
            assert_eq!(r, CommandReply::Accepted, "command {i} while a submit hangs");
        }
        // One tick per second keeps refreshing the snapshot clock during the 30 s.
        assert!(seen_now.windows(2).all(|w| w[1] > w[0]), "{seen_now:?}");
        assert!(*seen_now.last().unwrap() >= T0 + 29_000, "{seen_now:?}");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(snaps.borrow().pairs.len(), 30);
    }

    /// Answers each submit after a per-exchange delay on tokio's (paused) clock and logs when each
    /// call entered and returned.
    struct DelayedExchange {
        delay_ms: std::collections::HashMap<Exchange, u64>,
        log: std::sync::Mutex<Vec<(String, Instant)>>,
    }
    impl Executor for DelayedExchange {
        fn is_simulated(&self) -> bool {
            false
        }
        fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
            self.log.lock().unwrap().push((format!("enter {}", req.exchange.name()), tokio::time::Instant::now()));
            let delay = self.delay_ms[&req.exchange];
            Box::pin(async move {
                sleep(Duration::from_millis(delay)).await;
                self.log.lock().unwrap().push((format!("return {}", req.exchange.name()), tokio::time::Instant::now()));
                let status = OrderStatus {
                    client_order_id: req.client_order_id.clone(),
                    exchange_order_id: Some("x".into()),
                    filled_quantity: Decimal::ZERO,
                    avg_price: None,
                    fee: None,
                    fee_asset: None,
                    state: OrderState::Open,
                };
                SubmitOutcome::Accepted(status)
            })
        }
        fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            Box::pin(std::future::ready(QueryOutcome::NotFound))
        }
        fn query(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            Box::pin(std::future::ready(QueryOutcome::NotFound))
        }
    }

    // ---- exchange-demo-execution 1.3: parallel legs and latency events ------------------

    #[tokio::test(start_paused = true)]
    async fn both_legs_are_sent_before_either_answers_and_latency_events_carry_sent_ack_and_latency() {
        let (rig, mut deps) = rig();
        let x = Arc::new(DelayedExchange {
            delay_ms: [(Exchange::Binance, 180), (Exchange::Bybit, 200)].into_iter().collect(),
            log: std::sync::Mutex::default(),
        });
        deps.simulator = x.clone();
        let (actor, _h) = Actor::new(deps);
        let id = |leg| client_order_id(IdPrefix::Demo, "pair-lat", leg, OrderAction::Open, 0);
        let req = |leg, exchange, side| OrderRequest {
            client_order_id: id(leg),
            exchange,
            symbol: "BTCUSDT".into(),
            side,
            quantity: Decimal::new(19, 3),
            reduce_only: false,
        };
        let triggered = T0 - 40;
        let start = tokio::time::Instant::now();
        actor.spawn_submit("pair-lat".into(), Leg::Long, OrderAction::Open, req(Leg::Long, Exchange::Binance, OrderSide::Buy), Some(triggered));
        actor.spawn_submit("pair-lat".into(), Leg::Short, OrderAction::Open, req(Leg::Short, Exchange::Bybit, OrderSide::Sell), Some(triggered));
        tokio::spawn(actor.run());
        sleep(Duration::from_millis(400)).await;

        let log = x.log.lock().unwrap().clone();
        let names: Vec<&str> = log.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(&names[..2], ["enter Binance", "enter Bybit"], "both requests out before any answer: {names:?}");
        let last_return = log.iter().filter(|(n, _)| n.starts_with("return")).map(|(_, t)| *t).max().unwrap();
        assert_eq!((last_return - start).as_millis(), 200, "parallel: about 200 ms in total, not 380");

        let plain = rusqlite::Connection::open(rig.db.path()).unwrap();
        let mut st = plain.prepare("SELECT payload FROM events WHERE event_type = ?1 ORDER BY id").unwrap();
        let events: Vec<Value> = st
            .query_map([latency::ORDER_LATENCY], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect();
        assert_eq!(events.len(), 2, "{events:?}");
        let by_leg = |leg: &str| events.iter().find(|e| e["leg"] == json!(leg)).unwrap().clone();
        let (long, short) = (by_leg("long"), by_leg("short"));
        assert_eq!(long["latency_ms"], json!(180));
        assert_eq!(short["latency_ms"], json!(200));
        assert_eq!(long["client_order_id"], json!(id(Leg::Long)));
        assert_eq!(long["request_sent_at"], json!(T0));
        assert_eq!(long["ack_at"], json!(T0 + 180));
        assert_eq!((long["result"].clone(), long["triggered_at"].clone()), (json!("accepted"), json!(triggered)));
        // Aggregation for the T-5 decision: trigger -> both accepted = 200 + 40.
        let report = latency::LatencyReport::from_events(&events, false);
        assert_eq!(report.entry_to_both_accepted.unwrap().p99, 240);
        assert_eq!(report.t5_criterion_met(), Some(true));
    }

    #[tokio::test(start_paused = true)]
    async fn a_thousand_market_updates_in_a_second_yield_at_most_four_snapshots() {
        let (_rig, deps) = rig();
        let h = start(deps);
        let mut snaps = h.snapshots.clone();
        sleep(Duration::from_millis(500)).await;
        snaps.borrow_and_update();
        let key = (Exchange::Binance, "BTCUSDT".to_string());
        let mut pushes = 0;
        for k in 1..=1000i64 {
            h.market.send_modify(|m| {
                m.insert(key.clone(), Decimal::new(60_000_00 + k, 2));
            });
            sleep(Duration::from_millis(1)).await;
            if snaps.has_changed().unwrap() {
                pushes += 1;
                snaps.borrow_and_update();
            }
        }
        assert!((1..=4).contains(&pushes), "{pushes} snapshots in one second");
        sleep(Duration::from_millis(300)).await;
        let last = snaps.borrow_and_update().clone();
        assert_eq!(last.prices, vec![(Exchange::Binance, "BTCUSDT".to_string(), Decimal::new(60_000_00 + 1000, 2))]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_ui_that_stops_reading_snapshots_does_not_grow_a_queue() {
        let (_rig, deps) = rig();
        let h = start(deps);
        // Bounded by construction: a `watch` keeps one value; commands are a bounded mpsc.
        let _: &watch::Receiver<Snapshot> = &h.snapshots;
        assert_eq!(h.commands.max_capacity(), COMMAND_CAPACITY);
        let key = (Exchange::Bybit, "ETHUSDT".to_string());
        for k in 0..10_000i64 {
            h.market.send_modify(|m| {
                m.insert(key.clone(), Decimal::new(k, 0));
            });
            if k % 10 == 0 {
                sleep(Duration::from_millis(10)).await; // 10 s in total, nobody reads snapshots
            }
        }
        assert_eq!(h.commands.capacity(), COMMAND_CAPACITY, "no command backlog");
        let snap = h.snapshots.borrow().clone();
        assert!(snap.now_ms >= T0 + 9_000, "ticks kept being processed: {}", snap.now_ms);
        assert_eq!(ask(&h, Command::Tick).await, CommandReply::Accepted);
    }

    // ---- 1.3 land then act -------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn two_concurrent_adds_for_one_symbol_accept_exactly_one() {
        let (rig, deps) = rig();
        let h = start(deps);
        let (a, b) = tokio::join!(
            ask(&h, Command::AddPrepared(new_pair("ua", "BTCUSDT"))),
            ask(&h, Command::AddPrepared(new_pair("ub", "BTCUSDT")))
        );
        let mut got = [a, b];
        got.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(got, [CommandReply::Accepted, CommandReply::AlreadyPending]);
        let n: i64 = rig
            .db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM pairs WHERE symbol = 'BTCUSDT'", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_transition_write_halts_and_refuses_exposure_afterwards() {
        let (rig, deps) = rig();
        let h = start(deps);
        assert_eq!(ask(&h, Command::AddPrepared(new_pair("u1", "BTCUSDT"))).await, CommandReply::Accepted);
        break_event_inserts(&rig.db);
        let r = ask(&h, Command::CancelPrepared { pair: "u1".into(), reason: "test".into() }).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("store")), "{r:?}");
        assert!(rig.db.is_halted());
        for c in [Command::ManualEnter { pair: "u1".into() }, Command::EntryTrigger { pair: "u1".into() }] {
            let r = ask(&h, c).await;
            assert!(refused(&r, "store halted"), "{r:?}");
        }
        assert_eq!(rig.sim.submits.load(Ordering::SeqCst), 0, "Executor never called");
        sleep(Duration::from_millis(300)).await;
        let snap = h.snapshots.borrow().clone();
        assert!(snap.blockers.iter().any(|b| matches!(b, Blocker::StoreHalted(_))), "{:?}", snap.blockers);
        assert_eq!(snap.pairs[0].state, PairState::Prepared, "memory did not move ahead of the store");
    }

    #[tokio::test(start_paused = true)]
    async fn a_landed_cancel_moves_the_pair_and_writes_one_event() {
        let (rig, deps) = rig();
        let h = start(deps);
        ask(&h, Command::AddPrepared(new_pair("u1", "BTCUSDT"))).await;
        let r = ask(&h, Command::CancelPrepared { pair: "u1".into(), reason: "user".into() }).await;
        assert_eq!(r, CommandReply::Accepted);
        assert_eq!(status(&rig.db, "u1"), "CANCELLED");
        assert_eq!(count_events(&rig.db, PAIR_TRANSITION), 1);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().pairs[0].state, PairState::Cancelled);
        assert_eq!(ask(&h, Command::AddPrepared(new_pair("u2", "BTCUSDT"))).await, CommandReply::Accepted);
    }

    #[tokio::test(start_paused = true)]
    async fn an_illegal_transition_keeps_the_state_and_writes_an_event() {
        let (rig, deps) = rig();
        seed(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        let r = ask(&h, Command::CancelPrepared { pair: "u1".into(), reason: "x".into() }).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("illegal")), "{r:?}");
        assert_eq!(status(&rig.db, "u1"), "RECONCILED");
        assert_eq!(count_events(&rig.db, ILLEGAL_TRANSITION), 1);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().pairs[0].state, PairState::Reconciled);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_pair_is_rejected() {
        let (_rig, deps) = rig();
        let h = start(deps);
        let r = ask(&h, Command::CancelPrepared { pair: "nope".into(), reason: "x".into() }).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("unknown pair")), "{r:?}");
    }

    // ---- 3.2 gating --------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn the_kill_switch_refuses_the_entry_trigger_and_records_it() {
        let (rig, deps) = rig();
        let h = start(deps);
        ask(&h, Command::AddPrepared(new_pair("u1", "BTCUSDT"))).await;
        assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
        let r = ask(&h, Command::EntryTrigger { pair: "u1".into() }).await;
        assert!(refused(&r, "kill switch"), "{r:?}");
        assert_eq!(count_events(&rig.db, "COMMAND_REFUSED"), 1);
        assert_eq!(status(&rig.db, "u1"), "PREPARED");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().blockers, vec![Blocker::KillSwitch]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_kill_switch_never_closes_a_reconciled_pair() {
        let (rig, deps) = rig();
        seed(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
        sleep(Duration::from_secs(30)).await;
        assert_eq!(status(&rig.db, "u1"), "RECONCILED");
        assert_eq!(h.snapshots.borrow().pairs[0].state, PairState::Reconciled);
        assert_eq!(rig.sim.submits.load(Ordering::SeqCst), 0, "no order while halted");
        assert_eq!(count_events(&rig.db, PAIR_TRANSITION), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_kill_switch_lets_a_manual_close_through_the_gate() {
        let (rig, deps) = rig();
        seed(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        ask(&h, Command::SetKillSwitch { on: true }).await;
        let r = ask(&h, Command::ManualClose { pair: "u1".into() }).await;
        assert_eq!(r, CommandReply::Accepted, "a manual close is accepted while halted");
        assert_eq!(count_events(&rig.db, "COMMAND_REFUSED"), 0);
        // It landed RECONCILED -> CLOSING (this rig has no account, so the close then fails
        // into PARTIAL_FAILURE; the full close is covered in flow_tests).
        let plain = rusqlite::Connection::open(rig.db.path()).unwrap();
        let to_closing: i64 = plain
            .query_row(
                "SELECT COUNT(*) FROM events WHERE event_type = 'PAIR_TRANSITION' AND json_extract(payload, '$.to') = 'CLOSING'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(to_closing, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_kill_switch_counts_as_halted() {
        let (rig, deps) = rig();
        let h = start(deps);
        delete_kill_switch_row(&rig.db);
        let r = ask(&h, Command::ManualEnter { pair: "u1".into() }).await;
        assert!(refused(&r, "kill switch unreadable"), "{r:?}");
        sleep(Duration::from_millis(300)).await;
        let b = h.snapshots.borrow().blockers.clone();
        assert!(matches!(b.as_slice(), [Blocker::KillSwitchUnreadable(_)]), "{b:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_two_modes_change_independently() {
        let (rig, deps) = rig();
        let h = start(deps);
        assert_eq!(ask(&h, Command::SetTriggerMode(TriggerMode::Auto)).await, CommandReply::Accepted);
        assert_eq!(rig.db.flag_get(FLAG_TRIGGER_MODE).unwrap().as_deref(), Some("AUTO"));
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap(), None, "execution_mode untouched");
        assert_eq!(ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await, CommandReply::Accepted);
        assert_eq!(rig.db.flag_get(FLAG_TRIGGER_MODE).unwrap().as_deref(), Some("AUTO"), "trigger_mode untouched");
        assert_eq!(ask(&h, Command::SetTriggerMode(TriggerMode::Manual)).await, CommandReply::Accepted);
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap().as_deref(), Some("EXCHANGE_DEMO"));
        sleep(Duration::from_millis(300)).await;
        let s = h.snapshots.borrow().clone();
        assert_eq!((s.trigger_mode, s.execution_mode), (TriggerMode::Manual, ExecutionMode::ExchangeDemo));
        assert_eq!(rig.factory.calls(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn switching_execution_mode_is_refused_while_a_pair_is_open() {
        let (rig, deps) = rig();
        seed(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        let r = ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("open")), "{r:?}");
        assert_eq!(rig.factory.calls(), 0);
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation);
    }

    #[tokio::test(start_paused = true)]
    async fn a_factory_failure_keeps_simulation() {
        let (rig, deps) = rig_with(CountingFactory::failing("keychain locked"));
        let h = start(deps);
        let r = ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("keychain locked")), "{r:?}");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation);
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_factory_is_never_called_during_simulation() {
        let (rig, deps) = rig();
        let h = start(deps);
        ask(&h, Command::AddPrepared(new_pair("u1", "BTCUSDT"))).await;
        ask(&h, Command::SetTriggerMode(TriggerMode::Auto)).await;
        ask(&h, Command::SetExecutionMode(ExecutionMode::Simulation)).await;
        ask(&h, Command::SetKillSwitch { on: true }).await;
        ask(&h, Command::SetKillSwitch { on: false }).await;
        ask(&h, Command::EntryTrigger { pair: "u1".into() }).await;
        ask(&h, Command::CancelPrepared { pair: "u1".into(), reason: "done".into() }).await;
        sleep(Duration::from_secs(60)).await;
        assert_eq!(rig.factory.calls(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stored_exchange_demo_whose_executor_fails_starts_in_simulation() {
        let (rig, deps) = rig_with(CountingFactory::failing("no keys"));
        rig.db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        let h = start(deps);
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation);
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap().as_deref(), Some("SIMULATION"));
        assert_eq!(count_events(&rig.db, crate::engine::gate::EXECUTION_MODE_FALLBACK), 1);
    }

    /// Like `seed`, but the pair belongs to EXCHANGE_DEMO.
    fn seed_demo(db: &Db, uuid: &str, symbol: &str, state: PairState) {
        seed(db, uuid, symbol, state);
        let sql = format!("UPDATE pairs SET entry_json = json_set(entry_json, '$.simulated', json('false')) WHERE internal_uuid = '{uuid}'");
        db.with_conn(|c| Ok(c.execute(&sql, [])?)).unwrap();
    }

    /// The post-fallback recovery path (decision 2026-10-05 evening): a stored EXCHANGE_DEMO fell
    /// back to SIMULATION for lack of keys, leaving a demo pair open. Once the keys work, switching
    /// to EXCHANGE_DEMO is allowed because every open pair belongs to EXCHANGE_DEMO.
    #[tokio::test(start_paused = true)]
    async fn sim_to_demo_is_allowed_when_every_open_pair_is_a_demo_pair() {
        let (rig, deps) = rig_with(CountingFactory::failing("no keys"));
        rig.db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        seed_demo(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation, "fell back");
        *rig.factory.fail.lock().unwrap() = None; // keys fixed
        let r = ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await;
        assert_eq!(r, CommandReply::Accepted);
        assert_eq!(rig.factory.calls(), 2, "startup attempt + the key check of the switch");
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap().as_deref(), Some("EXCHANGE_DEMO"));
        sleep(Duration::from_millis(300)).await;
        let s = h.snapshots.borrow().clone();
        assert_eq!(s.execution_mode, ExecutionMode::ExchangeDemo);
        assert!(s.notices.is_empty(), "the fallback notice is gone once EXCHANGE_DEMO works: {:?}", s.notices);
    }

    #[tokio::test(start_paused = true)]
    async fn sim_to_demo_with_keys_still_missing_keeps_simulation() {
        let (rig, deps) = rig_with(CountingFactory::failing("no keys"));
        rig.db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        seed_demo(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        let r = ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("no keys")), "{r:?}");
        assert_eq!(rig.factory.calls(), 2, "the switch checked the keys again");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation);
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap().as_deref(), Some("SIMULATION"));
    }

    #[tokio::test(start_paused = true)]
    async fn sim_to_demo_is_refused_while_a_sim_pair_is_open() {
        let (rig, deps) = rig();
        seed_demo(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        seed(&rig.db, "u2", "ETHUSDT", PairState::Prepared);
        let h = start(deps);
        let r = ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("SIMULATION")), "{r:?}");
        assert_eq!(rig.factory.calls(), 0, "refused before the key check");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation);
    }

    #[tokio::test(start_paused = true)]
    async fn demo_to_sim_is_refused_while_a_demo_pair_is_open() {
        let (rig, deps) = rig();
        rig.db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        seed_demo(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        let h = start(deps);
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::ExchangeDemo);
        let r = ask(&h, Command::SetExecutionMode(ExecutionMode::Simulation)).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("EXCHANGE_DEMO")), "{r:?}");
        sleep(Duration::from_millis(300)).await;
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::ExchangeDemo);
        assert_eq!(rig.db.flag_get(FLAG_EXECUTION_MODE).unwrap().as_deref(), Some("EXCHANGE_DEMO"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_startup_fallback_puts_a_notice_in_the_snapshot() {
        let (rig, deps) = rig_with(CountingFactory::failing("keychain: item not found"));
        rig.db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        let h = start(deps);
        let s = h.snapshots.borrow().clone();
        assert_eq!(s.execution_mode, ExecutionMode::Simulation);
        assert_eq!(s.notices.len(), 1, "{:?}", s.notices);
        assert_eq!(s.notices[0].code, crate::engine::gate::EXECUTION_MODE_FALLBACK);
        assert!(s.notices[0].message.contains("keychain: item not found"), "{:?}", s.notices[0]);
        assert_eq!(count_events(&rig.db, crate::engine::gate::EXECUTION_MODE_FALLBACK), 1, "the event stays");
    }

    #[tokio::test(start_paused = true)]
    async fn no_notice_without_a_fallback() {
        let (_rig, deps) = rig();
        let h = start(deps);
        assert!(h.snapshots.borrow().notices.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_demo_pair_is_never_closed_through_the_simulator() {
        // Stored EXCHANGE_DEMO whose executor cannot be built falls back to SIMULATION; a close of
        // a real demo pair must not be routed to the simulator (it would close nothing on the
        // exchange while looking closed).
        let (rig, deps) = rig_with(CountingFactory::failing("no keys"));
        rig.db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        seed(&rig.db, "u1", "BTCUSDT", PairState::Reconciled);
        rig.db.with_conn(|c| Ok(c.execute("UPDATE pairs SET entry_json = json_set(entry_json, '$.simulated', json('false')) WHERE internal_uuid = 'u1'", [])?)).unwrap();
        let h = start(deps);
        assert_eq!(h.snapshots.borrow().execution_mode, ExecutionMode::Simulation);
        for cmd in [Command::ManualClose { pair: "u1".into() }, Command::ManualExit { pair: "u1".into() }] {
            let r = ask(&h, cmd).await;
            assert!(matches!(&r, CommandReply::Rejected(m) if m.contains("EXCHANGE_DEMO")), "{r:?}");
        }
        assert_eq!(status(&rig.db, "u1"), "RECONCILED", "no transition landed");
        assert!(rig.db.list_unfinished_intents().unwrap().is_empty(), "no close order intent was written");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_engine_rejects_commands() {
        let (_rig, deps) = rig();
        let h = start(deps);
        let EngineHandle { commands, snapshots, market } = h;
        drop(commands); // the actor exits once every command sender is gone
        sleep(Duration::from_millis(10)).await;
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let h = EngineHandle { commands: tx, snapshots, market };
        assert!(matches!(h.send(Command::Tick).await, CommandReply::Rejected(_)));
    }
}

//! The engine actor (design D1–D3; task 1.2). One tokio task owns every piece of mutable trading
//! state. Inputs: `Command`s (bounded mpsc, each may carry a oneshot for the reply), internal
//! `Event`s from spawned I/O tasks, market prices on a `watch` (latest value only) and the
//! scheduler tick. Output: rate-limited `Snapshot`s on a `watch`. The actor never awaits exchange
//! I/O; it spawns it and handles the result when the `Event` comes back.
//!
//! Every command, the scheduler's own `Tick` and whatever the scheduler triggers go through
//! [`Actor::dispatch`], the one place where the gate (`opens_exposure` vs kill switch / halted
//! store / pending reconciliation) is applied.
//!
//! A panic inside the actor ends the task: every later `send` gets `Rejected("engine stopped")`
//! and the snapshot stops updating. There is no automatic restart (design Risks; unverified).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, sleep_until};
use tong_funding_core::pair::{Event as PairEvent, ManualEvent, PairState};
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::{Decimal, Exchange};

use super::command::{Command, CommandReply, Event, PairUuid, PairView, Snapshot};
use super::gate::{self, ModeSwitch};
use super::ports::{Executor, ExecutorFactory, Leg, OrderAction, OrderRequest};
use super::timings::EngineTimings;
use super::transition::{self, TransitionError};
use crate::ports::Clock;
use crate::store::db::{Db, HaltReason};
use crate::store::events::EventStore;
use crate::store::state::{AddPairOutcome, FLAG_EXECUTION_MODE, NewPair};

/// Command queue bound: a UI that floods commands waits (backpressure) instead of growing memory.
pub const COMMAND_CAPACITY: usize = 64;
/// Bound of the internal queue from spawned I/O tasks back to the actor.
pub const EVENT_CAPACITY: usize = 256;

/// Written when the gate refuses an exposure-opening command (e.g. entry blocked by the kill switch).
pub const COMMAND_REFUSED: &str = "COMMAND_REFUSED";
pub const PAIR_PREPARED: &str = "PAIR_PREPARED";

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

struct Actor {
    db: Db,
    events: EventStore,
    clock: Arc<dyn Clock>,
    timings: EngineTimings,
    simulator: Arc<dyn Executor>,
    factory: Arc<dyn ExecutorFactory>,
    /// The executor of the current `execution_mode`: `simulator` in SIMULATION, the factory-built
    /// one in EXCHANGE_DEMO (dropped when switching back).
    executor: Arc<dyn Executor>,
    trigger_mode: TriggerMode,
    execution_mode: ExecutionMode,
    pairs: BTreeMap<PairUuid, PairView>,
    /// Set by startup reconciliation (wave 2) until it completes; refuses exposure meanwhile.
    reconciliation_pending: Option<String>,
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
        let EngineDeps { db, clock, timings, simulator, factory } = deps;
        let events = EventStore::new(db.clone());
        let (trigger_mode, mut execution_mode, warnings) = gate::load_modes(&db);
        for w in warnings {
            let _ = events.append("MODE_LOAD_WARNING", None, json!({ "warning": w }));
        }
        let mut executor = simulator.clone();
        if execution_mode == ExecutionMode::ExchangeDemo {
            match factory.create(ExecutionMode::ExchangeDemo) {
                Ok(e) => executor = e,
                Err(reason) => {
                    // Fail closed: no order-capable executor -> SIMULATION, persisted and recorded.
                    execution_mode = ExecutionMode::Simulation;
                    let _ = db.set_flag_with_event(
                        FLAG_EXECUTION_MODE,
                        gate::execution_mode_str(ExecutionMode::Simulation),
                        gate::EXECUTION_MODE_FALLBACK,
                        &json!({ "from": "EXCHANGE_DEMO", "to": "SIMULATION", "reason": reason }),
                    );
                }
            }
        }
        let pairs = load_open_pairs(&db);

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
            executor,
            trigger_mode,
            execution_mode,
            pairs,
            reconciliation_pending: None,
            cmd_rx,
            event_tx,
            event_rx,
            market_rx,
            market_open: true,
            snapshot_tx: watch::channel(dummy_snapshot()).0,
            dirty: false,
            last_push: None,
        };
        let (snapshot_tx, snapshots) = watch::channel(actor.snapshot());
        actor.snapshot_tx = snapshot_tx;
        (actor, EngineHandle { commands: cmd_tx, snapshots, market: market_tx })
    }

    async fn run(mut self) {
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

    fn on_command(&mut self, msg: CommandMsg) {
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
            if let Err(why) = gate::admit(&cmd, &blockers) {
                // Best effort: on a halted store this cannot be written (the halt is the record).
                let _ = self.events.append(COMMAND_REFUSED, pair_of(&cmd), json!({ "command": command_name(&cmd), "reason": why }));
                return CommandReply::Rejected(why);
            }
        }
        match cmd {
            Command::Tick => self.on_tick(),
            Command::AddPrepared(p) => self.add_prepared(p),
            Command::CancelPrepared { pair, reason } => {
                self.transition(&pair, ManualEvent::Cancel, json!({ "reason": reason }))
            }
            Command::ConfirmClosed { pair, verified_flat } => {
                self.transition(&pair, ManualEvent::ConfirmClosed { verified_flat }, json!({ "source": "user" }))
            }
            Command::SetTriggerMode(m) => self.set_trigger_mode(m),
            Command::SetExecutionMode(m) => self.set_execution_mode(m),
            Command::SetKillSwitch { on } => match self.db.set_kill_switch(on) {
                Ok(()) => CommandReply::Accepted,
                Err(e) => CommandReply::Rejected(format!("cannot save kill switch: {e}")),
            },
            // Flows that arrive with wave 2 (scheduler, Node 0–8, manual orders, config).
            Command::EntryTrigger { .. }
            | Command::ManualEnter { .. }
            | Command::ManualOrder(_)
            | Command::AutoExit { .. }
            | Command::ManualExit { .. }
            | Command::ManualClose { .. }
            | Command::UpdateConfig { .. } => not_implemented(),
        }
    }

    /// Scheduler extension point (wave 2: entry / exit / auto-cancel decisions, each sent back
    /// through `dispatch`). For now a tick only refreshes the Snapshot clock.
    fn on_tick(&mut self) -> CommandReply {
        CommandReply::Accepted
    }

    /// Results of spawned I/O. Only here may they change state (wave 2 fills these in).
    fn on_event(&mut self, ev: Event) {
        self.dirty = true;
        match ev {
            Event::BaselineFetched { .. }
            | Event::PretradeFetched { .. }
            | Event::MarginFetched { .. }
            | Event::Submitted { .. }
            | Event::Queried { .. }
            | Event::ManualSubmitted { .. } => {}
        }
    }

    fn add_prepared(&mut self, p: super::command::NewPreparedPair) -> CommandReply {
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
        // Atomic in the store (unique index); no check-then-insert here.
        match self.db.add_pair_if_not_pending(&row) {
            Ok(AddPairOutcome::Added) => {
                let _ = self.events.append(
                    PAIR_PREPARED,
                    Some(&p.internal_uuid),
                    json!({ "symbol": p.symbol, "settlement_ms": p.settlement_ms, "simulated": simulated }),
                );
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
                self.pairs.insert(p.internal_uuid, view);
                CommandReply::Accepted
            }
            Ok(AddPairOutcome::AlreadyPending) => CommandReply::AlreadyPending,
            Err(e) => CommandReply::Rejected(format!("cannot add pair: {e}")),
        }
    }

    /// Land a pair transition (store first), then update memory.
    fn transition(&mut self, pair: &str, event: impl Into<PairEvent>, detail: Value) -> CommandReply {
        let Some(from) = self.pairs.get(pair).map(|v| v.state) else {
            return CommandReply::Rejected(format!("unknown pair {pair}"));
        };
        match transition::land_then_act(&self.events, pair, from, event, detail, |_| ()) {
            Ok((to, ())) => {
                if let Some(v) = self.pairs.get_mut(pair) {
                    v.state = to;
                }
                CommandReply::Accepted
            }
            Err(e @ TransitionError::Stale { .. }) => {
                // Memory disagreed with the store: adopt the stored state, act on nothing.
                if let Ok(Some(row)) = self.db.get_pair(pair) {
                    if let (Ok(s), Some(v)) = (row.status.parse::<PairState>(), self.pairs.get_mut(pair)) {
                        v.state = s;
                    }
                }
                CommandReply::Rejected(e.to_string())
            }
            Err(e @ (TransitionError::Illegal(_) | TransitionError::StoreFailed(_))) => CommandReply::Rejected(e.to_string()),
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

    fn set_execution_mode(&mut self, target: ExecutionMode) -> CommandReply {
        let open = self.pairs.values().filter(|v| transition::is_open(v.state)).count();
        match gate::switch_execution_mode(&self.db, self.factory.as_ref(), self.execution_mode, target, open) {
            ModeSwitch::Unchanged => CommandReply::Accepted,
            ModeSwitch::Switched { mode, executor } => {
                self.execution_mode = mode;
                self.executor = executor.unwrap_or_else(|| self.simulator.clone());
                CommandReply::Accepted
            }
            ModeSwitch::Refused(why) => CommandReply::Rejected(why),
        }
    }

    /// The single order path: submits `req` on the current executor in a spawned task; the result
    /// comes back as `Event::Submitted`. Callers must have landed the transition and the order
    /// intent first (land then act; crash-recovery spec).
    fn spawn_submit(&self, pair: PairUuid, leg: Leg, action: OrderAction, req: OrderRequest) {
        let executor = self.executor.clone();
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let client_order_id = req.client_order_id.clone();
            let outcome = executor.submit(req).await;
            let _ = tx.send(Event::Submitted { pair, leg, action, client_order_id, outcome }).await;
        });
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
        }
    }
}

fn not_implemented() -> CommandReply {
    CommandReply::Rejected("not implemented yet".into())
}

fn dummy_snapshot() -> Snapshot {
    Snapshot {
        now_ms: 0,
        trigger_mode: TriggerMode::Manual,
        execution_mode: ExecutionMode::Simulation,
        pairs: Vec::new(),
        blockers: Vec::new(),
        prices: Vec::new(),
    }
}

/// Open pairs from the store (they count for mode switching and limits after a restart). An open
/// pair row that cannot be read halts the store: an unknown pair may hold exposure (fail closed).
fn load_open_pairs(db: &Db) -> BTreeMap<PairUuid, PairView> {
    let mut out = BTreeMap::new();
    let Ok(rows) = db.list_pairs() else { return out };
    for row in rows {
        let state = match row.status.parse::<PairState>() {
            Ok(s) if !transition::is_open(s) => continue,
            Ok(s) => s,
            Err(e) => {
                db.halt(HaltReason::ConfigReadFailed(format!("pair {}: {e}", row.internal_uuid)));
                continue;
            }
        };
        match serde_json::from_value::<PairEnvelope>(row.entry) {
            Ok(env) => {
                out.insert(
                    row.internal_uuid.clone(),
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
                );
            }
            Err(e) => db.halt(HaltReason::ConfigReadFailed(format!("pair {} entry unreadable: {e}", row.internal_uuid))),
        }
    }
    out
}

/// Short label for events (no payload data).
fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Tick => "Tick",
        Command::AddPrepared(_) => "AddPrepared",
        Command::EntryTrigger { .. } => "EntryTrigger",
        Command::ManualEnter { .. } => "ManualEnter",
        Command::ManualOrder(_) => "ManualOrder",
        Command::AutoExit { .. } => "AutoExit",
        Command::ManualExit { .. } => "ManualExit",
        Command::ManualClose { .. } => "ManualClose",
        Command::ConfirmClosed { .. } => "ConfirmClosed",
        Command::CancelPrepared { .. } => "CancelPrepared",
        Command::SetTriggerMode(_) => "SetTriggerMode",
        Command::SetExecutionMode(_) => "SetExecutionMode",
        Command::UpdateConfig { .. } => "UpdateConfig",
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
        | Command::SetTriggerMode(_)
        | Command::SetExecutionMode(_)
        | Command::UpdateConfig { .. }
        | Command::SetKillSwitch { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::command::{Blocker, NewPreparedPair};
    use crate::engine::gate::test_support::{CountingFactory, NullExecutor, delete_kill_switch_row};
    use crate::engine::ports::{Leg, OrderAction, OrderRequest, OrderSide};
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

    /// A pair already in the store (as after a restart) in `state`.
    fn seed(db: &Db, uuid: &str, symbol: &str, state: PairState) {
        let env = PairEnvelope {
            long_exchange: Exchange::Binance,
            short_exchange: Exchange::Bybit,
            settlement_ms: T0,
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
        actor.spawn_submit("u-hang".into(), Leg::Long, OrderAction::Open, req);
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
        // Passes the gate; the close flow itself arrives in wave 2 (then: Accepted).
        assert!(!refused(&r, ""), "{r:?}");
        assert_eq!(r, CommandReply::Rejected("not implemented yet".into()));
        assert_eq!(count_events(&rig.db, "COMMAND_REFUSED"), 0);
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

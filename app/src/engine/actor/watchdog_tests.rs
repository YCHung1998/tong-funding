//! naked-leg-watchdog through the actor (tasks 2.1–2.6). Child of `flow_tests` (reuses its rig, the
//! paused tokio clock and the in-memory exchange fakes; nothing here reaches a network).

use super::*;
use crate::engine::watchdog as wd;
use tong_funding_core::pair::{ManualEvent, SystemEvent};

/// A pair row in `state` (no events), simulated or demo, on the test symbol.
fn seed_pair(db: &Db, uuid: &str, state: PairState, simulated: bool) {
    let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T + 3_600_000, simulated, scan: json!({}) };
    let row = NewPair { internal_uuid: uuid.into(), pair_id: format!("pid-{uuid}"), symbol: SYM.into(), status: state, entry: serde_json::to_value(env).unwrap() };
    db.add_pair_if_not_pending(&row).unwrap();
}

fn watchdog_of(actor: &Actor, pair: &str) -> Option<naked_leg::WatchdogFlow> {
    actor.flows.get(pair).and_then(|f| f.watchdog.clone())
}

/// The simulator behind a gate: counts lookups and cancels and can hold every lookup (a round "in
/// flight"). It wraps the SAME simulator the account view reads, so positions stay consistent.
struct Watched {
    inner: Arc<SimulatedExecutor>,
    queries: AtomicUsize,
    cancels: AtomicUsize,
    /// The next this-many cancels do nothing (the order stays as it is), like a cancel that lost a race.
    swallow_cancels: AtomicUsize,
    cancelled: Mutex<Vec<String>>,
    hold: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
    /// Orders whose id starts with this prefix are accepted and never fill (Open until cancelled);
    /// they never reach the simulator, so they leave no position.
    idle_prefix: Mutex<Option<String>>,
    idle: Mutex<HashMap<String, OrderState>>,
}

impl Watched {
    fn new(inner: Arc<SimulatedExecutor>) -> Arc<Watched> {
        Arc::new(Watched {
            inner,
            queries: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            swallow_cancels: AtomicUsize::new(0),
            cancelled: Mutex::default(),
            hold: std::sync::atomic::AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
            idle_prefix: Mutex::default(),
            idle: Mutex::default(),
        })
    }
    /// Orders with this id prefix are accepted, never fill and stay Open until a cancel works.
    fn idle_orders(&self, prefix: &str) {
        *self.idle_prefix.lock().unwrap() = Some(prefix.to_string());
    }
    fn idle_status(id: &str, state: OrderState) -> OrderStatus {
        OrderStatus { client_order_id: id.into(), exchange_order_id: Some("idle-1".into()), filled_quantity: Decimal::ZERO, avg_price: None, fee: None, fee_asset: None, state }
    }
    fn idle_state(&self, id: &str) -> Option<OrderState> {
        self.idle.lock().unwrap().get(id).copied()
    }
    fn queries(&self) -> usize {
        self.queries.load(Ordering::SeqCst)
    }
    fn cancels(&self) -> usize {
        self.cancels.load(Ordering::SeqCst)
    }
    /// The ids every cancel call named, in order.
    fn cancelled_ids(&self) -> Vec<String> {
        self.cancelled.lock().unwrap().clone()
    }
    fn swallow_cancels(&self, n: usize) {
        self.swallow_cancels.store(n, Ordering::SeqCst);
    }
    fn hold(&self, on: bool) {
        self.hold.store(on, Ordering::SeqCst);
        if !on {
            self.release.notify_waiters();
        }
    }
}

impl Executor for Watched {
    fn is_simulated(&self) -> bool {
        true
    }
    fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
        let idle = self.idle_prefix.lock().unwrap().as_deref().is_some_and(|p| req.client_order_id.starts_with(p));
        if idle {
            self.idle.lock().unwrap().insert(req.client_order_id.clone(), OrderState::Open);
            let status = Watched::idle_status(&req.client_order_id, OrderState::Open);
            return Box::pin(std::future::ready(SubmitOutcome::Accepted(status)));
        }
        self.inner.submit(req)
    }
    fn cancel(&self, exchange: Exchange, symbol: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.cancelled.lock().unwrap().push(id.to_string());
        let swallowed = self.swallow_cancels.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok();
        if let Some(state) = self.idle_state(id) {
            let state = if swallowed { state } else { OrderState::Cancelled };
            self.idle.lock().unwrap().insert(id.to_string(), state);
            return Box::pin(std::future::ready(QueryOutcome::Found(Watched::idle_status(id, state))));
        }
        if swallowed {
            return self.inner.query(exchange, symbol, id); // the cancel did nothing: the order as it is
        }
        self.inner.cancel(exchange, symbol, id)
    }
    fn query(&self, exchange: Exchange, symbol: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        let (symbol, id) = (symbol.to_string(), id.to_string());
        Box::pin(async move {
            if self.hold.load(Ordering::SeqCst) {
                self.release.notified().await;
            }
            if let Some(state) = self.idle_state(&id) {
                return QueryOutcome::Found(Watched::idle_status(&id, state));
            }
            self.inner.query(exchange, &symbol, &id).await
        })
    }
}

/// The test engine on the SIMULATION executor behind [`Watched`], with the test pair added.
async fn started_watched(o: Opts) -> (Rig, EngineHandle, Arc<Watched>) {
    let (rig, mut deps) = rig(o);
    let watched = Watched::new(rig.sim.clone());
    deps.simulator = watched.clone();
    let h = start(deps);
    assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted);
    (rig, h, watched)
}

/// A pair whose short opening order is refused (the long leg is bare): PARTIAL_FAILURE at T-10 s.
/// Returns the time it entered the state.
async fn bare_long(o: Opts) -> (Rig, EngineHandle, Arc<Watched>, i64) {
    let (rig, h, watched) = started_watched(o).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted refusal".into()));
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    let since = ts_of(&rig.db, "PARTIAL_FAILURE")[0];
    (rig, h, watched, since)
}

/// The watchdog's round records of the pair, oldest first.
fn rounds(db: &Db) -> Vec<Value> {
    payloads(db, wd::NAKED_LEG_WATCHDOG).into_iter().filter(|p| p["kind"] == json!(wd::KIND_ROUND)).collect()
}

// ---- 2.1 alert_since_ms ----------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_pair_entering_an_alert_state_records_the_time_it_entered() {
    let (rig, deps) = rig(Opts::default());
    seed_pair(&rig.db, UUID, PairState::FillMonitor, true);
    let (mut actor, _h) = Actor::new(deps);
    assert!(watchdog_of(&actor, UUID).is_none(), "FILL_MONITOR is not an alert state");
    rig.clock.advance(12_345);
    let now = rig.clock.now_ms();
    assert_eq!(actor.land(UUID, SystemEvent::TimeoutPartialFill, json!({})), Ok(PairState::PartialFailure));
    let w = watchdog_of(&actor, UUID).expect("the watchdog tracks the entry");
    assert_eq!((w.alert_since_ms, w.next_round_ms, w.state), (now, now + 5_000, PairState::PartialFailure));
    assert_eq!((w.rounds, w.attempts_sent, w.waited_rounds, w.done), (0, 0, 0, false));
    // a manual close leaves the alert state: the progress goes with it
    assert_eq!(actor.land(UUID, ManualEvent::RequestClose, json!({})), Ok(PairState::Closing));
    assert!(watchdog_of(&actor, UUID).is_none());
}

#[tokio::test(start_paused = true)]
async fn each_entry_into_an_alert_state_gets_its_own_number() {
    let (rig, deps) = rig(Opts::default());
    seed_pair(&rig.db, UUID, PairState::FillMonitor, true);
    let (mut actor, _h) = Actor::new(deps);
    actor.land(UUID, SystemEvent::TimeoutPartialFill, json!({})).unwrap();
    let first = watchdog_of(&actor, UUID).unwrap().entry_no;
    actor.land(UUID, ManualEvent::RequestClose, json!({})).unwrap();
    rig.clock.advance(20_000);
    actor.land(UUID, SystemEvent::CloseFailed, json!({})).unwrap();
    let second = watchdog_of(&actor, UUID).unwrap();
    assert_ne!(first, second.entry_no, "a late result of the first entry must not be taken for the second");
    assert_eq!(second.alert_since_ms, rig.clock.now_ms(), "and the wait starts again");
}

#[tokio::test(start_paused = true)]
async fn after_a_restart_the_time_of_entry_is_restored_from_the_entry_event_not_taken_from_now() {
    let (rig, deps) = rig(Opts::default());
    seed_pair(&rig.db, UUID, PairState::FillMonitor, true);
    let (mut first, h1) = Actor::new(deps);
    let t0 = rig.clock.now_ms();
    first.land(UUID, SystemEvent::TimeoutPartialFill, json!({})).unwrap();
    drop((first, h1)); // process 1 ends
    rig.clock.advance(3_000); // down for 3 s

    let (mut second, _h2) = Actor::new(restarted(&rig, Arc::new(RecordingNotifier::default())));
    assert!(watchdog_of(&second, UUID).is_none(), "memory is empty after a restart ...");
    assert!(second.ensure_watchdog(UUID), "... and is rebuilt for a pair in an alert state");
    let w = watchdog_of(&second, UUID).unwrap();
    assert_eq!(w.alert_since_ms, t0, "from the PAIR_TRANSITION event, not from the restart");
    assert_eq!(w.next_round_ms, t0 + 5_000, "so the first round is still 5 s after the ENTRY (2 s from now)");
}

#[tokio::test(start_paused = true)]
async fn after_a_restart_what_the_watchdog_already_did_is_not_done_again() {
    let (rig, deps) = rig(Opts::default());
    seed_pair(&rig.db, UUID, PairState::FillMonitor, true);
    let (mut first, h1) = Actor::new(deps);
    first.land(UUID, SystemEvent::TimeoutPartialFill, json!({})).unwrap();
    rig.clock.advance(6_000);
    first.note("LEG_FLATTEN", Some(UUID), json!({ "by": "watchdog", "leg": "long" }));
    first.note("LEG_FLATTEN", Some(UUID), json!({ "by": "manual", "leg": "long" }));
    first.note("NAKED_LEG_WATCHDOG", Some(UUID), json!({ "kind": "ROUND", "conclusion": "WAIT_FOR_CANCEL" }));
    rig.clock.advance(5_000);
    first.note("NAKED_LEG_WATCHDOG", Some(UUID), json!({ "kind": "ROUND", "conclusion": "UNKNOWN", "reason": "x" }));
    drop((first, h1));

    let (mut second, _h2) = Actor::new(restarted(&rig, Arc::new(RecordingNotifier::default())));
    assert!(second.ensure_watchdog(UUID));
    let w = watchdog_of(&second, UUID).unwrap();
    assert_eq!((w.rounds, w.attempts_sent, w.waited_rounds), (2, 1, 0), "only the watchdog's own flatten counts as an attempt");
    assert!(w.done, "an UNKNOWN conclusion stays final until the user acts: a restart must not retry it");
}

#[tokio::test(start_paused = true)]
async fn ensure_does_nothing_for_a_pair_outside_the_alert_states() {
    let (rig, deps) = rig(Opts::default());
    seed_pair(&rig.db, UUID, PairState::Reconciled, true);
    let (mut actor, _h) = Actor::new(deps);
    assert!(!actor.ensure_watchdog(UUID));
    assert!(!actor.ensure_watchdog("no-such-pair"));
    assert!(watchdog_of(&actor, UUID).is_none());
}

// ---- 2.2 rounds: when they run, what they read, what they write -----------------------------

#[tokio::test(start_paused = true)]
async fn no_round_runs_before_five_seconds_after_the_pair_entered_the_alert_state() {
    let (rig, _h, watched, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 4_000).await;
    assert_eq!(count(&rig.db, wd::NAKED_LEG_WATCHDOG), 0, "nothing is recorded before t + 5 s");
    assert_eq!(watched.queries(), 0, "and nothing is even read");
    run_until(&rig.clock, since + 5_000).await;
    assert_eq!(ts_of(&rig.db, wd::NAKED_LEG_WATCHDOG), vec![since + 5_000], "the first round comes with the first tick at or after t + 5 s");
    assert_eq!(rounds(&rig.db).len(), 1);
    assert_eq!(watched.queries(), 1, "the one lookup: the long leg's opening order (the refused short order is not looked up)");
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "the watchdog never changes the pair's state");
}

#[tokio::test(start_paused = true)]
async fn the_round_reads_both_legs_and_writes_its_conclusion_and_basis() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 5_000).await;
    let rounds = rounds(&rig.db);
    assert_eq!(rounds.len(), 1, "{rounds:?}");
    let e = &rounds[0];
    assert_eq!((e["conclusion"].clone(), e["round"].clone(), e["alert_state"].clone(), e["alert_since_ms"].clone()), (json!("FLATTEN"), json!(1), json!("PARTIAL_FAILURE"), json!(since)), "{e}");
    assert_eq!((e["pair_id"].clone(), e["symbol"].clone(), e["simulated"].clone(), e["tolerance_pct"].clone()), (json!(format!("pid-{UUID}")), json!(SYM), json!(true), json!("1")), "{e}");
    assert_eq!((e["leg"].clone(), e["quantity"].clone(), e["attempt"].clone()), (json!("long"), json!("10"), json!(1)), "the leg and quantity the round decided on");
    let long = &e["legs"]["long"];
    assert_eq!((long["exchange"].clone(), long["position"].clone(), long["recorded"].clone(), long["orders_terminal"].clone()), (json!("Binance"), json!("10"), json!("10"), json!(true)), "{e}");
    assert_eq!((long["orders"][0]["action"].clone(), long["orders"][0]["state"].clone(), long["orders"][0]["filled"].clone()), (json!("open"), json!("Filled"), json!("10")), "{e}");
    let short = &e["legs"]["short"];
    assert_eq!((short["exchange"].clone(), short["position"].clone(), short["recorded"].clone(), short["orders_terminal"].clone()), (json!("Bybit"), json!("0"), json!("0"), json!(true)), "{e}");
    assert_eq!(short["orders"][0]["rejected_intent"], json!(true), "the refused order is evidence of 'never existed': {e}");
}

#[tokio::test(start_paused = true)]
async fn only_one_round_is_in_flight_per_pair_however_many_ticks_pass() {
    let (rig, _h, watched, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 1_000).await;
    watched.hold(true); // every lookup now waits: the first round cannot finish
    run_until(&rig.clock, since + 25_000).await;
    assert_eq!(watched.queries(), 1, "20 ticks later still the one lookup of the first round: no second round was started");
    assert_eq!(count(&rig.db, wd::NAKED_LEG_WATCHDOG), 0, "and nothing recorded yet");
    watched.hold(false);
    sleep(Duration::from_millis(50)).await;
    assert_eq!(rounds(&rig.db).len(), 1, "the round finishes once, with its own conclusion");
}

#[tokio::test(start_paused = true)]
async fn the_kill_switch_does_not_stop_the_rounds() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    rig.db.set_kill_switch(true).unwrap();
    run_until(&rig.clock, since + 5_000).await;
    assert_eq!(rounds(&rig.db).len(), 1, "closing is never held back by the kill switch");
}

#[tokio::test(start_paused = true)]
async fn a_pair_of_the_other_execution_mode_is_skipped_with_one_event_and_no_read() {
    // a demo pair while the engine runs the simulator (e.g. after a startup fallback): this engine can
    // neither read nor close it
    let (rig, mut deps) = rig(Opts::default());
    let watched = Watched::new(rig.sim.clone());
    deps.simulator = watched.clone();
    seed_pair(&rig.db, UUID, PairState::PartialFailure, false);
    let _h = start(deps);
    run_until(&rig.clock, rig.clock.now_ms() + 12_000).await;
    let records = payloads(&rig.db, wd::NAKED_LEG_WATCHDOG);
    assert_eq!(records.len(), 1, "once, not on every tick: {records:?}");
    assert_eq!(records[0]["kind"], json!("SKIPPED"));
    assert!(records[0]["reason"].as_str().unwrap().contains("EXCHANGE_DEMO"), "{}", records[0]);
    assert_eq!((watched.queries(), watched.cancels()), (0, 0));
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_round_that_cannot_finish_within_the_probe_limit_concludes_unknown() {
    let (rig, _h, watched, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 1_000).await;
    watched.hold(true); // the exchange never answers the lookup
    run_until(&rig.clock, since + 5_000 + wd::WATCHDOG_PROBE_TIMEOUT_MS + 3_000).await;
    let rounds = rounds(&rig.db);
    assert_eq!(rounds.len(), 1, "{rounds:?}");
    assert_eq!(rounds[0]["conclusion"], json!("UNKNOWN"), "{}", rounds[0]);
    let why = rounds[0]["reason"].as_str().unwrap();
    assert!(why.contains("did not finish") && why.contains("30 s"), "{why}");
    assert_eq!(rounds[0]["leg"], Value::Null, "an unknown round names no leg to close");
}

#[tokio::test(start_paused = true)]
async fn a_round_whose_result_never_arrives_is_given_up_as_unknown() {
    let (rig, deps) = rig(Opts::default());
    seed_pair(&rig.db, UUID, PairState::FillMonitor, true);
    let (mut actor, _h) = Actor::new(deps);
    actor.land(UUID, SystemEvent::TimeoutPartialFill, json!({})).unwrap();
    // a round was started long ago and its event was lost
    rig.clock.advance(120_000);
    let now = rig.clock.now_ms();
    if let Some(w) = actor.flows.get_mut(UUID).and_then(|f| f.watchdog.as_mut()) {
        w.phase = naked_leg::Phase::Verifying { started_ms: now - 60_000 };
        w.rounds = 1;
    }
    let view = actor.pairs.get(UUID).cloned().unwrap();
    actor.watchdog_tick(&view, now);
    let rounds = rounds(&rig.db);
    assert_eq!(rounds.len(), 1, "{rounds:?}");
    assert_eq!(rounds[0]["conclusion"], json!("UNKNOWN"));
    assert!(rounds[0]["reason"].as_str().unwrap().contains("did not report"), "{}", rounds[0]);
}

// ---- 2.3 an order that is not over: cancel, look again, give up after three rounds -----------

/// The pair at its fill timeout with the short order still open after the engine's own cancel
/// (that cancel is swallowed, plus `swallow_more` after it): UNRESOLVED, the long leg filled 10, the
/// short leg 7 of 10. Returns the time the pair entered the state.
async fn open_short(swallow_more: usize) -> (Rig, EngineHandle, Arc<Watched>, i64) {
    let (rig, h, watched) = started_watched(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Partial(dec("0.7")));
    watched.swallow_cancels(1 + swallow_more);
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED", "{:?}", labels(&rig.db));
    let since = ts_of(&rig.db, "UNRESOLVED")[0];
    (rig, h, watched, since)
}

fn conclusions(rounds: &[Value]) -> Vec<String> {
    rounds.iter().map(|r| r["conclusion"].as_str().unwrap_or("?").to_string()).collect()
}

fn watchdog_cancel_results(db: &Db) -> Vec<Value> {
    payloads(db, ORDER_CANCEL_RESULT).into_iter().filter(|p| p["by"] == json!("watchdog")).collect()
}

#[tokio::test(start_paused = true)]
async fn an_unfinished_order_is_cancelled_nothing_is_sent_and_the_next_round_looks_again() {
    let (rig, _h, watched, since) = open_short(1).await;
    let engine_cancels = watched.cancels();
    assert_eq!(engine_cancels, 1, "the fill timeout's own cancel (it did nothing)");
    run_until(&rig.clock, since + 5_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["WAIT_FOR_CANCEL"], "{r:?}");
    assert_eq!(r[0]["waited_rounds"], json!(0), "{}", r[0]);
    assert_eq!(watched.cancels(), engine_cancels + 1, "one cancel for the one order that is not over");
    assert_eq!(watched.cancelled_ids().last(), Some(&sim_id(Leg::Short, OrderAction::Open)), "the pair's own unfinished order, never the filled long one");
    assert_eq!(rig.sim.submitted().len(), 2, "while an order is unfinished nothing is sent");
    let results = watchdog_cancel_results(&rig.db);
    assert_eq!(results.len(), 1, "the cancel and the lookup after it are recorded: {results:?}");
    assert_eq!(results[0]["client_order_id"], json!(sim_id(Leg::Short, OrderAction::Open)));
    assert_eq!(results[0]["after"]["state"], json!("Open"), "the swallowed cancel left it open: {}", results[0]);

    // 5 s later the second cancel works; the third round finds both legs over and holding a position
    run_until(&rig.clock, since + 15_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["WAIT_FOR_CANCEL", "WAIT_FOR_CANCEL", "HEDGED"], "{r:?}");
    assert_eq!(ts_of(&rig.db, wd::NAKED_LEG_WATCHDOG), vec![since + 5_000, since + 10_000, since + 15_000], "rounds are 5 s apart");
    assert_eq!(r.iter().map(|x| x["waited_rounds"].clone()).collect::<Vec<_>>(), vec![json!(0), json!(1), json!(2)], "each record carries the earlier consecutive waits it was decided with");
    assert_eq!(rig.sim.submitted().len(), 2, "still no order of the watchdog's");
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED", "the pair state is never changed");
}

#[tokio::test(start_paused = true)]
async fn three_consecutive_unfinished_rounds_conclude_unknown_and_the_last_one_cancels_nothing() {
    let (rig, _h, watched, since) = open_short(usize::MAX / 2).await; // no cancel ever works
    let engine_cancels = watched.cancels();
    run_until(&rig.clock, since + 20_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["WAIT_FOR_CANCEL", "WAIT_FOR_CANCEL", "UNKNOWN"], "{r:?}");
    let why = r[2]["reason"].as_str().unwrap();
    assert!(why.contains("short") && why.contains("3 rounds"), "{why}");
    assert_eq!(watched.cancels(), engine_cancels + 2, "a cancel in each of the two waiting rounds, none in the Unknown one");
    assert_eq!(rig.sim.submitted().len(), 2, "nothing was ever sent");
    run_until(&rig.clock, since + 40_000).await;
    assert_eq!(rounds(&rig.db).len(), 3, "an Unknown conclusion ends the rounds: no fourth one");
}

// ---- 2.4 the flatten: one reduce-only order of the bare leg, intent first, by: "watchdog" ----

fn events_of(db: &Db, label: &str) -> Vec<(i64, Value)> {
    events(db).into_iter().filter(|(_, l, _)| l == label).map(|(t, _, p)| (t, p)).collect()
}

#[tokio::test(start_paused = true)]
async fn a_bare_leg_is_flattened_with_one_reduce_only_order_and_the_pair_state_does_not_change() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 5_000).await;
    let sent = rig.sim.submitted();
    assert_eq!(sent.len(), 3, "the two opening orders and exactly one more: {sent:?}");
    let close = &sent[2];
    assert_eq!(
        (close.client_order_id.clone(), close.exchange, close.side, close.quantity, close.reduce_only, close.leverage, close.intended_base_qty),
        (sim_id(Leg::Long, OrderAction::Close), Exchange::Binance, OrderSide::Sell, dec("10"), true, None, None),
        "a reduce-only market sell of the long leg's recorded position, nothing else"
    );
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO, "the bare leg is flat");
    assert_eq!(rig.sim.position(Exchange::Bybit, SYM), Decimal::ZERO, "the other exchange was never touched");
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "the pair still needs the user's confirmation");
    let flatten = events_of(&rig.db, "LEG_FLATTEN");
    assert_eq!(flatten.len(), 1, "{flatten:?}");
    let f = &flatten[0].1;
    assert_eq!((f["by"].clone(), f["leg"].clone(), f["exchange"].clone(), f["quantity"].clone(), f["position"].clone()), (json!("watchdog"), json!("long"), json!("Binance"), json!("10"), json!("10")), "{f}");
    assert_eq!((f["client_order_id"].clone(), f["simulated"].clone(), f["attempt"].clone()), (json!(sim_id(Leg::Long, OrderAction::Close)), json!(true), json!(1)), "{f}");
    let intent = rig.db.get_intent(&sim_id(Leg::Long, OrderAction::Close)).unwrap().expect("the order intent was landed");
    assert_eq!(intent.state, "FILLED");
}

#[tokio::test(start_paused = true)]
async fn after_the_order_the_attempt_is_recorded_and_one_more_round_confirms_both_legs_flat() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 12_000).await;
    let attempts: Vec<Value> = payloads(&rig.db, wd::NAKED_LEG_WATCHDOG).into_iter().filter(|p| p["kind"] == json!(wd::KIND_ATTEMPT_RESULT)).collect();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let a = &attempts[0];
    assert_eq!((a["ok"].clone(), a["attempt"].clone(), a["leg"].clone(), a["quantity"].clone()), (json!(true), json!(1), json!("long"), json!("10")), "{a}");
    assert_eq!(a["client_order_id"], json!(sim_id(Leg::Long, OrderAction::Close)));
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["FLATTEN", "FLAT"], "verified again after the order: nothing left on either leg");
    let progress_at: Vec<i64> = events(&rig.db)
        .into_iter()
        .filter(|(_, l, p)| l == wd::NAKED_LEG_WATCHDOG && (p["kind"] == json!(wd::KIND_ROUND) || p["kind"] == json!(wd::KIND_ATTEMPT_RESULT)))
        .map(|(t, _, _)| t)
        .collect();
    assert_eq!(progress_at, vec![since + 5_000, since + 6_000, since + 11_000], "the order's result is seen at the next tick; the next round is 5 s after it");
    assert_eq!(r[1]["attempts_sent"], json!(1));
    assert_eq!(rig.sim.submitted().len(), 3, "and no second order");
    run_until(&rig.clock, since + 40_000).await;
    assert_eq!(rig.sim.submitted().len(), 3);
    assert_eq!(rounds(&rig.db).len(), 2, "FLAT ends the rounds");
}

#[tokio::test(start_paused = true)]
async fn an_order_intent_that_cannot_be_landed_sends_nothing_and_halts_the_store() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 1_000).await;
    rig.db
        .with_raw_conn_for_tests(|c| Ok(c.execute_batch("CREATE TRIGGER no_more_intents BEFORE INSERT ON order_intents BEGIN SELECT RAISE(ABORT, 'injected failure'); END")?))
        .unwrap();
    run_until(&rig.clock, since + 5_000).await;
    assert_eq!(rig.sim.submitted().len(), 2, "no intent, no order: only the two opening orders ever reached the executor");
    assert!(rig.db.is_halted(), "a write that fails halts the store (fail closed)");
    let plain = rusqlite::Connection::open(rig.db.path()).unwrap(); // a halted store refuses reads; the file does not
    let rows: i64 = plain.query_row("SELECT COUNT(*) FROM order_intents WHERE client_order_id = ?1", [sim_id(Leg::Long, OrderAction::Close)], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0, "no close intent exists");
    run_until(&rig.clock, since + 20_000).await;
    assert_eq!(rig.sim.submitted().len(), 2, "and a halted store runs no further round");
}

#[tokio::test(start_paused = true)]
async fn the_watchdog_flattens_in_exchange_demo_too_after_a_restart_with_the_wait_counted_from_the_entry() {
    let (rig, deps) = rig(Opts { demo: true, ..Opts::default() });
    let entered = rig.clock.now_ms() - 1_000; // the pair entered the state 1 s before this process started
    seed_demo_bare_long(&rig, entered);
    let _h = start(deps);
    let start_ms = rig.clock.now_ms();
    run_until(&rig.clock, start_ms + 2_000).await;
    assert!(rig.demo.requests.lock().unwrap().is_empty(), "entered + 5 s has not come yet: the wait is counted from the entry event, not from the restart");
    assert_eq!(count(&rig.db, wd::NAKED_LEG_WATCHDOG), 0);
    run_until(&rig.clock, entered + 5_000).await;
    let requests = rig.demo.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let r = &requests[0];
    assert_eq!((r.exchange, r.side, r.quantity, r.reduce_only), (Exchange::Binance, OrderSide::Sell, dec("10"), true));
    assert!(r.client_order_id.starts_with("demo"), "{}", r.client_order_id);
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    let f = events_of(&rig.db, "LEG_FLATTEN");
    assert_eq!((f.len(), f[0].1["by"].clone(), f[0].1["simulated"].clone()), (1, json!("watchdog"), json!(false)));
    assert!(rig.sim.submitted().is_empty(), "the simulator was never involved");
}

/// A demo pair that entered PARTIAL_FAILURE at `entered_ms`: its long leg filled (10) and its short
/// order refused, as a restart finds it in the database. The fake exchange holds the long position.
fn seed_demo_bare_long(rig: &Rig, entered_ms: i64) {
    seed_pair(&rig.db, UUID, PairState::PartialFailure, false);
    let req = |leg: Leg| OrderRequest {
        prepared: None,
        notional_usdt: None,
        client_order_id: client_order_id(IdPrefix::Demo, UUID, leg, OrderAction::Open, 0),
        exchange: if leg == Leg::Long { Exchange::Binance } else { Exchange::Bybit },
        symbol: SYM.into(),
        side: if leg == Leg::Long { OrderSide::Buy } else { OrderSide::Sell },
        quantity: dec("10"),
        reduce_only: false,
        intended_base_qty: Some(dec("10")),
        leverage: Some(dec("5")),
    };
    let (long, short) = (req(Leg::Long), req(Leg::Short));
    intent::land_intents(&rig.db, UUID, &[(Leg::Long, &long), (Leg::Short, &short)]).unwrap();
    rig.db.update_intent_state(&long.client_order_id, IntentState::Filled, Some("demo-1")).unwrap();
    rig.db.update_intent_state(&short.client_order_id, IntentState::Failed, None).unwrap();
    rig.demo.positions.lock().unwrap().insert((Exchange::Binance, SYM.into()), dec("10"));
    let filled = OrderStatus { client_order_id: long.client_order_id.clone(), exchange_order_id: Some("demo-1".into()), filled_quantity: dec("10"), avg_price: Some(dec("100")), fee: None, fee_asset: None, state: OrderState::Filled };
    rig.demo.orders.lock().unwrap().insert(long.client_order_id.clone(), filled);
    let now = rig.clock.now_ms();
    rig.clock.set(entered_ms);
    crate::store::events::EventStore::new(rig.db.clone())
        .append(transition::PAIR_TRANSITION, Some(UUID), json!({ "from": "ORDER_SUBMIT", "to": "PARTIAL_FAILURE", "event": "System(OneLegSubmitFailed)", "detail": { "simulated": false } }))
        .unwrap();
    rig.clock.set(now);
}

// ---- 2.5 retries, the lock and the notification ----------------------------------------------

fn reduce_only_orders(rig: &Rig) -> Vec<OrderRequest> {
    rig.sim.submitted().into_iter().filter(|r| r.reduce_only).collect()
}

fn records_of_kind(db: &Db, kind: &str) -> Vec<Value> {
    payloads(db, wd::NAKED_LEG_WATCHDOG).into_iter().filter(|p| p["kind"] == json!(kind)).collect()
}

#[tokio::test(start_paused = true)]
async fn an_unknown_conclusion_sends_nothing_locks_the_engine_in_one_transaction_and_tells_the_user() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 1_000).await;
    rig.sim.fail_queries(Some("exchange unreachable (scripted)".into()));
    run_until(&rig.clock, since + 5_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["UNKNOWN"], "{r:?}");
    assert!(r[0]["reason"].as_str().unwrap().contains("unreachable"), "{}", r[0]);
    assert_eq!(rig.sim.submitted().len(), 2, "a failed lookup sends nothing");
    assert!(rig.db.kill_switch_halted(), "new entries are stopped");
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "the pair state is not touched");
    // KILL_SWITCH_CHANGED and NAKED_LEG_WATCHDOG_LOCK come from ONE transaction: adjacent, same time
    let all = events(&rig.db);
    let at = all.iter().position(|(_, l, _)| l == wd::NAKED_LEG_WATCHDOG_LOCK).expect("the lock event");
    assert_eq!((all[at - 1].1.as_str(), all[at - 1].2["on"].clone()), ("KILL_SWITCH_CHANGED", json!(true)), "{:?}", all[at - 1]);
    assert_eq!(all[at - 1].0, all[at].0);
    let lock = &all[at].2;
    assert_eq!((lock["pair"].clone(), lock["pair_id"].clone(), lock["symbol"].clone(), lock["alert_state"].clone()), (json!(UUID), json!(format!("pid-{UUID}")), json!(SYM), json!("PARTIAL_FAILURE")), "{lock}");
    assert!(lock["reason"].as_str().unwrap().contains("unreachable"), "{lock}");
    // the user is told, with the words of the shared copy
    sleep(Duration::from_millis(100)).await;
    let calls = rig.notifier.watchdog_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(matches!(&calls[0].outcome, crate::engine::alert::WatchdogOutcome::Locked { reason } if reason.contains("unreachable")), "{calls:?}");
    assert_eq!((calls[0].pair.as_str(), calls[0].symbol.as_str(), calls[0].simulated), (UUID, SYM, true));
    assert!(calls[0].body().contains("沒有送出任何單") && calls[0].body().contains("停止開新倉"), "{}", calls[0].body());
    assert_eq!(rig.notifier.calls().len(), 1, "the pair-alert notification is a separate one, still just one");
    let notified = records_of_kind(&rig.db, "NOTIFY_RESULT");
    assert_eq!((notified.len(), notified[0]["ok"].clone(), notified[0]["outcome"].clone()), (1, json!(true), json!("LOCKED")), "{notified:?}");
    // it stays final: the exchange is back and the user lifts the kill switch, still no further round
    rig.sim.fail_queries(None);
    rig.db.set_kill_switch(false).unwrap();
    run_until(&rig.clock, since + 40_000).await;
    assert_eq!(rounds(&rig.db).len(), 1, "no automatic action until the user acts");
    assert_eq!(rig.sim.submitted().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn three_rejected_flatten_orders_lock_the_engine_and_no_fourth_is_sent() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    rig.sim.script_prefix("simlc", SimBehavior::Reject("110090 risk limit (scripted)".into())); // every long close
    run_until(&rig.clock, since + 30_000).await;
    let closes = reduce_only_orders(&rig);
    assert_eq!(closes.len(), 3, "three attempts, never a fourth: {closes:?}");
    assert_eq!(closes.iter().map(|c| (c.exchange, c.side, c.quantity)).collect::<Vec<_>>(), vec![(Exchange::Binance, OrderSide::Sell, dec("10")); 3]);
    let ids: BTreeSet<String> = closes.iter().map(|c| c.client_order_id.clone()).collect();
    assert_eq!(ids.len(), 3, "each attempt is a new order with its own id");
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["FLATTEN", "FLATTEN", "FLATTEN", "UNKNOWN"], "{r:?}");
    assert_eq!(r.iter().map(|x| x["attempt"].clone()).collect::<Vec<_>>()[..3], [json!(1), json!(2), json!(3)]);
    assert_eq!(r[3]["reason"], json!("auto flatten failed 3 times"));
    sleep(Duration::from_millis(100)).await;
    let kinds: Vec<String> = payloads(&rig.db, wd::NAKED_LEG_WATCHDOG).iter().map(|p| p["kind"].as_str().unwrap_or("?").to_string()).collect();
    assert_eq!(kinds.iter().filter(|k| *k == wd::KIND_ROUND).count(), 4, "{kinds:?}");
    assert_eq!(kinds.iter().filter(|k| *k == wd::KIND_ATTEMPT_RESULT).count(), 3, "{kinds:?}");
    assert_eq!(kinds.iter().filter(|k| *k == wd::KIND_NOTIFY_RESULT).count(), 1, "{kinds:?}");
    let rounds_at: Vec<i64> = events(&rig.db).into_iter().filter(|(_, l, p)| l == wd::NAKED_LEG_WATCHDOG && p["kind"] == json!("ROUND")).map(|(t, _, _)| t - since).collect();
    assert_eq!(rounds_at, vec![5_000, 11_000, 17_000, 23_000], "5 s after the first grace, then 5 s after each attempt's result (seen at the next tick)");
    let attempts = records_of_kind(&rig.db, wd::KIND_ATTEMPT_RESULT);
    assert_eq!(attempts.iter().map(|a| (a["ok"].clone(), a["attempt"].clone())).collect::<Vec<_>>(), vec![(json!(false), json!(1)), (json!(false), json!(2)), (json!(false), json!(3))]);
    assert!(attempts.iter().all(|a| a["reason"].as_str().unwrap().contains("rejected")), "{attempts:?}");
    assert!(rig.db.kill_switch_halted());
    assert_eq!(count(&rig.db, wd::NAKED_LEG_WATCHDOG_LOCK), 1);
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.notifier.watchdog_calls().len(), 1, "told once");
    run_until(&rig.clock, since + 60_000).await;
    assert_eq!(reduce_only_orders(&rig).len(), 3, "and it stays at three");
}

#[tokio::test(start_paused = true)]
async fn a_successful_flatten_is_told_to_the_user_once_with_the_leg_and_the_quantity() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 12_000).await;
    sleep(Duration::from_millis(100)).await;
    let calls = rig.notifier.watchdog_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(matches!(&calls[0].outcome, crate::engine::alert::WatchdogOutcome::Flattened { leg: Leg::Long, exchange: Exchange::Binance, quantity } if *quantity == dec("10")), "{calls:?}");
    assert!(calls[0].body().contains("系統已自動平掉多腿，請確認兩所皆無部位後按「人工確認已平倉」"), "{}", calls[0].body());
    assert!(!rig.db.kill_switch_halted(), "a flatten that worked locks nothing");
    let notified = records_of_kind(&rig.db, "NOTIFY_RESULT");
    assert_eq!((notified.len(), notified[0]["outcome"].clone()), (1, json!("FLATTENED")), "{notified:?}");
}

#[tokio::test(start_paused = true)]
async fn a_failing_notifier_changes_nothing_but_one_record() {
    let (rig, _h, _w, since) = bare_long(Opts::default()).await;
    *rig.notifier.fail_with.lock().unwrap() = Some("osascript not available".into());
    rig.sim.script_prefix("simlc", SimBehavior::Reject("scripted".into()));
    run_until(&rig.clock, since + 30_000).await;
    sleep(Duration::from_millis(100)).await;
    assert!(rig.db.kill_switch_halted(), "the lock does not depend on the notification");
    let notified = records_of_kind(&rig.db, "NOTIFY_RESULT");
    assert_eq!((notified.len(), notified[0]["ok"].clone()), (1, json!(false)), "{notified:?}");
    assert!(notified[0]["error"].as_str().unwrap().contains("osascript"), "{}", notified[0]);
    assert_eq!(rig.notifier.watchdog_calls().len(), 1, "and it is not retried in a loop");
}

// ---- 2.6 the flows end to end on the simulator ----------------------------------------------
// (one leg filled / other not -> flattened, state unchanged: `a_bare_leg_is_flattened_...` above;
//  query failure -> locked: `an_unknown_conclusion_...`; 3 rejected flattens -> locked:
//  `three_rejected_flatten_orders_...`; restart: `the_watchdog_flattens_in_exchange_demo_too_after_a_restart_...`)

#[tokio::test(start_paused = true)]
async fn both_legs_filled_sends_nothing_locks_nothing_and_ends_the_rounds() {
    let (rig, _h, watched) = started_watched(Opts::default()).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Partial(dec("0.7")));
    run_until(&rig.clock, T + 5_000).await; // the fill timeout cancels the rest of the short order
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "{:?}", labels(&rig.db));
    let since = ts_of(&rig.db, "PARTIAL_FAILURE")[0];
    assert_eq!((rig.sim.position(Exchange::Binance, SYM), rig.sim.position(Exchange::Bybit, SYM)), (dec("10"), dec("-7")), "both legs hold a position");
    run_until(&rig.clock, since + 5_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["HEDGED"], "{r:?}");
    assert_eq!((r[0]["legs"]["long"]["position"].clone(), r[0]["legs"]["short"]["position"].clone()), (json!("10"), json!("-7")), "the basis shows both legs");
    assert_eq!(rig.sim.submitted().len(), 2, "no order: the pair is hedged, the user decides");
    assert!(!rig.db.kill_switch_halted(), "and nothing is locked");
    assert_eq!(events_of(&rig.db, "LEG_FLATTEN").len(), 0);
    run_until(&rig.clock, since + 60_000).await;
    assert_eq!(rounds(&rig.db).len(), 1, "a conclusion that needs the user ends the rounds");
    assert_eq!(rig.sim.submitted().len(), 2);
    assert_eq!(watched.cancels(), 1, "only the fill timeout's own cancel of the short order's rest");
    assert!(rig.notifier.watchdog_calls().is_empty(), "nothing was done, nothing to report beyond the pair alert");
}

#[tokio::test(start_paused = true)]
async fn the_other_legs_unfilled_order_is_cancelled_and_verified_before_the_filled_leg_is_flattened() {
    let (rig, _h, watched) = started_watched(Opts::default()).await;
    // the short order is accepted and never fills; the fill timeout's own cancel does not work, so
    // the pair ends UNRESOLVED with that order still open
    watched.idle_orders(&sim_id(Leg::Short, OrderAction::Open));
    watched.swallow_cancels(1);
    run_until(&rig.clock, T + 5_000).await;
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED", "{:?}", labels(&rig.db));
    let since = ts_of(&rig.db, "UNRESOLVED")[0];

    run_until(&rig.clock, since + 5_000).await;
    assert_eq!(conclusions(&rounds(&rig.db)), vec!["WAIT_FOR_CANCEL"], "the open order of the other leg is not proof of anything yet");
    assert!(reduce_only_orders(&rig).is_empty(), "nothing is flattened while the other leg's order could still fill");
    assert_eq!(watched.cancelled_ids().last(), Some(&sim_id(Leg::Short, OrderAction::Open)));

    run_until(&rig.clock, since + 10_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["WAIT_FOR_CANCEL", "FLATTEN"], "{r:?}");
    let short = &r[1]["legs"]["short"];
    assert_eq!((short["position"].clone(), short["recorded"].clone(), short["orders_terminal"].clone()), (json!("0"), json!("0"), json!(true)), "{short}");
    assert_eq!((short["orders"][0]["state"].clone(), short["orders"][0]["filled"].clone()), (json!("Cancelled"), json!("0")), "verified: over and unfilled");
    let closes = reduce_only_orders(&rig);
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert_eq!((closes[0].exchange, closes[0].side, closes[0].quantity), (Exchange::Binance, OrderSide::Sell, dec("10")));
    assert_eq!(status(&rig.db, UUID), "UNRESOLVED", "the state is the user's to move");

    run_until(&rig.clock, since + 17_000).await;
    assert_eq!(conclusions(&rounds(&rig.db)), vec!["WAIT_FOR_CANCEL", "FLATTEN", "FLAT"]);
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert!(!rig.db.kill_switch_halted());
    sleep(Duration::from_millis(100)).await;
    assert_eq!(rig.notifier.watchdog_calls().len(), 1, "told once that the long leg was flattened");
}

#[tokio::test(start_paused = true)]
async fn after_a_restart_the_wait_is_counted_from_the_entry_and_a_lost_simulated_ledger_is_unknown() {
    let (rig, h, _w, since) = bare_long(Opts::default()).await;
    run_until(&rig.clock, since + 2_000).await;
    drop(h); // process 1 ends; the simulated ledger (in memory only) dies with it
    sleep(Duration::from_millis(50)).await;
    let second = Arc::new(RecordingNotifier::default());
    let _h2 = start(restarted(&rig, second.clone()));
    run_until(&rig.clock, since + 4_000).await;
    assert_eq!(count(&rig.db, wd::NAKED_LEG_WATCHDOG), 0, "the new process has not waited 5 s since its own start: it counts from the entry event");
    run_until(&rig.clock, since + 5_000).await;
    let r = rounds(&rig.db);
    assert_eq!(conclusions(&r), vec!["UNKNOWN"], "{r:?}");
    assert!(r[0]["reason"].as_str().unwrap().contains("not found"), "the simulated order is gone with the ledger: {}", r[0]);
    assert!(rig.db.kill_switch_halted(), "unknown means locked, in SIMULATION too");
    sleep(Duration::from_millis(100)).await;
    assert_eq!(second.watchdog_calls().len(), 1);
    assert!(second.calls().is_empty(), "the pair alert itself is not notified again after a restart");
}

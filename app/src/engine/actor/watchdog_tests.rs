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
        })
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
        self.inner.submit(req)
    }
    fn cancel(&self, exchange: Exchange, symbol: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.cancelled.lock().unwrap().push(id.to_string());
        let swallowed = self.swallow_cancels.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok();
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

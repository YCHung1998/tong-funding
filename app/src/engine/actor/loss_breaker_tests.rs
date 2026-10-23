//! loss-circuit-breaker through the actor (tasks 2.3 / 2.4). Child of `flow_tests` (reuses its rig,
//! the paused tokio clock and the in-memory exchange fakes; nothing here reaches a network).

use super::live_guardrail_tests::{armed_actor, live_deps, live_risk, locked, manual, why};
use super::*;
use crate::engine::loss_breaker::testkit::{append, closed};
use crate::engine::loss_breaker::{self as lb, LOSS_BREAKER_BASELINE, LOSS_BREAKER_TRIPPED};

fn tripped(db: &Db) -> Vec<Value> {
    events(db).into_iter().filter(|(_, l, _)| l == LOSS_BREAKER_TRIPPED).map(|(_, _, p)| p).collect()
}

/// `(id, event_type)` of every event, oldest first (ids tell what was written in one transaction).
fn ids(db: &Db) -> Vec<(i64, String)> {
    let plain = rusqlite::Connection::open(db.path()).unwrap();
    let mut st = plain.prepare("SELECT id, event_type FROM events ORDER BY id").unwrap();
    st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).unwrap().map(Result::unwrap).collect()
}

/// A tick: the actor looks again when one of the watched events moved.
fn tick(actor: &mut Actor) {
    actor.breaker_check();
}

// ---- 2.3: when it looks and what a trip does ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_third_losing_close_trips_once_with_the_kill_switch_and_the_record_in_one_transaction_and_one_notification() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    assert_eq!(count(&rig.db, LOSS_BREAKER_BASELINE), 1, "the first start wrote the baseline");
    assert!(!locked(&rig));
    for (i, delta) in ["-0.5", "-0.3"].into_iter().enumerate() {
        closed(&rig.db, &rig.clock, &format!("p{i}"), rig.clock.now_ms() + 1_000, delta);
        tick(&mut actor);
        assert!(!locked(&rig), "{} losses: inside the limit of 3", i + 1);
    }
    closed(&rig.db, &rig.clock, "p2", rig.clock.now_ms() + 1_000, "-1.2");
    tick(&mut actor);
    assert!(locked(&rig), "kill switch ON");
    let all = ids(&rig.db);
    let at = all.iter().position(|(_, t)| t == LOSS_BREAKER_TRIPPED).expect("recorded");
    assert_eq!(all[at - 1].1, "KILL_SWITCH_CHANGED");
    assert_eq!(all[at].0, all[at - 1].0 + 1, "written together: consecutive ids from one transaction");
    let p = &tripped(&rig.db)[0];
    assert_eq!((p["reason"].as_str(), p["consecutive_losses"].as_u64()), (Some("CONSECUTIVE_LOSSES"), Some(3)));
    assert_eq!(p["thresholds"]["loss_breaker_max_consecutive_losses"], json!(3));
    assert_eq!(p["pairs"].as_array().unwrap().len(), 3);
    assert_eq!(p["provisional"], json!(true), "no PnL net yet: the notification says 暫計");

    sleep(Duration::from_millis(50)).await;
    let notices = rig.notifier.breaker_calls();
    assert_eq!(notices.len(), 1, "one notification");
    assert!(notices[0].body().contains("連虧 3／3") && notices[0].body().contains("暫計"), "{}", notices[0].body());
    assert_eq!(count(&rig.db, "LOSS_BREAKER_NOTIFY"), 1);

    // once per interval: a fourth loss and many more looks write nothing new
    closed(&rig.db, &rig.clock, "p3", rig.clock.now_ms() + 1_000, "-1");
    tick(&mut actor);
    assert_eq!((count(&rig.db, LOSS_BREAKER_TRIPPED), rig.notifier.breaker_calls().len()), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn the_snapshot_data_follows_the_interval_and_shows_the_trip() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    let v = actor.breaker.clone().expect("looked at start");
    assert_eq!((v.loss_used, v.consecutive, v.tripped, v.max_loss, v.max_consecutive), (dec("0"), 0, false, dec("15"), 3));
    closed(&rig.db, &rig.clock, "a", rig.clock.now_ms() + 1_000, "-6.2");
    tick(&mut actor);
    let v = actor.breaker.clone().unwrap();
    assert_eq!((v.loss_used, v.consecutive, v.tripped, v.counted), (dec("6.2"), 1, false, 1));
    closed(&rig.db, &rig.clock, "b", rig.clock.now_ms() + 1_000, "-9");
    tick(&mut actor);
    let v = actor.breaker.clone().unwrap();
    assert!(v.tripped && v.reason == Some(tong_funding_core::loss_breaker::TripReason::MaxLoss), "{v:?}");
}

#[tokio::test(start_paused = true)]
async fn a_database_already_over_the_limit_trips_at_start_but_old_history_before_the_first_start_does_not() {
    // upgrade: 17 old losing pairs, no baseline yet -> the first start writes the baseline, nothing trips
    let (rig, deps) = rig(Opts::default());
    for i in 0..17 {
        closed(&rig.db, &rig.clock, &format!("old{i}"), rig.clock.now_ms() + 1_000, "-3");
    }
    let (actor, _h) = Actor::new(deps);
    assert!(!locked(&rig), "升級後不因舊虧損停機");
    assert_eq!(actor.breaker.as_ref().map(|v| (v.loss_used, v.counted)), Some((dec("0"), 0)));
    drop(actor);

    // restart with losses recorded since the baseline that were never judged (e.g. the process died)
    for i in 0..3 {
        closed(&rig.db, &rig.clock, &format!("new{i}"), rig.clock.now_ms() + 1_000, "-0.1");
    }
    let (_a2, _h2) = Actor::new(restarted(&rig, Arc::new(RecordingNotifier::default())));
    assert!(locked(&rig), "evaluated at start");
    assert_eq!(tripped(&rig.db).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn simulated_pairs_never_count_a_simulation_round_leaves_trading_open() {
    let (rig, _h) = started(Opts::default()).await;
    run_until(&rig.clock, T + 20_000).await;
    assert_eq!(status(&rig.db, UUID), "FINALIZED");
    assert_eq!(count(&rig.db, CLOSE_CONFIRMED), 1);
    assert!(!locked(&rig) && tripped(&rig.db).is_empty(), "a simulated pair holds no money and has no PnL: it is not an unknown amount");
}

#[tokio::test(start_paused = true)]
async fn a_demo_pair_whose_fees_are_not_recorded_trips_right_after_its_close_confirmation_as_unknown() {
    // FakeDemo fills carry no fee: the amount cannot be told -> stop (fail closed), reason written
    let (rig, h) = started(Opts { demo: true, ..Opts::default() }).await;
    run_until(&rig.clock, T + 16_000).await;
    assert_eq!(count(&rig.db, CLOSE_CONFIRMED), 1);
    assert!(locked(&rig), "{:?}", labels(&rig.db));
    let p = &tripped(&rig.db)[0];
    assert_eq!(p["reason"], json!("UNKNOWN_PNL"));
    assert_eq!(p["reason_text"], json!("損益未知"));
    assert_eq!(p["pairs"][0]["pair"], json!(UUID));
    let seq = labels(&rig.db);
    let (c, t) = (seq.iter().position(|l| l == CLOSE_CONFIRMED).unwrap(), seq.iter().position(|l| l == LOSS_BREAKER_TRIPPED).unwrap());
    assert!(c < t, "after the confirmation: {seq:?}");
    // the snapshot says so
    sleep(Duration::from_millis(300)).await;
    assert!(h.snapshots.borrow().blockers.contains(&Blocker::KillSwitch));
}

#[tokio::test(start_paused = true)]
async fn a_pnl_result_written_elsewhere_is_picked_up_on_the_next_look_and_its_net_replaces_the_provisional_amount() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    closed(&rig.db, &rig.clock, "a", rig.clock.now_ms() + 1_000, "-0.4");
    tick(&mut actor);
    assert_eq!(actor.breaker.as_ref().unwrap().loss_used, dec("0.4"));
    assert!(!locked(&rig));
    // the funding runner records a PnL whose net is -20 (a late fee, say)
    let breakdown = json!({
        "legs": [], "missing": [], "status": "Complete", "other_cost_included": false,
        "total": { "funding": "0", "price_ref": "0", "price_actual": "-19", "opening_fee": "0.5", "closing_fee": "0.5", "slippage": "0", "other_cost": "0", "net": "-20" },
    });
    append(&rig.db, "PAIR_PNL_RECOMPUTED", Some("a"), json!({ "status": "COMPLETE", "breakdown": breakdown }));
    tick(&mut actor);
    assert!(locked(&rig), "a PnL written elsewhere is seen within a tick");
    let p = &tripped(&rig.db)[0];
    assert_eq!((p["reason"].as_str(), p["pairs"][0]["basis"].as_str(), p["pairs"][0]["amount"].as_str()), (Some("MAX_LOSS"), Some("net"), Some("-20")));
    assert_eq!(p["provisional"], json!(false));
}

#[tokio::test(start_paused = true)]
async fn a_limit_reached_while_the_kill_switch_is_already_on_is_still_recorded_notified_and_named_in_the_status() {
    // review round 1 (A5): the user stopped by hand, then the losses of pairs already running reach the limit
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    rig.db.set_kill_switch(true).unwrap(); // the user stopped by hand
    for i in 0..3 {
        closed(&rig.db, &rig.clock, &format!("m{i}"), rig.clock.now_ms() + 1_000, "-1");
    }
    tick(&mut actor);
    assert_eq!(tripped(&rig.db).len(), 1, "the trip is on record although the switch was already ON");
    assert_eq!(tripped(&rig.db)[0]["reason"], json!("CONSECUTIVE_LOSSES"));
    assert!(actor.breaker.as_ref().unwrap().tripped, "and the numbers are shown as over the limit");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.notifier.breaker_calls().len(), 1, "the user is told");
    assert_eq!(actor.snapshot().status.stop_reason, Some(StopReason::LossBreaker), "the status bar says 熔斷, not 手動");
    tick(&mut actor);
    tick(&mut actor);
    assert_eq!((tripped(&rig.db).len(), rig.notifier.breaker_calls().len()), (1, 1), "once per interval");
    // the user opens trading again: the count starts from zero and the old losses do not trip
    rig.clock.advance(5_000);
    rig.db.set_kill_switch(false).unwrap();
    tick(&mut actor);
    assert!(!locked(&rig) && !actor.breaker.as_ref().unwrap().tripped);
    assert_eq!(actor.breaker.as_ref().unwrap().loss_used, dec("0"));
    // ... and three new losses trip it again, once
    for i in 0..3 {
        closed(&rig.db, &rig.clock, &format!("n{i}"), rig.clock.now_ms() + 1_000, "-1");
    }
    tick(&mut actor);
    assert!(locked(&rig) && tripped(&rig.db).len() == 2);
}

#[tokio::test(start_paused = true)]
async fn after_a_trip_new_entries_are_refused_but_closing_and_manual_handling_stay_open() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    for i in 0..3 {
        closed(&rig.db, &rig.clock, &format!("p{i}"), rig.clock.now_ms() + 1_000, "-1");
    }
    tick(&mut actor);
    assert!(locked(&rig));
    let r = actor.dispatch(Command::ManualEnter { pair: "x".into() });
    assert!(why(&r).contains("kill switch") || why(&r).contains("refused"), "{r:?}");
    assert_eq!(actor.dispatch(manual(true, "0.01")), CommandReply::Accepted, "a reduce-only order still goes out");
}

// ---- Live: a trip disarms ------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_live_trip_disarms_locks_and_says_why() {
    let cfg = RiskConfig { loss_breaker_max_loss_usdt: dec("5"), loss_breaker_max_consecutive_losses: 2, ..live_risk() };
    let (rig, actor, h) = armed_actor(live_deps("MANUAL", &cfg));
    let mut actor = actor;
    assert!(actor.armed() && !locked(&rig));
    closed(&rig.db, &rig.clock, "l1", rig.clock.now_ms() + 1_000, "-0.5");
    tick(&mut actor);
    assert!(actor.armed() && !locked(&rig), "one loss: inside 2");
    closed(&rig.db, &rig.clock, "l2", rig.clock.now_ms() + 1_000, "-0.2");
    tick(&mut actor);
    assert!(locked(&rig) && !actor.armed(), "kill switch ON and disarmed");
    assert!(!h.live.borrow().armed);
    assert!(h.live.borrow().last_lock_reason.as_deref().is_some_and(|r| r.contains("loss circuit breaker") && r.contains("連虧")), "{:?}", h.live.borrow().last_lock_reason);
    let p = &tripped(&rig.db)[0];
    assert_eq!((p["reason"].as_str(), p["thresholds"]["loss_breaker_max_loss_usdt"].as_str()), (Some("CONSECUTIVE_LOSSES"), Some("5")));
    sleep(Duration::from_millis(50)).await;
    assert!(rig.notifier.breaker_calls()[0].body().contains("LIVE"), "{}", rig.notifier.breaker_calls()[0].body());
    // arming again is a new interval (KILL_SWITCH_CHANGED off)
    let before = lb::interval_start(&rig.db).unwrap();
    rig.db.set_kill_switch_with_event(false, "LIVE_ARMED", &json!({})).unwrap();
    assert!(lb::interval_start(&rig.db).unwrap().event_id > before.event_id);
}

#[tokio::test(start_paused = true)]
async fn unreadable_settings_fall_back_to_the_stricter_live_limits() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    let (mut actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    assert_eq!(actor.breaker_limits(), (dec("15"), 3), "stored limits are used when readable");
    rig.db.config_set(CONFIG_RISK, &json!("not a config"), rig.db.config_get(CONFIG_RISK).unwrap().map(|e| e.version)).unwrap();
    assert_eq!(actor.breaker_limits(), (dec("5"), 2), "Live: the live defaults");
    let (_r2, d2) = rig_with_bad_settings();
    let (a2, _h2) = Actor::new(d2);
    assert_eq!(a2.breaker_limits(), (dec("15"), 3), "Demo: the defaults");
    let _ = &mut actor;
}

fn rig_with_bad_settings() -> (Rig, EngineDeps) {
    let (rig, deps) = rig(Opts::default());
    rig.db.config_set(CONFIG_RISK, &json!("not a config"), rig.db.config_get(CONFIG_RISK).unwrap().map(|e| e.version)).unwrap();
    (rig, deps)
}

// ---- 2.4: what the snapshot publishes for the status bar -----------------------------------------

use crate::engine::status::StopReason;

#[tokio::test(start_paused = true)]
async fn the_snapshot_says_allowed_when_nothing_blocks_and_counts_the_queue() {
    let (_rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    let s = actor.snapshot().status;
    assert!(s.stop_reason.is_none(), "{s:?}");
    assert_eq!((s.queued_pairs, s.next_entry_ms, s.needs_handling), (0, None, 0));
    let b = s.breaker.expect("the breaker numbers are always published");
    assert_eq!((b.loss_used, b.consecutive, b.max_loss, b.max_consecutive, b.tripped), (dec("0"), 0, dec("15"), 3, false));

    assert_eq!(actor.dispatch(Command::AddPrepared(pair_at(UUID, SYM, T))), CommandReply::Accepted);
    let s = actor.snapshot().status;
    assert_eq!((s.queued_pairs, s.next_entry_ms), (1, Some(T - 10_000)), "entry at T-10 s: 15:59:50 for a 16:00 settlement");
}

#[tokio::test(start_paused = true)]
async fn the_snapshot_names_why_entries_stopped() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    // by hand
    rig.db.set_kill_switch(true).unwrap();
    let s = actor.snapshot().status;
    assert_eq!(s.stop_reason, Some(StopReason::Manual));
    // by the watchdog
    rig.db.set_kill_switch(false).unwrap();
    rig.db.set_kill_switch_with_event(true, "NAKED_LEG_WATCHDOG_LOCK", &json!({ "reason": "x" })).unwrap();
    assert_eq!(actor.snapshot().status.stop_reason, Some(StopReason::Watchdog));
    // by the breaker
    rig.db.set_kill_switch(false).unwrap();
    for i in 0..3 {
        closed(&rig.db, &rig.clock, &format!("p{i}"), rig.clock.now_ms() + 1_000, "-1");
    }
    tick(&mut actor);
    let s = actor.snapshot().status;
    assert_eq!(s.stop_reason, Some(StopReason::LossBreaker));
    assert!(s.breaker.unwrap().tripped);
    // opened again: allowed
    rig.db.set_kill_switch(false).unwrap();
    tick(&mut actor);
    let s = actor.snapshot().status;
    assert!(s.stop_reason.is_none() && !s.breaker.unwrap().tripped);
}

#[tokio::test(start_paused = true)]
async fn a_live_engine_that_is_not_armed_is_stopped_and_an_armed_one_is_allowed() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    let (actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    let s = actor.snapshot().status;
    assert_eq!(s.stop_reason, Some(StopReason::LiveNotArmed), "locked at start");
    let (_r2, armed, _h2) = armed_actor(live_deps("MANUAL", &live_risk()));
    let s = armed.snapshot().status;
    assert!(s.stop_reason.is_none(), "{s:?}");
    let _ = &rig;
}

#[tokio::test(start_paused = true)]
async fn pairs_in_the_three_alert_states_are_counted_as_needing_handling() {
    let (rig, deps) = rig(Opts::default());
    for (uuid, state) in [("a", PairState::PartialFailure), ("b", PairState::Imbalanced), ("c", PairState::Unresolved), ("d", PairState::Reconciled)] {
        let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T + 3_600_000, simulated: true, scan: json!({}) };
        let row = crate::store::state::NewPair { internal_uuid: uuid.into(), pair_id: format!("pid-{uuid}"), symbol: format!("{uuid}USDT"), status: state, entry: serde_json::to_value(env).unwrap() };
        rig.db.add_pair_if_not_pending(&row).unwrap();
    }
    let (actor, _h) = Actor::new(deps);
    assert_eq!(actor.snapshot().status.needs_handling, 3);
}

// ---- review round 1 -----------------------------------------------------------------------------

fn save_cmd(cfg: &RiskConfig) -> Command {
    Command::SaveRiskSettings { risk: serde_json::to_value(cfg).unwrap(), overrides: json!({}) }
}

/// A4: new limits are looked at at once, and the number in the status bar follows them.
#[tokio::test(start_paused = true)]
async fn saving_lower_limits_re_evaluates_at_once_and_the_snapshot_shows_the_new_numbers() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    for i in 0..2 {
        closed(&rig.db, &rig.clock, &format!("p{i}"), rig.clock.now_ms() + 1_000, "-1");
    }
    tick(&mut actor);
    assert!(!locked(&rig), "2 USDT lost, limits 15 and 3");
    assert_eq!(actor.breaker.as_ref().unwrap().max_loss, dec("15"));
    let lower = RiskConfig { loss_breaker_max_loss_usdt: dec("2"), ..risk() };
    assert_eq!(actor.dispatch(save_cmd(&lower)), CommandReply::Accepted);
    tick(&mut actor);
    let v = actor.breaker.clone().unwrap();
    assert_eq!((v.max_loss, v.tripped), (dec("2"), true), "the new limit is in the numbers and it is exceeded: {v:?}");
    assert!(locked(&rig) && tripped(&rig.db).len() == 1, "tripped without waiting for another close");
    assert_eq!(tripped(&rig.db)[0]["reason"], json!("MAX_LOSS"));
}

#[tokio::test(start_paused = true)]
async fn saving_higher_limits_updates_the_numbers_too() {
    let (rig, deps) = rig(Opts::default());
    let (mut actor, _h) = Actor::new(deps);
    tick(&mut actor);
    let higher = RiskConfig { loss_breaker_max_loss_usdt: dec("40"), loss_breaker_max_consecutive_losses: 9, ..risk() };
    assert_eq!(actor.dispatch(save_cmd(&higher)), CommandReply::Accepted);
    tick(&mut actor);
    let v = actor.breaker.clone().unwrap();
    assert_eq!((v.max_loss, v.max_consecutive), (dec("40"), 9));
    assert!(!locked(&rig));
}

/// A4: while armed the two breaker limits are frozen like the other caps: lowering is allowed, raising is refused.
#[tokio::test(start_paused = true)]
async fn while_armed_the_breaker_limits_can_only_be_lowered() {
    let cfg = RiskConfig { loss_breaker_max_loss_usdt: dec("5"), loss_breaker_max_consecutive_losses: 2, ..live_risk() };
    let (rig, mut actor, _h) = armed_actor(live_deps("MANUAL", &cfg));
    let raise_loss = RiskConfig { loss_breaker_max_loss_usdt: dec("6"), ..cfg.clone() };
    let r = actor.dispatch(save_cmd(&raise_loss));
    assert!(why(&r).contains("loss_breaker_max_loss_usdt") && why(&r).contains("cannot be raised"), "{r:?}");
    let raise_run = RiskConfig { loss_breaker_max_consecutive_losses: 3, ..cfg.clone() };
    let r = actor.dispatch(save_cmd(&raise_run));
    assert!(why(&r).contains("loss_breaker_max_consecutive_losses") && why(&r).contains("cannot be raised"), "{r:?}");
    let lower = RiskConfig { loss_breaker_max_loss_usdt: dec("4"), loss_breaker_max_consecutive_losses: 1, ..cfg.clone() };
    assert_eq!(actor.dispatch(save_cmd(&lower)), CommandReply::Accepted, "lowering is allowed");
    assert!(actor.armed() && !locked(&rig));
}

/// A3: an existing Live database whose stored settings predate the breaker gets 5 / 2 once.
fn strip_breaker_keys(db: &Db, keep_loss: Option<&str>) {
    let entry = db.config_get(CONFIG_RISK).unwrap().unwrap();
    let mut v = entry.value.clone();
    let m = v.as_object_mut().unwrap();
    m.remove("loss_breaker_max_loss_usdt");
    m.remove("loss_breaker_max_consecutive_losses");
    if let Some(l) = keep_loss {
        m.insert("loss_breaker_max_loss_usdt".into(), json!(l));
    }
    db.config_set(CONFIG_RISK, &v, Some(entry.version)).unwrap();
}

fn stored_breaker(db: &Db) -> (Value, Value) {
    let v = db.config_get(CONFIG_RISK).unwrap().unwrap().value;
    (v["loss_breaker_max_loss_usdt"].clone(), v["loss_breaker_max_consecutive_losses"].clone())
}

#[tokio::test(start_paused = true)]
async fn an_existing_live_database_without_the_breaker_fields_gets_the_live_defaults_once_with_a_record() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    strip_breaker_keys(&rig.db, None);
    assert_eq!(stored_breaker(&rig.db), (Value::Null, Value::Null), "an old Live settings row");
    let before = count(&rig.db, "RISK_CONFIG_UPDATED");
    let (actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    assert_eq!(stored_breaker(&rig.db), (json!("5"), json!(2)), "the stricter Live values, not the looser 15 / 3 of a missing field");
    assert_eq!(actor.breaker_limits(), (dec("5"), 2));
    let updates = payloads(&rig.db, "RISK_CONFIG_UPDATED");
    assert_eq!(updates.len(), before + 1, "written through the settings path, with a record");
    let u = updates.last().unwrap();
    assert!(u["before"]["risk"]["loss_breaker_max_loss_usdt"].is_null(), "before: {u}");
    assert_eq!((u["after"]["risk"]["loss_breaker_max_loss_usdt"].clone(), u["after"]["risk"]["loss_breaker_max_consecutive_losses"].clone()), (json!("5"), json!(2)), "after: {u}");
    // a second start finds both and writes nothing
    drop(actor);
    let (_a2, _h2) = Actor::with_probes(restarted_live(&rig), crate::engine::preflight::fakes::FakeProbes::green());
    assert_eq!(count(&rig.db, "RISK_CONFIG_UPDATED"), before + 1);
}

fn restarted_live(rig: &Rig) -> EngineDeps {
    let mut d = restarted(rig, Arc::new(RecordingNotifier::default()));
    d.identity = crate::identity::ProcessIdentity::Live;
    d
}

#[tokio::test(start_paused = true)]
async fn a_value_the_user_already_set_is_never_touched_by_the_live_backfill() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    strip_breaker_keys(&rig.db, Some("8"));
    let (_actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    assert_eq!(stored_breaker(&rig.db), (json!("8"), json!(2)), "only the missing field is filled");
}

#[tokio::test(start_paused = true)]
async fn a_demo_database_is_not_touched_by_the_live_backfill() {
    let (rig, deps) = rig(Opts::default());
    strip_breaker_keys(&rig.db, None);
    let before = count(&rig.db, "RISK_CONFIG_UPDATED");
    let (actor, _h) = Actor::new(deps);
    assert_eq!(stored_breaker(&rig.db), (Value::Null, Value::Null));
    assert_eq!(count(&rig.db, "RISK_CONFIG_UPDATED"), before);
    assert_eq!(actor.breaker_limits(), (dec("15"), 3), "Demo keeps the defaults of a missing field");
}

/// A12: a breaker that cannot read the events locks, tells the user and says why.
#[tokio::test(start_paused = true)]
async fn when_the_events_cannot_be_read_the_breaker_locks_a_live_engine_tells_the_user_and_names_the_reason() {
    let cfg = live_risk();
    let (rig, mut actor, _h) = armed_actor(live_deps("MANUAL", &cfg));
    assert!(actor.armed() && !locked(&rig));
    rig.db.with_raw_conn_for_tests(|c| Ok(c.execute_batch("ALTER TABLE events RENAME TO events_unreadable")?)).unwrap();
    tick(&mut actor);
    assert!(locked(&rig) && !actor.armed(), "fail closed: entries stopped and the engine disarmed");
    let reason = actor.live_status().last_lock_reason.expect("the lock reason is shown with the arming panel");
    assert!(reason.contains("loss circuit breaker") && reason.contains("無法評估熔斷"), "{reason}");
    sleep(Duration::from_millis(50)).await;
    let notices = rig.notifier.breaker_calls();
    assert_eq!(notices.len(), 1, "told once");
    assert!(notices[0].body().contains("無法評估熔斷") && !notices[0].body().contains("已虧 0.00"), "{}", notices[0].body());
    let v = actor.breaker.clone().expect("the status bar has something to say");
    assert!(v.tripped && v.reason == Some(tong_funding_core::loss_breaker::TripReason::Unreadable), "{v:?}");
    tick(&mut actor);
    assert_eq!(rig.notifier.breaker_calls().len(), 1, "and not again on every tick");
}

// ---- review round 2, B3: armed = min(stored, frozen); a Live row that lacks a limit is the Live default ----

fn set_stored_breaker(db: &Db, loss: &str, consecutive: u32) {
    let entry = db.config_get(CONFIG_RISK).unwrap().unwrap();
    let mut v = entry.value.clone();
    v["loss_breaker_max_loss_usdt"] = json!(loss);
    v["loss_breaker_max_consecutive_losses"] = json!(consecutive);
    db.config_set(CONFIG_RISK, &v, Some(entry.version)).unwrap(); // straight into the store, not through the app's checks
}

#[tokio::test(start_paused = true)]
async fn while_armed_the_breaker_uses_the_stricter_of_the_stored_and_the_frozen_limit_and_still_trips_on_the_frozen_one() {
    let cfg = RiskConfig { loss_breaker_max_loss_usdt: dec("5"), loss_breaker_max_consecutive_losses: 2, ..live_risk() };
    let (rig, mut actor, _h) = armed_actor(live_deps("MANUAL", &cfg));
    assert_eq!(actor.breaker_limits(), (dec("5"), 2));
    set_stored_breaker(&rig.db, "50", 10); // raised behind the app's back
    assert_eq!(actor.breaker_limits(), (dec("5"), 2), "armed: never looser than the frozen values");
    set_stored_breaker(&rig.db, "4", 1);
    assert_eq!(actor.breaker_limits(), (dec("4"), 1), "stricter than frozen is honoured at once");
    set_stored_breaker(&rig.db, "50", 10);
    for (i, delta) in ["-0.5", "-0.3"].into_iter().enumerate() {
        closed(&rig.db, &rig.clock, &format!("p{i}"), rig.clock.now_ms() + 1_000, delta);
        tick(&mut actor);
    }
    assert!(locked(&rig), "two losses trip the frozen limit of 2 although the stored one says 10");
}

#[tokio::test(start_paused = true)]
async fn disarmed_the_stored_limit_counts_again() {
    let cfg = RiskConfig { loss_breaker_max_loss_usdt: dec("5"), loss_breaker_max_consecutive_losses: 2, ..live_risk() };
    let (rig, mut actor, _h) = armed_actor(live_deps("MANUAL", &cfg));
    actor.live_disarm();
    set_stored_breaker(&rig.db, "50", 10);
    assert_eq!(actor.breaker_limits(), (dec("50"), 10), "nothing is frozen when not armed");
}

#[tokio::test(start_paused = true)]
async fn a_live_row_that_lacks_a_limit_is_judged_by_the_live_default_not_the_looser_demo_one() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    let (actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    strip_breaker_keys(&rig.db, None); // after the start backfill: the row lacks both
    assert_eq!(actor.breaker_limits(), (dec("5"), 2), "5 / 2 of Live, not the 15 / 3 a missing field loads as");
    strip_breaker_keys(&rig.db, Some("8"));
    assert_eq!(actor.breaker_limits(), (dec("8"), 2), "a value that is there is used, the missing one is the Live default");
    rig.db.config_set(CONFIG_RISK, &json!({ "max_leverage": "0" }), rig.db.config_get(CONFIG_RISK).unwrap().map(|e| e.version)).unwrap();
    assert_eq!(actor.breaker_limits(), (dec("5"), 2), "unreadable settings: the Live default too");
    let (demo_rig, demo_deps) = rig_demo();
    let (demo, _h2) = Actor::new(demo_deps);
    strip_breaker_keys(&demo_rig.db, None);
    assert_eq!(demo.breaker_limits(), (dec("15"), 3), "Demo is unchanged");
}

fn rig_demo() -> (Rig, EngineDeps) {
    rig(Opts::default())
}

// ---- review round 2, B5: the backfill also covers "no risk row at all" and never fails silently ----

#[tokio::test(start_paused = true)]
async fn a_live_database_with_no_risk_row_at_all_gets_the_live_breaker_defaults_with_a_record() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    rig.db.with_raw_conn_for_tests(|c| Ok(c.execute("DELETE FROM config WHERE key = 'risk'", [])?)).unwrap();
    assert!(rig.db.config_get(CONFIG_RISK).unwrap().is_none());
    let before = count(&rig.db, "RISK_CONFIG_UPDATED");
    let (actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    assert_eq!(stored_breaker(&rig.db), (json!("5"), json!(2)), "written as the Live defaults, not left to load as 15 / 3");
    assert_eq!(count(&rig.db, "RISK_CONFIG_UPDATED"), before + 1);
    assert_eq!(actor.breaker_limits(), (dec("5"), 2));
}

#[tokio::test(start_paused = true)]
async fn a_backfill_that_fails_is_recorded_told_and_keeps_the_engine_locked() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    strip_breaker_keys(&rig.db, None);
    // a value the settings path refuses: the backfill cannot be saved
    let entry = rig.db.config_get(CONFIG_RISK).unwrap().unwrap();
    let mut v = entry.value.clone();
    v["max_leverage"] = json!("0");
    rig.db.config_set(CONFIG_RISK, &v, Some(entry.version)).unwrap();
    let (actor, _h) = Actor::with_probes(deps, crate::engine::preflight::fakes::FakeProbes::green());
    let failed = events(&rig.db).into_iter().filter(|(_, l, _)| l == "LIVE_BREAKER_BACKFILL_FAILED").map(|(_, _, p)| p).collect::<Vec<_>>();
    assert_eq!(failed.len(), 1, "{:?}", labels(&rig.db));
    assert!(failed[0]["error"].as_str().unwrap().contains("max_leverage"), "{}", failed[0]);
    assert!(locked(&rig) && !actor.armed(), "stays locked");
    let reason = actor.live_status().last_lock_reason.expect("the arming panel says why");
    assert!(reason.contains("breaker") || reason.contains("熔斷"), "{reason}");
    sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.notifier.breaker_calls().len(), 1, "and the user is told");
    assert_eq!(actor.breaker_limits(), (dec("5"), 2), "meanwhile the Live default judges");
}

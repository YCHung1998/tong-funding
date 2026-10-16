//! live-guardrails: the engine's own enforcement under the Live identity (start lock, preflight and
//! `Arm`, leg-notional and pairs-per-arming caps, manual orders, `FlattenLeg`, automatic locks, frozen
//! settings). Child of `flow_tests` (reuses its rig on the paused tokio clock). Every Live test builds
//! the rig with `identity: Live`; the Demo tests at the end prove nothing new is switched on there.

use super::*;
use crate::engine::command::{CONTRACT_SETTINGS_UPDATED, RISK_CONFIG_UPDATED};
use crate::engine::live_guard::{
    ARM_CONFIRMATION, FrozenCaps, LEG_FLATTEN, LEG_FLATTEN_ABORTED, LIVE_ARM_REFUSED, LIVE_ARMED, LIVE_AUTO_LOCK, LIVE_PREFLIGHT, LIVE_STARTUP_LOCK, caps_hash,
};
use crate::engine::preflight::PreflightReport;
use crate::engine::preflight::fakes::FakeProbes;
use crate::identity::ProcessIdentity;

const SOL: &str = "SOLUSDT";

fn live_risk() -> RiskConfig {
    RiskConfig { live_max_leg_notional_usdt: Some(dec("10")), live_max_pairs_per_arming: Some(2), ..risk() }
}

fn live_deps(trigger: &'static str, cfg: &RiskConfig) -> (Rig, EngineDeps) {
    let (rig, mut deps) = rig(Opts { trigger, ..Opts::default() });
    deps.identity = ProcessIdentity::Live;
    store_risk(&rig.db, cfg);
    (rig, deps)
}

/// A Live engine on the SIMULATION executor (MANUAL trigger), locked as every Live start is.
async fn live_started(cfg: &RiskConfig) -> (Rig, EngineHandle, Arc<FakeProbes>) {
    let (rig, deps) = live_deps("MANUAL", cfg);
    let probes = FakeProbes::green();
    let h = start_with_probes(deps, probes.clone());
    sleep(Duration::from_millis(50)).await;
    (rig, h, probes)
}

fn pair_n(uuid: &str, symbol: &str, notional: &str) -> NewPreparedPair {
    let mut p = pair_at(uuid, symbol, T);
    p.entry["notional_usdt"] = json!(notional);
    p
}

fn of(db: &Db, label: &str) -> Vec<Value> {
    events(db).into_iter().filter(|(_, l, _)| l == label).map(|(_, _, p)| p).collect()
}

fn locked(rig: &Rig) -> bool {
    rig.db.kill_switch_halted()
}

async fn preflight(h: &EngineHandle) -> PreflightReport {
    assert_eq!(ask(h, Command::RunPreflight).await, CommandReply::Accepted);
    sleep(Duration::from_millis(50)).await;
    let r = h.live.borrow().preflight.clone();
    r.expect("a preflight result is published")
}

fn arm_cmd(rep: &PreflightReport) -> Command {
    Command::Arm {
        preflight_id: rep.id.clone().expect("a green preflight has an id"),
        caps_hash: rep.caps_hash.clone().expect("a hash"),
        confirmation: ARM_CONFIRMATION.into(),
        checklist_ack: true,
    }
}

/// Preflight and Arm with everything right.
async fn arm_ok(h: &EngineHandle) {
    let rep = preflight(h).await;
    assert!(rep.passed(), "{:?}", rep.failures());
    assert_eq!(ask(h, arm_cmd(&rep)).await, CommandReply::Accepted);
    sleep(Duration::from_millis(20)).await;
}

fn opens(rig: &Rig) -> Vec<OrderRequest> {
    rig.sim.submitted().into_iter().filter(|r| !r.reduce_only).collect()
}

fn why(r: &CommandReply) -> String {
    match r {
        CommandReply::Rejected(w) => w.clone(),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn manual(reduce_only: bool, qty: &str) -> Command {
    Command::ManualOrder(ManualOrder {
        exchange: Exchange::Binance,
        symbol: SYM.into(),
        side: OrderSide::Sell,
        quantity: dec(qty),
        reduce_only,
        leverage: None,
        intended_base_qty: None,
    })
}

fn save_risk(cfg: &RiskConfig) -> Command {
    Command::SaveRiskSettings { risk: serde_json::to_value(cfg).unwrap(), overrides: json!({}) }
}

fn lock_reasons(db: &Db) -> Vec<String> {
    events(db).into_iter().filter(|(_, l, _)| l == LIVE_AUTO_LOCK).map(|(_, _, p)| p["reason"].as_str().unwrap_or_default().to_string()).collect()
}

// ---- 2.1 Live start: kill switch forced ON ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn live_start_forces_the_kill_switch_on_whatever_it_was_and_only_closing_is_accepted() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    assert!(!locked(&rig), "precondition: the stored kill switch is OFF");
    let h = start_with_probes(deps, FakeProbes::green());
    sleep(Duration::from_millis(50)).await;
    assert!(locked(&rig), "Live start writes ON");
    assert_eq!(count(&rig.db, LIVE_STARTUP_LOCK), 1);
    assert!(!h.live.borrow().armed && h.live.borrow().is_live);

    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted);
    let r = ask(&h, Command::ManualEnter { pair: UUID.into() }).await;
    assert!(why(&r).contains("not armed"), "{r:?}");
    assert!(opens(&rig).is_empty());
    // closing is accepted while locked (reduce-only manual order reaches the executor)
    assert_eq!(ask(&h, manual(true, "0.01")).await, CommandReply::Accepted);
    sleep(Duration::from_millis(50)).await;
    assert!(rig.sim.submitted().iter().any(|r| r.reduce_only), "{:?}", rig.sim.submitted());
}

#[tokio::test(start_paused = true)]
async fn every_live_restart_starts_locked_and_unarmed_even_after_a_successful_arm() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    assert!(!locked(&rig) && h.live.borrow().armed);
    drop(h); // process 1 ends (crash)
    sleep(Duration::from_millis(50)).await;
    let mut deps = restarted(&rig, Arc::new(RecordingNotifier::default()));
    deps.identity = ProcessIdentity::Live;
    let h2 = start_with_probes(deps, FakeProbes::green());
    sleep(Duration::from_millis(50)).await;
    assert!(locked(&rig), "the restart locked again");
    assert!(!h2.live.borrow().armed, "memory-only arming does not survive");
    assert_eq!(count(&rig.db, LIVE_STARTUP_LOCK), 2);
    assert!(why(&ask(&h2, Command::ManualEnter { pair: "x".into() }).await).contains("not armed"));
}

#[tokio::test(start_paused = true)]
async fn live_never_runs_auto_a_stored_auto_is_taken_back_and_switching_to_auto_is_refused() {
    let (rig, deps) = live_deps("AUTO", &live_risk());
    let h = start_with_probes(deps, FakeProbes::green());
    sleep(Duration::from_millis(50)).await;
    assert_eq!(h.snapshots.borrow().trigger_mode, TriggerMode::Manual);
    assert_eq!(gate::load_modes(&rig.db).0, TriggerMode::Manual, "persisted too");
    assert!(why(&ask(&h, Command::SetTriggerMode(TriggerMode::Auto)).await).contains("AUTO"));
    assert_eq!(ask(&h, Command::SetTriggerMode(TriggerMode::Manual)).await, CommandReply::Accepted);
    assert_eq!(h.snapshots.borrow().trigger_mode, TriggerMode::Manual);
}

#[tokio::test(start_paused = true)]
async fn the_kill_switch_cannot_be_lifted_by_hand_in_live_only_locked_by_hand() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    let r = ask(&h, Command::SetKillSwitch { on: false }).await;
    assert!(why(&r).contains("Arm"), "{r:?}");
    assert!(locked(&rig));
    arm_ok(&h).await;
    assert!(h.live.borrow().armed);
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    assert!(locked(&rig) && !h.live.borrow().armed, "a hand lock disarms the engine too");
}

#[tokio::test(start_paused = true)]
async fn a_kill_switch_flag_edited_behind_the_engines_back_arms_nothing() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted);
    rig.db.set_kill_switch(false).unwrap(); // someone writes OFF directly into the database
    assert!(!locked(&rig));
    let r = ask(&h, Command::ManualEnter { pair: UUID.into() }).await;
    assert!(why(&r).contains("not armed"), "{r:?}");
    assert!(opens(&rig).is_empty());
}

// ---- 2.2 / 2.3 preflight and Arm -----------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_green_preflight_then_arm_lifts_the_kill_switch_freezes_the_caps_and_is_single_use() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    let rep = preflight(&h).await;
    assert!(rep.passed() && rep.id.is_some(), "{:?}", rep.failures());
    assert_eq!(count(&rig.db, LIVE_PREFLIGHT), 1);
    assert_eq!(ask(&h, arm_cmd(&rep)).await, CommandReply::Accepted);
    assert!(!locked(&rig));
    let st = h.live.borrow().clone();
    assert!(st.armed && st.pairs_entered == 0 && st.preflight.is_none(), "{st:?}");
    let f = st.frozen.unwrap();
    assert_eq!((f.leg_notional_usdt, f.pairs_per_arming, f.max_concurrent_pairs, f.max_leverage), (dec("10"), 2, 3, dec("5")));
    let ev = of(&rig.db, LIVE_ARMED);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(ev[0]["caps"]["live_max_leg_notional_usdt"], json!("10"));
    assert!(!ev[0].to_string().contains("真錢"), "the typed sentence is not stored: {}", ev[0]);
    // the preflight id is spent
    assert!(why(&ask(&h, arm_cmd(&rep)).await).contains("no preflight"), "single use");
}

#[tokio::test(start_paused = true)]
async fn each_wrong_arm_input_is_refused_and_the_engine_stays_locked() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    let rep = preflight(&h).await;
    let good = |f: &dyn Fn(&mut (String, String, String, bool))| {
        let mut t = (rep.id.clone().unwrap(), rep.caps_hash.clone().unwrap(), ARM_CONFIRMATION.to_string(), true);
        f(&mut t);
        Command::Arm { preflight_id: t.0, caps_hash: t.1, confirmation: t.2, checklist_ack: t.3 }
    };
    let cases: Vec<(&str, Command)> = vec![
        ("not the one the engine issued", good(&|t| t.0 = "pf-forged".into())),
        ("not the one the engine issued", good(&|t| t.0.clear())),
        ("caps_hash does not match", good(&|t| t.1 = "0".repeat(64))),
        ("confirmation sentence", good(&|t| t.2 = "我了解這是真錢 live".into())),
        ("confirmation sentence", good(&|t| t.2.clear())),
        ("checklist", good(&|t| t.3 = false)),
    ];
    for (n, (needle, cmd)) in cases.into_iter().enumerate() {
        let r = ask(&h, cmd).await;
        assert!(why(&r).contains(needle), "case {n}: {r:?}");
        assert!(locked(&rig) && !h.live.borrow().armed, "case {n}: still locked");
        assert_eq!(count(&rig.db, LIVE_ARMED), 0);
    }
    assert_eq!(count(&rig.db, LIVE_ARM_REFUSED), 6, "every refusal is recorded");
    // a sentence with stray white space around it is the same sentence
    let padded = good(&|t| t.2 = format!("  {ARM_CONFIRMATION}\t"));
    assert_eq!(ask(&h, padded).await, CommandReply::Accepted);
    assert!(!locked(&rig));
}

#[tokio::test(start_paused = true)]
async fn arm_without_a_preflight_or_with_an_old_one_is_refused() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    let none = Command::Arm { preflight_id: "pf-1".into(), caps_hash: "h".into(), confirmation: ARM_CONFIRMATION.into(), checklist_ack: true };
    assert!(why(&ask(&h, none).await).contains("no preflight"));
    let rep = preflight(&h).await;
    rig.clock.advance(61_000);
    let r = ask(&h, arm_cmd(&rep)).await;
    assert!(why(&r).contains("old"), "{r:?}");
    assert!(locked(&rig));
    // a fresh run works again
    arm_ok(&h).await;
    assert!(!locked(&rig));
}

#[tokio::test(start_paused = true)]
async fn caps_changed_between_preflight_and_arm_are_refused_until_a_new_preflight() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    let rep = preflight(&h).await;
    let changed = RiskConfig { live_max_leg_notional_usdt: Some(dec("8")), ..live_risk() };
    store_risk(&rig.db, &changed);
    let r = ask(&h, arm_cmd(&rep)).await; // old hash vs the caps stored now
    assert!(why(&r).contains("caps_hash does not match"), "{r:?}");
    let with_new_hash = Command::Arm {
        preflight_id: rep.id.clone().unwrap(),
        caps_hash: caps_hash(&changed).unwrap(),
        confirmation: ARM_CONFIRMATION.into(),
        checklist_ack: true,
    };
    let r = ask(&h, with_new_hash).await; // right hash, but the preflight saw other caps
    assert!(why(&r).contains("run it again"), "{r:?}");
    assert!(locked(&rig));
    let rep2 = preflight(&h).await;
    assert_eq!(rep2.caps_hash, caps_hash(&changed));
    assert_eq!(ask(&h, arm_cmd(&rep2)).await, CommandReply::Accepted);
    assert_eq!(h.live.borrow().frozen.as_ref().unwrap().leg_notional_usdt, dec("8"));
}

#[tokio::test(start_paused = true)]
async fn a_failed_preflight_has_no_id_names_the_item_and_leaves_no_way_to_arm() {
    let (rig, h, probes) = live_started(&live_risk()).await;
    probes.one_way.lock().unwrap().insert(Exchange::Bybit, Ok(false));
    let rep = preflight(&h).await;
    assert!(!rep.passed() && rep.id.is_none());
    assert!(rep.failures().iter().any(|f| f.starts_with("one_way_mode[Bybit]")), "{:?}", rep.failures());
    let forged = Command::Arm { preflight_id: "pf-anything".into(), caps_hash: rep.caps_hash.clone().unwrap(), confirmation: ARM_CONFIRMATION.into(), checklist_ack: true };
    assert!(why(&ask(&h, forged).await).contains("did not pass"));
    assert!(locked(&rig));
    // fixed, run again: armed
    probes.one_way.lock().unwrap().insert(Exchange::Bybit, Ok(true));
    arm_ok(&h).await;
    assert!(!locked(&rig));
}

#[tokio::test(start_paused = true)]
async fn an_account_with_an_open_position_cannot_be_armed_and_unset_caps_make_the_settings_incomplete() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    rig.demo.positions.lock().unwrap().insert((Exchange::Binance, SYM.to_string()), dec("0.5"));
    let rep = preflight(&h).await;
    assert!(rep.failures().iter().any(|f| f.starts_with("no_existing_exposure[Binance]")), "{:?}", rep.failures());
    assert!(rep.id.is_none());
    rig.demo.positions.lock().unwrap().clear();
    store_risk(&rig.db, &risk()); // the caps are not set
    let rep = preflight(&h).await;
    assert!(rep.failures().iter().any(|f| f.starts_with("settings_complete") && f.contains("live_max_leg_notional_usdt")), "{:?}", rep.failures());
    assert!(rep.id.is_none() && rep.caps_hash.is_none());
}

/// live-guardrails 4.2b, the user's real situation: a fresh live database has no caps, the preflight
/// fails three items (settings, and both margin checks because the caps are unknown). Typing 10 and 2
/// into the risk page (vm form -> `SaveRiskSettings` -> engine) clears all three; 9 USDT per leg then
/// passes and the stored caps are what the engine freezes at arming.
#[tokio::test(start_paused = true)]
async fn caps_saved_from_the_risk_page_clear_the_three_preflight_failures_and_a_9_usdt_leg_fits_a_10_cap() {
    use crate::ui::bridge::{CommandSink, Settings};
    use crate::ui::risk_settings::{self, Field, RiskForm};
    #[derive(Default)]
    struct Sink(std::sync::Mutex<Vec<Command>>);
    impl CommandSink for Sink {
        fn send(&self, _label: String, command: Command) {
            self.0.lock().unwrap().push(command);
        }
    }

    let (rig, h, _p) = live_started(&risk()).await; // the caps are not set
    let rep = preflight(&h).await;
    let failures = rep.failures();
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(failures.iter().any(|f| f.starts_with("settings_complete") && f.contains("live_max_leg_notional_usdt") && f.contains("live_max_pairs_per_arming")), "{failures:?}");
    for ex in ["Binance", "Bybit"] {
        assert!(failures.iter().any(|f| f.starts_with(&format!("margin_sufficient[{ex}]")) && f.contains("caps unknown")), "{failures:?}");
    }

    let settings = Settings { risk: risk(), ..Settings::default() };
    let mut form = RiskForm::from_settings_for(&settings, ProcessIdentity::Live);
    form.set(Field::LiveMaxLegNotionalUsdt, "10");
    form.set(Field::LiveMaxPairsPerArming, "2");
    let vm = risk_settings::evaluate(&form, &settings);
    let sink = Sink::default();
    assert!(risk_settings::save(&vm, &sink), "{:?}", vm.field_errors);
    let cmd = sink.0.lock().unwrap().remove(0);
    assert_eq!(ask(&h, cmd).await, CommandReply::Accepted);
    assert_eq!(count(&rig.db, RISK_CONFIG_UPDATED), 1);

    let rep = preflight(&h).await;
    assert!(rep.passed() && rep.id.is_some(), "{:?}", rep.failures());
    assert_eq!(ask(&h, Command::SaveContractTemplate { notional_usdt: dec("9"), leverage: dec("5") }).await, CommandReply::Accepted, "9 fits a cap of 10");
    assert_eq!(ask(&h, Command::AddPrepared(pair_n("nine", SYM, "9"))).await, CommandReply::Accepted);
    assert_eq!(ask(&h, arm_cmd(&rep)).await, CommandReply::Accepted);
    let f = h.live.borrow().frozen.clone().unwrap();
    assert_eq!((f.leg_notional_usdt, f.pairs_per_arming), (dec("10"), 2), "what the page saved is what gets frozen");
}

#[tokio::test(start_paused = true)]
async fn a_live_engine_started_without_probes_can_never_be_armed() {
    let (rig, deps) = live_deps("MANUAL", &live_risk());
    let h = start(deps); // no composition-root probes
    sleep(Duration::from_millis(50)).await;
    let rep = preflight(&h).await;
    assert!(!rep.passed() && rep.id.is_none(), "{:?}", rep.failures());
    assert!(locked(&rig));
}

// ---- 3.1 caps -----------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn adding_a_pair_over_the_cap_is_refused_and_the_template_cannot_exceed_it() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    let r = ask(&h, Command::AddPrepared(pair_n("big", SYM, "25"))).await;
    assert!(why(&r).contains("above the live cap 10"), "{r:?}");
    assert!(rig.db.get_pair("big").unwrap().is_none());
    assert_eq!(ask(&h, Command::AddPrepared(pair_n("exact", "ETHUSDT", "10"))).await, CommandReply::Accepted, "equal to the cap passes this check");
    for (notional, ok) in [("10", true), ("9.5", true), ("10.01", false), ("1000", false)] {
        let r = ask(&h, Command::SaveContractTemplate { notional_usdt: dec(notional), leverage: dec("5") }).await;
        assert_eq!(r == CommandReply::Accepted, ok, "{notional}: {r:?}");
        if !ok {
            assert!(why(&r).contains("live cap"), "{r:?}");
        }
    }
    assert_eq!(count(&rig.db, CONTRACT_SETTINGS_UPDATED), 2);
    // caps unset: the engine cannot judge, so it refuses
    store_risk(&rig.db, &risk());
    let r = ask(&h, Command::SaveContractTemplate { notional_usdt: dec("1"), leverage: dec("5") }).await;
    assert!(why(&r).contains("cannot judge"), "{r:?}");
}

#[tokio::test(start_paused = true)]
async fn settings_above_the_built_in_ceilings_are_refused() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    for cfg in [
        RiskConfig { live_max_leg_notional_usdt: Some(dec("500")), ..live_risk() },
        RiskConfig { live_max_pairs_per_arming: Some(9), ..live_risk() },
    ] {
        let r = ask(&h, save_risk(&cfg)).await;
        assert!(why(&r).contains("ceiling"), "{r:?}");
        let r = ask(&h, Command::UpdateConfig { key: CONFIG_RISK.into(), value: serde_json::to_value(&cfg).unwrap() }).await;
        assert!(why(&r).contains("live_max_"), "{r:?}");
    }
    assert_eq!(count(&rig.db, RISK_CONFIG_UPDATED), 0);
}

#[tokio::test(start_paused = true)]
async fn an_entry_whose_order_would_pass_the_cap_only_without_the_protection_buffer_is_refused_unsent() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    // 10 USDT at 100 = 0.1 coin; worst price 101 -> 10.1 > 10
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "10"))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 11_000).await;
    assert!(rig.sim.submitted().is_empty(), "nothing reached the executor: {:?}", rig.sim.submitted());
    let shown = events(&rig.db).iter().map(|(_, _, p)| p.to_string()).collect::<Vec<_>>().join("\n");
    assert!(shown.contains("live cap") && shown.contains("10.1"), "the refusal names the cap and the figure");
    assert_ne!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(h.live.borrow().pairs_entered, 0, "a refused entry does not use up the arming");
}

#[tokio::test(start_paused = true)]
async fn an_entry_inside_the_cap_is_sent_with_the_sized_quantity() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9.9"))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 8_000).await;
    let sent = opens(&rig);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent.iter().all(|r| r.quantity == dec("0.099") && r.quantity * dec("101") <= dec("10")), "{sent:?}");
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(h.live.borrow().pairs_entered, 1);
}

#[tokio::test(start_paused = true)]
async fn the_latest_market_price_counts_for_the_worst_price() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    // 0.099 coin x 100.04 x 1.01 = 10.003 > 10, while 0.099 x 100 x 1.01 = 9.999 <= 10
    h.market.send_modify(|m| {
        m.insert((Exchange::Binance, SYM.to_string()), dec("100.04"));
    });
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9.9"))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 11_000).await;
    assert!(rig.sim.submitted().is_empty(), "the higher feed price pushed the long leg over the cap");
    let shown = events(&rig.db).iter().map(|(_, _, p)| p.to_string()).collect::<Vec<_>>().join("\n");
    assert!(shown.contains("long: live cap"), "{shown}");
}

#[tokio::test(start_paused = true)]
async fn a_cap_lowered_after_the_pair_was_added_blocks_it_at_node_0() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted);
    let lower = RiskConfig { live_max_leg_notional_usdt: Some(dec("5")), ..live_risk() };
    assert_eq!(ask(&h, save_risk(&lower)).await, CommandReply::Accepted, "lowering is allowed while armed");
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 11_000).await;
    assert_eq!(status(&rig.db, UUID), "BLOCKED");
    let blocked = events(&rig.db).into_iter().find(|(_, l, _)| l == "BLOCKED").unwrap().2;
    assert!(blocked.to_string().contains("RiskLimits: live: pair notional 9") && blocked.to_string().contains("cap 5"), "{blocked}");
    assert!(rig.sim.submitted().is_empty());
}

#[tokio::test(start_paused = true)]
async fn closing_orders_are_never_limited_by_the_caps() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    // far more than 10 USDT of coin, reduce-only: accepted and sent
    assert_eq!(ask(&h, manual(true, "1000")).await, CommandReply::Accepted);
    sleep(Duration::from_millis(50)).await;
    let sent = rig.sim.submitted();
    assert!(sent.len() == 1 && sent[0].reduce_only && sent[0].quantity == dec("1000"), "{sent:?}");
}

// ---- 3.2 pairs per arming -------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn the_second_pair_completes_then_the_engine_locks_and_the_third_is_refused() {
    let (rig, h, _p) = live_started(&live_risk()).await; // cap: 2 pairs per arming
    arm_ok(&h).await;
    for (u, s) in [("pair-a", SYM), ("pair-b", "ETHUSDT"), ("pair-c", SOL)] {
        assert_eq!(ask(&h, Command::AddPrepared(pair_n(u, s, "9"))).await, CommandReply::Accepted);
    }
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: "pair-a".into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 11_000).await;
    assert_eq!(h.live.borrow().pairs_entered, 1);
    assert!(!locked(&rig) && h.live.borrow().armed, "one of two: still armed");
    assert_eq!(ask(&h, Command::ManualEnter { pair: "pair-b".into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 8_000).await;
    assert_eq!(status(&rig.db, "pair-a"), "RECONCILED");
    assert_eq!(status(&rig.db, "pair-b"), "RECONCILED", "the second pair is completed as usual");
    assert_eq!(opens(&rig).len(), 4);
    assert!(locked(&rig) && !h.live.borrow().armed, "then the engine locks");
    let reasons = lock_reasons(&rig.db);
    assert!(reasons.len() == 1 && reasons[0].contains("pairs per arming reached"), "{reasons:?}");
    let r = ask(&h, Command::ManualEnter { pair: "pair-c".into() }).await;
    assert!(why(&r).contains("not armed"), "{r:?}");
    assert_eq!(opens(&rig).len(), 4, "no third pair");
}

// ---- 3.3 manual orders and FlattenLeg -------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn live_manual_opening_orders_are_refused_armed_or_not_and_reduce_only_stays_open() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    for armed in [false, true] {
        if armed {
            arm_ok(&h).await;
        }
        let r = ask(&h, manual(false, "0.01")).await;
        assert!(why(&r).contains("manual opening orders are disabled"), "armed={armed}: {r:?}");
        assert_eq!(ask(&h, manual(true, "0.01")).await, CommandReply::Accepted, "armed={armed}");
    }
    sleep(Duration::from_millis(50)).await;
    assert!(opens(&rig).is_empty(), "no opening order ever reached the executor");
    assert_eq!(rig.sim.submitted().len(), 2);
}

/// Armed Live engine with a pair whose short opening order was refused: the long leg is bare.
async fn bare_long_leg() -> (Rig, EngineHandle) {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    rig.sim.script_prefix(&sim_id(Leg::Short, OrderAction::Open), SimBehavior::Reject("scripted refusal".into()));
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(ask(&h, Command::ManualEnter { pair: UUID.into() }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 9_000).await;
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE");
    (rig, h)
}

#[tokio::test(start_paused = true)]
async fn flatten_leg_closes_only_the_bare_leg_with_the_actual_position_while_locked() {
    let (rig, h) = bare_long_leg().await;
    assert!(locked(&rig), "PARTIAL_FAILURE locked the engine");
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("0.09"));
    assert_eq!(ask(&h, Command::FlattenLeg { pair: UUID.into(), leg: Leg::Long }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 6_000).await;
    let closes: Vec<_> = rig.sim.submitted().into_iter().filter(|r| r.reduce_only).collect();
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert_eq!((closes[0].exchange, closes[0].side, closes[0].quantity), (Exchange::Binance, OrderSide::Sell, dec("0.09")));
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), Decimal::ZERO);
    assert_eq!(status(&rig.db, UUID), "PARTIAL_FAILURE", "the pair state is not changed");
    let ev = of(&rig.db, LEG_FLATTEN);
    assert!(ev.len() == 1 && ev[0]["leg"] == json!("long") && ev[0]["quantity"] == json!("0.09"), "{ev:?}");
    // the other leg never had a fill: nothing to close, nothing sent
    assert_eq!(ask(&h, Command::FlattenLeg { pair: UUID.into(), leg: Leg::Short }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 4_000).await;
    assert_eq!(rig.sim.submitted().iter().filter(|r| r.reduce_only).count(), 1);
    let aborted = of(&rig.db, LEG_FLATTEN_ABORTED);
    assert!(aborted.len() == 1 && aborted[0]["reason"].as_str().unwrap().contains("already flat"), "{aborted:?}");
    // flattening the same leg again finds it flat
    assert_eq!(ask(&h, Command::FlattenLeg { pair: UUID.into(), leg: Leg::Long }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 2_000).await;
    assert_eq!(rig.sim.submitted().iter().filter(|r| r.reduce_only).count(), 1, "no second close");
}

#[tokio::test(start_paused = true)]
async fn flatten_leg_keeps_the_imbalance_check_of_every_close() {
    let (rig, h) = bare_long_leg().await;
    // the user added 0.05 on the same symbol: 0.14 held vs 0.09 recorded (> 1 %): not the pair's alone
    user_order(&rig, OrderSide::Buy, "0.05", false).await;
    assert_eq!(rig.sim.position(Exchange::Binance, SYM), dec("0.14"));
    assert_eq!(ask(&h, Command::FlattenLeg { pair: UUID.into(), leg: Leg::Long }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 6_000).await;
    assert!(rig.sim.submitted().iter().all(|r| !r.reduce_only), "nothing sent");
    assert_eq!(count(&rig.db, CLOSE_QUANTITY_MISMATCH), 1);
    assert_eq!(count(&rig.db, LEG_FLATTEN_ABORTED), 1);
}

#[tokio::test(start_paused = true)]
async fn flatten_leg_applies_only_to_a_locked_pair_in_live() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted);
    let r = ask(&h, Command::FlattenLeg { pair: UUID.into(), leg: Leg::Long }).await;
    assert!(why(&r).contains("PARTIAL_FAILURE"), "{r:?}");
    let r = ask(&h, Command::FlattenLeg { pair: "nope".into(), leg: Leg::Long }).await;
    assert!(why(&r).contains("unknown pair"), "{r:?}");
    assert!(rig.sim.submitted().is_empty());
}

// ---- 3.4 automatic locks --------------------------------------------------------------------

/// An Actor (not running) armed in memory with the stored caps, to drive `on_event` / `land` directly.
fn armed_actor(rig_deps: (Rig, EngineDeps)) -> (Rig, Actor, EngineHandle) {
    let (test_rig, deps) = rig_deps;
    let (mut actor, h) = Actor::with_probes(deps, FakeProbes::green());
    rearm(&test_rig, &mut actor);
    (test_rig, actor, h)
}

fn rearm(rig: &Rig, actor: &mut Actor) {
    let (risk, _) = load_risk_config(&rig.db).unwrap();
    actor.live.frozen = Some(FrozenCaps::from_risk(&risk).unwrap());
    rig.db.set_kill_switch(false).unwrap();
}

fn seed_state(db: &Db, uuid: &str, symbol: &str, state: PairState) {
    let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T + 3_600_000, simulated: true, scan: json!({}) };
    let p = crate::store::state::NewPair { internal_uuid: uuid.into(), pair_id: format!("pid-{uuid}"), symbol: symbol.into(), status: state, entry: serde_json::to_value(env).unwrap() };
    db.add_pair_if_not_pending(&p).unwrap();
}

#[tokio::test(start_paused = true)]
async fn partial_failure_imbalanced_and_unresolved_each_lock_and_closing_stays_open() {
    use tong_funding_core::pair::SystemEvent as Ev;
    let cases = [
        (Ev::TimeoutPartialFill, PairState::PartialFailure, "PARTIAL_FAILURE"),
        (Ev::FillsExceedTolerance, PairState::Imbalanced, "IMBALANCED"),
        (Ev::TimeoutUndetermined, PairState::Unresolved, "UNRESOLVED"),
    ];
    for (n, (event, expect, label)) in cases.into_iter().enumerate() {
        let (rig, deps) = live_deps("MANUAL", &live_risk());
        seed_state(&rig.db, UUID, SYM, PairState::FillMonitor);
        let (rig, mut actor, _h) = armed_actor((rig, deps));
        assert!(!locked(&rig) && actor.armed(), "case {n}: armed before");
        assert_eq!(actor.land(UUID, event, json!({})), Ok(expect), "case {n}");
        assert!(locked(&rig) && !actor.armed(), "case {n}: {label} locks");
        let reasons = lock_reasons(&rig.db);
        assert!(reasons.len() == 1 && reasons[0].contains(label), "case {n}: {reasons:?}");
        // closing is not held back: a manual close and a reduce-only order are accepted
        assert_eq!(actor.dispatch(manual(true, "0.01")), CommandReply::Accepted, "case {n}");
        assert_eq!(actor.dispatch(Command::ManualClose { pair: UUID.into() }), CommandReply::Accepted, "case {n}");
    }
}

#[tokio::test(start_paused = true)]
async fn authentication_errors_and_http_418_429_lock_an_armed_engine_and_ordinary_failures_do_not() {
    let (rig, mut actor, _h) = armed_actor(live_deps("MANUAL", &live_risk()));
    let lookup = |reason: &str| Event::Queried { pair: None, client_order_id: "c".into(), outcome: QueryOutcome::Failed { reason: reason.into() } };
    let must_lock = [
        ("HTTP status 401", "authentication"),
        ("HTTP status 403", "authentication"),
        ("binance -2015: Invalid API-key, IP, or permissions for action (status unknown)", "authentication"),
        ("bybit 10010: Unmatched IP", "authentication"),
        ("HTTP status 418", "rate limited"),
        ("HTTP 429 without an exchange error code", "rate limited"),
        ("rate limited (retry after Some(1000) ms)", "rate limited"),
    ];
    for (n, (text, cause)) in must_lock.into_iter().enumerate() {
        rearm(&rig, &mut actor);
        actor.on_event(lookup(text));
        assert!(locked(&rig) && !actor.armed(), "{text}");
        let reasons = lock_reasons(&rig.db);
        assert_eq!(reasons.len(), n + 1, "{reasons:?}");
        assert!(reasons[n].contains(cause), "{text}: {reasons:?}");
        assert!(!reasons[n].contains("API-key") && !reasons[n].contains("Unmatched"), "the exchange text stays out of the event: {reasons:?}");
    }
    // the same texts through other event kinds
    rearm(&rig, &mut actor);
    actor.on_event(Event::ManualSubmitted { client_order_id: "m".into(), outcome: SubmitOutcome::Rejected { reason: "HTTP status 401".into() }, resolved: true });
    assert!(locked(&rig), "a rejected manual submit with an auth error");
    rearm(&rig, &mut actor);
    actor.on_event(Event::MarginFetched { pair: "x".into(), long: Err("HTTP status 429".into()), short: Ok(dec("1")) });
    assert!(locked(&rig), "a margin read answered with 429");
    // ordinary failures lock nothing
    let before = lock_reasons(&rig.db).len();
    for text in ["request timed out", "network error: connection reset", "binance -2010: Account has insufficient balance", "HTTP status 500"] {
        rearm(&rig, &mut actor);
        actor.on_event(lookup(text));
        assert!(!locked(&rig) && actor.armed(), "{text}");
    }
    assert_eq!(lock_reasons(&rig.db).len(), before);
}

#[tokio::test(start_paused = true)]
async fn an_unarmed_engine_writes_no_lock_event_for_an_error_and_demo_never_locks() {
    let (lrig, ldeps) = live_deps("MANUAL", &live_risk());
    let (mut actor, _h) = Actor::with_probes(ldeps, FakeProbes::green());
    actor.on_event(Event::Queried { pair: None, client_order_id: "c".into(), outcome: QueryOutcome::Failed { reason: "HTTP status 418".into() } });
    assert!(lock_reasons(&lrig.db).is_empty(), "already locked at start: nothing new to lock");
    let (drig, ddeps) = rig(Opts::default()); // Demo
    let (mut dactor, _dh) = Actor::new(ddeps);
    dactor.on_event(Event::Queried { pair: None, client_order_id: "c".into(), outcome: QueryOutcome::Failed { reason: "HTTP status 418".into() } });
    assert!(!locked(&drig) && lock_reasons(&drig.db).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_clock_check_before_entry_locks() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 13_000).await;
    rig.offsets.0.lock().unwrap().insert(Exchange::Binance, None);
    let r = ask(&h, Command::ManualEnter { pair: UUID.into() }).await;
    assert!(why(&r).contains("serverTime offset unavailable"), "{r:?}");
    assert!(locked(&rig) && !h.live.borrow().armed);
    let reasons = lock_reasons(&rig.db);
    assert!(reasons.len() == 1 && reasons[0].contains("clock"), "{reasons:?}");
    assert!(rig.sim.submitted().is_empty());
}

// ---- 3.5 frozen settings --------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn raising_any_cap_while_armed_is_refused_lowering_applies_at_once() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    let base = live_risk();
    let raises: Vec<(&str, RiskConfig)> = vec![
        ("live_max_leg_notional_usdt", RiskConfig { live_max_leg_notional_usdt: Some(dec("11")), ..base.clone() }),
        ("live_max_leg_notional_usdt", RiskConfig { live_max_leg_notional_usdt: None, ..base.clone() }),
        ("live_max_pairs_per_arming", RiskConfig { live_max_pairs_per_arming: Some(3), ..base.clone() }),
        ("max_concurrent_pairs", RiskConfig { max_concurrent_pairs: base.max_concurrent_pairs + 1, ..base.clone() }),
        ("max_leverage", RiskConfig { max_leverage: dec("6"), ..base.clone() }),
    ];
    for (n, (field, cfg)) in raises.iter().enumerate() {
        let r = ask(&h, save_risk(cfg)).await;
        assert!(why(&r).contains(field) && why(&r).contains("lock first"), "case {n}: {r:?}");
        let r = ask(&h, Command::UpdateConfig { key: CONFIG_RISK.into(), value: serde_json::to_value(cfg).unwrap() }).await;
        assert!(why(&r).contains(field), "case {n} UpdateConfig: {r:?}");
    }
    let ov = json!({ "Bybit": { "max_leverage": "9" } });
    let r = ask(&h, Command::SaveRiskSettings { risk: serde_json::to_value(&base).unwrap(), overrides: ov.clone() }).await;
    assert!(why(&r).contains("max_leverage override"), "{r:?}");
    let r = ask(&h, Command::UpdateConfig { key: CONFIG_RISK_OVERRIDES.into(), value: ov }).await;
    assert!(why(&r).contains("max_leverage override"), "{r:?}");
    assert_eq!(count(&rig.db, RISK_CONFIG_UPDATED), 0, "nothing was stored");
    // the same values are fine, so are lower ones
    assert_eq!(ask(&h, save_risk(&base)).await, CommandReply::Accepted);
    let lower = RiskConfig { live_max_leg_notional_usdt: Some(dec("5")), max_leverage: dec("3"), ..base.clone() };
    assert_eq!(ask(&h, save_risk(&lower)).await, CommandReply::Accepted);
    // effective at once: 9 USDT per leg is now above the cap in force
    let r = ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await;
    assert!(why(&r).contains("above the live cap 5"), "{r:?}");
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "5"))).await, CommandReply::Accepted);
    // a raise back to the frozen value is a raise over the stored one but not over the frozen one
    assert_eq!(ask(&h, save_risk(&base)).await, CommandReply::Accepted, "back up to the frozen value is allowed");
}

#[tokio::test(start_paused = true)]
async fn after_a_lock_the_caps_can_be_raised_again_and_the_next_arm_freezes_the_new_ones() {
    let (rig, h, _p) = live_started(&live_risk()).await;
    arm_ok(&h).await;
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    let up = RiskConfig { live_max_leg_notional_usdt: Some(dec("12")), ..live_risk() };
    assert_eq!(ask(&h, save_risk(&up)).await, CommandReply::Accepted, "locked: raising is allowed");
    arm_ok(&h).await;
    assert_eq!(h.live.borrow().frozen.as_ref().unwrap().leg_notional_usdt, dec("12"));
    assert!(!locked(&rig));
}

// ---- Demo: nothing new is switched on -------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn demo_is_unchanged_no_start_lock_no_arm_no_flatten_no_caps_and_manual_opens_still_work() {
    let (rig, deps) = rig(Opts::default());
    let h = start(deps);
    sleep(Duration::from_millis(50)).await;
    assert!(!locked(&rig), "Demo start does not touch the kill switch");
    assert_eq!(count(&rig.db, LIVE_STARTUP_LOCK), 0);
    assert!(!h.live.borrow().is_live);
    let arm = Command::Arm { preflight_id: "pf".into(), caps_hash: "h".into(), confirmation: ARM_CONFIRMATION.into(), checklist_ack: true };
    assert!(why(&ask(&h, arm).await).contains("Live"));
    assert!(why(&ask(&h, Command::RunPreflight).await).contains("Live"));
    assert!(why(&ask(&h, Command::FlattenLeg { pair: UUID.into(), leg: Leg::Long }).await).contains("Live"));
    // the Demo settings are not capped, the kill switch can be lifted by hand, manual opens work
    let r = ask(&h, Command::SaveContractTemplate { notional_usdt: dec("1000"), leverage: dec("5") }).await;
    assert_eq!(r, CommandReply::Accepted);
    assert_eq!(ask(&h, Command::AddPrepared(pair_at(UUID, SYM, T))).await, CommandReply::Accepted, "1000 USDT per leg");
    assert_eq!(ask(&h, Command::SetKillSwitch { on: true }).await, CommandReply::Accepted);
    assert_eq!(ask(&h, Command::SetKillSwitch { on: false }).await, CommandReply::Accepted);
    assert_eq!(ask(&h, Command::SetTriggerMode(TriggerMode::Auto)).await, CommandReply::Accepted);
    assert_eq!(ask(&h, manual(false, "0.5")).await, CommandReply::Accepted);
    sleep(Duration::from_millis(50)).await;
    assert_eq!(opens(&rig).len(), 1);
    assert_eq!(count(&rig.db, LIVE_AUTO_LOCK) + count(&rig.db, LIVE_ARM_REFUSED), 0);
}

// ---- the last gate before the opening legs are sent ----------------------------------------

fn open_set(rig: &Rig, qty: &str, worst: &str) -> (OrderSet, Option<Decimal>) {
    let (risk, overrides) = load_risk_config(&rig.db).unwrap();
    let req = |leg: Leg, exchange: Exchange| OrderRequest {
        prepared: None,
        notional_usdt: Some(dec("9")),
        client_order_id: sim_id(leg, OrderAction::Open),
        exchange,
        symbol: SYM.into(),
        side: OrderSide::for_leg(side_of(leg), OrderAction::Open),
        quantity: dec(qty),
        reduce_only: false,
        intended_base_qty: Some(dec(qty)),
        leverage: Some(dec("5")),
    };
    let set = OrderSet {
        sent_at_ms: 0,
        eff: effective_for_pair(&risk, &overrides, Exchange::Binance, Exchange::Bybit),
        legs: [Some(LegOrder::new(req(Leg::Long, Exchange::Binance), Decimal::ONE)), Some(LegOrder::new(req(Leg::Short, Exchange::Bybit), Decimal::ONE))],
        cancel_started: false,
    };
    (set, Some(dec(worst)))
}

#[tokio::test(start_paused = true)]
async fn the_final_check_before_sending_uses_the_cap_and_the_armed_state_as_they_stand_now() {
    let (test_rig, mut actor, _h) = armed_actor(live_deps("MANUAL", &live_risk()));
    let (set, worst) = open_set(&test_rig, "0.09", "101");
    actor.flows.entry(UUID.into()).or_default().live_worst_price = [worst, worst];
    assert_eq!(actor.live_final_check(UUID, &set), Ok(()), "0.09 x 101 = 9.09 <= 10");
    // the cap is lowered while the pre-send barrier ran
    store_risk(&test_rig.db, &RiskConfig { live_max_leg_notional_usdt: Some(dec("9")), ..live_risk() });
    let err = actor.live_final_check(UUID, &set).unwrap_err();
    assert!(err.starts_with("long:") && err.contains("above the live cap 9"), "{err}");
    // restore the cap; a lock during the barrier stops the send even though the numbers are fine
    store_risk(&test_rig.db, &live_risk());
    assert_eq!(actor.live_final_check(UUID, &set), Ok(()));
    actor.lock_live("test lock");
    assert!(actor.live_final_check(UUID, &set).unwrap_err().contains("not armed"));
    // an order with no usable price cannot be bounded: refuse
    rearm(&test_rig, &mut actor);
    actor.flows.entry(UUID.into()).or_default().live_worst_price = [None, None];
    assert!(actor.live_final_check(UUID, &set).unwrap_err().contains("no usable price"));
    // Demo: no check at all
    let (_demo_rig, demo_deps) = rig(Opts::default());
    let (demo_actor, _dh) = Actor::new(demo_deps);
    assert_eq!(demo_actor.live_final_check(UUID, &set), Ok(()));
}

/// live-process 5.2: a Live engine takes no OKX leg anywhere (add, entry, manual order) and its
/// effective `allowed_exchanges` never lists OKX; Demo keeps OKX.
#[tokio::test(start_paused = true)]
async fn a_live_engine_refuses_every_okx_leg_while_demo_keeps_them() {
    let okx_pair = |uuid: &str| {
        let mut p = pair_n(uuid, SYM, "9");
        p.short_exchange = Exchange::Okx;
        p
    };
    // add_prepared
    let (_rig, h, _p) = live_started(&live_risk()).await;
    let r = ask(&h, Command::AddPrepared(okx_pair("okx-1"))).await;
    assert!(matches!(&r, CommandReply::Rejected(m) if m.contains("OKX") && m.contains("live")), "{r:?}");
    assert_eq!(ask(&h, Command::AddPrepared(pair_n(UUID, SYM, "9"))).await, CommandReply::Accepted, "Binance/Bybit pairs still work");
    // manual order on OKX
    let mut o = manual(true, "0.01");
    if let Command::ManualOrder(m) = &mut o {
        m.exchange = Exchange::Okx;
    }
    let r = ask(&h, o).await;
    assert!(matches!(&r, CommandReply::Rejected(m) if m.contains("OKX")), "{r:?}");
    // start_entry (second line of defence: a pair that got in some other way)
    let (_rig2, deps) = live_deps("MANUAL", &live_risk());
    let (mut actor, _h2) = Actor::new(deps);
    actor.pairs.insert(
        "okx-2".into(),
        PairView { internal_uuid: "okx-2".into(), pair_id: "p".into(), symbol: SYM.into(), long_exchange: Exchange::Binance, short_exchange: Exchange::Okx, state: PairState::Prepared, settlement_ms: 1, simulated: true, flat_confirmed: false },
    );
    let r = actor.start_entry("okx-2", "test");
    assert!(matches!(&r, CommandReply::Rejected(m) if m.contains("OKX") && m.contains("live")), "{r:?}");
    // effective risk: OKX never allowed in live
    let mut cfg = live_risk();
    cfg.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx];
    let (_rig3, deps3) = live_deps("MANUAL", &cfg);
    let (actor3, _h3) = Actor::new(deps3);
    assert!(!actor3.load_risk().unwrap().0.allowed_exchanges.contains(&Exchange::Okx));
    // Demo keeps OKX
    let (rig4, deps4) = rig(Opts::default());
    store_risk(&rig4.db, &cfg);
    let (actor4, _h4) = Actor::new(deps4);
    assert!(actor4.load_risk().unwrap().0.allowed_exchanges.contains(&Exchange::Okx));
}

/// live-process: the id prefix follows the process identity for orders that reach an exchange
/// executor (`live` for Live, `demo` for Demo); only the simulator keeps `sim`.
#[tokio::test(start_paused = true)]
async fn real_orders_carry_the_prefix_of_the_process_identity() {
    for (identity, expect, other) in [(ProcessIdentity::Live, "live", "demo"), (ProcessIdentity::Demo, "demo", "live")] {
        let (rig, mut deps) = rig(Opts { demo: true, trigger: "MANUAL", ..Opts::default() });
        deps.identity = identity;
        store_risk(&rig.db, &live_risk());
        let h = start_with_probes(deps, FakeProbes::green());
        sleep(Duration::from_millis(50)).await;
        assert_eq!(ask(&h, Command::SetExecutionMode(ExecutionMode::ExchangeDemo)).await, CommandReply::Accepted, "{identity:?}");
        sleep(Duration::from_millis(50)).await;
        assert_eq!(ask(&h, manual(true, "0.01")).await, CommandReply::Accepted);
        sleep(Duration::from_millis(50)).await;
        let sent = rig.demo.requests.lock().unwrap().clone();
        assert!(!sent.is_empty(), "{identity:?}: the real executor must receive the order");
        assert!(sent.iter().all(|r| r.client_order_id.starts_with(expect) && !r.client_order_id.starts_with(other)), "{identity:?}: {sent:?}");
    }
}

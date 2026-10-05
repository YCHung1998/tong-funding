//! ui-trading-pages: the engine side of the trading pages. One-click submit (`EnterSelected`) in
//! AUTO races the scheduler only through the land-then-act PREPARED → PRE_TRADE_CHECK transition
//! (user decision 2026-10-05 evening); manual cancel goes through the current executor; risk
//! settings and the contract template are saved with their audit event in one transaction.
//! Child of `flow_tests` (reuses its rig on the paused tokio clock).

use super::*;
use crate::engine::command::{CONTRACT_SETTINGS_UPDATED, RISK_CONFIG_UPDATED};

fn opens(rig: &Rig) -> usize {
    rig.sim.submitted().iter().filter(|r| !r.reduce_only).count()
}

#[tokio::test(start_paused = true)]
async fn auto_one_click_after_the_scheduler_took_the_pair_is_refused_and_sends_nothing_extra() {
    let (rig, h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 10_000).await; // the scheduler's entry time (T−10 s)
    assert_ne!(status(&rig.db, UUID), "PREPARED", "the scheduler entered first");
    let r = ask(&h, Command::EnterSelected { pairs: vec![UUID.into()] }).await;
    assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("not PREPARED")), "{r:?}");
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(opens(&rig), 2, "exactly one entry: two opening orders");
    assert_eq!(count(&rig.db, "PRE_TRADE_CHECK"), 1);
}

#[tokio::test(start_paused = true)]
async fn auto_one_click_before_the_entry_time_enters_once_and_the_scheduler_does_not_enter_again() {
    let (rig, h) = started(Opts::default()).await;
    run_until(&rig.clock, T - 13_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
    assert_eq!(ask(&h, Command::EnterSelected { pairs: vec![UUID.into()] }).await, CommandReply::Accepted);
    run_until(&rig.clock, T - 5_000).await; // past the scheduler's own entry time
    assert_eq!(status(&rig.db, UUID), "RECONCILED");
    assert_eq!(opens(&rig), 2);
    assert_eq!(count(&rig.db, "PRE_TRADE_CHECK"), 1);
}

#[tokio::test(start_paused = true)]
async fn one_click_with_two_pairs_reports_each_refusal_and_enters_the_rest() {
    let (rig, h) = started(Opts { trigger: "MANUAL", ..Opts::default() }).await;
    run_until(&rig.clock, T - 13_000).await;
    let r = ask(&h, Command::EnterSelected { pairs: vec![UUID.into(), "nope".into()] }).await;
    assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("1/2") && why.contains("unknown pair nope")), "{r:?}");
    run_until(&rig.clock, T - 11_000).await;
    assert_eq!(status(&rig.db, UUID), "RECONCILED", "the valid pair still entered");
    assert_eq!(opens(&rig), 2);
}

#[tokio::test(start_paused = true)]
async fn one_click_is_refused_by_the_kill_switch_gate_and_sends_nothing() {
    let (rig, h) = started(Opts { trigger: "MANUAL", ..Opts::default() }).await;
    ask(&h, Command::SetKillSwitch { on: true }).await;
    let r = ask(&h, Command::EnterSelected { pairs: vec![UUID.into()] }).await;
    assert!(matches!(&r, CommandReply::Rejected(why) if why.contains("kill switch")), "{r:?}");
    run_until(&rig.clock, T - 5_000).await;
    assert_eq!(status(&rig.db, UUID), "PREPARED");
    assert!(rig.sim.submitted().is_empty());
    assert!(Command::EnterSelected { pairs: vec![] }.opens_exposure());
}

#[tokio::test(start_paused = true)]
async fn a_manual_cancel_goes_to_the_current_executor_and_its_answer_is_recorded_as_is() {
    let (rig, h) = started(Opts::default()).await;
    let cancel = Command::ManualCancel { exchange: Exchange::Bybit, symbol: "ETHUSDT".into(), client_order_id: "simunknown".into() };
    assert!(!cancel.opens_exposure(), "cancelling only reduces exposure");
    ask(&h, Command::SetKillSwitch { on: true }).await;
    assert_eq!(ask(&h, cancel).await, CommandReply::Accepted, "never refused by the kill switch");
    sleep(Duration::from_millis(50)).await;
    let ev: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == MANUAL_CANCEL_RESULT).map(|(_, _, p)| p).collect();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["result"], json!("not found"), "{}", ev[0]);
    assert_eq!(ev[0]["simulated"], json!(true));
    assert_eq!(ev[0]["client_order_id"], json!("simunknown"));
    assert_eq!(rig.factory.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn risk_settings_are_saved_together_with_one_event_holding_before_and_after() {
    let (rig, h) = started(Opts::default()).await;
    let mut next = risk();
    next.max_leverage = dec("4");
    let overrides = json!({"Bybit": {"max_leverage": "3"}});
    let r = ask(&h, Command::SaveRiskSettings { risk: serde_json::to_value(&next).unwrap(), overrides: overrides.clone() }).await;
    assert_eq!(r, CommandReply::Accepted);
    let (cfg, ov) = load_risk_config(&rig.db).unwrap();
    assert_eq!(cfg.max_leverage, dec("4"));
    assert_eq!(ov.get(&Exchange::Bybit).and_then(|o| o.max_leverage), Some(dec("3")));
    let ev: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == RISK_CONFIG_UPDATED).map(|(_, _, p)| p).collect();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["before"]["risk"]["max_leverage"], json!("5"));
    assert_eq!(ev[0]["after"]["risk"]["max_leverage"], json!("4"));
    assert_eq!(ev[0]["after"]["overrides"], overrides);
}

#[tokio::test(start_paused = true)]
async fn invalid_risk_settings_are_refused_by_field_and_nothing_changes() {
    let (rig, h) = started(Opts::default()).await;
    let before = load_risk_config(&rig.db).unwrap();
    for (risk_v, ov, field) in [
        (json!({"max_leverage": "0"}), json!({}), "max_leverage"),
        (json!({"execution_mode": "LIVE"}), json!({}), "LIVE"),
        (serde_json::to_value(risk()).unwrap(), json!({"Bybit": {"trigger_mode": "AUTO"}}), "trigger_mode"),
    ] {
        let r = ask(&h, Command::SaveRiskSettings { risk: risk_v, overrides: ov }).await;
        assert!(matches!(&r, CommandReply::Rejected(why) if why.contains(field)), "{field}: {r:?}");
    }
    assert_eq!(load_risk_config(&rig.db).unwrap(), before);
    assert_eq!(count(&rig.db, RISK_CONFIG_UPDATED), 0);
}

#[tokio::test(start_paused = true)]
async fn the_contract_template_is_validated_and_saved_with_its_event() {
    let (rig, h) = started(Opts::default()).await;
    let bad = ask(&h, Command::SaveContractTemplate { notional_usdt: dec("0"), leverage: dec("3") }).await;
    assert!(matches!(&bad, CommandReply::Rejected(why) if why.contains("notional_usdt")), "{bad:?}");
    let ok = ask(&h, Command::SaveContractTemplate { notional_usdt: dec("1200"), leverage: dec("3") }).await;
    assert_eq!(ok, CommandReply::Accepted);
    let stored = rig.db.config_get(CONFIG_CONTRACT_TEMPLATE).unwrap().unwrap().value;
    assert_eq!(stored, json!({"notional_usdt": "1200", "leverage": "3"}));
    let ev: Vec<Value> = events(&rig.db).into_iter().filter(|(_, l, _)| l == CONTRACT_SETTINGS_UPDATED).map(|(_, _, p)| p).collect();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["before"], Value::Null);
    assert_eq!(ev[0]["after"], stored);
    // The open pair is untouched by a template change.
    assert_eq!(status(&rig.db, UUID), "PREPARED");
}

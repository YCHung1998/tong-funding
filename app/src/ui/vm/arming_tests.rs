use super::*;
use crate::engine::live_guard::FrozenCaps;
use crate::engine::preflight::{CheckOutcome, PreflightItem, PreflightReport};
use crate::identity::ProcessIdentity;
use crate::ui::bridge::{CommandSink, EngineState, LiveStatusView, UiSnapshot};
use crate::ui::testkit::{complete_settings, d};
use std::cell::RefCell;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

const NOW: i64 = 1_800_000_000_000;

#[derive(Default)]
struct Sink(RefCell<Vec<(String, Command)>>);
impl CommandSink for Sink {
    fn send(&self, label: String, command: Command) {
        self.0.borrow_mut().push((label, command));
    }
}

fn live_snap() -> UiSnapshot {
    let mut s = UiSnapshot { identity: ProcessIdentity::Live, settings: complete_settings("0.01"), ..UiSnapshot::default() };
    s.settings.risk.live_max_leg_notional_usdt = Some(d("10"));
    s.settings.risk.live_max_pairs_per_arming = Some(2);
    s.engine = Some(EngineState { now_ms: NOW, trigger_mode: TriggerMode::Manual, execution_mode: ExecutionMode::ExchangeDemo, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] });
    s.live = LiveStatusView { is_live: true, ..LiveStatusView::default() };
    s
}

fn item(name: &'static str, ex: Option<Exchange>, ok: bool) -> PreflightItem {
    PreflightItem { name, exchange: ex, outcome: if ok { CheckOutcome::Pass } else { CheckOutcome::Fail("read failed".into()) } }
}

fn green(s: &UiSnapshot, issued: i64) -> PreflightReport {
    PreflightReport {
        id: Some("pf-1".into()),
        issued_at_ms: issued,
        items: vec![item(preflight::CHECK_KEYS, Some(Exchange::Binance), true), item(preflight::CHECK_DB, None, true)],
        caps_hash: live_guard::caps_hash(&s.settings.risk),
    }
}

fn ready_form() -> ArmingForm {
    ArmingForm { checked: [true; 4], confirmation: ARM_CONFIRMATION.into() }
}

/// A snapshot with a fresh green preflight and a form that is ready to arm.
fn ready() -> (UiSnapshot, ArmingForm) {
    let mut s = live_snap();
    s.live.preflight = Some(green(&s, NOW - 5_000));
    (s, ready_form())
}

#[test]
fn the_panel_never_exists_in_demo() {
    let (mut s, f) = ready();
    s.identity = ProcessIdentity::Demo;
    assert!(evaluate(&s, &f, NOW).is_none());
    let sink = Sink::default();
    assert!(arm(&s, &f, NOW, &sink).is_err());
    assert!(sink.0.borrow().is_empty(), "Demo can never send Arm from this panel");
}

#[test]
fn everything_in_order_enables_arm_and_sends_the_engines_own_ids() {
    let (s, f) = ready();
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(vm.can_arm && vm.arm_blockers.is_empty(), "{:?}", vm.arm_blockers);
    let sink = Sink::default();
    arm(&s, &f, NOW, &sink).unwrap();
    let sent = sink.0.borrow();
    assert_eq!(sent.len(), 1);
    let hash = live_guard::caps_hash(&s.settings.risk).unwrap();
    assert_eq!(sent[0].1, Command::Arm { preflight_id: "pf-1".into(), caps_hash: hash, confirmation: ARM_CONFIRMATION.into(), checklist_ack: true });
}

#[test]
fn no_preflight_or_a_failed_one_cannot_arm() {
    let (mut s, f) = ready();
    s.live.preflight = None;
    assert!(!evaluate(&s, &f, NOW).unwrap().can_arm);
    let mut r = green(&s, NOW - 1_000);
    r.items.push(item(preflight::CHECK_DISK, None, false));
    r.id = None; // the engine issues an id only for an all-green report
    s.live.preflight = Some(r);
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(!vm.can_arm);
    assert!(vm.preflight_lines.iter().any(|l| !l.pass && l.text.contains("read failed")), "{:?}", vm.preflight_lines);
    // even if an id were somehow present, a failed item still blocks
    s.live.preflight.as_mut().unwrap().id = Some("pf-1".into());
    assert!(!evaluate(&s, &f, NOW).unwrap().can_arm);
    let sink = Sink::default();
    assert!(arm(&s, &f, NOW, &sink).is_err() && sink.0.borrow().is_empty());
}

#[test]
fn an_expired_preflight_cannot_arm_and_the_edge_is_the_engines() {
    let (mut s, f) = ready();
    s.live.preflight = Some(green(&s, NOW - PREFLIGHT_VALID_MS));
    assert!(evaluate(&s, &f, NOW).unwrap().can_arm, "exactly 60 s old is still valid, like the engine");
    s.live.preflight = Some(green(&s, NOW - PREFLIGHT_VALID_MS - 1));
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(!vm.can_arm && vm.preflight_summary.contains("過期"), "{}", vm.preflight_summary);
    // a report from the future is not trusted either
    s.live.preflight = Some(green(&s, NOW + 1));
    assert!(!evaluate(&s, &f, NOW).unwrap().can_arm);
}

#[test]
fn a_confirmation_that_differs_by_one_character_cannot_arm() {
    let (s, mut f) = ready();
    for bad in ["", "我了解這是真錢 LIV", "我了解這是真錢 live", "我了解這是真錢LIVE", "我了解這是真钱 LIVE", "我了解這是真錢 LIVE!", "我了解這是真錢  LIVE"] {
        f.confirmation = bad.into();
        assert!(!evaluate(&s, &f, NOW).unwrap().can_arm, "{bad:?}");
    }
    f.confirmation = format!("  {ARM_CONFIRMATION}\n");
    assert!(evaluate(&s, &f, NOW).unwrap().can_arm, "surrounding whitespace is trimmed, as the engine does");
}

#[test]
fn every_checklist_item_must_be_ticked() {
    let (s, mut f) = ready();
    for i in 0..CHECKLIST.len() {
        f.checked = [true; 4];
        f.checked[i] = false;
        let vm = evaluate(&s, &f, NOW).unwrap();
        assert!(!vm.can_arm, "item {i}");
        assert!(vm.arm_blockers.iter().any(|b| b.contains("清單")));
    }
}

#[test]
fn caps_changed_after_the_preflight_cannot_arm() {
    let (mut s, f) = ready();
    s.settings.risk.live_max_leg_notional_usdt = Some(d("20"));
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(!vm.can_arm && vm.arm_blockers.iter().any(|b| b.contains("重新執行預檢")), "{:?}", vm.arm_blockers);
}

#[test]
fn a_running_preflight_or_a_stopped_engine_cannot_arm() {
    let (mut s, f) = ready();
    s.live.preflight_running = true;
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(!vm.can_arm && !vm.can_run_preflight);
    s.live.preflight_running = false;
    s.engine = None;
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(!vm.can_arm && !vm.can_run_preflight && !vm.can_lock);
}

#[test]
fn once_armed_the_panel_says_so_and_offers_lock_not_arm() {
    let (mut s, f) = ready();
    s.live.armed = true;
    s.live.pairs_entered = 1;
    s.live.frozen = Some(FrozenCaps { leg_notional_usdt: d("10"), pairs_per_arming: 2, max_concurrent_pairs: 3, max_leverage: d("5") });
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert_eq!(vm.status_text, "已武裝：本次已進場 1 / 上限 2 筆");
    assert!(vm.armed && !vm.can_arm && vm.can_lock && !vm.can_run_preflight);
    assert!(vm.caps_lines.iter().any(|l| l.contains("10 USDT")) && vm.caps_lines.iter().any(|l| l.contains("已凍結")));
}

#[test]
fn locked_state_says_why_and_lock_is_offered_only_while_unlocked() {
    let (mut s, f) = ready();
    s.live.last_lock_reason = Some("認證類錯誤".into());
    s.system.kill_switch = crate::ui::bridge::KillSwitchState::On;
    let vm = evaluate(&s, &f, NOW).unwrap();
    assert!(vm.status_text.contains("已鎖定") && vm.status_text.contains("認證類錯誤"));
    assert!(!vm.can_lock);
    s.system.kill_switch = crate::ui::bridge::KillSwitchState::Unknown;
    assert!(!evaluate(&s, &f, NOW).unwrap().can_lock, "unknown reads as locked");
}

#[test]
fn lock_now_sends_the_kill_switch_on() {
    let (mut s, f) = ready();
    s.live.armed = true;
    let vm = evaluate(&s, &f, NOW).unwrap();
    let sink = Sink::default();
    lock_now(&vm, &sink).unwrap();
    assert_eq!(sink.0.borrow()[0].1, Command::SetKillSwitch { on: true });
    let (mut s2, f2) = ready();
    s2.system.kill_switch = crate::ui::bridge::KillSwitchState::On;
    let locked = evaluate(&s2, &f2, NOW).unwrap();
    let sink2 = Sink::default();
    assert!(lock_now(&locked, &sink2).is_err() && sink2.0.borrow().is_empty());
}

#[test]
fn run_preflight_sends_the_command_only_when_allowed() {
    let (s, f) = ready();
    let vm = evaluate(&s, &f, NOW).unwrap();
    let sink = Sink::default();
    run_preflight(&vm, &sink).unwrap();
    assert_eq!(sink.0.borrow()[0].1, Command::RunPreflight);
    let mut busy = vm;
    busy.can_run_preflight = false;
    assert!(run_preflight(&busy, &sink).is_err());
    assert_eq!(sink.0.borrow().len(), 1);
}

#[test]
fn caps_before_arming_come_from_the_stored_settings_and_unset_is_said() {
    let mut s = live_snap();
    let vm = evaluate(&s, &ArmingForm::default(), NOW).unwrap();
    assert!(vm.caps_lines[0].contains("10 USDT") && vm.caps_lines[1].contains('2'));
    s.settings.risk.live_max_leg_notional_usdt = None;
    let vm = evaluate(&s, &ArmingForm::default(), NOW).unwrap();
    assert!(vm.caps_lines[0].contains("未設定"));
    assert!(!vm.can_arm);
}

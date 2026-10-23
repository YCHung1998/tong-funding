use super::*;
use crate::engine::loss_breaker::BreakerView;
use crate::engine::status::{EngineStatus, StopReason};
use crate::identity::ProcessIdentity;
use crate::ui::bridge::EngineState;
use tong_funding_core::risk::ExecutionMode;
use tong_funding_core::types::Decimal;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// 2026-10-05 15:59:20 UTC.
const NOW: i64 = 1_791_215_960_000;
/// The 16:00:00 UTC settlement of that day and its entry time 10 s earlier.
const SETTLE: i64 = 1_791_216_000_000;

fn breaker(used: &str, consecutive: u32, tripped: bool) -> BreakerView {
    BreakerView {
        start_ms: 0,
        net_sum: -d(used),
        loss_used: d(used),
        consecutive,
        max_loss: d("15"),
        max_consecutive: 3,
        tripped,
        reason: None,
        counted: consecutive as usize,
    }
}

fn status(stop: Option<StopReason>) -> EngineStatus {
    EngineStatus { stop_reason: stop, queued_pairs: 0, next_entry_ms: None, breaker: Some(breaker("0", 0, false)), needs_handling: 0 }
}

fn snap(identity: ProcessIdentity, mode: ExecutionMode, trigger: TriggerMode, s: EngineStatus) -> UiSnapshot {
    let engine = EngineState { now_ms: NOW, trigger_mode: trigger, execution_mode: mode, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] };
    UiSnapshot { identity, engine: Some(engine), engine_status: Some(s), ..UiSnapshot::default() }
}

fn demo(s: EngineStatus) -> UiSnapshot {
    snap(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, TriggerMode::Manual, s)
}

fn texts(v: &StatusBarVm) -> Vec<String> {
    vec![v.env.to_string(), v.trading.text.clone(), v.trigger.text.clone(), v.queue.text.clone(), v.breaker.text.clone(), v.handling.text.clone()]
}

#[test]
fn the_environment_is_plain_chinese_and_never_the_internal_code() {
    let words = |id, mode| build(&snap(id, mode, TriggerMode::Manual, status(None)), NOW).env;
    assert_eq!(words(ProcessIdentity::Demo, ExecutionMode::Simulation), "模擬");
    assert_eq!(words(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo), "Demo 帳戶");
    assert_eq!(words(ProcessIdentity::Live, ExecutionMode::ExchangeDemo), "真錢");
    for (id, mode) in [(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo), (ProcessIdentity::Live, ExecutionMode::ExchangeDemo), (ProcessIdentity::Demo, ExecutionMode::Simulation)] {
        for t in texts(&build(&snap(id, mode, TriggerMode::Auto, status(None)), NOW)) {
            assert!(!t.contains("EXCHANGE_DEMO") && !t.contains("Kill switch"), "{t}");
        }
    }
}

#[test]
fn trading_is_allowed_or_stopped_with_the_reason_in_words() {
    assert_eq!(build(&demo(status(None)), NOW).trading.text, "交易：允許");
    for (reason, word) in [(StopReason::Manual, "手動"), (StopReason::LossBreaker, "熔斷"), (StopReason::Watchdog, "看門狗"), (StopReason::LiveNotArmed, "Live 未武裝")] {
        assert_eq!(build(&demo(status(Some(reason))), NOW).trading.text, format!("交易：已停止（{word}）"));
    }
}

#[test]
fn a_tripped_breaker_shows_the_stop_reason_and_the_tripped_text() {
    // spec scenario 熔斷觸發後
    let s = EngineStatus { breaker: Some(breaker("16", 2, true)), ..status(Some(StopReason::LossBreaker)) };
    let v = build(&demo(s), NOW);
    assert_eq!(v.trading.text, "交易：已停止（熔斷）");
    assert_eq!(v.breaker.text, "熔斷已觸發");
}

#[test]
fn the_breaker_allowance_shows_used_over_limit_and_the_streak() {
    // spec scenario 狀態列顯示額度: -6.2 USDT, 1 loss, limits 15 and 3
    let s = EngineStatus { breaker: Some(breaker("6.2", 1, false)), ..status(None) };
    assert_eq!(build(&demo(s), NOW).breaker.text, "熔斷：已虧 6.20／15 USDT · 連虧 1／3");
    let none_yet = build(&demo(status(None)), NOW);
    assert_eq!(none_yet.breaker.text, "熔斷：已虧 0.00／15 USDT · 連虧 0／3");
}

#[test]
fn the_trigger_mode_is_automatic_with_the_reminder_colour_or_manual_without() {
    let auto = build(&snap(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, TriggerMode::Auto, status(None)), NOW);
    assert_eq!((auto.trigger.text.as_str(), auto.trigger.tone), ("觸發：自動", Some(Tone::Warning)));
    let manual = build(&demo(status(None)), NOW);
    assert_eq!((manual.trigger.text.as_str(), manual.trigger.tone), ("觸發：手動", None));
}

#[test]
fn a_queued_pair_shows_its_entry_time_in_utc_and_the_countdown() {
    // spec scenario: AUTO, one queued pair settling 16:00 UTC -> 15:59:50 UTC entry
    let s = EngineStatus { queued_pairs: 1, next_entry_ms: Some(SETTLE - 10_000), ..status(None) };
    let v = build(&snap(ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, TriggerMode::Auto, s), NOW);
    assert_eq!(v.queue.text, "排隊 1 對 · 15:59:50 UTC 進場（倒數 00:30）");
    assert_eq!(v.trigger.text, "觸發：自動");
}

#[test]
fn the_countdown_runs_down_clamps_at_zero_and_counts_minutes_past_an_hour() {
    let at = |next: i64, now: i64| {
        let s = EngineStatus { queued_pairs: 2, next_entry_ms: Some(next), ..status(None) };
        build(&demo(s), now).queue.text
    };
    assert!(at(SETTLE - 10_000, NOW + 29_000).ends_with("倒數 00:01）"));
    assert!(at(SETTLE - 10_000, SETTLE - 10_000).ends_with("倒數 00:00）"));
    assert!(at(SETTLE - 10_000, SETTLE).ends_with("倒數 00:00）"), "past the entry time: never negative");
    assert!(at(SETTLE - 10_000 + 75 * 60_000, SETTLE - 10_000).contains("倒數 75:00）"), "minutes keep counting");
    assert!(at(SETTLE - 10_000, NOW).starts_with("排隊 2 對"));
}

#[test]
fn no_queued_pair_says_so() {
    assert_eq!(build(&demo(status(None)), NOW).queue.text, "無排隊配對");
}

#[test]
fn pairs_needing_a_person_use_the_warning_colour_only_above_zero() {
    let zero = build(&demo(status(None)), NOW);
    assert_eq!((zero.handling.text.as_str(), zero.handling.tone), ("需處理：0", None));
    let two = build(&demo(EngineStatus { needs_handling: 2, ..status(None) }), NOW);
    assert_eq!((two.handling.text.as_str(), two.handling.tone), ("需處理：2", Some(Tone::Negative)));
}

#[test]
fn without_a_running_engine_nothing_is_allowed_and_nothing_is_guessed() {
    let v = build(&UiSnapshot::default(), NOW);
    assert_eq!(v.trading.text, "交易：已停止（引擎未運行）");
    assert_eq!(v.trigger.text, "觸發：—");
    assert_eq!(v.trigger.tone, None);
    assert_eq!((v.queue.text.as_str(), v.breaker.text.as_str(), v.handling.text.as_str()), ("無排隊配對", "熔斷：—", "需處理：—"));
}

#[test]
fn the_exchange_connection_dots_stay() {
    let v = build(&demo(status(None)), NOW);
    assert_eq!(v.exchanges.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["Binance", "Bybit", "OKX"]);
    assert!(v.exchanges.iter().all(|(_, c)| *c == Connection::Disconnected), "no health data: not shown as connected");
}

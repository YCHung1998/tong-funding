//! What the status bar needs from the engine (loss-circuit-breaker 2.4): whether new entries are
//! allowed and, when not, why; the queue of pairs waiting for their entry time; how many pairs need
//! a person; the breaker numbers. Pure functions over data the actor already holds, so the words
//! the page shows can be tested without an engine.

use tong_funding_core::pair::PairState;

use super::alert::is_alert_state;
use super::command::{Blocker, PairView};
use super::loss_breaker::BreakerView;
use super::timings::EngineTimings;
use tong_funding_core::risk::ExecutionMode;

/// Why new entries are stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The kill switch was turned on by the user (or by anything that wrote no reason).
    Manual,
    /// The loss circuit breaker tripped.
    LossBreaker,
    /// The naked-leg watchdog could not prove a situation and stopped entries.
    Watchdog,
    /// Live: not armed (locked at start, locked automatically, or simply not armed).
    LiveNotArmed,
    /// The store is halted or the kill switch cannot be read (fail closed).
    SystemHalt,
    /// Startup reconciliation of unfinished orders has not completed.
    Reconciling,
}

impl StopReason {
    /// The word in the status bar's 「交易：已停止（…）」.
    pub fn label(self) -> &'static str {
        match self {
            StopReason::Manual => "手動",
            StopReason::LossBreaker => "熔斷",
            StopReason::Watchdog => "看門狗",
            StopReason::LiveNotArmed => "Live 未武裝",
            StopReason::SystemHalt => "系統停機",
            StopReason::Reconciling => "啟動對帳中",
        }
    }
}

/// The reason behind a kill-switch-ON, from the `cause` the switch event itself carries (the type of
/// the reason event written in the same transaction; `None` = a plain switch = manual).
pub fn kill_cause(cause: Option<&str>) -> StopReason {
    match cause {
        Some("LOSS_BREAKER_TRIPPED") => StopReason::LossBreaker,
        Some("NAKED_LEG_WATCHDOG_LOCK") => StopReason::Watchdog,
        Some("LIVE_AUTO_LOCK" | "LIVE_STARTUP_LOCK") => StopReason::LiveNotArmed,
        _ => StopReason::Manual,
    }
}

/// The cause of the kill switch being ON right now: the `cause` of the newest `KILL_SWITCH_CHANGED`
/// when that one turned it on.
pub fn latest_kill_cause(db: &crate::store::db::Db) -> StopReason {
    let read = db.with_conn(|c| {
        let last: Option<String> =
            rusqlite::OptionalExtension::optional(c.query_row("SELECT payload FROM events WHERE event_type = 'KILL_SWITCH_CHANGED' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)))?;
        Ok(last.and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok()))
    });
    match read {
        Ok(Some(v)) if v["on"].as_bool() == Some(true) => kill_cause(v["cause"].as_str()),
        _ => StopReason::Manual,
    }
}

/// `None` = entries are allowed. `kill_cause` is only read when the kill switch is on; `live_armed` is
/// `Some(armed)` in a Live process and `None` otherwise.
pub fn stop_reason(blockers: &[Blocker], mode: ExecutionMode, kill_cause: StopReason, live_armed: Option<bool>) -> Option<StopReason> {
    let refusing = super::gate::refusing(blockers, mode);
    if refusing.iter().any(|b| matches!(b, Blocker::StoreHalted(_) | Blocker::KillSwitchUnreadable(_))) {
        Some(StopReason::SystemHalt)
    } else if refusing.contains(&Blocker::KillSwitch) {
        Some(kill_cause)
    } else if refusing.iter().any(|b| matches!(b, Blocker::ReconciliationPending(_))) {
        Some(StopReason::Reconciling)
    } else if live_armed == Some(false) {
        Some(StopReason::LiveNotArmed)
    } else {
        None
    }
}

/// Pairs waiting for their entry time (`PREPARED`) and the earliest entry time among them.
pub fn queue_summary(pairs: &[PairView], timings: &EngineTimings) -> (usize, Option<i64>) {
    let queued: Vec<&PairView> = pairs.iter().filter(|p| p.state == PairState::Prepared).collect();
    (queued.len(), queued.iter().map(|p| timings.entry_at(p.settlement_ms)).min())
}

/// Pairs that need a person: `PARTIAL_FAILURE`, `IMBALANCED`, `UNRESOLVED`.
pub fn needs_handling(pairs: &[PairView]) -> usize {
    pairs.iter().filter(|p| is_alert_state(p.state)).count()
}

/// Published with every engine snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineStatus {
    pub stop_reason: Option<StopReason>,
    pub queued_pairs: usize,
    /// Earliest entry time (UTC ms) among the queued pairs.
    pub next_entry_ms: Option<i64>,
    pub breaker: Option<BreakerView>,
    pub needs_handling: usize,
}

impl Default for EngineStatus {
    /// Before the engine published anything nothing is allowed (fail closed).
    fn default() -> Self {
        EngineStatus { stop_reason: Some(StopReason::SystemHalt), queued_pairs: 0, next_entry_ms: None, breaker: None, needs_handling: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tong_funding_core::types::Exchange;

    fn pair(state: PairState, settlement_ms: i64) -> PairView {
        PairView {
            internal_uuid: format!("u{settlement_ms}"),
            pair_id: "p".into(),
            symbol: "BTCUSDT".into(),
            long_exchange: Exchange::Binance,
            short_exchange: Exchange::Bybit,
            state,
            settlement_ms,
            simulated: false,
            flat_confirmed: false,
        }
    }

    #[test]
    fn the_reason_event_written_with_the_kill_switch_names_the_cause() {
        assert_eq!(kill_cause(Some("LOSS_BREAKER_TRIPPED")), StopReason::LossBreaker);
        assert_eq!(kill_cause(Some("NAKED_LEG_WATCHDOG_LOCK")), StopReason::Watchdog);
        assert_eq!(kill_cause(Some("LIVE_AUTO_LOCK")), StopReason::LiveNotArmed);
        assert_eq!(kill_cause(Some("LIVE_STARTUP_LOCK")), StopReason::LiveNotArmed);
        assert_eq!(kill_cause(None), StopReason::Manual, "a plain switch");
        assert_eq!(kill_cause(Some("ORDER_SUBMITTED")), StopReason::Manual, "any other cause is not a reason");
    }

    #[test]
    fn the_labels_are_the_words_of_the_spec() {
        let all = [StopReason::Manual, StopReason::LossBreaker, StopReason::Watchdog, StopReason::LiveNotArmed];
        assert_eq!(all.map(StopReason::label), ["手動", "熔斷", "看門狗", "Live 未武裝"]);
    }

    #[test]
    fn nothing_blocks_means_allowed_and_a_kill_switch_reports_its_cause() {
        let m = ExecutionMode::ExchangeDemo;
        assert_eq!(stop_reason(&[], m, StopReason::Manual, None), None);
        assert_eq!(stop_reason(&[Blocker::KillSwitch], m, StopReason::LossBreaker, None), Some(StopReason::LossBreaker));
        assert_eq!(stop_reason(&[Blocker::KillSwitch], m, StopReason::Watchdog, Some(false)), Some(StopReason::Watchdog), "the stored cause beats 'not armed'");
    }

    #[test]
    fn a_halted_store_wins_over_everything_and_an_unarmed_live_engine_is_stopped() {
        let m = ExecutionMode::ExchangeDemo;
        assert_eq!(stop_reason(&[Blocker::KillSwitch, Blocker::StoreHalted("x".into())], m, StopReason::Manual, None), Some(StopReason::SystemHalt));
        assert_eq!(stop_reason(&[Blocker::KillSwitchUnreadable("x".into())], m, StopReason::Manual, None), Some(StopReason::SystemHalt));
        assert_eq!(stop_reason(&[], m, StopReason::Manual, Some(false)), Some(StopReason::LiveNotArmed));
        assert_eq!(stop_reason(&[], m, StopReason::Manual, Some(true)), None, "armed and nothing blocks");
    }

    #[test]
    fn a_pending_reconciliation_stops_demo_entries_only() {
        let b = [Blocker::ReconciliationPending("r".into())];
        assert_eq!(stop_reason(&b, ExecutionMode::ExchangeDemo, StopReason::Manual, None), Some(StopReason::Reconciling));
        assert_eq!(stop_reason(&b, ExecutionMode::Simulation, StopReason::Manual, None), None, "the engine's own gate rule: SIMULATION entries stay allowed");
    }

    #[test]
    fn the_queue_counts_prepared_pairs_and_the_entry_time_is_settlement_minus_the_lead() {
        // spec: one queued pair settling 16:00 UTC enters at 15:59:50 (T-10 s)
        let t = EngineTimings::default();
        let settle = 1_800_000_000_000_i64;
        let pairs = [pair(PairState::Prepared, settle + 3_600_000), pair(PairState::Prepared, settle), pair(PairState::Reconciled, settle - 1), pair(PairState::Cancelled, settle - 2)];
        assert_eq!(queue_summary(&pairs, &t), (2, Some(settle - 10_000)), "the earliest of the two");
        assert_eq!(queue_summary(&[], &t), (0, None));
        assert_eq!(queue_summary(&[pair(PairState::Closing, settle)], &t), (0, None));
        let early = EngineTimings { entry_lead_ms: 5_000, ..EngineTimings::default() };
        assert_eq!(queue_summary(&pairs, &early).1, Some(settle - 5_000), "follows the configured lead");
    }

    #[test]
    fn needs_handling_counts_the_three_alert_states() {
        let pairs = [
            pair(PairState::PartialFailure, 1),
            pair(PairState::Imbalanced, 2),
            pair(PairState::Unresolved, 3),
            pair(PairState::Reconciled, 4),
            pair(PairState::Closing, 5),
            pair(PairState::Blocked, 6),
        ];
        assert_eq!(needs_handling(&pairs), 3);
        assert_eq!(needs_handling(&[]), 0);
    }

    #[test]
    fn the_cause_is_read_from_the_events_of_the_newest_kill_switch_on() {
        use crate::store::db::test_support::open_tmp;
        let (_d, db, _c) = open_tmp();
        assert_eq!(latest_kill_cause(&db), StopReason::Manual, "no event at all");
        db.set_kill_switch(true).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::Manual);
        db.set_kill_switch(false).unwrap();
        db.set_kill_switch_with_event(true, "LOSS_BREAKER_TRIPPED", &serde_json::json!({})).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::LossBreaker);
        db.set_kill_switch_with_event(true, "NAKED_LEG_WATCHDOG_LOCK", &serde_json::json!({})).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::Watchdog, "the newest wins");
        db.set_kill_switch_with_event(true, "LIVE_STARTUP_LOCK", &serde_json::json!({})).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::LiveNotArmed);
        db.set_kill_switch_with_event(false, "LIVE_ARMED", &serde_json::json!({})).unwrap();
        db.set_kill_switch(true).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::Manual, "an earlier trip does not follow a later plain switch");
    }

    #[test]
    fn the_cause_comes_from_the_switch_event_itself_not_from_the_event_after_it() {
        use crate::store::db::test_support::open_tmp;
        use crate::store::events::EventStore;
        let (_d, db, _c) = open_tmp();
        let es = EventStore::new(db.clone());
        // the newest switch event names its cause; an unrelated event right after it changes nothing
        es.append("KILL_SWITCH_CHANGED", None, serde_json::json!({ "on": true, "cause": "LOSS_BREAKER_TRIPPED" })).unwrap();
        es.append("SOMETHING_ELSE", None, serde_json::json!({})).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::LossBreaker);
        // a switch event without a cause (a plain switch, or one written before causes existed) is manual,
        // even when the next event happens to be a lock event
        es.append("KILL_SWITCH_CHANGED", None, serde_json::json!({ "on": true })).unwrap();
        es.append("NAKED_LEG_WATCHDOG_LOCK", None, serde_json::json!({})).unwrap();
        assert_eq!(latest_kill_cause(&db), StopReason::Manual);
    }

    #[test]
    fn the_default_status_allows_nothing() {
        let s = EngineStatus::default();
        assert!(s.stop_reason.is_some());
    }
}

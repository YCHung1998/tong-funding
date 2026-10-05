//! Partial-failure alerting (change exchange-demo-execution, task 3.1; design D8). Three channels
//! for every entry into PARTIAL_FAILURE / IMBALANCED / UNRESOLVED:
//! 1. the banner data in the `Snapshot` (`Snapshot::alerts`), DERIVED from the pairs' stored
//!    state, so it survives restarts and disappears only when a manual command moves the pair on;
//! 2. one system notification through a [`Notifier`] (best effort: a failure writes
//!    `ALERT_NOTIFY_FAILED` and changes nothing else);
//! 3. one immutable `PAIR_ALERT` event with exactly one [`AlertReason`] and both legs' data.
//!
//! "Once per entry" is keyed by the id of the `PAIR_TRANSITION` event that moved the pair into
//! the alert state: `PAIR_ALERT` / `ALERT_NOTIFIED` / `ALERT_NOTIFY_FAILED` events written after
//! that id mean "done", which is what makes a restart not notify again. A failed notification
//! counts as done too (the banner and the event remain; no retry loop, no spam).
//!
//! The real macOS mechanism (osascript or a notification framework that may need a signed app
//! bundle) is UNVERIFIED and needs the Mac (design Open Question 11): only the interface, a
//! logging implementation and a test recorder exist here.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::Value;
use tong_funding_core::pair::PairState;
use tong_funding_core::redact::redact_secrets;

use super::transition::PAIR_TRANSITION;
use crate::store::db::{Db, StoreError};

/// One alert entry (written once per entry into an alert state).
pub const PAIR_ALERT: &str = "PAIR_ALERT";
/// The notifier was called and reported success.
pub const ALERT_NOTIFIED: &str = "ALERT_NOTIFIED";
/// The notifier failed; the banner and `PAIR_ALERT` are unaffected.
pub const ALERT_NOTIFY_FAILED: &str = "ALERT_NOTIFY_FAILED";

/// The states that raise an alert (and hold exposure, design D13). Exhaustive on purpose.
pub const fn is_alert_state(state: PairState) -> bool {
    match state {
        PairState::PartialFailure | PairState::Imbalanced | PairState::Unresolved => true,
        PairState::Prepared
        | PairState::PreTradeCheck
        | PairState::Blocked
        | PairState::OrderSubmit
        | PairState::FillMonitor
        | PairState::Reconciled
        | PairState::Closing
        | PairState::Finalized
        | PairState::Cancelled => false,
    }
}

/// Why a pair entered an alert state. Every `PAIR_ALERT` carries exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertReason {
    /// One leg's submit was refused by the exchange.
    SubmitRejected,
    /// A submit's outcome stayed unknown (timeout, disconnect) and lookups did not settle it.
    SubmitUnknown,
    /// Fill timeout with one leg (partially) filled and the other not completely.
    FillTimeoutOneLeg,
    /// Both legs filled beyond `max_leg_imbalance_pct`.
    Imbalance,
    /// Restart reconciliation found something that does not add up.
    ReconcileMismatch,
    /// A close order failed / the pair is not flat after closing / quantity mismatch at close.
    CloseLegFailed,
    /// Fill confirmation could not be completed (lookups failed, cancel unconfirmed).
    FillUnconfirmed,
    /// Any other way into an alert state (kept so the classification stays total).
    Other,
}

impl AlertReason {
    pub const ALL: [AlertReason; 8] = [
        AlertReason::SubmitRejected,
        AlertReason::SubmitUnknown,
        AlertReason::FillTimeoutOneLeg,
        AlertReason::Imbalance,
        AlertReason::ReconcileMismatch,
        AlertReason::CloseLegFailed,
        AlertReason::FillUnconfirmed,
        AlertReason::Other,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AlertReason::SubmitRejected => "SUBMIT_REJECTED",
            AlertReason::SubmitUnknown => "SUBMIT_UNKNOWN",
            AlertReason::FillTimeoutOneLeg => "FILL_TIMEOUT_ONE_LEG",
            AlertReason::Imbalance => "IMBALANCE",
            AlertReason::ReconcileMismatch => "RECONCILE_MISMATCH",
            AlertReason::CloseLegFailed => "CLOSE_LEG_FAILED",
            AlertReason::FillUnconfirmed => "FILL_UNCONFIRMED",
            AlertReason::Other => "OTHER",
        }
    }

    pub fn parse(s: &str) -> Option<AlertReason> {
        AlertReason::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// From a `PAIR_TRANSITION` payload (`{from, to, event, detail}`); `detail.alert_reason`
    /// (set by the actor, which knows the legs) wins over the event name.
    pub fn of_transition(payload: &Value) -> AlertReason {
        if let Some(r) = payload.pointer("/detail/alert_reason").and_then(Value::as_str).and_then(AlertReason::parse) {
            return r;
        }
        let event = payload.get("event").and_then(Value::as_str).unwrap_or("");
        let has = |name: &str| event.contains(name);
        if has("OneLegSubmitFailed") {
            AlertReason::SubmitRejected
        } else if has("TimeoutPartialFill") {
            AlertReason::FillTimeoutOneLeg
        } else if has("TimeoutUndetermined") {
            AlertReason::FillUnconfirmed
        } else if has("FillsExceedTolerance") {
            AlertReason::Imbalance
        } else if has("CloseFailed") {
            AlertReason::CloseLegFailed
        } else if has("RestartFoundPartial") || has("RestartUndetermined") {
            AlertReason::ReconcileMismatch
        } else {
            AlertReason::Other
        }
    }
}

/// What a notification says (no secrets: ids, symbol, state and reason only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertNotice {
    pub pair: String,
    pub pair_id: String,
    pub symbol: String,
    pub state: PairState,
    pub reason: AlertReason,
    pub simulated: bool,
}

impl AlertNotice {
    pub fn title(&self) -> String {
        format!("tong-funding: {} {}", self.symbol, self.state)
    }
    pub fn body(&self) -> String {
        let mode = if self.simulated { "SIMULATION" } else { "EXCHANGE_DEMO" };
        redact_secrets(&format!("{} ({mode}) needs a manual decision: {}. Nothing is done automatically.", self.pair_id, self.reason.as_str()))
    }
}

/// One-shot system notification channel. May block (it runs on a blocking thread); `Err` is
/// recorded as `ALERT_NOTIFY_FAILED` and never retried automatically.
pub trait Notifier: Send + Sync {
    fn notify(&self, notice: &AlertNotice) -> Result<(), String>;
}

/// Placeholder until the macOS mechanism is chosen on the Mac (task 3.1): writes one line to
/// stderr and reports success. It is NOT a system notification.
pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn notify(&self, notice: &AlertNotice) -> Result<(), String> {
        eprintln!("[alert] {} — {}", notice.title(), notice.body());
        Ok(())
    }
}

/// Test notifier: records every call, optionally failing.
#[derive(Default)]
pub struct RecordingNotifier {
    pub calls: Mutex<Vec<AlertNotice>>,
    pub fail_with: Mutex<Option<String>>,
}

impl RecordingNotifier {
    pub fn calls(&self) -> Vec<AlertNotice> {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl Notifier for RecordingNotifier {
    fn notify(&self, notice: &AlertNotice) -> Result<(), String> {
        self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(notice.clone());
        match self.fail_with.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Where a pair stands with the alert of its current entry.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertEntry {
    /// Id of the `PAIR_TRANSITION` event that moved the pair into its current state.
    pub transition_id: i64,
    /// That event's payload.
    pub transition: Value,
    /// The `PAIR_ALERT` already written for this entry (its reason), if any.
    pub alerted: Option<AlertReason>,
    /// A notification was already attempted for this entry.
    pub notified: bool,
}

/// Reads the current entry of `pair` (which must be in `state`) from the event log.
pub fn entry_of(db: &Db, pair: &str, state: PairState) -> Result<Option<AlertEntry>, StoreError> {
    db.with_conn(|c| {
        let row: Option<(i64, String)> = c
            .query_row(
                "SELECT id, payload FROM events WHERE event_type = ?1 AND pair_id = ?2 AND json_extract(payload, '$.to') = ?3 ORDER BY id DESC LIMIT 1",
                rusqlite::params![PAIR_TRANSITION, pair, state.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
        let Some((transition_id, payload)) = row else { return Ok(None) };
        let alerted: Option<String> = c
            .query_row(
                "SELECT json_extract(payload, '$.reason') FROM events WHERE event_type = ?1 AND pair_id = ?2 AND id > ?3 ORDER BY id LIMIT 1",
                rusqlite::params![PAIR_ALERT, pair, transition_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
        let notified: i64 = c.query_row(
            "SELECT COUNT(*) FROM events WHERE event_type IN (?1, ?2) AND pair_id = ?3 AND id > ?4",
            rusqlite::params![ALERT_NOTIFIED, ALERT_NOTIFY_FAILED, pair, transition_id],
            |r| r.get(0),
        )?;
        Ok(Some(AlertEntry {
            transition_id,
            transition: serde_json::from_str(&payload).unwrap_or(Value::Null),
            alerted: alerted.as_deref().and_then(AlertReason::parse),
            notified: notified > 0,
        }))
    })
}

/// `PAIR_ALERT` count per reason with `from_ms <= ts_ms < to_ms` (spec "警示事件可依原因統計頻率").
/// Every reason is present (0 when none), so the counts always sum to the number of alerts.
pub fn count_by_reason(db: &Db, from_ms: i64, to_ms: i64) -> Result<BTreeMap<&'static str, i64>, StoreError> {
    let mut out: BTreeMap<&'static str, i64> = AlertReason::ALL.into_iter().map(|r| (r.as_str(), 0)).collect();
    let rows: Vec<(Option<String>, i64)> = db.with_conn(|c| {
        let mut st = c.prepare(
            "SELECT json_extract(payload, '$.reason'), COUNT(*) FROM events WHERE event_type = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 GROUP BY 1",
        )?;
        let rows = st.query_map(rusqlite::params![PAIR_ALERT, from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })?;
    for (reason, n) in rows {
        let key = reason.as_deref().and_then(AlertReason::parse).unwrap_or(AlertReason::Other).as_str();
        *out.entry(key).or_insert(0) += n;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_way_into_an_alert_state_has_exactly_one_reason() {
        let t = |event: &str| json!({ "from": "X", "to": "PARTIAL_FAILURE", "event": event, "detail": {} });
        assert_eq!(AlertReason::of_transition(&t("System(OneLegSubmitFailed)")), AlertReason::SubmitRejected);
        assert_eq!(AlertReason::of_transition(&t("System(TimeoutPartialFill)")), AlertReason::FillTimeoutOneLeg);
        assert_eq!(AlertReason::of_transition(&t("System(TimeoutUndetermined)")), AlertReason::FillUnconfirmed);
        assert_eq!(AlertReason::of_transition(&t("System(FillsExceedTolerance)")), AlertReason::Imbalance);
        assert_eq!(AlertReason::of_transition(&t("System(CloseFailed)")), AlertReason::CloseLegFailed);
        assert_eq!(AlertReason::of_transition(&t("System(RestartFoundPartial)")), AlertReason::ReconcileMismatch);
        assert_eq!(AlertReason::of_transition(&t("System(RestartUndetermined)")), AlertReason::ReconcileMismatch);
        assert_eq!(AlertReason::of_transition(&t("System(Retry)")), AlertReason::Other);
        let hinted = json!({ "event": "System(TimeoutUndetermined)", "detail": { "alert_reason": "SUBMIT_UNKNOWN" } });
        assert_eq!(AlertReason::of_transition(&hinted), AlertReason::SubmitUnknown);
        for r in AlertReason::ALL {
            assert_eq!(AlertReason::parse(r.as_str()), Some(r));
        }
    }

    #[test]
    fn exactly_the_three_locked_states_alert() {
        let alerting: Vec<PairState> = PairState::ALL.into_iter().filter(|s| is_alert_state(*s)).collect();
        assert_eq!(alerting, vec![PairState::Imbalanced, PairState::PartialFailure, PairState::Unresolved]);
    }

    #[test]
    fn the_notice_names_the_pair_and_reason_but_nothing_secret() {
        let n = AlertNotice {
            pair: "u1".into(),
            pair_id: "pid-u1".into(),
            symbol: "BTCUSDT".into(),
            state: PairState::PartialFailure,
            reason: AlertReason::SubmitRejected,
            simulated: false,
        };
        assert!(n.title().contains("BTCUSDT") && n.title().contains("PARTIAL_FAILURE"));
        assert!(n.body().contains("SUBMIT_REJECTED") && n.body().contains("EXCHANGE_DEMO"));
    }
}

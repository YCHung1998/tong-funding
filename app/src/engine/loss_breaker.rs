//! loss-circuit-breaker, store side: where the interval starts, which closed pairs count and what
//! each is worth. Everything here reads the append-only `events` table (and, to recompute a PnL,
//! the same assembly the PnL record uses); the pure arithmetic lives in `core::loss_breaker`.
//!
//! The actor (`actor::loss_breaker`) decides WHEN to evaluate and what to do on a trip.

use rusqlite::params;
use serde_json::{Value, json};
use tong_funding_core::loss_breaker::{BreakerState, TripReason, evaluate};
use tong_funding_core::risk::RiskConfig;
use tong_funding_core::types::Decimal;

use crate::store::db::Db;
use crate::store::events::{EventRow, EventStore};

pub const KILL_SWITCH_CHANGED: &str = "KILL_SWITCH_CHANGED";
/// Written once, the first time this feature runs on a database: the history before it is never counted.
pub const LOSS_BREAKER_BASELINE: &str = "LOSS_BREAKER_BASELINE";
/// The breaker tripped: written in the same transaction as the kill switch turning ON.
pub const LOSS_BREAKER_TRIPPED: &str = "LOSS_BREAKER_TRIPPED";

/// What opened the current interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    /// The latest `KILL_SWITCH_CHANGED {on: false}` (the user opened trading; a Live arming does too).
    TradingOpened,
    /// The `LOSS_BREAKER_BASELINE` of the first start: nothing before it counts.
    Baseline,
}

/// The event the current interval starts after (compared by event id, which follows append order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalStart {
    pub event_id: i64,
    pub ts_ms: i64,
    pub kind: StartKind,
}

/// `(id, ts_ms, payload)` of every event of `ty`, oldest first.
fn events_of(db: &Db, ty: &str) -> Result<Vec<(i64, i64, Option<String>, Value)>, String> {
    db.with_conn(|c| {
        let mut st = c.prepare("SELECT id, ts_ms, pair_id, payload FROM events WHERE event_type = ?1 ORDER BY id")?;
        let rows = st.query_map(params![ty], |r| {
            let payload: String = r.get(3)?;
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, serde_json::from_str(&payload).unwrap_or(Value::Null)))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .map_err(|e| e.to_string())
}

/// The start of the current interval: the later (by event id) of the newest
/// `KILL_SWITCH_CHANGED {on: false}` and the `LOSS_BREAKER_BASELINE`. The baseline is written here
/// when this database never had one, so the history before the first start never counts, even when
/// the kill switch was opened (and the pairs lost money) before this version existed.
pub fn interval_start(db: &Db) -> Result<IntervalStart, String> {
    let mut baseline = events_of(db, LOSS_BREAKER_BASELINE)?.into_iter().next_back();
    if baseline.is_none() {
        let payload = json!({ "reason": "first start with the loss circuit breaker: earlier history is not counted" });
        let id = EventStore::new(db.clone()).append(LOSS_BREAKER_BASELINE, None, payload.clone()).map_err(|e| e.to_string())?.ok_or("baseline event not stored")?;
        baseline = Some((id, db.now_ms(), None, payload));
    }
    let opened = events_of(db, KILL_SWITCH_CHANGED)?.into_iter().rfind(|(_, _, _, p)| p["on"] == Value::Bool(false));
    let (id, ts, _, _) = baseline.ok_or("no baseline")?;
    let baseline = IntervalStart { event_id: id, ts_ms: ts, kind: StartKind::Baseline };
    Ok(match opened {
        Some((oid, ots, _, _)) if oid > baseline.event_id => IntervalStart { event_id: oid, ts_ms: ots, kind: StartKind::TradingOpened },
        _ => baseline,
    })
}

/// A pair whose `CLOSE_CONFIRMED` falls inside the interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedPair {
    pub pair: String,
    pub event_id: i64,
    pub ts_ms: i64,
}

/// Pairs with a `CLOSE_CONFIRMED` after `start`, ordered by that event's time (then id), each pair once.
/// Simulated pairs are left out: they hold no money and have no PnL by design.
pub fn closed_pairs_since(db: &Db, start: &IntervalStart) -> Result<Vec<ClosedPair>, String> {
    let mut out: Vec<ClosedPair> = Vec::new();
    for (id, ts, pair, payload) in events_of(db, crate::engine::actor::CLOSE_CONFIRMED)? {
        let Some(pair) = pair else { continue };
        if id <= start.event_id || payload["simulated"] == Value::Bool(true) || out.iter().any(|c| c.pair == pair) {
            continue;
        }
        out.push(ClosedPair { pair, event_id: id, ts_ms: ts });
    }
    out.sort_by_key(|c| (c.ts_ms, c.event_id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::test_support::open_tmp;

    fn append(db: &Db, ty: &str, pair: Option<&str>, payload: Value) -> i64 {
        EventStore::new(db.clone()).append(ty, pair, payload).unwrap().unwrap()
    }

    fn count(db: &Db, ty: &str) -> usize {
        db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events WHERE event_type = ?1", [ty], |r| r.get::<_, i64>(0))?)).unwrap() as usize
    }

    fn close(db: &Db, pair: &str, simulated: bool) -> i64 {
        append(db, "CLOSE_CONFIRMED", Some(pair), json!({ "verified_flat": true, "simulated": simulated }))
    }

    // ---- 2.1 interval start --------------------------------------------------------------

    #[test]
    fn the_first_start_writes_one_baseline_and_counts_nothing_before_it() {
        // spec: 17 old losing pairs, kill switch never closed, first start with this version
        let (_d, db, clock) = open_tmp();
        for i in 0..17 {
            clock.set(1_000 + i);
            close(&db, &format!("old{i}"), false);
        }
        clock.set(5_000);
        let start = interval_start(&db).unwrap();
        assert_eq!(start.kind, StartKind::Baseline);
        assert_eq!(start.ts_ms, 5_000);
        assert_eq!(count(&db, LOSS_BREAKER_BASELINE), 1);
        assert!(closed_pairs_since(&db, &start).unwrap().is_empty(), "the old history is not counted");
        // a second start finds the same baseline and writes nothing
        clock.set(9_000);
        assert_eq!(interval_start(&db).unwrap(), start);
        assert_eq!(count(&db, LOSS_BREAKER_BASELINE), 1);
    }

    #[test]
    fn pairs_closed_after_the_baseline_are_counted() {
        let (_d, db, clock) = open_tmp();
        let start = interval_start(&db).unwrap();
        clock.set(2_000_000);
        close(&db, "a", false);
        let got = closed_pairs_since(&db, &start).unwrap();
        assert_eq!(got.iter().map(|c| c.pair.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn opening_trading_again_starts_a_new_interval_and_resets() {
        // spec: after a trip the user closes the kill switch -> a new interval from that moment
        let (_d, db, clock) = open_tmp();
        let first = interval_start(&db).unwrap();
        clock.set(2_000_000);
        close(&db, "lost", false);
        db.set_kill_switch(true).unwrap();
        clock.set(3_000_000);
        db.set_kill_switch(false).unwrap();
        let start = interval_start(&db).unwrap();
        assert_eq!(start.kind, StartKind::TradingOpened);
        assert_eq!(start.ts_ms, 3_000_000);
        assert!(start.event_id > first.event_id);
        assert!(closed_pairs_since(&db, &start).unwrap().is_empty(), "loss and streak start from zero");
        clock.set(3_500_000);
        close(&db, "next", false);
        assert_eq!(closed_pairs_since(&db, &start).unwrap().len(), 1);
    }

    #[test]
    fn turning_the_kill_switch_on_does_not_move_the_start() {
        let (_d, db, clock) = open_tmp();
        let first = interval_start(&db).unwrap();
        clock.set(2_000_000);
        db.set_kill_switch(true).unwrap();
        assert_eq!(interval_start(&db).unwrap(), first);
    }

    #[test]
    fn a_live_arming_is_an_opening_too() {
        // Live: arming writes KILL_SWITCH_CHANGED {on:false} together with LIVE_ARMED
        let (_d, db, clock) = open_tmp();
        interval_start(&db).unwrap();
        clock.set(2_000_000);
        db.set_kill_switch_with_event(false, "LIVE_ARMED", &json!({})).unwrap();
        let start = interval_start(&db).unwrap();
        assert_eq!((start.kind, start.ts_ms), (StartKind::TradingOpened, 2_000_000));
    }

    #[test]
    fn an_opening_from_before_the_upgrade_does_not_pull_old_history_in() {
        // The user opened trading long ago, 3 pairs lost since, then this version first starts.
        // The baseline (later) wins, so nothing is counted retroactively (design D1: 不追溯).
        let (_d, db, clock) = open_tmp();
        db.set_kill_switch(true).unwrap();
        clock.set(1_500_000);
        db.set_kill_switch(false).unwrap();
        for i in 0..3 {
            clock.set(1_600_000 + i);
            close(&db, &format!("old{i}"), false);
        }
        clock.set(2_000_000);
        let start = interval_start(&db).unwrap();
        assert_eq!(start.kind, StartKind::Baseline);
        assert!(closed_pairs_since(&db, &start).unwrap().is_empty());
    }

    // ---- which pairs count -----------------------------------------------------------------

    #[test]
    fn closed_pairs_are_ordered_by_their_close_event_time_each_pair_once() {
        let (_d, db, clock) = open_tmp();
        let start = interval_start(&db).unwrap();
        clock.set(3_000_000);
        close(&db, "late", false);
        clock.set(2_000_000);
        close(&db, "early", false);
        clock.set(2_500_000);
        close(&db, "early", false); // the same pair confirmed again: still one pair, at its first confirmation
        let got = closed_pairs_since(&db, &start).unwrap();
        assert_eq!(got.iter().map(|c| (c.pair.as_str(), c.ts_ms)).collect::<Vec<_>>(), [("early", 2_000_000), ("late", 3_000_000)]);
    }

    #[test]
    fn simulated_pairs_and_other_event_types_are_not_counted() {
        let (_d, db, clock) = open_tmp();
        let start = interval_start(&db).unwrap();
        clock.set(2_000_000);
        close(&db, "sim", true);
        append(&db, "PAIR_PNL_COMPUTED", Some("x"), json!({}));
        append(&db, "ORDER_SUBMITTED", Some("y"), json!({}));
        assert!(closed_pairs_since(&db, &start).unwrap().is_empty());
    }
}

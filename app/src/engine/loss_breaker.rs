//! loss-circuit-breaker, store side: where the interval starts, which closed pairs count and what
//! each is worth. Everything here reads the append-only `events` table (and, to recompute a PnL,
//! the same assembly the PnL record uses); the pure arithmetic lives in `core::loss_breaker`.
//!
//! The actor (`actor::loss_breaker`) decides WHEN to evaluate and what to do on a trip.

use rusqlite::params;
use serde_json::{Value, json};
use tong_funding_core::loss_breaker::{BreakerState, PairAmount, TripReason, amount_of, evaluate};
use tong_funding_core::pnl::{PnlBreakdown, compute_pnl};
use tong_funding_core::types::Decimal;

use crate::funding::pnl_record::{assemble, latest_pnl};
use crate::store::db::Db;
use crate::store::events::EventStore;

pub const KILL_SWITCH_CHANGED: &str = "KILL_SWITCH_CHANGED";
/// Written once, the first time this feature runs on a database: the history before it is never counted.
pub const LOSS_BREAKER_BASELINE: &str = "LOSS_BREAKER_BASELINE";
/// The breaker tripped: written in the same transaction as the kill switch turning ON.
pub const LOSS_BREAKER_TRIPPED: &str = "LOSS_BREAKER_TRIPPED";

/// What opened the current interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartKind {
    /// The latest `KILL_SWITCH_CHANGED {on: false}` that followed an ON (the user opened trading; a Live arming does too).
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

/// One stored event, as much as the breaker reads of it.
struct Ev {
    id: i64,
    ts_ms: i64,
    pair: Option<String>,
    payload: Value,
}

/// Every event of `ty`, oldest first.
fn events_of(db: &Db, ty: &str) -> Result<Vec<Ev>, String> {
    db.with_conn(|c| {
        let mut st = c.prepare("SELECT id, ts_ms, pair_id, payload FROM events WHERE event_type = ?1 ORDER BY id")?;
        let rows = st.query_map(params![ty], |r| {
            let payload: String = r.get(3)?;
            Ok(Ev { id: r.get(0)?, ts_ms: r.get(1)?, pair: r.get(2)?, payload: serde_json::from_str(&payload).unwrap_or(Value::Null) })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    })
    .map_err(|e| e.to_string())
}

/// The start of the current interval: the later (by event id) of the newest
/// `KILL_SWITCH_CHANGED {on: false}` that switched the kill switch from ON to OFF and the `LOSS_BREAKER_BASELINE`. The baseline is written here
/// when this database never had one, so the history before the first start never counts, even when
/// the kill switch was opened (and the pairs lost money) before this version existed.
pub fn interval_start(db: &Db) -> Result<IntervalStart, String> {
    let baseline = match events_of(db, LOSS_BREAKER_BASELINE)?.pop() {
        Some(e) => IntervalStart { event_id: e.id, ts_ms: e.ts_ms, kind: StartKind::Baseline },
        None => {
            let payload = json!({ "reason": "first start with the loss circuit breaker: earlier history is not counted" });
            let id = EventStore::new(db.clone()).append(LOSS_BREAKER_BASELINE, None, payload).map_err(|e| e.to_string())?.ok_or("baseline event not stored")?;
            IntervalStart { event_id: id, ts_ms: db.now_ms(), kind: StartKind::Baseline }
        }
    };
    // Only an OFF that follows a real ON opens trading (the switch starts OFF): an OFF written while it
    // was already OFF switched nothing and must not make the breaker forget what happened since.
    let mut is_on = false;
    let mut opened: Option<Ev> = None;
    for e in events_of(db, KILL_SWITCH_CHANGED)? {
        match e.payload["on"].as_bool() {
            Some(true) => is_on = true,
            Some(false) => {
                if is_on {
                    opened = Some(e);
                }
                is_on = false;
            }
            None => {}
        }
    }
    Ok(match opened {
        Some(e) if e.id > baseline.event_id => IntervalStart { event_id: e.id, ts_ms: e.ts_ms, kind: StartKind::TradingOpened },
        _ => baseline,
    })
}

/// A pair that counts in the interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedPair {
    pub pair: String,
    pub event_id: i64,
    pub ts_ms: i64,
}

/// The states a pair is left in for a person; confirming it closed from one of them ends in FINALIZED.
const ALERT_STATES: [&str; 3] = ["PARTIAL_FAILURE", "IMBALANCED", "UNRESOLVED"];

/// Every event that says "this pair is closed", oldest first: a `CLOSE_CONFIRMED`, or the transition
/// of a pair from an alert state into FINALIZED (the user's "confirm closed": such a pair never gets
/// a `CLOSE_CONFIRMED`, and the money it lost is real all the same). The payload carries `simulated`.
fn closing_events(db: &Db) -> Result<Vec<Ev>, String> {
    let mut out = events_of(db, crate::engine::actor::CLOSE_CONFIRMED)?;
    for mut e in events_of(db, crate::engine::transition::PAIR_TRANSITION)? {
        if e.payload["to"] == "FINALIZED" && e.payload["from"].as_str().is_some_and(|f| ALERT_STATES.contains(&f)) {
            e.payload = json!({ "simulated": e.payload["detail"]["simulated"] });
            out.push(e);
        }
    }
    out.sort_by_key(|e| e.id);
    Ok(out)
}

/// Pairs that count since `start`, ordered by the time of their first closing event (then id), each
/// pair once: the pair's earliest `CLOSE_CONFIRMED` / hand-confirmed finalization decides when it is
/// counted, and a pair whose first such event lies before `start` was counted in an earlier interval.
/// Simulated pairs are left out: they hold no money and have no PnL by design.
pub fn closed_pairs_since(db: &Db, start: &IntervalStart) -> Result<Vec<ClosedPair>, String> {
    let mut first: Vec<(ClosedPair, bool)> = Vec::new(); // (earliest event, any event says simulated)
    for e in closing_events(db)? {
        let Some(pair) = e.pair else { continue };
        let simulated = e.payload["simulated"] == Value::Bool(true);
        match first.iter_mut().find(|(c, _)| c.pair == pair) {
            Some((_, sim)) => *sim |= simulated,
            None => first.push((ClosedPair { pair, event_id: e.id, ts_ms: e.ts_ms }, simulated)),
        }
    }
    let mut out: Vec<ClosedPair> = first.into_iter().filter(|(c, sim)| c.event_id > start.event_id && !sim).map(|(c, _)| c).collect();
    out.sort_by_key(|c| (c.ts_ms, c.event_id));
    Ok(out)
}

/// What one closed pair is worth: the newest stored PnL when its net is known, otherwise a fresh
/// recomputation from the fills now in the store (price PnL minus both fees; the net itself when
/// that became known meanwhile), otherwise unknown.
pub fn pair_amount(db: &Db, pair: &str, now_ms: i64) -> PairAmount {
    if let Ok(Some(stored)) = latest_pnl(db, pair)
        && let Ok(b) = serde_json::from_value::<PnlBreakdown>(stored.payload["breakdown"].clone())
        && let net @ PairAmount::Net(_) = amount_of(&b)
    {
        return net;
    }
    match assemble(db, pair, now_ms) {
        Ok(a) => amount_of(&compute_pnl(&a.input)),
        // Not readable: nothing can be said about this pair, which the breaker treats as a stop.
        Err(_) => PairAmount::Unknown,
    }
}

/// One pair that counts in the interval.
#[derive(Debug, Clone, PartialEq)]
pub struct CountedPair {
    pub pair: String,
    pub closed_at_ms: i64,
    pub amount: PairAmount,
}

/// The breaker as the UI shows it (published with every snapshot).
#[derive(Debug, Clone, PartialEq)]
pub struct BreakerView {
    pub start_ms: i64,
    /// Signed cumulative amount of the interval.
    pub net_sum: Decimal,
    /// The loss used so far (never negative).
    pub loss_used: Decimal,
    pub consecutive: u32,
    pub max_loss: Decimal,
    pub max_consecutive: u32,
    /// Tripped now, or a trip is on record for this interval (a trip is never undone by a better number).
    pub tripped: bool,
    pub reason: Option<TripReason>,
    pub counted: usize,
}

/// The whole picture of the current interval.
#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub start: IntervalStart,
    pub pairs: Vec<CountedPair>,
    pub state: BreakerState,
    pub max_loss: Decimal,
    pub max_consecutive: u32,
    /// The `LOSS_BREAKER_TRIPPED` already written for this interval, if any (its payload).
    pub recorded_trip: Option<Value>,
}

/// Counts the pairs closed since the interval start, values each one and evaluates the limits.
pub fn assess(db: &Db, max_loss: Decimal, max_consecutive: u32, now_ms: i64) -> Result<Assessment, String> {
    let start = interval_start(db)?;
    let pairs: Vec<CountedPair> = closed_pairs_since(db, &start)?
        .into_iter()
        .map(|c| CountedPair { amount: pair_amount(db, &c.pair, now_ms), pair: c.pair, closed_at_ms: c.ts_ms })
        .collect();
    let amounts: Vec<Option<Decimal>> = pairs.iter().map(|p| p.amount.value()).collect();
    let state = evaluate(&amounts, max_loss, max_consecutive);
    let recorded_trip = events_of(db, LOSS_BREAKER_TRIPPED)?.into_iter().rfind(|e| e.id > start.event_id).map(|e| e.payload);
    Ok(Assessment { start, pairs, state, max_loss, max_consecutive, recorded_trip })
}

impl Assessment {
    /// The limits are exceeded now, or this interval already tripped.
    pub fn tripped(&self) -> bool {
        self.state.tripped || self.recorded_trip.is_some()
    }

    pub fn reason(&self) -> Option<TripReason> {
        self.state.reason.or_else(|| {
            let code = self.recorded_trip.as_ref()?["reason"].as_str()?;
            [TripReason::UnknownPnl, TripReason::MaxLoss, TripReason::ConsecutiveLosses, TripReason::Unreadable].into_iter().find(|r| r.code() == code)
        })
    }

    pub fn view(&self) -> BreakerView {
        BreakerView {
            start_ms: self.start.ts_ms,
            net_sum: self.state.net_sum,
            loss_used: self.state.loss_used(),
            consecutive: self.state.consecutive,
            max_loss: self.max_loss,
            max_consecutive: self.max_consecutive,
            tripped: self.tripped(),
            reason: self.reason(),
            counted: self.pairs.len(),
        }
    }

    /// The `LOSS_BREAKER_TRIPPED` payload: totals, thresholds, the pairs counted and why.
    pub fn trip_payload(&self) -> Value {
        let reason = self.state.reason;
        json!({
            "reason": reason.map(TripReason::code),
            "reason_text": reason.map(TripReason::text),
            "net_sum": self.state.net_sum.normalize().to_string(),
            "loss_sum": self.state.loss_used().normalize().to_string(),
            "consecutive_losses": self.state.consecutive,
            "unknown_pairs": self.state.unknown_count,
            "thresholds": {
                "loss_breaker_max_loss_usdt": self.max_loss.normalize().to_string(),
                "loss_breaker_max_consecutive_losses": self.max_consecutive,
            },
            "interval_start_ms": self.start.ts_ms,
            "interval_start": match self.start.kind { StartKind::TradingOpened => "TRADING_OPENED", StartKind::Baseline => "BASELINE" },
            // an amount that is not the PnL net leaves funding out: the notification says "暫計"
            "provisional": self.pairs.iter().any(|p| !matches!(p.amount, PairAmount::Net(_))),
            "pairs": self.pairs.iter().map(|p| json!({
                "pair": p.pair,
                "closed_at_ms": p.closed_at_ms,
                "amount": p.amount.value().map(|v| v.normalize().to_string()),
                "basis": match p.amount { PairAmount::Net(_) => "net", PairAmount::Provisional(_) => "price_pnl_minus_fees", PairAmount::Unknown => "unknown" },
            })).collect::<Vec<_>>(),
        })
    }
}

/// The highest event id among the events the breaker reacts to (closes, hand confirmations, PnL results,
/// kill switch changes, trips, baselines, risk settings changes): the engine re-assesses only when this
/// moved. Event types only (review round 2, B7): a hand confirmation is seen through `MANUAL_CONFIRM_RESULT`,
/// which the engine writes right after the transition into FINALIZED lands, in the same actor step, so the
/// look that follows it already sees the transition; what COUNTS is still that transition (`closed_pairs_since`).
pub fn watermark(db: &Db) -> Result<i64, String> {
    db.with_conn(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM events WHERE event_type IN ('CLOSE_CONFIRMED', 'PAIR_PNL_COMPUTED', 'PAIR_PNL_RECOMPUTED', 'KILL_SWITCH_CHANGED', 'LOSS_BREAKER_TRIPPED', 'LOSS_BREAKER_BASELINE', 'RISK_CONFIG_UPDATED', 'MANUAL_CONFIRM_RESULT')",
            [],
            |r| r.get::<_, i64>(0),
        )?)
    })
    .map_err(|e| e.to_string())
}


/// Fixtures shared with the actor tests.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::engine::actor::PairEnvelope;
    use crate::store::state::NewPair;
    use tong_funding_core::pair::PairState;
    use tong_funding_core::types::Exchange;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    pub(crate) fn append(db: &Db, ty: &str, pair: Option<&str>, payload: Value) -> i64 {
        EventStore::new(db.clone()).append(ty, pair, payload).unwrap().unwrap()
    }

    pub(crate) fn close(db: &Db, pair: &str, simulated: bool) -> i64 {
        append(db, crate::engine::actor::CLOSE_CONFIRMED, Some(pair), json!({ "verified_flat": true, "simulated": simulated }))
    }

    /// The manual "confirm closed" of a pair in an alert state: `PAIR_TRANSITION` `from` -> FINALIZED, then
    /// the `MANUAL_CONFIRM_RESULT` the engine writes right after it (what the watermark watches). Returns the transition's id.
    pub(crate) fn finalize_by_hand(db: &Db, pair: &str, from: &str, simulated: bool) -> i64 {
        let id = append(db, crate::engine::transition::PAIR_TRANSITION, Some(pair), json!({ "from": from, "to": "FINALIZED", "event": "Manual(ConfirmClosed)", "detail": { "simulated": simulated } }));
        append(db, crate::engine::actor::MANUAL_CONFIRM_RESULT, Some(pair), json!({ "accepted": true, "simulated": simulated }));
        id
    }

    /// A closed demo pair worth `delta` (long closes `delta` away from its open, no fees) at `ts`.
    pub(crate) fn closed(db: &Db, clock: &crate::ports::ManualClock, uuid: &str, ts: i64, delta: &str) {
        pair_with_fills(db, clock, uuid, ts, delta);
        close(db, uuid, false);
    }

    /// The same pair as [`closed`], but the user confirmed it by hand from PARTIAL_FAILURE: there is
    /// no `CLOSE_CONFIRMED`, only the transition into FINALIZED.
    pub(crate) fn closed_by_hand(db: &Db, clock: &crate::ports::ManualClock, uuid: &str, ts: i64, delta: &str) {
        pair_with_fills(db, clock, uuid, ts, delta);
        finalize_by_hand(db, uuid, "PARTIAL_FAILURE", false);
    }

    fn pair_with_fills(db: &Db, clock: &crate::ports::ManualClock, uuid: &str, ts: i64, delta: &str) {
        let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: ts, simulated: false, scan: json!({}) };
        let row = NewPair { internal_uuid: uuid.into(), pair_id: format!("pid-{uuid}"), symbol: "BTCUSDT".into(), status: PairState::Closing, entry: serde_json::to_value(env).unwrap() };
        db.add_pair_if_not_pending(&row).unwrap();
        clock.set(ts);
        let close_long = dec("100") + dec(delta);
        for (leg, action, ex, avg) in [
            ("long", "open", "Binance", "100".to_string()),
            ("short", "open", "Bybit", "100".to_string()),
            ("long", "close", "Binance", close_long.to_string()),
            ("short", "close", "Bybit", "100".to_string()),
        ] {
            append(
                db,
                "ORDER_SUBMITTED",
                Some(uuid),
                json!({
                    "outcome": "accepted", "state": "Filled", "filled_quantity": "1", "avg_price": avg, "fee": "0", "fee_asset": "USDT",
                    "client_order_id": format!("demo-{uuid}-{leg}-{action}"), "leg": leg, "action": action, "simulated": false, "exchange": ex, "symbol": "BTCUSDT",
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::test_support::open_tmp;
    use tong_funding_core::types::Exchange;

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
    fn an_off_that_switched_nothing_does_not_reset_the_interval() {
        // review round 1 (A6): only an OFF that follows a real ON opens trading; an OFF written while the
        // switch was already OFF (a redundant save, a re-arm of an armed engine) must not forget the losses
        let (_d, db, clock) = open_tmp();
        interval_start(&db).unwrap();
        clock.set(2_000_000);
        db.set_kill_switch(true).unwrap();
        clock.set(3_000_000);
        db.set_kill_switch(false).unwrap();
        let opened = interval_start(&db).unwrap();
        assert_eq!((opened.kind, opened.ts_ms), (StartKind::TradingOpened, 3_000_000));
        clock.set(3_500_000);
        close(&db, "lost", false);
        clock.set(4_000_000);
        db.set_kill_switch(false).unwrap(); // already OFF: nothing was switched
        let still = interval_start(&db).unwrap();
        assert_eq!(still, opened, "the start stays where the real opening was");
        assert_eq!(closed_pairs_since(&db, &still).unwrap().len(), 1, "and the loss since then still counts");
    }

    #[test]
    fn an_off_before_any_on_is_not_an_opening() {
        // the switch starts OFF: an OFF event with nothing ON before it opens nothing
        let (_d, db, clock) = open_tmp();
        let first = interval_start(&db).unwrap();
        clock.set(2_000_000);
        db.set_kill_switch(false).unwrap();
        assert_eq!(interval_start(&db).unwrap(), first);
    }

    #[test]
    fn a_live_arming_is_an_opening_too() {
        // Live: arming writes KILL_SWITCH_CHANGED {on:false} together with LIVE_ARMED
        let (_d, db, clock) = open_tmp();
        interval_start(&db).unwrap();
        db.set_kill_switch_with_event(true, "LIVE_STARTUP_LOCK", &json!({})).unwrap(); // every Live start is locked
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

    // ---- review round 1 (A2): a pair the user confirmed by hand counts too ----

    #[test]
    fn a_pair_finalized_by_hand_from_an_alert_state_counts_without_any_close_confirmation() {
        let (_d, db, clock) = open_tmp();
        let start = interval_start(&db).unwrap();
        clock.set(2_000_000);
        for (i, from) in ["PARTIAL_FAILURE", "IMBALANCED", "UNRESOLVED"].into_iter().enumerate() {
            clock.set(2_000_000 + i as i64);
            testkit::finalize_by_hand(&db, &format!("h{i}"), from, false);
        }
        let got = closed_pairs_since(&db, &start).unwrap();
        assert_eq!(got.iter().map(|c| c.pair.as_str()).collect::<Vec<_>>(), ["h0", "h1", "h2"]);
    }

    #[test]
    fn a_pair_counts_once_at_the_earlier_of_its_two_events() {
        let (_d, db, clock) = open_tmp();
        let start = interval_start(&db).unwrap();
        clock.set(2_000_000);
        testkit::finalize_by_hand(&db, "p", "PARTIAL_FAILURE", false);
        clock.set(5_000_000);
        close(&db, "p", false);
        clock.set(3_000_000);
        close(&db, "q", false);
        let got = closed_pairs_since(&db, &start).unwrap();
        assert_eq!(got.iter().map(|c| (c.pair.as_str(), c.ts_ms)).collect::<Vec<_>>(), [("p", 2_000_000), ("q", 3_000_000)], "p at its confirmation by hand, once");
    }

    #[test]
    fn a_pair_closed_before_the_start_and_finalized_by_hand_after_it_belongs_to_the_earlier_interval() {
        let (_d, db, clock) = open_tmp();
        let first = interval_start(&db).unwrap();
        clock.set(2_000_000);
        close(&db, "old", false);
        db.set_kill_switch(true).unwrap();
        clock.set(3_000_000);
        db.set_kill_switch(false).unwrap();
        let start = interval_start(&db).unwrap();
        assert!(start.event_id > first.event_id);
        clock.set(4_000_000);
        testkit::finalize_by_hand(&db, "old", "PARTIAL_FAILURE", false);
        assert!(closed_pairs_since(&db, &start).unwrap().is_empty(), "it was already counted where it was closed");
    }

    #[test]
    fn a_simulated_pair_finalized_by_hand_and_an_ordinary_finalization_are_not_counted() {
        let (_d, db, clock) = open_tmp();
        let start = interval_start(&db).unwrap();
        clock.set(2_000_000);
        testkit::finalize_by_hand(&db, "sim", "PARTIAL_FAILURE", true);
        testkit::finalize_by_hand(&db, "normal", "CLOSING", false); // the pipeline's own finalization follows a CLOSE_CONFIRMED
        assert!(closed_pairs_since(&db, &start).unwrap().is_empty());
    }

    #[test]
    fn the_watermark_moves_on_a_hand_finalization_and_on_a_risk_settings_change_but_not_on_other_transitions() {
        let (_d, db, _clock) = open_tmp();
        let w0 = watermark(&db).unwrap();
        append(&db, "PAIR_TRANSITION", Some("a"), json!({ "from": "PREPARED", "to": "ORDER_SUBMIT" }));
        assert_eq!(watermark(&db).unwrap(), w0, "an ordinary transition is not watched");
        // review round 2 (B7): the transition itself is not looked at (that needed a json_extract over every
        // PAIR_TRANSITION); the engine writes MANUAL_CONFIRM_RESULT right after a hand confirmation lands
        append(&db, "PAIR_TRANSITION", Some("a"), json!({ "from": "UNRESOLVED", "to": "FINALIZED", "event": "Manual(ConfirmClosed)" }));
        assert_eq!(watermark(&db).unwrap(), w0, "a bare transition into FINALIZED is not watched either");
        testkit::finalize_by_hand(&db, "a", "UNRESOLVED", false);
        let w1 = watermark(&db).unwrap();
        assert!(w1 > w0, "the hand confirmation's own result event moves it");
        append(&db, "RISK_CONFIG_UPDATED", None, json!({}));
        assert!(watermark(&db).unwrap() > w1, "new limits are looked at at once");
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

    // ---- 2.2 what a pair is worth ---------------------------------------------------------------

    mod amount {
        use super::*;
        use crate::funding::pnl_record::tests::{Fx, both_fetched, d, ledger, scenario};
        use crate::funding::pnl_record::{recompute_if_changed, settle_pnl};

        const T: i64 = crate::funding::pnl_record::tests::T;

        /// Close references for both close orders (what the engine writes before a reduce-only close);
        /// they make the PnL net computable once funding is in.
        fn close_references(fx: &Fx) {
            for (leg, ex, avg) in [("long", "Binance", "59997.5"), ("short", "Bybit", "60000")] {
                append(
                    &fx.db,
                    "ORDER_FILL",
                    Some("p1"),
                    json!({
                        "client_order_id": format!("demo-p1-{leg}-close"), "leg": leg, "action": "close", "exchange": ex,
                        "filled_quantity": "0.02", "avg_price": avg, "fee": "0.24", "fee_asset": "USDT", "reference_price": avg,
                    }),
                );
            }
        }

        #[test]
        fn funding_not_booked_yet_is_price_pnl_minus_both_fees() {
            // long 0.02 x (59997.5 - 60002.5) = -0.10, short 0; four fees of 0.24 = 0.96
            let fx = scenario(true);
            assert_eq!(pair_amount(&fx.db, "p1", T + 80_000), PairAmount::Provisional(d("-1.06")));
        }

        #[test]
        fn a_stored_pnl_with_a_known_net_wins() {
            let fx = scenario(true);
            close_references(&fx);
            fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T), ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
            both_fetched(&fx);
            settle_pnl(&fx.db, "p1", T + 80_000, false).unwrap();
            assert_eq!(pair_amount(&fx.db, "p1", T + 90_000), PairAmount::Net(d("0.24") + d("-0.10") - d("0.96")), "the spec's -0.82");
        }

        #[test]
        fn an_incomplete_stored_pnl_falls_back_to_the_recomputation_and_switches_to_net_when_it_arrives() {
            let fx = scenario(true);
            close_references(&fx);
            fx.db.write_funding_ledger(&[ledger(Exchange::Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
            both_fetched(&fx);
            fx.clock.set(T + 700_000);
            settle_pnl(&fx.db, "p1", T + 700_000, true).unwrap(); // final attempt: INCOMPLETE (Binance funding missing)
            assert_eq!(pair_amount(&fx.db, "p1", T + 700_000), PairAmount::Provisional(d("-1.06")), "net unknown: price PnL - fees, funding left out");
            // the late entry arrives and the PnL is recomputed: now the net counts
            fx.db.write_funding_ledger(&[ledger(Exchange::Binance, "BTCUSDT", "1", "-0.12", T)]).unwrap();
            assert!(recompute_if_changed(&fx.db, "p1", T + 800_000).unwrap().is_some());
            assert_eq!(pair_amount(&fx.db, "p1", T + 800_000), PairAmount::Net(d("-0.82")));
        }

        #[test]
        fn a_pair_without_any_fill_record_or_that_does_not_exist_is_unknown() {
            let fx = scenario(true);
            let env = crate::engine::actor::PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: T, simulated: false, scan: json!({}) };
            let row = crate::store::state::NewPair { internal_uuid: "empty".into(), pair_id: "pid-empty".into(), symbol: "BTCUSDT".into(), status: tong_funding_core::pair::PairState::Closing, entry: serde_json::to_value(env).unwrap() };
            fx.db.add_pair_if_not_pending(&row).unwrap();
            assert_eq!(pair_amount(&fx.db, "empty", T), PairAmount::Unknown);
            assert_eq!(pair_amount(&fx.db, "no-such-pair", T), PairAmount::Unknown);
        }

        #[test]
        fn a_fill_record_that_lost_its_price_is_unknown() {
            let fx = scenario(true);
            append(&fx.db, "ORDER_FILL", Some("p1"), json!({ "client_order_id": "demo-p1-long-close", "leg": "long", "action": "close", "exchange": "Binance", "filled_quantity": "0.02", "fee": "0.24", "fee_asset": "USDT" }));
            // the newest event of an order is its record: without a price the price PnL cannot be told
            assert_eq!(pair_amount(&fx.db, "p1", T), PairAmount::Unknown);
        }
    }

    // ---- 2.3 the assessment ----------------------------------------------------------------------

    mod assessment {
        use super::*;
        use crate::engine::actor::PairEnvelope;
        use crate::store::state::NewPair;
        use super::super::testkit::closed;
        use tong_funding_core::pair::PairState;

        fn dec(s: &str) -> Decimal {
            s.parse().unwrap()
        }

        fn setup() -> (tempfile::TempDir, Db, crate::ports::ManualClock) {
            let (d, db, clock) = open_tmp();
            interval_start(&db).unwrap();
            (d, db, clock)
        }

        #[test]
        fn three_losses_in_a_row_trip_on_the_third_with_the_consecutive_reason() {
            // spec scenario: limit 3, -0.5, -0.3, -1.2
            let (_d, db, clock) = setup();
            closed(&db, &clock, "a", 2_000_000, "-0.5");
            closed(&db, &clock, "b", 2_100_000, "-0.3");
            let a = assess(&db, dec("15"), 3, 3_000_000).unwrap();
            assert!(!a.tripped() && a.state.consecutive == 2);
            closed(&db, &clock, "c", 2_200_000, "-1.2");
            let a = assess(&db, dec("15"), 3, 3_000_000).unwrap();
            assert!(a.tripped());
            assert_eq!(a.reason(), Some(TripReason::ConsecutiveLosses));
            assert_eq!(a.pairs.iter().map(|p| p.pair.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
            let v = a.view();
            assert_eq!((v.net_sum, v.loss_used, v.consecutive, v.max_consecutive, v.tripped, v.counted), (dec("-2.0"), dec("2.0"), 3, 3, true, 3));
        }

        #[test]
        fn a_profit_in_between_resets_the_run() {
            // spec scenario: -0.5, +0.2, -0.3, -1.2 with limit 3 -> not tripped
            let (_d, db, clock) = setup();
            for (i, delta) in ["-0.5", "0.2", "-0.3", "-1.2"].into_iter().enumerate() {
                closed(&db, &clock, &format!("p{i}"), 2_000_000 + i as i64 * 1_000, delta);
            }
            let a = assess(&db, dec("15"), 3, 3_000_000).unwrap();
            assert!(!a.tripped(), "{a:?}");
            assert_eq!(a.state.consecutive, 2);
        }

        #[test]
        fn the_cumulative_loss_trips_exactly_at_the_limit() {
            let (_d, db, clock) = setup();
            closed(&db, &clock, "a", 2_000_000, "-10");
            closed(&db, &clock, "b", 2_100_000, "5");
            closed(&db, &clock, "c", 2_200_000, "-9.99");
            assert!(!assess(&db, dec("15"), 9, 3_000_000).unwrap().tripped(), "-14.99");
            closed(&db, &clock, "d", 2_300_000, "-0.01");
            let a = assess(&db, dec("15"), 9, 3_000_000).unwrap();
            assert_eq!((a.reason(), a.state.net_sum), (Some(TripReason::MaxLoss), dec("-15.00")));
        }

        #[test]
        fn a_pair_whose_amount_cannot_be_told_trips_with_the_unknown_reason() {
            // spec scenario: a closed pair whose fill price cannot be had
            let (_d, db, clock) = setup();
            let env = PairEnvelope { long_exchange: Exchange::Binance, short_exchange: Exchange::Bybit, settlement_ms: 1, simulated: false, scan: json!({}) };
            db.add_pair_if_not_pending(&NewPair { internal_uuid: "u".into(), pair_id: "pid-u".into(), symbol: "BTCUSDT".into(), status: PairState::Closing, entry: serde_json::to_value(env).unwrap() }).unwrap();
            clock.set(2_000_000);
            close(&db, "u", false);
            let a = assess(&db, dec("15"), 3, 3_000_000).unwrap();
            assert!(a.tripped());
            assert_eq!(a.reason(), Some(TripReason::UnknownPnl));
            assert_eq!(a.trip_payload()["reason_text"], json!("損益未知"));
        }

        #[test]
        fn nothing_closed_is_a_clean_zero() {
            let (_d, db, _c) = setup();
            let a = assess(&db, dec("15"), 3, 3_000_000).unwrap();
            let v = a.view();
            assert!(!v.tripped && v.reason.is_none());
            assert_eq!((v.net_sum, v.loss_used, v.consecutive, v.counted), (dec("0"), dec("0"), 0, 0));
        }

        #[test]
        fn reopening_trading_starts_the_count_again() {
            let (_d, db, clock) = setup();
            closed(&db, &clock, "a", 2_000_000, "-20");
            assert!(assess(&db, dec("15"), 3, 3_000_000).unwrap().tripped());
            clock.set(3_000_000);
            db.set_kill_switch(true).unwrap();
            clock.set(3_100_000);
            db.set_kill_switch(false).unwrap();
            let a = assess(&db, dec("15"), 3, 3_200_000).unwrap();
            assert!(!a.tripped());
            assert_eq!(a.view().loss_used, dec("0"));
        }

        #[test]
        fn a_trip_on_record_keeps_the_interval_tripped_even_when_the_numbers_get_better() {
            let (_d, db, clock) = setup();
            closed(&db, &clock, "a", 2_000_000, "-20");
            let a = assess(&db, dec("15"), 3, 3_000_000).unwrap();
            db.set_kill_switch_with_event(true, LOSS_BREAKER_TRIPPED, &a.trip_payload()).unwrap();
            // a bigger limit now: the numbers alone would not trip, the record does
            let b = assess(&db, dec("500"), 3, 3_000_000).unwrap();
            assert!(!b.state.tripped && b.tripped() && b.view().tripped);
            assert_eq!(b.reason(), Some(TripReason::MaxLoss), "the recorded reason is kept");
            // the next interval starts clean
            clock.set(4_000_000);
            db.set_kill_switch(false).unwrap();
            assert!(!assess(&db, dec("500"), 3, 4_100_000).unwrap().tripped());
        }

        #[test]
        fn the_trip_payload_names_totals_thresholds_pairs_and_the_reason() {
            let (_d, db, clock) = setup();
            closed(&db, &clock, "a", 2_000_000, "-2");
            closed(&db, &clock, "b", 2_100_000, "-1");
            let p = assess(&db, dec("3"), 9, 3_000_000).unwrap().trip_payload();
            assert_eq!(p["reason"], json!("MAX_LOSS"));
            assert_eq!(p["loss_sum"], json!("3"));
            assert_eq!(p["consecutive_losses"], json!(2));
            assert_eq!(p["thresholds"]["loss_breaker_max_loss_usdt"], json!("3"));
            assert_eq!(p["thresholds"]["loss_breaker_max_consecutive_losses"], json!(9));
            assert_eq!(p["pairs"].as_array().unwrap().len(), 2);
            assert_eq!(p["pairs"][0]["amount"], json!("-2"));
            assert_eq!(p["provisional"], json!(true), "no PnL net yet: the amounts leave funding out");
        }

        #[test]
        fn the_watermark_moves_with_closes_pnl_results_and_the_kill_switch_only() {
            let (_d, db, clock) = setup();
            let w0 = watermark(&db).unwrap();
            append(&db, "ORDER_SUBMITTED", Some("x"), json!({}));
            assert_eq!(watermark(&db).unwrap(), w0, "other events do not move it");
            closed(&db, &clock, "a", 2_000_000, "-1");
            let w1 = watermark(&db).unwrap();
            assert!(w1 > w0);
            append(&db, "PAIR_PNL_RECOMPUTED", Some("a"), json!({}));
            assert!(watermark(&db).unwrap() > w1);
            let w2 = watermark(&db).unwrap();
            db.set_kill_switch(true).unwrap();
            assert!(watermark(&db).unwrap() > w2);
        }
    }
}

//! naked-leg-watchdog (authorization B): the pure part. One round of verification reads, per leg,
//! the exchange position, the pair's recorded fill and whether every one of the pair's own orders on
//! that leg is over; [`decide`] turns those reads into exactly one of five conclusions. Nothing here
//! touches the store, an exchange or the clock: the actor (`actor::naked_leg`) reads, writes events
//! and sends the one kind of order the watchdog may send (a reduce-only market close of ONE leg).
//!
//! Rule of this file: when anything is unknown the answer is `Unknown` (no order, kill switch ON,
//! notification), never a guess.

use serde_json::{Value, json};
use tong_funding_core::pair::PairState;
use tong_funding_core::types::{Decimal, Exchange};

use super::actor::{LegClosePlanError, plan_leg_close};
use super::live_guard::LEG_FLATTEN;
use super::ports::{Leg, OrderAction};
use super::transition::PAIR_TRANSITION;
use crate::store::db::{Db, StoreError};

/// The watchdog waits this long after the pair entered an alert state before its first round (the
/// order timeout has already passed by then: the alert state is entered at the timeout).
pub const WATCHDOG_GRACE_MS: i64 = 5_000;
/// Rounds are this far apart (also between a failed flatten attempt and the next verification).
pub const WATCHDOG_INTERVAL_MS: i64 = 5_000;
/// A flatten order is sent at most this many times per alert entry; after that the conclusion is `Unknown`.
pub const MAX_FLATTEN_ATTEMPTS: u32 = 3;
/// This many consecutive rounds that still find an unfinished order end in `Unknown`.
pub const MAX_CANCEL_WAIT_ROUNDS: u32 = 3;
/// A round that has not read both legs by now is given up (`Unknown`): an exchange that does not
/// answer is not "nothing there".
pub const WATCHDOG_PROBE_TIMEOUT_MS: i64 = 30_000;

/// One event per round (and per attempt result, notification, skip): the conclusion and its basis.
pub const NAKED_LEG_WATCHDOG: &str = "NAKED_LEG_WATCHDOG";
/// Written in the same transaction as `KILL_SWITCH_CHANGED` when the watchdog locks the engine.
pub const NAKED_LEG_WATCHDOG_LOCK: &str = "NAKED_LEG_WATCHDOG_LOCK";

/// What one round read for one leg. `Err` = could not be read / looked up: unknown, never "zero".
#[derive(Debug, Clone, PartialEq)]
pub struct LegRead {
    pub leg: Leg,
    pub exchange: Exchange,
    /// Signed position of the leg's symbol on its exchange (long positive), in the exchange order unit.
    pub position: Result<Decimal, String>,
    /// The pair's recorded fill on the leg: open fills minus earlier close fills, never negative.
    pub recorded: Result<Decimal, String>,
    /// Every order the pair has on this leg is over (filled, cancelled or rejected).
    pub orders_terminal: Result<bool, String>,
}

/// Everything [`decide`] needs for one round.
#[derive(Debug, Clone, PartialEq)]
pub struct DecideInput<'a> {
    pub symbol: &'a str,
    pub simulated: bool,
    /// The pair's effective `max_leg_imbalance_pct` (the tolerance of `plan_leg_close`).
    pub tolerance_pct: Decimal,
    /// `[long, short]`.
    pub legs: [LegRead; 2],
    /// Earlier consecutive rounds of this entry that ended in `WaitForCancel`.
    pub waited_rounds: u32,
    /// Flatten orders already sent for this entry.
    pub attempts_sent: u32,
}

/// The conclusion of one round.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Exactly one leg holds the pair's position and the other is confirmed empty: close `qty` of `leg`.
    Flatten { leg: Leg, qty: Decimal },
    /// Both legs hold a position: nothing is sent.
    Hedged,
    /// Neither leg holds a position: nothing is sent.
    Flat,
    /// An order is not over yet: it is cancelled, nothing is sent, the next round looks again.
    WaitForCancel,
    /// Not provable: nothing is sent, the engine is locked and the user is told why.
    Unknown { reason: String },
}

/// One round's conclusion. Order of the checks matters: a failed read beats everything (nothing is
/// known), an unfinished order beats the position rules (positions are still moving), and the
/// position rules are `plan_leg_close`'s, the same rules every close of this system uses.
pub fn decide(input: &DecideInput<'_>) -> Decision {
    // 1. A read that failed means the leg is unknown: never "zero", never a guess.
    let mut failures = Vec::new();
    for l in &input.legs {
        let name = l.leg.as_str();
        if let Err(e) = &l.position {
            failures.push(format!("{name} position unreadable: {e}"));
        }
        if let Err(e) = &l.recorded {
            failures.push(format!("{name} recorded fill unknown: {e}"));
        }
        if let Err(e) = &l.orders_terminal {
            failures.push(format!("{name} order status unknown: {e}"));
        }
    }
    if !failures.is_empty() {
        return Decision::Unknown { reason: failures.join("; ") };
    }

    // 2. An order that is not over can still fill: cancel it and look again, a bounded number of times.
    let unfinished: Vec<&str> = input.legs.iter().filter(|l| matches!(l.orders_terminal, Ok(false))).map(|l| l.leg.as_str()).collect();
    if !unfinished.is_empty() {
        if input.waited_rounds.saturating_add(1) >= MAX_CANCEL_WAIT_ROUNDS {
            return Decision::Unknown {
                reason: format!("{} order(s) still not over after {MAX_CANCEL_WAIT_ROUNDS} rounds of waiting for the cancel", unfinished.join(" and ")),
            };
        }
        return Decision::WaitForCancel;
    }

    // 3. What each leg holds, by the rules of every close: min(recorded fill, |position|) when the two
    //    agree within the tolerance; the wrong side or a disagreement is not provably the pair's.
    let mut held: [Option<Decimal>; 2] = [None, None];
    let mut problems = Vec::new();
    for (i, l) in input.legs.iter().enumerate() {
        let (Ok(position), Ok(recorded)) = (&l.position, &l.recorded) else {
            problems.push(format!("{}: reads missing", l.leg.as_str()));
            continue;
        };
        match plan_leg_close(l.leg, l.exchange, input.symbol, input.simulated, input.tolerance_pct, *position, Ok(*recorded)) {
            // A huge tolerance can plan "0" for a held position: that is not an empty leg.
            Ok((qty, _)) if qty.is_zero() && !position.is_zero() => {
                problems.push(format!("{}: position {} without a recorded fill is not provably the pair's", l.leg.as_str(), position.normalize()));
            }
            Ok((qty, _)) if qty.is_zero() => {}
            Ok((qty, _)) => held[i] = Some(qty),
            Err(LegClosePlanError::WrongSide(reason)) => problems.push(reason),
            Err(LegClosePlanError::Mismatch(alert)) => problems.push(mismatch_text(l.leg, &alert)),
        }
    }
    if !problems.is_empty() {
        return Decision::Unknown { reason: problems.join("; ") };
    }

    // 4. Exactly one leg holding the pair's position is the only case that may send an order.
    match (held[0], held[1]) {
        (None, None) => Decision::Flat,
        (Some(_), Some(_)) => Decision::Hedged,
        (Some(qty), None) => flatten_or_give_up(Leg::Long, qty, input.attempts_sent),
        (None, Some(qty)) => flatten_or_give_up(Leg::Short, qty, input.attempts_sent),
    }
}

fn flatten_or_give_up(leg: Leg, qty: Decimal, attempts_sent: u32) -> Decision {
    if attempts_sent >= MAX_FLATTEN_ATTEMPTS {
        return Decision::Unknown { reason: format!("auto flatten failed {MAX_FLATTEN_ATTEMPTS} times") };
    }
    Decision::Flatten { leg, qty }
}

/// The `CLOSE_QUANTITY_MISMATCH` payload of `plan_leg_close` as one sentence.
fn mismatch_text(leg: Leg, alert: &Value) -> String {
    let field = |k: &str| alert.get(k).and_then(Value::as_str).unwrap_or("unknown").to_string();
    let reason = alert.get("reason").and_then(Value::as_str).unwrap_or("position differs from the pair's recorded fill");
    format!(
        "{}: {reason} (position {}, recorded fill {}, difference {} %, tolerance {} %)",
        leg.as_str(),
        field("position_quantity"),
        field("recorded_quantity"),
        field("diff_pct"),
        field("tolerance_pct")
    )
}

/// The five conclusions as written in `NAKED_LEG_WATCHDOG.conclusion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    Flatten,
    Hedged,
    Flat,
    WaitForCancel,
    Unknown,
}

impl Conclusion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Conclusion::Flatten => "FLATTEN",
            Conclusion::Hedged => "HEDGED",
            Conclusion::Flat => "FLAT",
            Conclusion::WaitForCancel => "WAIT_FOR_CANCEL",
            Conclusion::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(s: &str) -> Option<Conclusion> {
        [Conclusion::Flatten, Conclusion::Hedged, Conclusion::Flat, Conclusion::WaitForCancel, Conclusion::Unknown].into_iter().find(|c| c.as_str() == s)
    }

    /// After one of these no further round runs for the alert entry (until the user acts).
    pub const fn ends_the_rounds(self) -> bool {
        matches!(self, Conclusion::Hedged | Conclusion::Flat | Conclusion::Unknown)
    }
}

impl Decision {
    pub fn conclusion(&self) -> Conclusion {
        match self {
            Decision::Flatten { .. } => Conclusion::Flatten,
            Decision::Hedged => Conclusion::Hedged,
            Decision::Flat => Conclusion::Flat,
            Decision::WaitForCancel => Conclusion::WaitForCancel,
            Decision::Unknown { .. } => Conclusion::Unknown,
        }
    }
}

/// `NAKED_LEG_WATCHDOG.kind` of the record of one round (its conclusion and basis).
pub const KIND_ROUND: &str = "ROUND";
/// `NAKED_LEG_WATCHDOG.kind` of what became of one flatten order the watchdog sent.
pub const KIND_ATTEMPT_RESULT: &str = "ATTEMPT_RESULT";
/// `NAKED_LEG_WATCHDOG.kind` of a pair the watchdog cannot act on (written once, with the reason).
pub const KIND_SKIPPED: &str = "SKIPPED";
/// `NAKED_LEG_WATCHDOG.kind` of what became of a notification (sent, or why not).
pub const KIND_NOTIFY_RESULT: &str = "NOTIFY_RESULT";

/// One stored event of a pair as far as the watchdog's progress reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub ts_ms: i64,
    pub event_type: String,
    pub payload: Value,
}

/// Where the watchdog stands for one alert entry of a pair, rebuilt from the stored events (so a
/// restart continues instead of starting over, and the page can show the same thing).
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// When the pair entered its current alert state (the entry's `PAIR_TRANSITION` time).
    pub alert_since_ms: i64,
    pub rounds: u32,
    /// Flatten orders the watchdog sent for this entry (`LEG_FLATTEN` with `by: "watchdog"`).
    pub attempts_sent: u32,
    /// Consecutive newest rounds that concluded `WAIT_FOR_CANCEL`.
    pub waited_rounds: u32,
    /// Time of the newest watchdog record of this entry.
    pub last_record_ms: Option<i64>,
    /// A conclusion that ends the rounds was reached.
    pub terminal: bool,
}

impl Progress {
    /// The earliest time of the next round: never before the grace has passed, and one interval after
    /// the newest record.
    pub fn next_round_ms(&self) -> i64 {
        let first = self.alert_since_ms.saturating_add(WATCHDOG_GRACE_MS);
        self.last_record_ms.map_or(first, |t| first.max(t.saturating_add(WATCHDOG_INTERVAL_MS)))
    }
}

/// `records` = the watchdog-relevant events of the pair after its entry, oldest first.
pub fn progress_from_records(alert_since_ms: i64, records: &[Record]) -> Progress {
    let mut p = Progress { alert_since_ms, rounds: 0, attempts_sent: 0, waited_rounds: 0, last_record_ms: None, terminal: false };
    for r in records {
        let text = |k: &str| r.payload.get(k).and_then(Value::as_str);
        match r.event_type.as_str() {
            // Only the watchdog's own flatten orders are its attempts (a manual one is the user's).
            LEG_FLATTEN if text("by") == Some("watchdog") => {
                p.attempts_sent = p.attempts_sent.saturating_add(1);
                p.last_record_ms = Some(r.ts_ms);
            }
            NAKED_LEG_WATCHDOG => match text("kind") {
                Some(KIND_ROUND) => {
                    p.rounds = p.rounds.saturating_add(1);
                    p.last_record_ms = Some(r.ts_ms);
                    match text("conclusion").and_then(Conclusion::parse) {
                        Some(Conclusion::WaitForCancel) => p.waited_rounds = p.waited_rounds.saturating_add(1),
                        Some(c) => {
                            p.waited_rounds = 0;
                            p.terminal |= c.ends_the_rounds();
                        }
                        None => p.waited_rounds = 0,
                    }
                }
                Some(KIND_ATTEMPT_RESULT) => p.last_record_ms = Some(r.ts_ms),
                // notifications, skips and the like are not progress
                Some(_) | None => {}
            },
            _ => {}
        }
    }
    p
}

/// Reads the pair's current entry (the newest `PAIR_TRANSITION` into `state`) and what the watchdog
/// recorded since. A pair without an entry event (a seeded state) counts from `now_ms`.
pub fn load_progress(db: &Db, pair: &str, state: PairState, now_ms: i64) -> Result<Progress, StoreError> {
    use rusqlite::OptionalExtension;
    db.with_conn(|c| {
        let entry: Option<(i64, i64)> = c
            .query_row(
                "SELECT id, ts_ms FROM events WHERE event_type = ?1 AND pair_id = ?2 AND json_extract(payload, '$.to') = ?3 ORDER BY id DESC LIMIT 1",
                rusqlite::params![PAIR_TRANSITION, pair, state.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (after_id, alert_since_ms) = entry.map_or((0, now_ms), |(id, ts)| (id, ts));
        let mut st = c.prepare("SELECT ts_ms, event_type, payload FROM events WHERE pair_id = ?1 AND id > ?2 AND event_type IN (?3, ?4) ORDER BY id")?;
        let rows = st.query_map(rusqlite::params![pair, after_id, NAKED_LEG_WATCHDOG, LEG_FLATTEN], |r| {
            let payload: String = r.get(2)?;
            Ok(Record { ts_ms: r.get(0)?, event_type: r.get(1)?, payload: serde_json::from_str(&payload).unwrap_or(Value::Null) })
        })?;
        let records = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(progress_from_records(alert_since_ms, &records))
    })
}

/// One order of the pair on a leg as a round saw it (also the evidence written to the event).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderLine {
    pub client_order_id: String,
    pub action: OrderAction,
    /// The intent says it was refused (FAILED): the order never existed, so it counts 0 and is over
    /// without a lookup.
    pub rejected_intent: bool,
    /// `OrderState` of the lookup (`Open`, `Filled`, `Cancelled`, `Rejected`).
    pub state: Option<String>,
    pub filled: Option<Decimal>,
    /// Not over: it can still fill (a lookup that failed is not "over" either, see `error`).
    pub open: bool,
    /// The lookup failed or found nothing: unknown, never "unfilled".
    pub error: Option<String>,
}

/// What one round read for one leg: the reads [`decide`] needs plus the orders behind them.
#[derive(Debug, Clone, PartialEq)]
pub struct LegProbe {
    pub read: LegRead,
    pub orders: Vec<OrderLine>,
}

/// Both legs of one round, `[long, short]`.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchdogProbe {
    pub legs: [LegProbe; 2],
}

impl WatchdogProbe {
    /// A round that could not read anything (it did not finish in time): every read failed with `why`,
    /// so [`decide`] concludes `Unknown`.
    pub fn unreadable(legs: [(Leg, Exchange); 2], why: &str) -> WatchdogProbe {
        let leg = |(leg, exchange): (Leg, Exchange)| LegProbe {
            read: LegRead { leg, exchange, position: Err(why.to_string()), recorded: Err(why.to_string()), orders_terminal: Err(why.to_string()) },
            orders: Vec::new(),
        };
        WatchdogProbe { legs: [leg(legs[0]), leg(legs[1])] }
    }
}

impl LegProbe {
    /// Folds the order lookups of a leg into what [`decide`] reads: the recorded fill (open fills minus
    /// close fills, never below 0; any lookup that failed makes it unknown) and whether every order is
    /// over. Same arithmetic as `recorded_fill`, over the same lookups.
    pub fn from_reads(leg: Leg, exchange: Exchange, position: Result<Decimal, String>, orders: Vec<OrderLine>) -> LegProbe {
        // A refused intent never existed on the exchange: 0 fill, over, no lookup was made for it.
        let looked_up = || orders.iter().filter(|o| !o.rejected_intent);
        // One lookup that failed makes the whole leg unknown: never guessed, never "zero".
        let first_error = looked_up().find_map(|o| o.error.clone());
        let (recorded, orders_terminal) = match first_error {
            Some(e) => (Err(e.clone()), Err(e)),
            None => {
                let mut total = Decimal::ZERO;
                for o in looked_up() {
                    let filled = o.filled.unwrap_or(Decimal::ZERO);
                    match o.action {
                        OrderAction::Open => total += filled,
                        OrderAction::Close => total -= filled,
                    }
                }
                (Ok(total.max(Decimal::ZERO)), Ok(looked_up().all(|o| !o.open)))
            }
        };
        LegProbe { read: LegRead { leg, exchange, position, recorded, orders_terminal }, orders }
    }

    /// The orders that can still fill: to be cancelled when a round concludes `WaitForCancel`.
    pub fn open_order_ids(&self) -> Vec<String> {
        self.orders.iter().filter(|o| !o.rejected_intent && o.error.is_none() && o.open).map(|o| o.client_order_id.clone()).collect()
    }
}

/// The basis of a round for its event: per leg the numbers read (or why not) and the orders.
pub fn evidence(probe: &WatchdogProbe) -> Value {
    let number = |r: &Result<Decimal, String>| match r {
        Ok(d) => json!(d.normalize().to_string()),
        Err(e) => json!({ "error": e }),
    };
    let leg = |l: &LegProbe| {
        let orders: Vec<Value> = l
            .orders
            .iter()
            .map(|o| {
                let mut v = json!({
                    "client_order_id": o.client_order_id,
                    "action": match o.action {
                        OrderAction::Open => "open",
                        OrderAction::Close => "close",
                    },
                });
                if o.rejected_intent {
                    v["rejected_intent"] = json!(true);
                }
                if let Some(state) = &o.state {
                    v["state"] = json!(state);
                }
                if let Some(filled) = o.filled {
                    v["filled"] = json!(filled.normalize().to_string());
                }
                if let Some(e) = &o.error {
                    v["error"] = json!(e);
                }
                v
            })
            .collect();
        json!({
            "exchange": l.read.exchange.name(),
            "position": number(&l.read.position),
            "recorded": number(&l.read.recorded),
            "orders_terminal": match &l.read.orders_terminal {
                Ok(b) => json!(b),
                Err(e) => json!({ "error": e }),
            },
            "orders": orders,
        })
    };
    json!({ "long": leg(&probe.legs[0]), "short": leg(&probe.legs[1]) })
}

/// Where the watchdog stands for the pair's current alert entry, as the manual-handling card shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No round yet: the first one is due at `due_ms` (`None` = the entry time is not known).
    Waiting { due_ms: Option<i64> },
    /// An order of the pair is not over yet: it was cancelled, the next look is at `next_ms`.
    WaitingCancel { waited: u32, next_ms: i64 },
    /// A flatten order (attempt `attempt`) was sent and is awaited.
    Flattening { leg: Leg, quantity: Decimal, attempt: u32 },
    /// An attempt did not work; the next look is at `next_ms`.
    Retrying { attempt: u32, reason: String, next_ms: i64 },
    /// The bare leg was closed by the watchdog; the user still confirms.
    Flattened { leg: Leg, quantity: Decimal },
    /// Both legs hold a position: nothing was sent.
    Hedged,
    /// Neither leg holds a position: nothing was sent.
    Flat,
    /// Not provable: nothing was sent, new entries are stopped.
    Unknown { reason: String },
    /// The watchdog cannot act on this pair.
    Skipped { reason: String },
}

/// The status from the watchdog records of one entry (oldest first). `alert_since_ms` = when the pair
/// entered the state, if known. The newest record of a round, an attempt result or a skip decides;
/// a flatten that worked stays visible after the round that confirmed both legs flat.
pub fn status_from_records(alert_since_ms: Option<i64>, records: &[Record]) -> Status {
    let text = |r: &Record, k: &str| r.payload.get(k).and_then(Value::as_str).map(str::to_string);
    let leg_of = |r: &Record| match r.payload.get("leg").and_then(Value::as_str) {
        Some("long") => Some(Leg::Long),
        Some("short") => Some(Leg::Short),
        _ => None,
    };
    let quantity_of = |r: &Record| text(r, "quantity").and_then(|q| q.parse::<Decimal>().ok());
    let attempt_of = |r: &Record| r.payload.get("attempt").and_then(Value::as_u64).map_or(0, |a| u32::try_from(a).unwrap_or(u32::MAX));

    let mut flattened: Option<(Leg, Decimal)> = None;
    let mut newest: Option<&Record> = None;
    for r in records.iter().filter(|r| r.event_type == NAKED_LEG_WATCHDOG) {
        match text(r, "kind").as_deref() {
            Some(KIND_ATTEMPT_RESULT) => {
                if r.payload.get("ok").and_then(Value::as_bool) == Some(true)
                    && let (Some(leg), Some(quantity)) = (leg_of(r), quantity_of(r))
                {
                    flattened = Some((leg, quantity));
                }
                newest = Some(r);
            }
            Some(KIND_ROUND | KIND_SKIPPED) => newest = Some(r),
            // notifications and the like are not progress
            Some(_) | None => {}
        }
    }
    let waiting = Status::Waiting { due_ms: alert_since_ms.map(|t| t.saturating_add(WATCHDOG_GRACE_MS)) };
    let Some(r) = newest else { return waiting };
    let next_ms = r.ts_ms.saturating_add(WATCHDOG_INTERVAL_MS);
    match text(r, "kind").as_deref() {
        Some(KIND_SKIPPED) => Status::Skipped { reason: text(r, "reason").unwrap_or_default() },
        Some(KIND_ATTEMPT_RESULT) => {
            if r.payload.get("ok").and_then(Value::as_bool) == Some(true) {
                flattened.map_or(waiting, |(leg, quantity)| Status::Flattened { leg, quantity })
            } else {
                Status::Retrying { attempt: attempt_of(r), reason: text(r, "reason").unwrap_or_default(), next_ms }
            }
        }
        Some(KIND_ROUND) => match text(r, "conclusion").as_deref().and_then(Conclusion::parse) {
            Some(Conclusion::WaitForCancel) => {
                let earlier = r.payload.get("waited_rounds").and_then(Value::as_u64).map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX));
                Status::WaitingCancel { waited: earlier.saturating_add(1), next_ms }
            }
            Some(Conclusion::Flatten) => match (leg_of(r), quantity_of(r)) {
                (Some(leg), Some(quantity)) => Status::Flattening { leg, quantity, attempt: attempt_of(r) },
                _ => waiting,
            },
            Some(Conclusion::Hedged) => Status::Hedged,
            // FLAT after our own flatten is that flatten, not an unexplained empty pair
            Some(Conclusion::Flat) => flattened.map_or(Status::Flat, |(leg, quantity)| Status::Flattened { leg, quantity }),
            Some(Conclusion::Unknown) => Status::Unknown { reason: text(r, "reason").unwrap_or_default() },
            None => waiting,
        },
        Some(_) | None => waiting,
    }
}

/// A stored event of one pair (the page reads them from its event window).
#[derive(Debug, Clone, PartialEq)]
pub struct PairEvent {
    pub id: i64,
    pub ts_ms: i64,
    pub event_type: String,
    pub payload: Value,
}

/// The status of the pair's CURRENT alert entry (the newest `PAIR_TRANSITION` into `state`) from its
/// events, in any order. Records of an earlier entry do not count; without an entry event every
/// record counts and the time of the first round is unknown.
pub fn status_of_pair(state: PairState, events: &[PairEvent]) -> Status {
    let mut sorted: Vec<&PairEvent> = events.iter().collect();
    sorted.sort_by_key(|e| (e.ts_ms, e.id));
    let entry = sorted.iter().rposition(|e| e.event_type == PAIR_TRANSITION && e.payload.get("to").and_then(Value::as_str) == Some(state.as_str()));
    let (alert_since_ms, after) = match entry {
        Some(i) => (Some(sorted[i].ts_ms), &sorted[i + 1..]),
        None => (None, &sorted[..]),
    };
    let records: Vec<Record> = after
        .iter()
        .filter(|e| e.event_type == NAKED_LEG_WATCHDOG || e.event_type == LEG_FLATTEN)
        .map(|e| Record { ts_ms: e.ts_ms, event_type: e.event_type.clone(), payload: e.payload.clone() })
        .collect();
    status_from_records(alert_since_ms, &records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn read(leg: Leg, position: &str, recorded: &str, terminal: bool) -> LegRead {
        LegRead {
            leg,
            exchange: if leg == Leg::Long { Exchange::Binance } else { Exchange::Bybit },
            position: Ok(dec(position)),
            recorded: Ok(dec(recorded)),
            orders_terminal: Ok(terminal),
        }
    }

    fn input(long: LegRead, short: LegRead) -> DecideInput<'static> {
        DecideInput { symbol: "BTCUSDT", simulated: false, tolerance_pct: dec("1"), legs: [long, short], waited_rounds: 0, attempts_sent: 0 }
    }

    fn unknown_reason(d: Decision) -> String {
        match d {
            Decision::Unknown { reason } => reason,
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // ---- the constants of the spec ----

    #[test]
    fn the_constants_are_the_ones_of_the_spec() {
        assert_eq!((WATCHDOG_GRACE_MS, WATCHDOG_INTERVAL_MS, MAX_FLATTEN_ATTEMPTS, MAX_CANCEL_WAIT_ROUNDS), (5_000, 5_000, 3, 3));
        assert_eq!((NAKED_LEG_WATCHDOG, NAKED_LEG_WATCHDOG_LOCK), ("NAKED_LEG_WATCHDOG", "NAKED_LEG_WATCHDOG_LOCK"));
    }

    // ---- spec: 只在確認只有一腿成交時自動平倉 ----

    #[test]
    fn one_leg_filled_and_the_other_confirmed_unfilled_flattens_the_filled_leg() {
        // spec scenario: long recorded 100 / position 100; the short order is over, filled 0, position 0
        let d = decide(&input(read(Leg::Long, "100", "100", true), read(Leg::Short, "0", "0", true)));
        assert_eq!(d, Decision::Flatten { leg: Leg::Long, qty: dec("100") });
    }

    #[test]
    fn a_bare_short_leg_is_flattened_with_its_own_quantity() {
        // the short position is negative; the quantity to close is its size
        let d = decide(&input(read(Leg::Long, "0", "0", true), read(Leg::Short, "-50", "50", true)));
        assert_eq!(d, Decision::Flatten { leg: Leg::Short, qty: dec("50") });
    }

    #[test]
    fn both_legs_holding_a_position_is_hedged_and_sends_nothing() {
        // spec scenario: 另一腿其實也成交了
        let d = decide(&input(read(Leg::Long, "100", "100", true), read(Leg::Short, "-100", "100", true)));
        assert_eq!(d, Decision::Hedged);
    }

    #[test]
    fn an_imbalanced_pair_with_both_legs_held_is_hedged_too() {
        let d = decide(&input(read(Leg::Long, "100", "100", true), read(Leg::Short, "-60", "60", true)));
        assert_eq!(d, Decision::Hedged);
    }

    #[test]
    fn both_legs_without_a_position_is_flat() {
        let d = decide(&input(read(Leg::Long, "0", "0", true), read(Leg::Short, "0", "0", true)));
        assert_eq!(d, Decision::Flat);
    }

    #[test]
    fn a_position_beyond_the_tolerance_of_the_recorded_fill_is_unknown() {
        // spec scenario: 持倉 150、記錄成交 100，超出容差
        let d = decide(&input(read(Leg::Long, "150", "100", true), read(Leg::Short, "0", "0", true)));
        let why = unknown_reason(d);
        assert!(why.contains("long") && why.contains("differs"), "{why}");
    }

    #[test]
    fn a_position_on_the_wrong_side_is_unknown() {
        let d = decide(&input(read(Leg::Long, "-5", "5", true), read(Leg::Short, "0", "0", true)));
        let why = unknown_reason(d);
        assert!(why.contains("long") && why.contains("wrong side"), "{why}");
    }

    #[test]
    fn the_quantity_is_the_smaller_of_position_and_recorded_fill_within_tolerance() {
        let d = decide(&input(read(Leg::Long, "100", "99.5", true), read(Leg::Short, "0", "0", true)));
        assert_eq!(d, Decision::Flatten { leg: Leg::Long, qty: dec("99.5") });
        let d = decide(&input(read(Leg::Long, "99.5", "100", true), read(Leg::Short, "0", "0", true)));
        assert_eq!(d, Decision::Flatten { leg: Leg::Long, qty: dec("99.5") });
    }

    #[test]
    fn a_difference_exactly_at_the_tolerance_passes() {
        // |100 - 99| / 100 = 1 % = tolerance: equal passes (plan_leg_close), one more digit does not
        let d = decide(&input(read(Leg::Long, "100", "99", true), read(Leg::Short, "0", "0", true)));
        assert_eq!(d, Decision::Flatten { leg: Leg::Long, qty: dec("99") });
        let d = decide(&input(read(Leg::Long, "100", "98.9", true), read(Leg::Short, "0", "0", true)));
        assert!(matches!(d, Decision::Unknown { .. }), "{d:?}");
    }

    #[test]
    fn a_position_without_any_recorded_fill_is_unknown_even_with_a_huge_tolerance() {
        // 100 % tolerance lets plan_leg_close return a zero quantity for a held position: the watchdog
        // must not read that as "empty leg"
        let mut i = input(read(Leg::Long, "5", "0", true), read(Leg::Short, "0", "0", true));
        i.tolerance_pct = dec("100");
        let why = unknown_reason(decide(&i));
        assert!(why.contains("long"), "{why}");
    }

    #[test]
    fn the_other_leg_with_a_recorded_fill_but_no_position_is_unknown() {
        // e.g. liquidated or closed outside the pair: the other leg is not "confirmed unfilled"
        let d = decide(&input(read(Leg::Long, "100", "100", true), read(Leg::Short, "0", "50", true)));
        let why = unknown_reason(d);
        assert!(why.contains("short"), "{why}");
    }

    #[test]
    fn a_position_of_the_other_leg_without_a_recorded_fill_is_unknown() {
        // exposure on the same symbol that is not the pair's (manual trading)
        let d = decide(&input(read(Leg::Long, "100", "100", true), read(Leg::Short, "-3", "0", true)));
        let why = unknown_reason(d);
        assert!(why.contains("short"), "{why}");
    }

    // ---- spec: 查詢失敗 ----

    #[test]
    fn any_failed_read_is_unknown_and_names_every_failure() {
        let mut long = read(Leg::Long, "100", "100", true);
        long.position = Err("Binance positions list incomplete".into());
        let mut short = read(Leg::Short, "0", "0", true);
        short.recorded = Err("order x not found".into());
        let why = unknown_reason(decide(&input(long, short)));
        assert!(why.contains("long position") && why.contains("incomplete"), "{why}");
        assert!(why.contains("short recorded fill") && why.contains("not found"), "{why}");

        let mut long = read(Leg::Long, "100", "100", true);
        long.orders_terminal = Err("order lookup failed: timeout".into());
        let why = unknown_reason(decide(&input(long, read(Leg::Short, "0", "0", true))));
        assert!(why.contains("long order status") && why.contains("timeout"), "{why}");
    }

    // ---- spec: 委託仍未終結 ----

    #[test]
    fn an_unfinished_order_waits_for_the_cancel_and_sends_nothing() {
        let d = decide(&input(read(Leg::Long, "100", "100", true), read(Leg::Short, "0", "0", false)));
        assert_eq!(d, Decision::WaitForCancel);
        let mut i = input(read(Leg::Long, "100", "100", true), read(Leg::Short, "0", "0", false));
        i.waited_rounds = 1;
        assert_eq!(decide(&i), Decision::WaitForCancel, "the second round still waits");
    }

    #[test]
    fn the_third_consecutive_unfinished_round_is_unknown() {
        let mut i = input(read(Leg::Long, "100", "100", true), read(Leg::Short, "0", "0", false));
        i.waited_rounds = MAX_CANCEL_WAIT_ROUNDS - 1;
        let why = unknown_reason(decide(&i));
        assert!(why.contains("short") && why.contains("3"), "{why}");
    }

    #[test]
    fn a_failed_read_wins_over_an_unfinished_order() {
        let mut long = read(Leg::Long, "100", "100", true);
        long.position = Err("unreadable".into());
        let short = read(Leg::Short, "0", "0", false);
        assert!(matches!(decide(&input(long, short)), Decision::Unknown { .. }));
    }

    #[test]
    fn an_unfinished_order_wins_over_a_mismatch_because_positions_are_still_moving() {
        // 150 vs 100 would be Unknown, but the short order is still open: look again first
        let d = decide(&input(read(Leg::Long, "150", "100", true), read(Leg::Short, "0", "0", false)));
        assert_eq!(d, Decision::WaitForCancel);
    }

    // ---- spec: 三次都失敗 ----

    #[test]
    fn after_three_flatten_attempts_it_is_unknown_not_a_fourth_order() {
        let mut i = input(read(Leg::Long, "100", "100", true), read(Leg::Short, "0", "0", true));
        i.attempts_sent = MAX_FLATTEN_ATTEMPTS - 1;
        assert_eq!(decide(&i), Decision::Flatten { leg: Leg::Long, qty: dec("100") }, "the third attempt is still allowed");
        i.attempts_sent = MAX_FLATTEN_ATTEMPTS;
        let why = unknown_reason(decide(&i));
        assert!(why.contains("auto flatten failed 3 times"), "{why}");
    }

    #[test]
    fn attempts_do_not_matter_when_there_is_nothing_to_flatten() {
        let mut i = input(read(Leg::Long, "0", "0", true), read(Leg::Short, "0", "0", true));
        i.attempts_sent = MAX_FLATTEN_ATTEMPTS;
        assert_eq!(decide(&i), Decision::Flat, "the third attempt worked: both legs are flat");
        let mut i = input(read(Leg::Long, "100", "100", true), read(Leg::Short, "-100", "100", true));
        i.attempts_sent = MAX_FLATTEN_ATTEMPTS;
        assert_eq!(decide(&i), Decision::Hedged);
    }

    #[test]
    fn it_never_flattens_both_legs_in_one_round() {
        // sweep a grid of positions: Flatten always names exactly one leg and never both
        for (lp, sp) in [("0", "0"), ("10", "0"), ("0", "-10"), ("10", "-10"), ("10", "-4")] {
            let d = decide(&input(read(Leg::Long, lp, lp.trim_start_matches('-'), true), read(Leg::Short, sp, sp.trim_start_matches('-'), true)));
            let flattened = matches!(d, Decision::Flatten { .. });
            let held = [lp, sp].iter().filter(|p| **p != "0").count();
            assert_eq!(flattened, held == 1, "{lp}/{sp}: {d:?}");
        }
    }

    // ---- 2.1: where the watchdog stands, rebuilt from events ----

    use serde_json::json;

    fn rec(ts_ms: i64, event_type: &str, payload: Value) -> Record {
        Record { ts_ms, event_type: event_type.into(), payload }
    }

    fn round(ts_ms: i64, conclusion: &str) -> Record {
        rec(ts_ms, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": conclusion }))
    }

    #[test]
    fn without_any_record_the_first_round_is_due_when_the_grace_has_passed() {
        let p = progress_from_records(10_000, &[]);
        assert_eq!((p.alert_since_ms, p.rounds, p.attempts_sent, p.waited_rounds, p.terminal, p.last_record_ms), (10_000, 0, 0, 0, false, None));
        assert_eq!(p.next_round_ms(), 15_000, "alert_since + 5 s");
    }

    #[test]
    fn consecutive_waits_are_counted_and_any_other_conclusion_resets_them() {
        let p = progress_from_records(10_000, &[round(15_000, "WAIT_FOR_CANCEL"), round(20_000, "WAIT_FOR_CANCEL")]);
        assert_eq!((p.rounds, p.waited_rounds, p.terminal, p.last_record_ms), (2, 2, false, Some(20_000)));
        assert_eq!(p.next_round_ms(), 25_000, "one interval after the newest record");
        let p = progress_from_records(10_000, &[round(15_000, "WAIT_FOR_CANCEL"), round(20_000, "FLATTEN"), round(30_000, "WAIT_FOR_CANCEL")]);
        assert_eq!((p.rounds, p.waited_rounds), (3, 1), "the FLATTEN round in between broke the run");
    }

    #[test]
    fn only_watchdog_flatten_orders_count_as_attempts() {
        let flat = |ts, by: Option<&str>| rec(ts, LEG_FLATTEN, by.map_or(json!({ "leg": "long" }), |b| json!({ "leg": "long", "by": b })));
        let records = [flat(16_000, Some("watchdog")), flat(18_000, Some("manual")), flat(19_000, None), flat(22_000, Some("watchdog"))];
        let p = progress_from_records(10_000, &records);
        assert_eq!(p.attempts_sent, 2, "a manual flatten (or an old one without `by`) is not the watchdog's attempt");
        assert_eq!(p.last_record_ms, Some(22_000));
    }

    #[test]
    fn a_conclusion_that_ends_the_rounds_makes_the_progress_terminal() {
        for (c, terminal) in [("HEDGED", true), ("FLAT", true), ("UNKNOWN", true), ("FLATTEN", false), ("WAIT_FOR_CANCEL", false)] {
            let p = progress_from_records(0, &[round(5_000, c)]);
            assert_eq!(p.terminal, terminal, "{c}");
            assert_eq!(Conclusion::parse(c).map(Conclusion::ends_the_rounds), Some(terminal), "{c}");
        }
        // records of other kinds are not rounds
        let p = progress_from_records(0, &[rec(6_000, NAKED_LEG_WATCHDOG, json!({ "kind": "NOTIFY_RESULT", "ok": true }))]);
        assert_eq!((p.rounds, p.terminal), (0, false));
    }

    #[test]
    fn the_next_round_never_precedes_the_grace() {
        // a record older than alert_since + grace (clock skew, a seeded state) cannot pull the round forward
        let p = progress_from_records(100_000, &[round(90_000, "WAIT_FOR_CANCEL")]);
        assert_eq!(p.next_round_ms(), 105_000);
    }

    #[test]
    fn the_progress_is_rebuilt_from_the_stored_events_of_the_current_entry() {
        use crate::store::db::test_support::open_tmp;
        use crate::store::events::EventStore;
        let (_d, db, clock) = open_tmp();
        let es = EventStore::new(db.clone());
        let transition = |from: &str, to: &str| json!({ "from": from, "to": to, "event": "System(x)", "detail": {} });
        clock.set(1_000);
        es.append(PAIR_TRANSITION, Some("p1"), transition("FILL_MONITOR", "PARTIAL_FAILURE")).unwrap();
        clock.set(7_000);
        es.append(NAKED_LEG_WATCHDOG, Some("p1"), json!({ "kind": KIND_ROUND, "conclusion": "UNKNOWN" })).unwrap();
        // the pair was closed by hand and failed again: a NEW entry; older records do not belong to it
        clock.set(50_000);
        es.append(PAIR_TRANSITION, Some("p1"), transition("PARTIAL_FAILURE", "CLOSING")).unwrap();
        clock.set(60_000);
        es.append(PAIR_TRANSITION, Some("p1"), transition("CLOSING", "PARTIAL_FAILURE")).unwrap();
        clock.set(66_000);
        es.append(NAKED_LEG_WATCHDOG, Some("p1"), json!({ "kind": KIND_ROUND, "conclusion": "WAIT_FOR_CANCEL" })).unwrap();
        es.append(LEG_FLATTEN, Some("p1"), json!({ "by": "watchdog", "leg": "long" })).unwrap();
        // another pair's events never leak in
        es.append(NAKED_LEG_WATCHDOG, Some("p2"), json!({ "kind": KIND_ROUND, "conclusion": "HEDGED" })).unwrap();
        let p = load_progress(&db, "p1", PairState::PartialFailure, 999_999).unwrap();
        assert_eq!(p.alert_since_ms, 60_000, "the newest transition into the state, not the first");
        assert_eq!((p.rounds, p.waited_rounds, p.attempts_sent, p.terminal, p.last_record_ms), (1, 1, 1, false, Some(66_000)));
    }

    #[test]
    fn a_pair_without_an_entry_event_counts_from_now() {
        use crate::store::db::test_support::open_tmp;
        let (_d, db, _c) = open_tmp();
        let p = load_progress(&db, "seeded", PairState::PartialFailure, 123_000).unwrap();
        assert_eq!((p.alert_since_ms, p.rounds, p.terminal), (123_000, 0, false));
        assert_eq!(p.next_round_ms(), 128_000);
    }

    // ---- 2.2: folding a leg's order lookups into what `decide` reads ----

    fn line(id: &str, action: OrderAction, state: &str, filled: &str) -> OrderLine {
        OrderLine { client_order_id: id.into(), action, rejected_intent: false, state: Some(state.into()), filled: Some(dec(filled)), open: state == "Open", error: None }
    }

    fn probe_leg(orders: Vec<OrderLine>, position: &str) -> LegProbe {
        LegProbe::from_reads(Leg::Long, Exchange::Binance, Ok(dec(position)), orders)
    }

    #[test]
    fn a_filled_open_order_is_recorded_and_terminal() {
        let p = probe_leg(vec![line("o1", OrderAction::Open, "Filled", "10")], "10");
        assert_eq!((p.read.recorded.clone(), p.read.orders_terminal.clone(), p.read.position.clone()), (Ok(dec("10")), Ok(true), Ok(dec("10"))));
        assert!(p.open_order_ids().is_empty());
    }

    #[test]
    fn close_fills_reduce_the_recorded_fill_and_never_below_zero() {
        let p = probe_leg(vec![line("o1", OrderAction::Open, "Filled", "10"), line("c1", OrderAction::Close, "Filled", "4")], "6");
        assert_eq!(p.read.recorded, Ok(dec("6")));
        let p = probe_leg(vec![line("o1", OrderAction::Open, "Filled", "10"), line("c1", OrderAction::Close, "Filled", "12")], "0");
        assert_eq!(p.read.recorded, Ok(dec("0")), "a close larger than the open fills cannot make the record negative");
    }

    #[test]
    fn an_order_that_can_still_fill_makes_the_leg_not_terminal_and_is_listed_for_the_cancel() {
        let p = probe_leg(vec![line("o1", OrderAction::Open, "Open", "3"), line("c1", OrderAction::Close, "Cancelled", "0")], "3");
        assert_eq!(p.read.orders_terminal, Ok(false));
        assert_eq!(p.read.recorded, Ok(dec("3")), "the partial fill so far");
        assert_eq!(p.open_order_ids(), vec!["o1".to_string()]);
    }

    #[test]
    fn a_refused_intent_counts_zero_and_is_over_without_a_lookup() {
        let refused = OrderLine { client_order_id: "o1".into(), action: OrderAction::Open, rejected_intent: true, state: None, filled: None, open: false, error: None };
        let p = probe_leg(vec![refused], "0");
        assert_eq!((p.read.recorded.clone(), p.read.orders_terminal.clone()), (Ok(dec("0")), Ok(true)));
        assert!(p.open_order_ids().is_empty());
    }

    #[test]
    fn a_lookup_that_failed_makes_the_recorded_fill_and_the_terminal_flag_unknown() {
        let mut failed = line("o1", OrderAction::Open, "Filled", "10");
        failed.state = None;
        failed.filled = None;
        failed.error = Some("order o1 not found".into());
        let p = probe_leg(vec![line("o0", OrderAction::Open, "Filled", "5"), failed], "10");
        assert_eq!(p.read.recorded, Err("order o1 not found".into()));
        assert_eq!(p.read.orders_terminal, Err("order o1 not found".into()));
        assert!(p.open_order_ids().is_empty(), "an order that cannot be looked up is not cancelled blindly");
    }

    #[test]
    fn a_leg_without_orders_records_zero_and_is_terminal() {
        let p = probe_leg(vec![], "0");
        assert_eq!((p.read.recorded.clone(), p.read.orders_terminal.clone()), (Ok(dec("0")), Ok(true)));
    }

    #[test]
    fn the_evidence_names_both_legs_with_their_numbers_orders_and_errors() {
        let long = probe_leg(vec![line("o1", OrderAction::Open, "Filled", "10")], "10");
        let mut short = LegProbe::from_reads(Leg::Short, Exchange::Bybit, Err("Bybit positions list incomplete".into()), vec![]);
        short.orders = vec![];
        let e = evidence(&WatchdogProbe { legs: [long, short] });
        assert_eq!(e["long"]["exchange"], json!("Binance"));
        assert_eq!((e["long"]["position"].clone(), e["long"]["recorded"].clone(), e["long"]["orders_terminal"].clone()), (json!("10"), json!("10"), json!(true)));
        assert_eq!(e["long"]["orders"][0], json!({ "client_order_id": "o1", "action": "open", "state": "Filled", "filled": "10" }));
        assert_eq!(e["short"]["position"], json!({ "error": "Bybit positions list incomplete" }), "an unreadable number is an error, never 0");
        assert_eq!(e["short"]["exchange"], json!("Bybit"));
    }

    // ---- 3.3: the status the manual-handling card shows, from the events ----

    fn at(ts_ms: i64, event_type: &str, payload: Value) -> Record {
        Record { ts_ms, event_type: event_type.into(), payload }
    }

    fn flatten_round(ts: i64, attempt: u32) -> Record {
        at(ts, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": "FLATTEN", "leg": "long", "quantity": "10", "attempt": attempt }))
    }

    fn attempt_result(ts: i64, ok: bool, attempt: u32) -> Record {
        let mut p = json!({ "kind": KIND_ATTEMPT_RESULT, "ok": ok, "attempt": attempt, "leg": "long", "quantity": "10" });
        if !ok {
            p["reason"] = json!("order rejected: risk limit");
        }
        at(ts, NAKED_LEG_WATCHDOG, p)
    }

    #[test]
    fn without_a_record_the_status_is_waiting_for_the_first_round() {
        assert_eq!(status_from_records(Some(10_000), &[]), Status::Waiting { due_ms: Some(15_000) });
        assert_eq!(status_from_records(None, &[]), Status::Waiting { due_ms: None });
    }

    #[test]
    fn a_round_that_waits_for_the_cancel_says_when_it_looks_again() {
        let r = [at(15_000, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": "WAIT_FOR_CANCEL", "waited_rounds": 1 }))];
        assert_eq!(status_from_records(Some(10_000), &r), Status::WaitingCancel { waited: 2, next_ms: 20_000 });
    }

    #[test]
    fn a_sent_flatten_is_flattening_until_its_result_is_recorded() {
        assert_eq!(status_from_records(Some(10_000), &[flatten_round(15_000, 1)]), Status::Flattening { leg: Leg::Long, quantity: dec("10"), attempt: 1 });
    }

    #[test]
    fn a_failed_attempt_is_retrying_with_its_reason_and_the_next_look() {
        let r = [flatten_round(15_000, 1), attempt_result(16_000, false, 1)];
        assert_eq!(status_from_records(Some(10_000), &r), Status::Retrying { attempt: 1, reason: "order rejected: risk limit".into(), next_ms: 21_000 });
    }

    #[test]
    fn a_successful_attempt_is_flattened_and_stays_flattened_after_the_confirming_round() {
        let done = Status::Flattened { leg: Leg::Long, quantity: dec("10") };
        let r = [flatten_round(15_000, 1), attempt_result(16_000, true, 1)];
        assert_eq!(status_from_records(Some(10_000), &r), done);
        let confirmed = [flatten_round(15_000, 1), attempt_result(16_000, true, 1), at(21_000, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": "FLAT" }))];
        assert_eq!(status_from_records(Some(10_000), &confirmed), done, "FLAT after our own flatten is the flatten, not an unexplained empty pair");
        let notified = [flatten_round(15_000, 1), attempt_result(16_000, true, 1), at(16_001, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_NOTIFY_RESULT, "ok": true }))];
        assert_eq!(status_from_records(Some(10_000), &notified), done, "a notification record changes nothing");
    }

    #[test]
    fn hedged_flat_and_unknown_conclusions_are_the_status() {
        let round = |c: &str| at(15_000, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": c, "reason": "long position unreadable: timeout" }));
        assert_eq!(status_from_records(Some(10_000), &[round("HEDGED")]), Status::Hedged);
        assert_eq!(status_from_records(Some(10_000), &[round("FLAT")]), Status::Flat);
        assert_eq!(status_from_records(Some(10_000), &[round("UNKNOWN")]), Status::Unknown { reason: "long position unreadable: timeout".into() });
    }

    #[test]
    fn a_skip_is_the_status_only_while_nothing_else_happened() {
        let skip = at(11_000, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_SKIPPED, "reason": "pair belongs to EXCHANGE_DEMO" }));
        assert_eq!(status_from_records(Some(10_000), std::slice::from_ref(&skip)), Status::Skipped { reason: "pair belongs to EXCHANGE_DEMO".into() });
        let later = [skip, at(15_000, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": "HEDGED" }))];
        assert_eq!(status_from_records(Some(10_000), &later), Status::Hedged);
    }

    fn transition(id: i64, ts: i64, to: &str) -> PairEvent {
        PairEvent { id, ts_ms: ts, event_type: PAIR_TRANSITION.into(), payload: json!({ "from": "X", "to": to }) }
    }

    fn pair_event(id: i64, r: Record) -> PairEvent {
        PairEvent { id, ts_ms: r.ts_ms, event_type: r.event_type, payload: r.payload }
    }

    #[test]
    fn the_status_of_a_pair_counts_only_the_records_after_its_current_entry() {
        let old_unknown = pair_event(2, at(7_000, NAKED_LEG_WATCHDOG, json!({ "kind": KIND_ROUND, "conclusion": "UNKNOWN", "reason": "old" })));
        let events = [transition(1, 1_000, "PARTIAL_FAILURE"), old_unknown.clone(), transition(3, 50_000, "CLOSING"), transition(4, 60_000, "PARTIAL_FAILURE")];
        assert_eq!(status_of_pair(PairState::PartialFailure, &events), Status::Waiting { due_ms: Some(65_000) }, "the old entry's UNKNOWN is history");
        let mut with_new = events.to_vec();
        with_new.push(pair_event(5, flatten_round(66_000, 1)));
        assert_eq!(status_of_pair(PairState::PartialFailure, &with_new), Status::Flattening { leg: Leg::Long, quantity: dec("10"), attempt: 1 });
    }

    #[test]
    fn the_status_of_a_pair_does_not_depend_on_the_order_of_the_events() {
        let mut events = vec![transition(1, 1_000, "UNRESOLVED"), pair_event(2, flatten_round(6_000, 1)), pair_event(3, attempt_result(7_000, true, 1))];
        let forward = status_of_pair(PairState::Unresolved, &events);
        events.reverse(); // the page's window is newest first
        assert_eq!(status_of_pair(PairState::Unresolved, &events), forward);
        assert_eq!(forward, Status::Flattened { leg: Leg::Long, quantity: dec("10") });
    }

    #[test]
    fn a_pair_without_an_entry_event_has_a_status_without_a_time() {
        assert_eq!(status_of_pair(PairState::Imbalanced, &[]), Status::Waiting { due_ms: None });
    }
}

//! Funding slots (change funding-slot-tracing, design D1): one row per (pair, leg, settlement
//! time) the position was held through, with the staged tracing state. Writes that settle a slot
//! also append the immutable `PAIR_FUNDING_LINKED` event (carrying the pair id) in the same
//! transaction; a ledger row can settle at most one slot (unique dedupe key).

use rusqlite::{OptionalExtension, Row, params};
use serde_json::json;
use tong_funding_core::types::{Decimal, Exchange};

use super::db::{Db, StoreError};
use super::events::insert_event_on;

pub const PAIR_FUNDING_LINKED: &str = "PAIR_FUNDING_LINKED";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Tracing1,
    Tracing2,
    Settled,
    ManualCheck,
}

impl SlotState {
    pub fn as_str(self) -> &'static str {
        match self {
            SlotState::Tracing1 => "TRACING_1",
            SlotState::Tracing2 => "TRACING_2",
            SlotState::Settled => "SETTLED",
            SlotState::ManualCheck => "MANUAL_CHECK",
        }
    }
    pub fn parse(s: &str) -> Option<SlotState> {
        Some(match s {
            "TRACING_1" => SlotState::Tracing1,
            "TRACING_2" => SlotState::Tracing2,
            "SETTLED" => SlotState::Settled,
            "MANUAL_CHECK" => SlotState::ManualCheck,
            _ => return None,
        })
    }
}

/// Why a slot needs a human (kept so a later trace knows the real situation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualReason {
    /// The checks completed and found no ledger row.
    NotFound,
    /// The checks at and after the deadline failed: nothing can be said about the row.
    FetchFailed,
    /// Put back into tracing by the backfill of an earlier incomplete pair.
    Backfill,
    /// The row falls inside two pairs' windows: never linked by guess.
    Ambiguous,
}

impl ManualReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ManualReason::NotFound => "NOT_FOUND",
            ManualReason::FetchFailed => "FETCH_FAILED",
            ManualReason::Backfill => "BACKFILL",
            ManualReason::Ambiguous => "AMBIGUOUS",
        }
    }
    pub fn parse(s: &str) -> Option<ManualReason> {
        Some(match s {
            "NOT_FOUND" => ManualReason::NotFound,
            "FETCH_FAILED" => ManualReason::FetchFailed,
            "BACKFILL" => ManualReason::Backfill,
            "AMBIGUOUS" => ManualReason::Ambiguous,
            _ => return None,
        })
    }
}

/// How a settled slot was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// A ledger row was found.
    Ledger,
    /// No row, and the exchange's public history shows a funding rate of exactly 0.
    ZeroRate,
    /// No row, and the exchange's public history has no funding at that time either (the expected
    /// settlement did not happen on that exchange): settled with amount 0 after the tracing period.
    NoSettlement,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Ledger => "LEDGER",
            Resolution::ZeroRate => "ZERO_RATE",
            Resolution::NoSettlement => "NO_SETTLEMENT",
        }
    }
    pub fn parse(s: &str) -> Option<Resolution> {
        Some(match s {
            "LEDGER" => Resolution::Ledger,
            "ZERO_RATE" => Resolution::ZeroRate,
            "NO_SETTLEMENT" => Resolution::NoSettlement,
            _ => return None,
        })
    }
}

/// Which side of the pair the slot belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotLeg {
    Long,
    Short,
}

impl SlotLeg {
    pub fn as_str(self) -> &'static str {
        match self {
            SlotLeg::Long => "long",
            SlotLeg::Short => "short",
        }
    }
    pub fn parse(s: &str) -> Option<SlotLeg> {
        match s {
            "long" => Some(SlotLeg::Long),
            "short" => Some(SlotLeg::Short),
            _ => None,
        }
    }
}

/// One stored slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotRow {
    pub pair_id: String,
    pub leg: SlotLeg,
    pub exchange: Exchange,
    pub symbol: String,
    pub settle_ms: i64,
    pub state: SlotState,
    pub manual_reason: Option<ManualReason>,
    pub resolution: Option<Resolution>,
    pub amount: Option<Decimal>,
    pub ledger_dedupe_key: Option<String>,
    pub ledger_exchange_id: Option<String>,
    pub zero_rate_source: Option<String>,
    pub first_seen_ms: Option<i64>,
    pub next_check_ms: i64,
    pub checks_ok: i64,
    pub checks_failed: i64,
    pub last_error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl SlotRow {
    /// A fresh slot in `TRACING_1`, first check due at `settle_ms + first_check_ms`.
    pub fn new(pair_id: &str, leg: SlotLeg, exchange: Exchange, symbol: &str, settle_ms: i64, next_check_ms: i64, now_ms: i64) -> SlotRow {
        SlotRow {
            pair_id: pair_id.into(),
            leg,
            exchange,
            symbol: symbol.into(),
            settle_ms,
            state: SlotState::Tracing1,
            manual_reason: None,
            resolution: None,
            amount: None,
            ledger_dedupe_key: None,
            ledger_exchange_id: None,
            zero_rate_source: None,
            first_seen_ms: None,
            next_check_ms,
            checks_ok: 0,
            checks_failed: 0,
            last_error: None,
            created_ms: now_ms,
            updated_ms: now_ms,
        }
    }
}

const COLUMNS: &str = "pair_id, leg, exchange, symbol, settle_ms, state, manual_reason, resolution, amount, ledger_dedupe_key, ledger_exchange_id, zero_rate_source, first_seen_ms, next_check_ms, checks_ok, checks_failed, last_error, created_ms, updated_ms";

fn bad(col: &str, v: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, format!("funding_slots.{col}: unreadable value {v:?}").into())
}

fn exchange_of(name: &str) -> Option<Exchange> {
    Exchange::ALL.into_iter().find(|e| e.name() == name)
}

fn read(r: &Row<'_>) -> rusqlite::Result<SlotRow> {
    let text = |i: usize| r.get::<_, Option<String>>(i);
    let leg: String = r.get(1)?;
    let exchange: String = r.get(2)?;
    let state: String = r.get(5)?;
    let amount = text(8)?.map(|s| s.parse::<Decimal>().map_err(|_| bad("amount", &s))).transpose()?;
    Ok(SlotRow {
        pair_id: r.get(0)?,
        leg: SlotLeg::parse(&leg).ok_or_else(|| bad("leg", &leg))?,
        exchange: exchange_of(&exchange).ok_or_else(|| bad("exchange", &exchange))?,
        symbol: r.get(3)?,
        settle_ms: r.get(4)?,
        state: SlotState::parse(&state).ok_or_else(|| bad("state", &state))?,
        manual_reason: text(6)?.map(|s| ManualReason::parse(&s).ok_or_else(|| bad("manual_reason", &s))).transpose()?,
        resolution: text(7)?.map(|s| Resolution::parse(&s).ok_or_else(|| bad("resolution", &s))).transpose()?,
        amount,
        ledger_dedupe_key: text(9)?,
        ledger_exchange_id: text(10)?,
        zero_rate_source: text(11)?,
        first_seen_ms: r.get(12)?,
        next_check_ms: r.get(13)?,
        checks_ok: r.get(14)?,
        checks_failed: r.get(15)?,
        last_error: text(16)?,
        created_ms: r.get(17)?,
        updated_ms: r.get(18)?,
    })
}

impl Db {
    /// Inserts the slot unless its (pair, leg, settlement time) already exists. Returns whether it
    /// was new.
    pub fn insert_slot_if_absent(&self, s: &SlotRow) -> Result<bool, StoreError> {
        self.with_conn(|c| {
            let n = c.execute(
                &format!("INSERT INTO funding_slots ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19) ON CONFLICT DO NOTHING"),
                params![
                    s.pair_id,
                    s.leg.as_str(),
                    s.exchange.name(),
                    s.symbol,
                    s.settle_ms,
                    s.state.as_str(),
                    s.manual_reason.map(ManualReason::as_str),
                    s.resolution.map(Resolution::as_str),
                    s.amount.map(|a| a.normalize().to_string()),
                    s.ledger_dedupe_key,
                    s.ledger_exchange_id,
                    s.zero_rate_source,
                    s.first_seen_ms,
                    s.next_check_ms,
                    s.checks_ok,
                    s.checks_failed,
                    s.last_error,
                    s.created_ms,
                    s.updated_ms
                ],
            )?;
            Ok(n == 1)
        })
    }

    /// Every slot of one pair, by settlement time then leg.
    pub fn slots_of_pair(&self, pair_id: &str) -> Result<Vec<SlotRow>, StoreError> {
        self.with_conn(|c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM funding_slots WHERE pair_id = ?1 ORDER BY settle_ms, leg"))?;
            let rows = st.query_map([pair_id], read)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Every slot (oldest settlement first).
    pub fn all_slots(&self) -> Result<Vec<SlotRow>, StoreError> {
        self.with_conn(|c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM funding_slots ORDER BY settle_ms, pair_id, leg"))?;
            let rows = st.query_map([], read)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Slots that are not settled and whose next check time has come.
    pub fn due_slots(&self, now_ms: i64) -> Result<Vec<SlotRow>, StoreError> {
        self.with_conn(|c| {
            let mut st = c.prepare(&format!("SELECT {COLUMNS} FROM funding_slots WHERE state != 'SETTLED' AND next_check_ms <= ?1 ORDER BY settle_ms, pair_id, leg"))?;
            let rows = st.query_map([now_ms], read)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Stores the new state of an unsettled slot (checks, stage, next check, error). A slot that is
    /// already `SETTLED` is never changed by this.
    pub fn update_slot_progress(&self, s: &SlotRow) -> Result<bool, StoreError> {
        self.with_conn(|c| {
            let n = c.execute(
                "UPDATE funding_slots SET state = ?1, manual_reason = ?2, next_check_ms = ?3, checks_ok = ?4, checks_failed = ?5, last_error = ?6, updated_ms = ?7 \
                 WHERE pair_id = ?8 AND leg = ?9 AND settle_ms = ?10 AND state != 'SETTLED'",
                params![
                    s.state.as_str(),
                    s.manual_reason.map(ManualReason::as_str),
                    s.next_check_ms,
                    s.checks_ok,
                    s.checks_failed,
                    s.last_error,
                    s.updated_ms,
                    s.pair_id,
                    s.leg.as_str(),
                    s.settle_ms
                ],
            )?;
            Ok(n == 1)
        })
    }

    /// Settles the slot and appends the `PAIR_FUNDING_LINKED` event (with the pair id) in one
    /// transaction. Returns `false` (and writes no event) when the slot was already settled or the
    /// ledger row is already linked to another slot. `s` carries the settled fields.
    pub fn settle_slot(&self, s: &SlotRow) -> Result<bool, StoreError> {
        let ts_ms = self.now_ms();
        self.with_conn(|c| {
            let run = |c: &mut rusqlite::Connection| -> rusqlite::Result<bool> {
                let tx = c.transaction()?;
                let changed = tx.execute(
                    "UPDATE funding_slots SET state = 'SETTLED', manual_reason = NULL, resolution = ?1, amount = ?2, ledger_dedupe_key = ?3, ledger_exchange_id = ?4, \
                     zero_rate_source = ?5, first_seen_ms = ?6, checks_ok = ?7, checks_failed = ?8, last_error = NULL, updated_ms = ?9 \
                     WHERE pair_id = ?10 AND leg = ?11 AND settle_ms = ?12 AND state != 'SETTLED'",
                    params![
                        s.resolution.map(Resolution::as_str),
                        s.amount.map(|a| a.normalize().to_string()),
                        s.ledger_dedupe_key,
                        s.ledger_exchange_id,
                        s.zero_rate_source,
                        s.first_seen_ms,
                        s.checks_ok,
                        s.checks_failed,
                        s.updated_ms,
                        s.pair_id,
                        s.leg.as_str(),
                        s.settle_ms
                    ],
                );
                // A ledger row already linked elsewhere violates the unique key: report "not linked".
                let changed = match changed {
                    Ok(n) => n,
                    Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::ConstraintViolation => return Ok(false),
                    Err(e) => return Err(e),
                };
                if changed == 0 {
                    return Ok(false);
                }
                let delay = s.first_seen_ms.map(|t| t - s.settle_ms);
                let payload = json!({
                    "pair_id": s.pair_id,
                    "leg": s.leg.as_str(),
                    "exchange": s.exchange.name(),
                    "symbol": s.symbol,
                    "settle_ms": s.settle_ms,
                    "resolution": s.resolution.map(Resolution::as_str),
                    "amount": s.amount.map(|a| a.normalize().to_string()),
                    "ledger_dedupe_key": s.ledger_dedupe_key,
                    "ledger_exchange_id": s.ledger_exchange_id,
                    "zero_rate_source": s.zero_rate_source,
                    "first_seen_ms": s.first_seen_ms,
                    "delay_ms": delay,
                });
                insert_event_on(&tx, ts_ms, PAIR_FUNDING_LINKED, Some(&s.pair_id), &payload)?;
                tx.commit()?;
                Ok(true)
            };
            run(c).map_err(|e| self.event_write_failed(PAIR_FUNDING_LINKED, &e))
        })
    }

    /// "重新查詢": every unsettled slot of the pair becomes due now. State and reason stay as they
    /// are; the next slot tick checks them. Returns how many slots were made due.
    pub fn request_slot_recheck(&self, pair_id: &str, now_ms: i64) -> Result<usize, StoreError> {
        self.with_conn(|c| {
            Ok(c.execute("UPDATE funding_slots SET next_check_ms = ?1, updated_ms = ?1 WHERE pair_id = ?2 AND state != 'SETTLED'", params![now_ms, pair_id])?)
        })
    }

    /// When a stored event was written (the first time the system saw that ledger row).
    pub fn event_ts_ms(&self, event_id: i64) -> Result<Option<i64>, StoreError> {
        self.with_conn(|c| Ok(c.query_row("SELECT ts_ms FROM events WHERE id = ?1", [event_id], |r| r.get(0)).optional()?))
    }

    /// One slot, if it exists.
    pub fn get_slot(&self, pair_id: &str, leg: SlotLeg, settle_ms: i64) -> Result<Option<SlotRow>, StoreError> {
        self.with_conn(|c| {
            Ok(c.query_row(&format!("SELECT {COLUMNS} FROM funding_slots WHERE pair_id = ?1 AND leg = ?2 AND settle_ms = ?3"), params![pair_id, leg.as_str(), settle_ms], read).optional()?)
        })
    }
}

#[cfg(test)]
#[path = "funding_slots_tests.rs"]
mod tests;

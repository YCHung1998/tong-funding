//! Durable state on top of [`Db`]: config (optimistic versions), flags and the kill switch,
//! `pairs` (atomic add), `order_intents` and the 7-day asset history.
//! Every read that feeds a safety decision fails closed: an error halts the store.

#![allow(dead_code)]

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use tong_funding_core::pair::PairState;

use super::db::{Db, HaltReason, StoreError};
use super::events::insert_event_on;
use super::secrets::safe_event_payload;
use tong_funding_core::redact::redact_secrets;

pub const FLAG_KILL_SWITCH: &str = "kill_switch";
pub const FLAG_TRIGGER_MODE: &str = "trigger_mode";
pub const FLAG_EXECUTION_MODE: &str = "execution_mode";
const KILL_ON: &str = "ON";
const KILL_OFF: &str = "OFF";

/// Reject (never silently rewrite) a JSON value that redaction would change.
fn reject_secret_json(field: &str, v: &Value) -> Result<(), StoreError> {
    if safe_event_payload(v.clone()) != *v {
        return Err(StoreError::SecretInValue { field: field.to_string() });
    }
    Ok(())
}

/// Asset history is kept for the last 7 days.
pub const PORTFOLIO_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigEntry {
    pub value: Value,
    pub version: i64,
}

/// One write inside [`Db::config_set_many`]. `expected_version: None` means "key must not exist yet".
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigChange {
    pub key: String,
    pub value: Value,
    pub expected_version: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewPair {
    pub internal_uuid: String,
    pub pair_id: String,
    pub symbol: String,
    pub status: PairState,
    pub entry: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddPairOutcome {
    Added,
    /// The symbol already has a PREPARED pair; nothing was written.
    AlreadyPending,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PairRow {
    pub internal_uuid: String,
    pub pair_id: String,
    pub symbol: String,
    pub status: String,
    pub entry: Value,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Intended,
    Submitted,
    Acknowledged,
    Filled,
    Cancelled,
    Failed,
}

impl IntentState {
    pub const fn as_str(self) -> &'static str {
        match self {
            IntentState::Intended => "INTENDED",
            IntentState::Submitted => "SUBMITTED",
            IntentState::Acknowledged => "ACKNOWLEDGED",
            IntentState::Filled => "FILLED",
            IntentState::Cancelled => "CANCELLED",
            IntentState::Failed => "FAILED",
        }
    }
    pub fn parse(s: &str) -> Option<IntentState> {
        [
            IntentState::Intended,
            IntentState::Submitted,
            IntentState::Acknowledged,
            IntentState::Filled,
            IntentState::Cancelled,
            IntentState::Failed,
        ]
        .into_iter()
        .find(|st| st.as_str() == s)
    }

    /// The order lifecycle: `Intended -> Submitted | Failed | Cancelled`;
    /// `Submitted -> Acknowledged | Filled | Cancelled | Failed`;
    /// `Acknowledged -> Filled | Cancelled | Failed`; terminal states never move again.
    /// (Staying in the same state is handled by the caller as an idempotent no-op.)
    pub const fn can_transition_to(self, to: IntentState) -> bool {
        use IntentState::*;
        matches!(
            (self, to),
            (Intended, Submitted | Failed | Cancelled)
                | (Submitted, Acknowledged | Filled | Cancelled | Failed)
                | (Acknowledged, Filled | Cancelled | Failed)
        )
    }

    /// Not yet in a terminal state: the engine must reconcile it with the exchange after a restart.
    pub const fn is_unfinished(self) -> bool {
        matches!(self, IntentState::Intended | IntentState::Submitted | IntentState::Acknowledged)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIntent {
    pub client_order_id: String,
    pub pair_uuid: String,
    pub leg: String,
    pub exchange: String,
    pub symbol: String,
    pub side: String,
    /// Decimal string; money and quantities are never stored as floats.
    pub quantity: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRow {
    pub client_order_id: String,
    pub pair_uuid: String,
    pub leg: String,
    pub exchange: String,
    pub symbol: String,
    pub side: String,
    pub quantity: String,
    pub state: String,
    pub exchange_order_id: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioPoint {
    pub ts_ms: i64,
    pub exchange: String,
    pub total_usdt: String,
}

impl Db {
    // ---- config -----------------------------------------------------------------------

    /// `Ok(None)` = key never written. A read failure halts the store.
    pub fn config_get(&self, key: &str) -> Result<Option<ConfigEntry>, StoreError> {
        let res = self.with_conn(|c| {
            let row: Option<(String, i64)> = c
                .query_row("SELECT value_json, version FROM config WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            Ok(match row {
                Some((json, version)) => Some(ConfigEntry { value: serde_json::from_str(&json)?, version }),
                None => None,
            })
        });
        self.fail_closed(res, HaltReason::ConfigReadFailed)
    }

    /// Write one key under optimistic locking; returns the new version.
    pub fn config_set(&self, key: &str, value: &Value, expected_version: Option<i64>) -> Result<i64, StoreError> {
        let change = ConfigChange { key: key.to_string(), value: value.clone(), expected_version };
        Ok(self.config_set_many(&[change])?.remove(0))
    }

    /// All-or-nothing: if any change conflicts, none is applied. Returns the new versions in order.
    pub fn config_set_many(&self, changes: &[ConfigChange]) -> Result<Vec<i64>, StoreError> {
        for ch in changes {
            reject_secret_json(&ch.key, &ch.value)?;
        }
        self.with_conn(|c| {
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut versions = Vec::with_capacity(changes.len());
            for ch in changes {
                let actual: Option<i64> =
                    tx.query_row("SELECT version FROM config WHERE key = ?1", [&ch.key], |r| r.get(0)).optional()?;
                if actual != ch.expected_version {
                    return Err(StoreError::VersionConflict { key: ch.key.clone(), expected: ch.expected_version, actual });
                }
                let next = actual.unwrap_or(0) + 1;
                tx.execute(
                    "INSERT INTO config (key, value_json, version, updated_ms) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, version = excluded.version, updated_ms = excluded.updated_ms",
                    params![ch.key, ch.value.to_string(), next, now],
                )?;
                versions.push(next);
            }
            tx.commit()?;
            Ok(versions)
        })
    }

    // ---- flags & kill switch ----------------------------------------------------------

    pub fn flag_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let res = self.with_conn(|c| {
            Ok(c.query_row("SELECT value FROM system_flags WHERE key = ?1", [key], |r| r.get::<_, String>(0)).optional()?)
        });
        self.fail_closed(res, HaltReason::ConfigReadFailed)
    }

    /// Set a flag. The `kill_switch` key is reserved: it can only change through
    /// [`Db::set_kill_switch`], which also writes the audit event.
    pub fn flag_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        if key.eq_ignore_ascii_case(FLAG_KILL_SWITCH) {
            return Err(StoreError::ReservedFlag(key.to_string()));
        }
        if redact_secrets(value) != value {
            return Err(StoreError::SecretInValue { field: format!("system_flags.{key}") });
        }
        self.with_conn(|c| {
            upsert_flag(c, key, value, self.now_ms())?;
            Ok(())
        })
    }

    /// `true` = trading must stay stopped. Fails closed: a halted store, a read error or an
    /// unrecognised stored value all count as "halted".
    pub fn kill_switch_halted(&self) -> bool {
        let res = self.with_conn(|c| {
            Ok(c.query_row("SELECT value FROM system_flags WHERE key = ?1", [FLAG_KILL_SWITCH], |r| r.get::<_, String>(0)).optional()?)
        });
        match self.fail_closed(res, HaltReason::KillSwitchReadFailed) {
            // The row is seeded by the schema; if it is gone the data was tampered with: stay stopped.
            Ok(None) => {
                self.halt(HaltReason::KillSwitchReadFailed("kill_switch row is missing".into()));
                true
            }
            Ok(Some(v)) if v == KILL_OFF => false,
            Ok(Some(v)) if v == KILL_ON => true,
            Ok(Some(other)) => {
                self.halt(HaltReason::KillSwitchReadFailed(format!("unrecognised kill switch value {other:?}")));
                true
            }
            Err(_) => true,
        }
    }

    /// Persist the kill switch and write a `KILL_SWITCH_CHANGED` event in the same transaction.
    pub fn set_kill_switch(&self, on: bool) -> Result<(), StoreError> {
        self.with_conn(|c| {
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            upsert_flag(&tx, FLAG_KILL_SWITCH, if on { KILL_ON } else { KILL_OFF }, now)?;
            insert_event_on(&tx, now, "KILL_SWITCH_CHANGED", None, &serde_json::json!({ "on": on }))
                .map_err(|e| self.event_write_failed("KILL_SWITCH_CHANGED", &e))?;
            tx.commit()?;
            Ok(())
        })
    }

    /// Explicit recovery for the one halt that has a way out: the `kill_switch` row is missing (or
    /// unreadable). Writes the row as `on`, records `KILL_SWITCH_ROW_RESTORED` in the same
    /// transaction and lifts the halt. Refused for every other halt reason and when not halted.
    /// This is a deliberate user action; never call it automatically.
    pub fn restore_kill_switch_row(&self, on: bool) -> Result<(), StoreError> {
        // Hold the connection lock for the whole check-and-restore so no writer interleaves.
        self.with_conn_even_if_halted(|c| {
            match self.halt_reason() {
                Some(HaltReason::KillSwitchReadFailed(_)) => {}
                Some(other) => return Err(StoreError::RestoreNotApplicable(format!("store is halted for another reason: {other}"))),
                None => return Err(StoreError::RestoreNotApplicable("store is not halted".into())),
            }
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            upsert_flag(&tx, FLAG_KILL_SWITCH, if on { KILL_ON } else { KILL_OFF }, now)?;
            insert_event_on(&tx, now, "KILL_SWITCH_ROW_RESTORED", None, &serde_json::json!({ "on": on }))
                .map_err(|e| self.event_write_failed("KILL_SWITCH_ROW_RESTORED", &e))?;
            tx.commit()?;
            self.clear_halt();
            Ok(())
        })
    }

    // ---- pairs ------------------------------------------------------------------------

    /// Atomic: relies only on the `uniq_prepared_symbol` index (no check-then-insert).
    pub fn add_pair_if_not_pending(&self, pair: &NewPair) -> Result<AddPairOutcome, StoreError> {
        reject_secret_json("pairs.entry_json", &pair.entry)?;
        self.with_conn(|c| {
            let now = self.now_ms();
            let n = c.execute(
                "INSERT INTO pairs (internal_uuid, pair_id, symbol, status, entry_json, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT (symbol) WHERE status = 'PREPARED' DO NOTHING",
                params![pair.internal_uuid, pair.pair_id, pair.symbol, pair.status.as_str(), pair.entry.to_string(), now],
            )?;
            Ok(if n == 1 { AddPairOutcome::Added } else { AddPairOutcome::AlreadyPending })
        })
    }

    /// [`Db::add_pair_if_not_pending`] plus one event, in ONE transaction: either the pair and
    /// its event both exist, or neither does. `AlreadyPending` writes nothing. A failed event
    /// write halts the store (like every event write).
    pub fn add_pair_with_event(
        &self,
        pair: &NewPair,
        event_type: &str,
        payload: &Value,
    ) -> Result<AddPairOutcome, StoreError> {
        reject_secret_json("pairs.entry_json", &pair.entry)?;
        self.with_conn(|c| {
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let n = tx.execute(
                "INSERT INTO pairs (internal_uuid, pair_id, symbol, status, entry_json, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT (symbol) WHERE status = 'PREPARED' DO NOTHING",
                params![pair.internal_uuid, pair.pair_id, pair.symbol, pair.status.as_str(), pair.entry.to_string(), now],
            )?;
            if n != 1 {
                return Ok(AddPairOutcome::AlreadyPending); // dropping `tx` rolls back (nothing written)
            }
            insert_event_on(&tx, now, event_type, Some(&pair.internal_uuid), payload)
                .map_err(|e| self.event_write_failed(event_type, &e))?;
            tx.commit()?;
            Ok(AddPairOutcome::Added)
        })
    }

    /// `Ok(false)` when no such pair.
    pub fn set_pair_status(&self, internal_uuid: &str, status: PairState) -> Result<bool, StoreError> {
        self.with_conn(|c| {
            let n = c.execute(
                "UPDATE pairs SET status = ?2, updated_ms = ?3 WHERE internal_uuid = ?1",
                params![internal_uuid, status.as_str(), self.now_ms()],
            )?;
            Ok(n == 1)
        })
    }

    pub fn get_pair(&self, internal_uuid: &str) -> Result<Option<PairRow>, StoreError> {
        self.with_conn(|c| {
            let row = c
                .query_row(
                    "SELECT internal_uuid, pair_id, symbol, status, entry_json, created_ms, updated_ms FROM pairs WHERE internal_uuid = ?1",
                    [internal_uuid],
                    |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, String>(4)?, r.get(5)?, r.get(6)?))
                    },
                )
                .optional()?;
            row.map(|(internal_uuid, pair_id, symbol, status, entry, created_ms, updated_ms)| {
                Ok(PairRow { internal_uuid, pair_id, symbol, status, entry: serde_json::from_str(&entry)?, created_ms, updated_ms })
            })
            .transpose()
        })
    }

    /// Land one pair state change: `status` moves `expected_from -> to` and one immutable event is
    /// written, in ONE transaction. `Ok(false)` (nothing written) when the row is missing or not in
    /// `expected_from`. Any other failure halts the store: a transition that did not land must
    /// never be acted on (engine "land then act").
    pub fn transition_pair(
        &self,
        internal_uuid: &str,
        expected_from: PairState,
        to: PairState,
        event_type: &str,
        payload: &Value,
    ) -> Result<bool, StoreError> {
        let res = self.with_conn(|c| {
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let n = tx.execute(
                "UPDATE pairs SET status = ?3, updated_ms = ?4 WHERE internal_uuid = ?1 AND status = ?2",
                params![internal_uuid, expected_from.as_str(), to.as_str(), now],
            )?;
            if n == 0 {
                return Ok(false); // dropping `tx` rolls back (nothing was written anyway)
            }
            insert_event_on(&tx, now, event_type, Some(internal_uuid), payload)?;
            tx.commit()?;
            Ok(true)
        });
        match res {
            Err(StoreError::Halted(r)) => Err(StoreError::Halted(r)),
            Err(e) => Err(self.event_write_failed(event_type, &e)),
            Ok(v) => Ok(v),
        }
    }

    /// Every pair, oldest first. A read failure halts the store (it feeds the open-pair count).
    pub fn list_pairs(&self) -> Result<Vec<PairRow>, StoreError> {
        let res = self.with_conn(|c| {
            let mut st = c.prepare(
                "SELECT internal_uuid, pair_id, symbol, status, entry_json, created_ms, updated_ms FROM pairs ORDER BY created_ms, rowid",
            )?;
            let rows = st.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, String>(4)?, r.get(5)?, r.get(6)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (internal_uuid, pair_id, symbol, status, entry, created_ms, updated_ms) = row?;
                out.push(PairRow { internal_uuid, pair_id, symbol, status, entry: serde_json::from_str(&entry)?, created_ms, updated_ms });
            }
            Ok(out)
        });
        self.fail_closed(res, HaltReason::ConfigReadFailed)
    }

    /// Set a non-reserved flag (e.g. `trigger_mode`) and write `event_type` in the same
    /// transaction. A failed event write halts the store and leaves the flag unchanged.
    pub fn set_flag_with_event(&self, key: &str, value: &str, event_type: &str, payload: &Value) -> Result<(), StoreError> {
        if key.eq_ignore_ascii_case(FLAG_KILL_SWITCH) {
            return Err(StoreError::ReservedFlag(key.to_string()));
        }
        if redact_secrets(value) != value {
            return Err(StoreError::SecretInValue { field: format!("system_flags.{key}") });
        }
        self.with_conn(|c| {
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            upsert_flag(&tx, key, value, now)?;
            insert_event_on(&tx, now, event_type, None, payload).map_err(|e| self.event_write_failed(event_type, &e))?;
            tx.commit()?;
            Ok(())
        })
    }

    // ---- order intents ----------------------------------------------------------------

    /// Persist an `INTENDED` order before it is sent. A reused `client_order_id` is rejected.
    pub fn create_intent(&self, intent: &NewIntent) -> Result<(), StoreError> {
        self.with_conn(|c| {
            let now = self.now_ms();
            let r = c.execute(
                "INSERT INTO order_intents (client_order_id, pair_uuid, leg, exchange, symbol, side, quantity, state, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    intent.client_order_id,
                    intent.pair_uuid,
                    intent.leg,
                    intent.exchange,
                    intent.symbol,
                    intent.side,
                    intent.quantity,
                    IntentState::Intended.as_str(),
                    now
                ],
            );
            match r {
                Ok(_) => Ok(()),
                Err(rusqlite::Error::SqliteFailure(f, _))
                    if f.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
                {
                    Err(StoreError::DuplicateClientOrderId(intent.client_order_id.clone()))
                }
                Err(e) => Err(e.into()),
            }
        })
    }

    /// Change the state (and optionally record the exchange order id) and write an
    /// `ORDER_INTENT_STATE` event in the same transaction.
    pub fn update_intent_state(
        &self,
        client_order_id: &str,
        state: IntentState,
        exchange_order_id: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_conn(|c| {
            let now = self.now_ms();
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let prev: Option<(String, String, Option<String>)> = tx
                .query_row(
                    "SELECT state, pair_uuid, exchange_order_id FROM order_intents WHERE client_order_id = ?1",
                    [client_order_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((from, pair_uuid, current_order_id)) = prev else {
                return Err(StoreError::IntentNotFound(client_order_id.to_string()));
            };
            // An exchange order id is an opaque id, but it comes from the network: keep it out of
            // both the row and the event if it ever carries something secret-looking.
            let exchange_order_id = exchange_order_id.map(redact_secrets);
            let exchange_order_id = exchange_order_id.as_deref();
            let from_state = IntentState::parse(&from);
            let backfill = from_state == Some(state);
            if backfill {
                // Same state again is idempotent, except that a missing exchange order id may be
                // filled in (once); that is recorded as an event.
                if !(current_order_id.is_none() && exchange_order_id.is_some()) {
                    return Ok(());
                }
            } else if !from_state.is_some_and(|f| f.can_transition_to(state)) {
                return Err(StoreError::IllegalIntentTransition { from, to: state.as_str().to_string() });
            }
            tx.execute(
                "UPDATE order_intents SET state = ?2, exchange_order_id = COALESCE(?3, exchange_order_id), updated_ms = ?4
                 WHERE client_order_id = ?1",
                params![client_order_id, state.as_str(), exchange_order_id, now],
            )?;
            let payload = serde_json::json!({
                "client_order_id": client_order_id,
                "from": from,
                "to": state.as_str(),
                "exchange_order_id": exchange_order_id,
                "backfill": backfill,
            });
            insert_event_on(&tx, now, "ORDER_INTENT_STATE", Some(&pair_uuid), &payload)
                .map_err(|e| self.event_write_failed("ORDER_INTENT_STATE", &e))?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_intent(&self, client_order_id: &str) -> Result<Option<IntentRow>, StoreError> {
        self.with_conn(|c| {
            Ok(c.query_row(&format!("{INTENT_SELECT} WHERE client_order_id = ?1"), [client_order_id], intent_row).optional()?)
        })
    }

    /// Intents that are not terminal, oldest first, for post-restart reconciliation.
    pub fn list_unfinished_intents(&self) -> Result<Vec<IntentRow>, StoreError> {
        self.with_conn(|c| {
            let sql = format!(
                // Anything that is not a known terminal state counts as unfinished, including values
                // that were tampered with: failing open here would hide a live order.
                "{INTENT_SELECT} WHERE state NOT IN ('{}', '{}', '{}') ORDER BY created_ms, rowid",
                IntentState::Filled.as_str(),
                IntentState::Cancelled.as_str(),
                IntentState::Failed.as_str()
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map([], intent_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Every intent of one pair (any state), oldest first. Restart reconciliation uses it to see
    /// the legs of an in-flight pair, including intents that already reached a terminal state.
    pub fn list_intents_for_pair(&self, pair_uuid: &str) -> Result<Vec<IntentRow>, StoreError> {
        self.with_conn(|c| {
            let mut st = c.prepare(&format!("{INTENT_SELECT} WHERE pair_uuid = ?1 ORDER BY created_ms, rowid"))?;
            let rows = st.query_map([pair_uuid], intent_row)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    // ---- asset history ----------------------------------------------------------------

    /// Record a total at the current (injected) time.
    pub fn record_portfolio(&self, exchange: &str, total_usdt: &str) -> Result<(), StoreError> {
        self.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO portfolio_history (ts_ms, exchange, total_usdt) VALUES (?1, ?2, ?3)",
                params![self.now_ms(), exchange, total_usdt],
            )?;
            Ok(())
        })
    }

    /// Oldest first.
    pub fn portfolio_history(&self) -> Result<Vec<PortfolioPoint>, StoreError> {
        self.with_conn(|c| {
            let mut st = c.prepare("SELECT ts_ms, exchange, total_usdt FROM portfolio_history ORDER BY ts_ms, exchange")?;
            let rows = st.query_map([], |r| Ok(PortfolioPoint { ts_ms: r.get(0)?, exchange: r.get(1)?, total_usdt: r.get(2)? }))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Delete points older than 7 days in a single transaction; returns rows removed.
    /// Never touches `events`.
    pub fn purge_portfolio_history(&self) -> Result<usize, StoreError> {
        self.with_conn(|c| {
            let cutoff = self.now_ms() - PORTFOLIO_RETENTION_MS;
            let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let n = tx.execute("DELETE FROM portfolio_history WHERE ts_ms < ?1", [cutoff])?;
            tx.commit()?;
            Ok(n)
        })
    }

    /// Turn a failed safety-relevant read into a sticky halt (reads of a halted store pass through).
    fn fail_closed<T>(&self, res: Result<T, StoreError>, reason: fn(String) -> HaltReason) -> Result<T, StoreError> {
        match res {
            Err(StoreError::Halted(r)) => Err(StoreError::Halted(r)),
            Err(e) => {
                let r = reason(e.to_string());
                self.halt(r.clone());
                Err(StoreError::Halted(self.halt_reason().unwrap_or(r)))
            }
            Ok(v) => Ok(v),
        }
    }
}

fn upsert_flag(conn: &rusqlite::Connection, key: &str, value: &str, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO system_flags (key, value, updated_ms) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_ms = excluded.updated_ms",
        params![key, value, now],
    )
}

const INTENT_SELECT: &str = "SELECT client_order_id, pair_uuid, leg, exchange, symbol, side, quantity, state, exchange_order_id, created_ms, updated_ms FROM order_intents";

fn intent_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<IntentRow> {
    Ok(IntentRow {
        client_order_id: r.get(0)?,
        pair_uuid: r.get(1)?,
        leg: r.get(2)?,
        exchange: r.get(3)?,
        symbol: r.get(4)?,
        side: r.get(5)?,
        quantity: r.get(6)?,
        state: r.get(7)?,
        exchange_order_id: r.get(8)?,
        created_ms: r.get(9)?,
        updated_ms: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::test_support::*;
    use crate::store::events::EventStore;
    use serde_json::json;
    use std::sync::{Arc, Barrier};

    /// Make every INSERT into `events` fail, like a full disk or a constraint failure would.
    fn break_event_inserts(db: &Db) {
        db.with_raw_conn_for_tests(|c| {
            Ok(c.execute_batch("CREATE TRIGGER inject_fail BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected failure'); END")?)
        })
        .unwrap();
    }

    fn plain_flag(db: &Db, key: &str) -> String {
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        plain.query_row("SELECT value FROM system_flags WHERE key = ?1", [key], |r| r.get(0)).unwrap()
    }

    fn event_count(db: &Db) -> i64 {
        db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)).unwrap()
    }
    fn events_of(db: &Db, ty: &str) -> Vec<(Option<String>, Value)> {
        db.with_conn(|c| {
            let mut st = c.prepare("SELECT pair_id, payload FROM events WHERE event_type = ?1 ORDER BY id")?;
            let rows = st.query_map([ty], |r| {
                let p: String = r.get(1)?;
                Ok((r.get::<_, Option<String>>(0)?, serde_json::from_str::<Value>(&p).unwrap()))
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap()
    }

    // ---- config ----

    #[test]
    fn config_roundtrips_with_versions() {
        let (_d, db, _) = open_tmp();
        assert_eq!(db.config_get("risk").unwrap(), None);
        assert_eq!(db.config_set("risk", &json!({"max_leverage": 3}), None).unwrap(), 1);
        assert_eq!(db.config_set("risk", &json!({"max_leverage": 5}), Some(1)).unwrap(), 2);
        let e = db.config_get("risk").unwrap().unwrap();
        assert_eq!((e.value, e.version), (json!({"max_leverage": 5}), 2));
    }

    #[test]
    fn config_survives_a_restart() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        {
            let (c, _) = clock(1);
            let db = Db::open(&p, c);
            db.config_set("risk", &json!({"max_leverage": 7}), None).unwrap();
            db.config_set("exchange_override:Bybit", &json!({"enabled": false}), None).unwrap();
            db.flag_set(FLAG_TRIGGER_MODE, "manual").unwrap();
            db.flag_set(FLAG_EXECUTION_MODE, "demo").unwrap();
            db.set_kill_switch(true).unwrap();
        }
        let (c, _) = clock(2);
        let db = Db::open(&p, c);
        assert_eq!(db.config_get("risk").unwrap().unwrap().value, json!({"max_leverage": 7}));
        assert_eq!(db.config_get("exchange_override:Bybit").unwrap().unwrap().value, json!({"enabled": false}));
        assert_eq!(db.flag_get(FLAG_TRIGGER_MODE).unwrap().as_deref(), Some("manual"));
        assert_eq!(db.flag_get(FLAG_EXECUTION_MODE).unwrap().as_deref(), Some("demo"));
        assert!(db.kill_switch_halted());
    }

    #[test]
    fn stale_writer_gets_a_version_conflict_and_does_not_overwrite() {
        let (_d, db, _) = open_tmp();
        db.config_set("risk", &json!({"v": "a"}), None).unwrap();
        db.config_set("risk", &json!({"v": "b"}), Some(1)).unwrap();
        let r = db.config_set("risk", &json!({"v": "stale"}), Some(1));
        assert!(matches!(r, Err(StoreError::VersionConflict { expected: Some(1), actual: Some(2), .. })), "{r:?}");
        assert_eq!(db.config_get("risk").unwrap().unwrap().value, json!({"v": "b"}));
    }

    #[test]
    fn creating_an_existing_key_without_a_version_conflicts() {
        let (_d, db, _) = open_tmp();
        db.config_set("risk", &json!(1), None).unwrap();
        let r = db.config_set("risk", &json!(2), None);
        assert!(matches!(r, Err(StoreError::VersionConflict { expected: None, actual: Some(1), .. })), "{r:?}");
    }

    #[test]
    fn failed_multi_key_write_leaves_no_partial_state() {
        let (_d, db, _) = open_tmp();
        db.config_set("a", &json!("old-a"), None).unwrap();
        db.config_set("b", &json!("old-b"), None).unwrap();
        let r = db.config_set_many(&[
            ConfigChange { key: "a".into(), value: json!("new-a"), expected_version: Some(1) },
            ConfigChange { key: "fresh".into(), value: json!("new"), expected_version: None },
            ConfigChange { key: "b".into(), value: json!("new-b"), expected_version: Some(99) }, // conflicts
        ]);
        assert!(r.is_err());
        let a = db.config_get("a").unwrap().unwrap();
        assert_eq!((a.value, a.version), (json!("old-a"), 1));
        assert_eq!(db.config_get("fresh").unwrap(), None);
        assert_eq!(db.config_get("b").unwrap().unwrap().value, json!("old-b"));
    }

    #[test]
    fn config_read_failure_halts_the_store() {
        let (_d, db, _) = open_tmp();
        db.config_set("risk", &json!(1), None).unwrap();
        db.with_conn(|c| Ok(c.execute_batch("DROP TABLE config")?)).unwrap();
        let r = db.config_get("risk");
        assert!(matches!(r, Err(StoreError::Halted(HaltReason::ConfigReadFailed(_)))), "{r:?}");
        assert!(db.is_halted());
        assert!(matches!(db.config_set("x", &json!(1), None), Err(StoreError::Halted(_))));
    }

    #[test]
    fn corrupt_config_json_halts_the_store() {
        let (_d, db, _) = open_tmp();
        // The store's own connection forbids ignore_check_constraints; use a separate plain one.
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        plain
            .execute_batch("PRAGMA ignore_check_constraints = ON; INSERT INTO config VALUES ('bad', '{not json', 1, 1);")
            .unwrap();
        drop(plain);
        assert!(matches!(db.config_get("bad"), Err(StoreError::Halted(HaltReason::ConfigReadFailed(_)))));
        assert!(db.is_halted());
    }

    #[test]
    fn halted_store_refuses_config_writes_and_reads() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let (c, _) = clock(1);
        let db = Db::open(&p, c);
        assert!(matches!(db.config_set("x", &json!(1), None), Err(StoreError::Halted(_))));
        assert!(matches!(db.config_get("x"), Err(StoreError::Halted(_))));
        assert!(matches!(db.flag_set("x", "y"), Err(StoreError::Halted(_))));
        assert!(matches!(db.set_kill_switch(false), Err(StoreError::Halted(_))));
        assert_eq!(std::fs::read(&p).unwrap(), b"garbage garbage garbage".repeat(200), "file untouched");
    }

    // ---- kill switch ----

    #[test]
    fn kill_switch_defaults_to_off_and_toggles() {
        let (_d, db, _) = open_tmp();
        assert!(!db.kill_switch_halted());
        db.set_kill_switch(true).unwrap();
        assert!(db.kill_switch_halted());
        db.set_kill_switch(false).unwrap();
        assert!(!db.kill_switch_halted());
    }

    #[test]
    fn kill_switch_change_writes_an_event_atomically() {
        let (_d, db, _) = open_tmp();
        db.set_kill_switch(true).unwrap();
        let ev = events_of(&db, "KILL_SWITCH_CHANGED");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].1["on"], json!(true));
        // If the event cannot be written, the flag must not change either.
        break_event_inserts(&db);
        assert!(db.set_kill_switch(false).is_err());
        let v: String = plain_flag(&db, "kill_switch");
        assert_eq!(v, "ON", "flag unchanged after the failed transaction");
    }

    #[test]
    fn kill_switch_read_failure_counts_as_halted_and_halts_the_store() {
        let (_d, db, _) = open_tmp();
        assert!(!db.kill_switch_halted());
        db.with_conn(|c| Ok(c.execute_batch("DROP TABLE system_flags")?)).unwrap();
        assert!(db.kill_switch_halted(), "read error must mean halted, not running");
        assert!(matches!(db.halt_reason(), Some(HaltReason::KillSwitchReadFailed(_))));
        assert!(matches!(db.config_set("x", &json!(1), None), Err(StoreError::Halted(_))));
    }

    #[test]
    fn unrecognised_kill_switch_value_counts_as_halted() {
        let (_d, db, _) = open_tmp();
        db.with_conn(|c| Ok(c.execute("UPDATE system_flags SET value = 'maybe' WHERE key = 'kill_switch'", [])?)).unwrap();
        assert!(db.kill_switch_halted());
    }

    #[test]
    fn kill_switch_on_a_halted_store_is_halted() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let (c, _) = clock(1);
        assert!(Db::open(&p, c).kill_switch_halted());
    }

    // ---- pairs ----

    fn pair(uuid: &str, symbol: &str, status: PairState) -> NewPair {
        NewPair { internal_uuid: uuid.into(), pair_id: format!("pid-{uuid}"), symbol: symbol.into(), status, entry: json!({"edge": "0.0012"}) }
    }

    #[test]
    fn second_prepared_pair_for_a_symbol_is_reported_as_already_pending() {
        let (_d, db, _) = open_tmp();
        assert_eq!(db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap(), AddPairOutcome::Added);
        assert_eq!(db.add_pair_if_not_pending(&pair("u2", "BTCUSDT", PairState::Prepared)).unwrap(), AddPairOutcome::AlreadyPending);
        assert_eq!(db.add_pair_if_not_pending(&pair("u3", "ETHUSDT", PairState::Prepared)).unwrap(), AddPairOutcome::Added);
        assert!(db.get_pair("u2").unwrap().is_none(), "losing insert wrote nothing");
        let p = db.get_pair("u1").unwrap().unwrap();
        assert_eq!((p.symbol.as_str(), p.status.as_str(), p.entry), ("BTCUSDT", "PREPARED", json!({"edge": "0.0012"})));
    }

    #[test]
    fn a_new_prepared_pair_is_allowed_once_the_old_one_moved_on() {
        let (_d, db, clock) = open_tmp();
        db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap();
        clock.set(2_000_000);
        assert!(db.set_pair_status("u1", PairState::OrderSubmit).unwrap());
        let p = db.get_pair("u1").unwrap().unwrap();
        assert_eq!((p.status.as_str(), p.updated_ms), ("ORDER_SUBMIT", 2_000_000));
        assert_eq!(db.add_pair_if_not_pending(&pair("u2", "BTCUSDT", PairState::Prepared)).unwrap(), AddPairOutcome::Added);
        assert!(!db.set_pair_status("nope", PairState::Finalized).unwrap());
    }

    #[test]
    fn a_reused_internal_uuid_is_an_error_not_already_pending() {
        let (_d, db, _) = open_tmp();
        db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap();
        let r = db.add_pair_if_not_pending(&pair("u1", "ETHUSDT", PairState::Prepared));
        assert!(r.is_err(), "primary-key violation must surface, got {r:?}");
    }

    #[test]
    fn concurrent_adds_for_the_same_symbol_succeed_exactly_once() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        let (c, _) = clock(1);
        let a = Db::open(&p, c.clone());
        let b = Db::open_unlocked(&p, c); // a second connection: contention is resolved by the index, not by our mutex
        assert!(!a.is_halted() && !b.is_halted());
        for round in 0..25 {
            let symbol = format!("SYM{round}");
            let barrier = Arc::new(Barrier::new(2));
            let spawn = |db: Db, uuid: String, symbol: String, barrier: Arc<Barrier>| {
                std::thread::spawn(move || {
                    barrier.wait();
                    db.add_pair_if_not_pending(&pair(&uuid, &symbol, PairState::Prepared))
                })
            };
            let t1 = spawn(a.clone(), format!("a{round}"), symbol.clone(), barrier.clone());
            let t2 = spawn(b.clone(), format!("b{round}"), symbol.clone(), barrier);
            let outcomes = [t1.join().unwrap().unwrap(), t2.join().unwrap().unwrap()];
            let added = outcomes.iter().filter(|o| **o == AddPairOutcome::Added).count();
            assert_eq!(added, 1, "round {round}: {outcomes:?}");
            assert!(outcomes.contains(&AddPairOutcome::AlreadyPending));
            let n: i64 = a
                .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM pairs WHERE symbol = ?1", [&symbol], |r| r.get(0))?))
                .unwrap();
            assert_eq!(n, 1);
        }
    }

    // ---- order intents ----

    fn intent(id: &str) -> NewIntent {
        NewIntent {
            client_order_id: id.into(),
            pair_uuid: "pair-1".into(),
            leg: "long".into(),
            exchange: "Binance".into(),
            symbol: "BTCUSDT".into(),
            side: "BUY".into(),
            quantity: "0.019".into(),
        }
    }

    #[test]
    fn intent_is_persisted_as_intended_with_exact_quantity() {
        let (_d, db, clock) = open_tmp();
        clock.set(5_000);
        db.create_intent(&intent("c1")).unwrap();
        let row = db.get_intent("c1").unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.quantity.as_str(), row.created_ms, row.exchange_order_id), ("INTENDED", "0.019", 5_000, None));
    }

    #[test]
    fn duplicate_client_order_id_is_rejected_and_creates_no_second_row() {
        let (_d, db, _) = open_tmp();
        db.create_intent(&intent("c1")).unwrap();
        let r = db.create_intent(&intent("c1"));
        assert!(matches!(r, Err(StoreError::DuplicateClientOrderId(ref id)) if id == "c1"), "{r:?}");
        db.create_intent(&intent("c2")).unwrap();
        let n: i64 = db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM order_intents", [], |r| r.get(0))?)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn state_update_writes_an_immutable_event_in_the_same_transaction() {
        let (_d, db, clock) = open_tmp();
        db.create_intent(&intent("c1")).unwrap();
        clock.set(9_000);
        db.update_intent_state("c1", IntentState::Submitted, Some("EX-77")).unwrap();
        let row = db.get_intent("c1").unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.exchange_order_id.as_deref(), row.updated_ms), ("SUBMITTED", Some("EX-77"), 9_000));
        let ev = events_of(&db, "ORDER_INTENT_STATE");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0.as_deref(), Some("pair-1"));
        assert_eq!(ev[0].1["client_order_id"], json!("c1"));
        assert_eq!((ev[0].1["from"].clone(), ev[0].1["to"].clone()), (json!("INTENDED"), json!("SUBMITTED")));
        // the event is immutable
        let es = EventStore::new(db.clone());
        assert!(es.db().with_conn(|c| Ok(c.execute("DELETE FROM events", [])?)).is_err());
    }

    #[test]
    fn if_the_event_cannot_be_written_the_state_does_not_change() {
        let (_d, db, _) = open_tmp();
        db.create_intent(&intent("c1")).unwrap();
        break_event_inserts(&db);
        assert!(db.update_intent_state("c1", IntentState::Submitted, None).is_err());
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        let st: String = plain.query_row("SELECT state FROM order_intents WHERE client_order_id='c1'", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "INTENDED");
    }

    #[test]
    fn updating_an_unknown_intent_is_an_error_and_writes_no_event() {
        let (_d, db, _) = open_tmp();
        let r = db.update_intent_state("ghost", IntentState::Filled, None);
        assert!(matches!(r, Err(StoreError::IntentNotFound(ref id)) if id == "ghost"), "{r:?}");
        assert_eq!(event_count(&db), 0);
    }

    #[test]
    fn unfinished_intents_are_listed_after_a_restart() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        {
            let (c, clock) = clock(100);
            let db = Db::open(&p, c);
            db.create_intent(&intent("intended")).unwrap();
            clock.set(200);
            db.create_intent(&intent("submitted")).unwrap();
            db.update_intent_state("submitted", IntentState::Submitted, Some("E1")).unwrap();
            db.create_intent(&intent("acked")).unwrap();
            db.update_intent_state("acked", IntentState::Submitted, Some("E2")).unwrap();
            db.update_intent_state("acked", IntentState::Acknowledged, None).unwrap();
            db.create_intent(&intent("filled")).unwrap();
            db.update_intent_state("filled", IntentState::Submitted, Some("E3")).unwrap();
            db.update_intent_state("filled", IntentState::Filled, None).unwrap();
            db.create_intent(&intent("cancelled")).unwrap();
            db.update_intent_state("cancelled", IntentState::Cancelled, None).unwrap();
            db.create_intent(&intent("failed")).unwrap();
            db.update_intent_state("failed", IntentState::Failed, None).unwrap();
        } // "process killed"
        let (c, _) = clock(300);
        let db = Db::open(&p, c);
        let ids: Vec<String> = db.list_unfinished_intents().unwrap().into_iter().map(|r| r.client_order_id).collect();
        assert_eq!(ids, vec!["intended", "submitted", "acked"]);
    }

    #[test]
    fn intents_of_one_pair_are_listed_in_every_state_oldest_first() {
        let (_d, db, clock) = open_tmp();
        clock.set(100);
        intent_in_state(&db, "a-filled", IntentState::Filled);
        clock.set(200);
        db.create_intent(&intent("b-intended")).unwrap();
        db.create_intent(&NewIntent { pair_uuid: "pair-2".into(), ..intent("other-pair") }).unwrap();
        let ids: Vec<String> = db.list_intents_for_pair("pair-1").unwrap().into_iter().map(|r| r.client_order_id).collect();
        assert_eq!(ids, vec!["a-filled", "b-intended"]);
        assert!(db.list_intents_for_pair("nope").unwrap().is_empty());
    }

    // ---- asset history ----

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[test]
    fn purge_removes_points_older_than_7_days_and_keeps_events() {
        let (_d, db, clock) = open_tmp();
        let now = 100 * DAY;
        clock.set(now - 8 * DAY);
        db.record_portfolio("Binance", "1000.5").unwrap();
        clock.set(now - 1 * DAY);
        db.record_portfolio("Binance", "1100.25").unwrap();
        db.with_conn(|c| Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'OLD','{}')", [])?)).unwrap();
        clock.set(now);
        assert_eq!(db.purge_portfolio_history().unwrap(), 1);
        let h = db.portfolio_history().unwrap();
        assert_eq!(h, vec![PortfolioPoint { ts_ms: now - DAY, exchange: "Binance".into(), total_usdt: "1100.25".into() }]);
        assert_eq!(event_count(&db), 1, "events untouched");
    }

    #[test]
    fn purge_keeps_exactly_7_day_old_points_and_drops_older_by_a_millisecond() {
        let (_d, db, clock) = open_tmp();
        let now = 100 * DAY;
        clock.set(now - PORTFOLIO_RETENTION_MS);
        db.record_portfolio("Bybit", "1").unwrap();
        clock.set(now - PORTFOLIO_RETENTION_MS - 1);
        db.record_portfolio("Bybit", "2").unwrap();
        clock.set(now);
        assert_eq!(db.purge_portfolio_history().unwrap(), 1);
        let h = db.portfolio_history().unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].total_usdt, "1");
    }

    #[test]
    fn history_is_oldest_first_and_per_exchange() {
        let (_d, db, clock) = open_tmp();
        clock.set(10);
        db.record_portfolio("Binance", "5").unwrap();
        db.record_portfolio("Bybit", "6").unwrap();
        clock.set(20);
        db.record_portfolio("Binance", "7").unwrap();
        let h = db.portfolio_history().unwrap();
        assert_eq!(h.iter().map(|p| (p.ts_ms, p.exchange.as_str(), p.total_usdt.as_str())).collect::<Vec<_>>(), vec![(10, "Binance", "5"), (10, "Bybit", "6"), (20, "Binance", "7")]);
    }

    #[test]
    fn purge_is_a_single_transaction() {
        let (_d, db, clock) = open_tmp();
        clock.set(0);
        db.record_portfolio("Binance", "1").unwrap();
        db.record_portfolio("Bybit", "2").unwrap();
        // A trigger that fails on the second deleted row proves that a mid-way failure rolls back all of it.
        db.with_conn(|c| {
            c.execute_batch(
                "CREATE TABLE del_count (n INTEGER);
                 CREATE TRIGGER fail_second BEFORE DELETE ON portfolio_history
                 BEGIN INSERT INTO del_count VALUES (1);
                       SELECT RAISE(ABORT, 'boom') WHERE (SELECT COUNT(*) FROM del_count) >= 2; END;",
            )?;
            Ok(())
        })
        .unwrap();
        clock.set(PORTFOLIO_RETENTION_MS + 10);
        assert!(db.purge_portfolio_history().is_err());
        assert_eq!(db.portfolio_history().unwrap().len(), 2, "nothing deleted when the purge fails midway");
    }

    // ---- hardening (review round 1) ----

    #[test]
    fn flag_set_refuses_the_reserved_kill_switch_key() {
        let (_d, db, _) = open_tmp();
        for key in ["kill_switch", "KILL_SWITCH", "Kill_Switch"] {
            let r = db.flag_set(key, "ON");
            assert!(matches!(r, Err(StoreError::ReservedFlag(_))), "{key}: {r:?}");
        }
        assert!(!db.kill_switch_halted(), "kill switch untouched");
        db.flag_set("trigger_mode", "auto").unwrap();
        assert_eq!(db.flag_get("trigger_mode").unwrap().as_deref(), Some("auto"));
    }

    #[test]
    fn a_new_database_has_an_explicit_kill_switch_off_row_and_is_not_halted() {
        let (_d, db, _) = open_tmp();
        assert_eq!(db.flag_get(FLAG_KILL_SWITCH).unwrap().as_deref(), Some("OFF"));
        assert!(!db.kill_switch_halted());
        assert!(!db.is_halted());
    }

    #[test]
    fn a_missing_kill_switch_row_means_halted_fail_closed() {
        let (_d, db, _) = open_tmp();
        db.with_conn(|c| Ok(c.execute("DELETE FROM system_flags WHERE key = 'kill_switch'", [])?)).unwrap();
        assert!(db.kill_switch_halted(), "a missing row means someone touched the data: stay stopped");
    }

    const ALL: [IntentState; 6] = [
        IntentState::Intended,
        IntentState::Submitted,
        IntentState::Acknowledged,
        IntentState::Filled,
        IntentState::Cancelled,
        IntentState::Failed,
    ];

    fn legal(from: IntentState, to: IntentState) -> bool {
        use IntentState::*;
        matches!(
            (from, to),
            (Intended, Submitted | Failed | Cancelled)
                | (Submitted, Acknowledged | Filled | Cancelled | Failed)
                | (Acknowledged, Filled | Cancelled | Failed)
        )
    }

    /// Walk a fresh intent to `target` along legal transitions only.
    fn intent_in_state(db: &Db, id: &str, target: IntentState) {
        use IntentState::*;
        db.create_intent(&intent(id)).unwrap();
        let path: &[IntentState] = match target {
            Intended => &[],
            Submitted => &[Submitted],
            Acknowledged => &[Submitted, Acknowledged],
            Filled => &[Submitted, Filled],
            Cancelled => &[Cancelled],
            Failed => &[Failed],
        };
        for st in path {
            db.update_intent_state(id, *st, None).unwrap();
        }
    }

    #[test]
    fn every_intent_transition_is_checked_against_the_state_machine() {
        let (_d, db, clock) = open_tmp();
        let mut n = 0;
        for from in ALL {
            for to in ALL {
                n += 1;
                let id = format!("c{n}");
                intent_in_state(&db, &id, from);
                clock.set(1_000_000 + n);
                let events_before = event_count(&db);
                let updated_before = db.get_intent(&id).unwrap().unwrap().updated_ms;
                let r = db.update_intent_state(&id, to, None);
                let row = db.get_intent(&id).unwrap().unwrap();
                if from == to {
                    r.unwrap_or_else(|e| panic!("{from:?}->{to:?} should be an idempotent no-op: {e}"));
                    assert_eq!(row.state, from.as_str());
                    assert_eq!(event_count(&db), events_before, "{from:?}->{to:?}: no-op writes no event");
                    assert_eq!(row.updated_ms, updated_before, "{from:?}->{to:?}: no-op touches nothing");
                } else if legal(from, to) {
                    r.unwrap_or_else(|e| panic!("{from:?}->{to:?} should be legal: {e}"));
                    assert_eq!(row.state, to.as_str());
                    assert_eq!(event_count(&db), events_before + 1);
                } else {
                    match r {
                        Err(StoreError::IllegalIntentTransition { from: f, to: t }) => {
                            assert_eq!((f.as_str(), t.as_str()), (from.as_str(), to.as_str()));
                        }
                        other => panic!("{from:?}->{to:?} must be IllegalIntentTransition, got {other:?}"),
                    }
                    assert_eq!(row.state, from.as_str(), "{from:?}->{to:?}: state unchanged");
                    assert_eq!(event_count(&db), events_before, "{from:?}->{to:?}: no event");
                }
            }
        }
    }

    #[test]
    fn a_filled_intent_cannot_come_back_to_the_unfinished_list() {
        let (_d, db, _) = open_tmp();
        intent_in_state(&db, "c1", IntentState::Filled);
        assert!(db.update_intent_state("c1", IntentState::Intended, None).is_err());
        assert!(db.list_unfinished_intents().unwrap().is_empty());
    }

    #[test]
    fn a_tampered_unknown_stored_state_is_an_illegal_transition_not_a_panic() {
        let (_d, db, _) = open_tmp();
        db.create_intent(&intent("c1")).unwrap();
        db.with_conn(|c| Ok(c.execute("UPDATE order_intents SET state = 'WEIRD'", [])?)).unwrap();
        let r = db.update_intent_state("c1", IntentState::Filled, None);
        assert!(matches!(r, Err(StoreError::IllegalIntentTransition { .. })), "{r:?}");
    }

    #[test]
    fn state_and_kill_switch_events_and_rows_never_contain_raw_secrets() {
        let (_d, db, _) = open_tmp();
        db.create_intent(&intent("c1")).unwrap();
        db.update_intent_state("c1", IntentState::Submitted, Some("EX-1 signature=RAWSIG")).unwrap();
        db.set_kill_switch(true).unwrap();
        let dump = crate::store::db::test_support::dump_db(&db);
        assert!(!dump.contains("RAWSIG"), "{dump}");
        assert!(!crate::store::db::test_support::files_contain(db.path(), "RAWSIG"));
    }

    // ---- hardening round 2 ----

    #[test]
    fn a_failed_event_write_in_update_intent_state_halts_and_rolls_back() {
        let (_d, db, _) = open_tmp();
        db.create_intent(&intent("c1")).unwrap();
        break_event_inserts(&db);
        assert!(db.update_intent_state("c1", IntentState::Submitted, None).is_err());
        assert!(matches!(db.halt_reason(), Some(HaltReason::EventWriteFailed(_))), "{:?}", db.halt_reason());
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        let st: String = plain.query_row("SELECT state FROM order_intents", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "INTENDED", "transaction rolled back");
    }

    #[test]
    fn a_failed_event_write_in_set_kill_switch_halts() {
        let (_d, db, _) = open_tmp();
        break_event_inserts(&db);
        assert!(db.set_kill_switch(true).is_err());
        assert!(matches!(db.halt_reason(), Some(HaltReason::EventWriteFailed(_))), "{:?}", db.halt_reason());
        assert_eq!(plain_flag(&db, "kill_switch"), "OFF", "rolled back");
    }

    fn delete_kill_switch_row(db: &Db) {
        db.with_conn(|c| Ok(c.execute("DELETE FROM system_flags WHERE key = 'kill_switch'", [])?)).unwrap();
    }

    #[test]
    fn restore_kill_switch_row_recovers_from_a_missing_row_and_records_an_event() {
        for on in [false, true] {
            let (_d, db, _) = open_tmp();
            delete_kill_switch_row(&db);
            assert!(db.kill_switch_halted());
            assert!(db.is_halted());
            db.restore_kill_switch_row(on).unwrap();
            assert!(!db.is_halted(), "{:?}", db.halt_reason());
            assert_eq!(db.kill_switch_halted(), on);
            assert_eq!(events_of(&db, "KILL_SWITCH_ROW_RESTORED").len(), 1);
            assert_eq!(events_of(&db, "KILL_SWITCH_ROW_RESTORED")[0].1["on"], json!(on));
        }
    }

    #[test]
    fn restore_kill_switch_row_is_refused_for_every_other_halt_reason_and_when_not_halted() {
        // not halted
        let (_d, db, _) = open_tmp();
        assert!(matches!(db.restore_kill_switch_row(false), Err(StoreError::RestoreNotApplicable(_))));
        assert_eq!(events_of(&db, "KILL_SWITCH_ROW_RESTORED").len(), 0);
        // halted for another reason
        let (_d2, db2, _) = open_tmp();
        db2.halt(HaltReason::EventWriteFailed("x".into()));
        assert!(matches!(db2.restore_kill_switch_row(false), Err(StoreError::RestoreNotApplicable(_))));
        assert!(matches!(db2.halt_reason(), Some(HaltReason::EventWriteFailed(_))), "still halted");
        // a database that never opened
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let (c, _) = clock(1);
        let db3 = Db::open(&p, c);
        assert!(db3.restore_kill_switch_row(false).is_err());
        assert!(db3.is_halted());
    }

    #[test]
    fn restore_failing_to_write_leaves_the_store_halted() {
        let (_d, db, _) = open_tmp();
        delete_kill_switch_row(&db);
        assert!(db.kill_switch_halted());
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        plain.execute_batch("CREATE TRIGGER inject BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'no'); END").unwrap();
        drop(plain);
        assert!(db.restore_kill_switch_row(false).is_err());
        assert!(db.is_halted());
    }

    #[test]
    fn an_unknown_stored_state_is_listed_as_unfinished() {
        let (_d, db, _) = open_tmp();
        intent_in_state(&db, "done", IntentState::Filled);
        db.create_intent(&intent("odd")).unwrap();
        db.with_conn(|c| Ok(c.execute("UPDATE order_intents SET state = 'filled' WHERE client_order_id = 'odd'", [])?)).unwrap();
        db.create_intent(&intent("odd2")).unwrap();
        db.with_conn(|c| Ok(c.execute("UPDATE order_intents SET state = '' WHERE client_order_id = 'odd2'", [])?)).unwrap();
        let ids: Vec<String> = db.list_unfinished_intents().unwrap().into_iter().map(|r| r.client_order_id).collect();
        assert_eq!(ids, vec!["odd", "odd2"], "tampered states must not disappear from reconciliation");
    }

    #[test]
    fn a_same_state_update_backfills_a_missing_exchange_order_id_with_an_event() {
        let (_d, db, clock) = open_tmp();
        intent_in_state(&db, "c1", IntentState::Submitted);
        assert_eq!(db.get_intent("c1").unwrap().unwrap().exchange_order_id, None);
        let before = event_count(&db);
        clock.set(7_777_777);
        db.update_intent_state("c1", IntentState::Submitted, Some("EX-9")).unwrap();
        let row = db.get_intent("c1").unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.exchange_order_id.as_deref(), row.updated_ms), ("SUBMITTED", Some("EX-9"), 7_777_777));
        assert_eq!(event_count(&db), before + 1, "the backfill is recorded");
        // Once set, it is not overwritten by a later same-state call, and no event is written.
        db.update_intent_state("c1", IntentState::Submitted, Some("EX-OTHER")).unwrap();
        assert_eq!(db.get_intent("c1").unwrap().unwrap().exchange_order_id.as_deref(), Some("EX-9"));
        assert_eq!(event_count(&db), before + 1);
    }

    #[test]
    fn secrets_in_config_values_are_rejected_not_silently_rewritten() {
        let (_d, db, _) = open_tmp();
        for (key, v) in [
            ("a", json!({"api_key": "abc123"})),
            ("b", json!({"note": "call with signature=RAWSIG"})),
            ("c", json!(["x", {"nested": {"apiSecret": "zzz"}}])),
        ] {
            let r = db.config_set(key, &v, None);
            assert!(matches!(&r, Err(StoreError::SecretInValue { field }) if field == key), "{key}: {r:?}");
            assert_eq!(db.config_get(key).unwrap(), None, "nothing written");
        }
        // all-or-nothing in a batch
        let r = db.config_set_many(&[
            ConfigChange { key: "ok".into(), value: json!(1), expected_version: None },
            ConfigChange { key: "bad".into(), value: json!({"api_key": "x"}), expected_version: None },
        ]);
        assert!(matches!(r, Err(StoreError::SecretInValue { .. })));
        assert_eq!(db.config_get("ok").unwrap(), None);
    }

    #[test]
    fn the_full_default_risk_config_is_not_mistaken_for_a_secret() {
        let (_d, db, _) = open_tmp();
        let v = serde_json::to_value(tong_funding_core::risk::RiskConfig::default()).unwrap();
        db.config_set("risk", &v, None).unwrap();
        assert_eq!(db.config_get("risk").unwrap().unwrap().value, v);
    }

    #[test]
    fn secret_looking_flag_values_and_pair_entries_are_rejected() {
        let (_d, db, _) = open_tmp();
        let r = db.flag_set("note", "api_key=abc123");
        assert!(matches!(&r, Err(StoreError::SecretInValue { field }) if field.contains("note")), "{r:?}");
        assert_eq!(db.flag_get("note").unwrap(), None);
        db.flag_set("trigger_mode", "auto").unwrap();
        let mut p = pair("u1", "BTCUSDT", PairState::Prepared);
        p.entry = json!({"edge": "0.001", "api_key": "abc"});
        let r = db.add_pair_if_not_pending(&p);
        assert!(matches!(&r, Err(StoreError::SecretInValue { field }) if field.contains("entry")), "{r:?}");
        assert!(db.get_pair("u1").unwrap().is_none());
        p.entry = json!({"edge": "0.001"});
        assert_eq!(db.add_pair_if_not_pending(&p).unwrap(), AddPairOutcome::Added);
    }

    // ---- engine additions: land-then-act transition, pair listing, flag + event ----

    #[test]
    fn transition_pair_moves_status_and_writes_one_event_atomically() {
        let (_d, db, clock) = open_tmp();
        db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap();
        clock.set(2_000_000);
        let before = event_count(&db);
        assert!(db.transition_pair("u1", PairState::Prepared, PairState::Cancelled, "PAIR_TRANSITION", &json!({"why": "x"})).unwrap());
        let p = db.get_pair("u1").unwrap().unwrap();
        assert_eq!((p.status.as_str(), p.updated_ms), ("CANCELLED", 2_000_000));
        assert_eq!(event_count(&db), before + 1);
        assert_eq!(events_of(&db, "PAIR_TRANSITION"), vec![(Some("u1".to_string()), json!({"why": "x"}))]);
    }

    #[test]
    fn transition_pair_from_the_wrong_state_or_unknown_pair_writes_nothing() {
        let (_d, db, _) = open_tmp();
        db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap();
        let before = event_count(&db);
        assert!(!db.transition_pair("u1", PairState::Reconciled, PairState::Closing, "PAIR_TRANSITION", &json!({})).unwrap());
        assert!(!db.transition_pair("nope", PairState::Prepared, PairState::Cancelled, "PAIR_TRANSITION", &json!({})).unwrap());
        assert_eq!(db.get_pair("u1").unwrap().unwrap().status, "PREPARED");
        assert_eq!(event_count(&db), before);
        assert!(!db.is_halted());
    }

    #[test]
    fn transition_pair_event_failure_rolls_back_the_status_and_halts() {
        let (_d, db, _) = open_tmp();
        db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap();
        break_event_inserts(&db);
        let r = db.transition_pair("u1", PairState::Prepared, PairState::Cancelled, "PAIR_TRANSITION", &json!({}));
        assert!(matches!(r, Err(StoreError::Halted(HaltReason::EventWriteFailed(_)))), "{r:?}");
        assert!(db.is_halted());
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        let status: String = plain.query_row("SELECT status FROM pairs WHERE internal_uuid = 'u1'", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "PREPARED", "status must roll back with the event");
    }

    #[test]
    fn add_pair_with_event_writes_both_or_neither() {
        let (_d, db, clock) = open_tmp();
        clock.set(3_000_000);
        let ok = db.add_pair_with_event(&pair("u1", "BTCUSDT", PairState::Prepared), "PAIR_PREPARED", &json!({"s": "BTCUSDT"}));
        assert_eq!(ok.unwrap(), AddPairOutcome::Added);
        assert_eq!(events_of(&db, "PAIR_PREPARED"), vec![(Some("u1".to_string()), json!({"s": "BTCUSDT"}))]);
        // Same symbol still PREPARED: nothing at all is written.
        let dup = db.add_pair_with_event(&pair("u2", "BTCUSDT", PairState::Prepared), "PAIR_PREPARED", &json!({}));
        assert_eq!(dup.unwrap(), AddPairOutcome::AlreadyPending);
        assert_eq!(events_of(&db, "PAIR_PREPARED").len(), 1);
        // A failing event insert rolls the pair back and halts.
        break_event_inserts(&db);
        let r = db.add_pair_with_event(&pair("u3", "ETHUSDT", PairState::Prepared), "PAIR_PREPARED", &json!({}));
        assert!(matches!(r, Err(StoreError::Halted(HaltReason::EventWriteFailed(_)))), "{r:?}");
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        let n: i64 = plain.query_row("SELECT COUNT(*) FROM pairs WHERE internal_uuid = 'u3'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "the pair must roll back with its event");
    }

    #[test]
    fn list_pairs_returns_every_pair_oldest_first() {
        let (_d, db, clock) = open_tmp();
        db.add_pair_if_not_pending(&pair("u1", "BTCUSDT", PairState::Prepared)).unwrap();
        clock.advance(1);
        db.add_pair_if_not_pending(&pair("u2", "ETHUSDT", PairState::Prepared)).unwrap();
        db.set_pair_status("u1", PairState::Finalized).unwrap();
        let rows = db.list_pairs().unwrap();
        let got: Vec<(&str, &str)> = rows.iter().map(|r| (r.internal_uuid.as_str(), r.status.as_str())).collect();
        assert_eq!(got, vec![("u1", "FINALIZED"), ("u2", "PREPARED")]);
        assert_eq!(rows[1].entry, json!({"edge": "0.0012"}));
    }

    #[test]
    fn list_pairs_read_failure_halts_the_store() {
        let (_d, db, _) = open_tmp();
        db.with_raw_conn_for_tests(|c| {
            Ok(c.execute_batch("INSERT INTO pairs VALUES ('u9','p','X','PREPARED','{}',1,1); DROP INDEX uniq_prepared_symbol; ALTER TABLE pairs RENAME TO pairs_gone;")?)
        })
        .ok();
        assert!(db.list_pairs().is_err());
        assert!(db.is_halted());
    }

    #[test]
    fn set_flag_with_event_writes_both_or_neither() {
        let (_d, db, _) = open_tmp();
        db.set_flag_with_event(FLAG_TRIGGER_MODE, "AUTO", "TRIGGER_MODE_CHANGED", &json!({"to": "AUTO"})).unwrap();
        assert_eq!(db.flag_get(FLAG_TRIGGER_MODE).unwrap().as_deref(), Some("AUTO"));
        assert_eq!(events_of(&db, "TRIGGER_MODE_CHANGED"), vec![(None, json!({"to": "AUTO"}))]);
        assert!(matches!(
            db.set_flag_with_event("Kill_Switch", "OFF", "X", &json!({})),
            Err(StoreError::ReservedFlag(_))
        ));
        assert!(matches!(
            db.set_flag_with_event("note", "api_key=abc123", "X", &json!({})),
            Err(StoreError::SecretInValue { .. })
        ));
        break_event_inserts(&db);
        assert!(db.set_flag_with_event(FLAG_TRIGGER_MODE, "MANUAL", "TRIGGER_MODE_CHANGED", &json!({})).is_err());
        assert!(db.is_halted());
        assert_eq!(plain_flag(&db, FLAG_TRIGGER_MODE), "AUTO", "flag rolls back with the event");
    }
}

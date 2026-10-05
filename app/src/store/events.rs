//! Append-only business events (`events` table) plus the in-memory `SCAN_RUN` buffer.
//! Immutability is enforced by triggers in the schema; this module only ever INSERTs.

#![allow(dead_code)]

use std::sync::Arc;

use rusqlite::{Connection, params};
use serde_json::Value;
use tong_funding_core::redact::redact_secrets;

use super::db::{Db, HaltReason, StoreError};
use super::scan_buffer::{DEFAULT_CAPACITY, ScanBuffer, ScanRecord};
use super::secrets::safe_event_payload;
use crate::ports::EventSink;

/// The only event type that stays out of the `events` table.
pub const SCAN_RUN: &str = "SCAN_RUN";

#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: i64,
    pub ts_ms: i64,
    pub event_type: String,
    pub pair_id: Option<String>,
    pub payload: Value,
}

/// INSERT one event on `conn` (also used inside larger transactions). Returns the new id.
pub(super) fn insert_event_on(
    conn: &Connection,
    ts_ms: i64,
    event_type: &str,
    pair_id: Option<&str>,
    payload: &Value,
) -> rusqlite::Result<i64> {
    // Every event passes through redaction on its way into the database, whoever the caller is.
    let payload = safe_event_payload(payload.clone());
    conn.execute(
        "INSERT INTO events (ts_ms, event_type, pair_id, payload) VALUES (?1, ?2, ?3, ?4)",
        params![ts_ms, event_type, pair_id, payload.to_string()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// `EventSink` backed by SQLite; `SCAN_RUN` goes to the memory ring buffer instead.
#[derive(Clone)]
pub struct EventStore {
    db: Db,
    scan: Arc<ScanBuffer>,
}

impl EventStore {
    pub fn new(db: Db) -> Self {
        Self::with_scan_capacity(db, DEFAULT_CAPACITY)
    }

    pub fn with_scan_capacity(db: Db, capacity: usize) -> Self {
        EventStore { db, scan: Arc::new(ScanBuffer::new(capacity)) }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Record an event. `Ok(None)` for `SCAN_RUN` (buffered in memory, not stored);
    /// `Ok(Some(id))` for everything else. Fails when the store is halted. The payload is always
    /// redacted first. A failed write halts the store (`HaltReason::EventWriteFailed`): losing an
    /// audit event silently is not acceptable.
    pub fn append(&self, event_type: &str, pair_id: Option<&str>, payload: Value) -> Result<Option<i64>, StoreError> {
        let ts_ms = self.db.now_ms();
        if event_type == SCAN_RUN {
            // Still refuse while halted, like every other write path.
            if let Some(r) = self.db.halt_reason() {
                return Err(StoreError::Halted(r));
            }
            self.scan.push(ScanRecord { ts_ms, pair_id: pair_id.map(str::to_string), payload: safe_event_payload(payload) });
            return Ok(None);
        }
        match self.db.with_conn(|c| Ok(insert_event_on(c, ts_ms, event_type, pair_id, &payload)?)) {
            Ok(id) => Ok(Some(id)),
            Err(StoreError::Halted(r)) => Err(StoreError::Halted(r)),
            Err(e) => {
                let reason = HaltReason::EventWriteFailed(redact_secrets(&format!("{event_type}: {e}")));
                self.db.halt(reason.clone());
                // Hand back the (redacted) halt reason, not the raw SQLite error text.
                Err(StoreError::Halted(self.db.halt_reason().unwrap_or(reason)))
            }
        }
    }

    pub fn count(&self) -> Result<i64, StoreError> {
        self.db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?))
    }

    /// Newest first, at most `limit` rows.
    pub fn list(&self, limit: usize) -> Result<Vec<EventRow>, StoreError> {
        self.db.with_conn(|c| {
            let mut st = c.prepare("SELECT id, ts_ms, event_type, pair_id, payload FROM events ORDER BY id DESC LIMIT ?1")?;
            let rows = st.query_map([limit as i64], |r| {
                let payload: String = r.get(4)?;
                Ok(EventRow {
                    id: r.get(0)?,
                    ts_ms: r.get(1)?,
                    event_type: r.get(2)?,
                    pair_id: r.get(3)?,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    /// Buffered `SCAN_RUN` summaries of this run, oldest first.
    pub fn scan_runs(&self) -> Vec<ScanRecord> {
        self.scan.snapshot()
    }
}

impl EventSink for EventStore {
    fn emit(&self, event_type: &str, pair_id: Option<&str>, payload: Value) {
        if let Err(e) = self.append(event_type, pair_id, payload) {
            eprintln!("event dropped ({event_type}): {}", redact_secrets(&e.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::test_support::*;
    use crate::store::db::{HaltReason, Db};
    use serde_json::json;

    fn store() -> (tempfile::TempDir, EventStore, crate::ports::ManualClock) {
        let (d, db, m) = open_tmp();
        (d, EventStore::new(db), m)
    }

    fn raw(es: &EventStore, sql: &'static str) -> Result<usize, StoreError> {
        es.db().with_conn(|c| Ok(c.execute(sql, [])?))
    }

    #[test]
    fn append_stores_all_fields_with_the_injected_clock() {
        let (_d, es, clock) = store();
        clock.set(1_700_000_000_123);
        let id = es.append("ORDER_SUBMITTED", Some("p1"), json!({"qty": "0.019", "nested": {"a": [1, 2]}})).unwrap();
        assert!(id.is_some());
        let rows = es.list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts_ms, 1_700_000_000_123);
        assert_eq!(rows[0].event_type, "ORDER_SUBMITTED");
        assert_eq!(rows[0].pair_id.as_deref(), Some("p1"));
        assert_eq!(rows[0].payload, json!({"qty": "0.019", "nested": {"a": [1, 2]}}));
    }

    #[test]
    fn ids_increase_and_pair_id_may_be_null() {
        let (_d, es, _) = store();
        let a = es.append("A", None, json!({})).unwrap().unwrap();
        let b = es.append("B", None, json!({})).unwrap().unwrap();
        assert!(b > a);
        assert_eq!(es.list(10).unwrap()[0].pair_id, None);
    }

    #[test]
    fn large_payload_is_not_truncated() {
        let (_d, es, _) = store();
        let big = "y".repeat(200_000);
        es.append("BIG", None, json!({ "blob": big.clone() })).unwrap();
        assert_eq!(es.list(1).unwrap()[0].payload["blob"].as_str().unwrap().len(), big.len());
    }

    #[test]
    fn events_cannot_be_updated() {
        let (_d, es, _) = store();
        es.append("A", None, json!({})).unwrap();
        let e = raw(&es, "UPDATE events SET event_type = 'HACKED'").unwrap_err().to_string();
        assert!(e.contains("append-only"), "{e}");
        assert_eq!(es.list(1).unwrap()[0].event_type, "A");
    }

    #[test]
    fn events_cannot_be_deleted_even_without_a_where_clause() {
        let (_d, es, _) = store();
        es.append("A", None, json!({})).unwrap();
        es.append("B", None, json!({})).unwrap();
        assert!(raw(&es, "DELETE FROM events WHERE id = 1").unwrap_err().to_string().contains("append-only"));
        assert!(raw(&es, "DELETE FROM events").unwrap_err().to_string().contains("append-only"));
        assert_eq!(es.count().unwrap(), 2);
    }

    #[test]
    fn invalid_json_payload_is_rejected_by_the_database() {
        let (_d, es, _) = store();
        let r = es.db().with_conn(|c| Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'X','not json')", [])?));
        assert!(r.is_err());
        assert_eq!(es.count().unwrap(), 0);
    }

    #[test]
    fn scan_run_is_buffered_in_memory_and_not_stored() {
        let (_d, es, clock) = store();
        clock.set(42);
        let r = es.append(SCAN_RUN, None, json!({"n": 1})).unwrap();
        assert_eq!(r, None);
        assert_eq!(es.count().unwrap(), 0);
        let buf = es.scan_runs();
        assert_eq!(buf.len(), 1);
        assert_eq!((buf[0].ts_ms, buf[0].payload.clone()), (42, json!({"n": 1})));
    }

    #[test]
    fn scan_run_buffer_is_capped_at_200_dropping_the_oldest() {
        let (_d, es, _) = store();
        for n in 0..250 {
            es.append(SCAN_RUN, None, json!({ "n": n })).unwrap();
        }
        let buf = es.scan_runs();
        assert_eq!(buf.len(), 200);
        assert_eq!(buf[0].payload, json!({"n": 50}));
        assert_eq!(buf[199].payload, json!({"n": 249}));
        assert_eq!(es.count().unwrap(), 0);
    }

    #[test]
    fn trade_events_are_persisted_and_survive_a_restart() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        {
            let (c, _) = clock(7);
            let es = EventStore::new(Db::open(&p, c));
            es.append("ORDER_SUBMITTED", Some("p9"), json!({"ok": true})).unwrap();
        }
        let (c, _) = clock(8);
        let es = EventStore::new(Db::open(&p, c));
        let rows = es.list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_type, "ORDER_SUBMITTED");
    }

    #[test]
    fn event_store_works_as_an_event_sink() {
        let (_d, es, _) = store();
        let sink: &dyn EventSink = &es;
        sink.emit("PAIR_FINALIZED", Some("p1"), json!({"x": 1}));
        sink.emit(SCAN_RUN, None, json!({}));
        assert_eq!(es.count().unwrap(), 1);
        assert_eq!(es.scan_runs().len(), 1);
    }

    #[test]
    fn halted_store_refuses_appends_and_emit_does_not_panic() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let (c, _) = clock(1);
        let es = EventStore::new(Db::open(&p, c));
        assert!(matches!(es.append("A", None, json!({})), Err(StoreError::Halted(HaltReason::Corrupt(_)))));
        es.emit("A", None, json!({}));
        assert!(es.count().is_err());
    }

    // ---- hardening (review round 1) ----

    #[test]
    fn append_redacts_secrets_so_no_table_or_file_byte_contains_them() {
        let (_d, es, _) = store();
        es.append(
            "ORDER_FAILED",
            Some("p1"),
            json!({
                "error": "GET /fapi?symbol=BTCUSDT&signature=RAWSIG failed",
                "api_key": "RAWKEY",
                "nested": { "hdr": "X-MBX-APIKEY: RAWKEY", "raw_json": "{\"api_key\":\"RAWKEY\"}" }
            }),
        )
        .unwrap();
        es.append(SCAN_RUN, None, json!({"note": "signature=RAWSIG"})).unwrap();
        let dump = dump_db(es.db());
        assert!(!dump.contains("RAWSIG") && !dump.contains("RAWKEY"), "{dump}");
        assert!(!files_contain(es.db().path(), "RAWSIG") && !files_contain(es.db().path(), "RAWKEY"));
        let buffered = format!("{:?}", es.scan_runs());
        assert!(!buffered.contains("RAWSIG"), "{buffered}");
        // Structure and harmless data survive.
        let row = &es.list(1).unwrap()[0];
        assert!(row.payload["error"].as_str().unwrap().contains("symbol=BTCUSDT"));
    }

    #[test]
    fn emit_redacts_too() {
        let (_d, es, _) = store();
        es.emit("X", None, json!({"error": "signature=RAWSIG"}));
        assert!(!dump_db(es.db()).contains("RAWSIG"));
    }

    #[test]
    fn a_failed_event_write_halts_the_store_with_a_redacted_reason() {
        let (_d, es, _) = store();
        es.db()
            .with_conn(|c| {
                Ok(c.execute_batch(
                    "CREATE TRIGGER inject BEFORE INSERT ON events WHEN NEW.event_type = 'BOOM'
                     BEGIN SELECT RAISE(ABORT, 'disk full signature=RAWSIG'); END",
                )?)
            })
            .unwrap();
        es.emit("OK_BEFORE", None, json!({}));
        assert!(!es.db().is_halted());
        es.emit("BOOM", None, json!({}));
        assert!(es.db().is_halted(), "a dropped event must stop the store");
        match es.db().halt_reason() {
            Some(HaltReason::EventWriteFailed(m)) => assert!(!m.contains("RAWSIG") && m.contains("disk full"), "{m}"),
            other => panic!("expected EventWriteFailed, got {other:?}"),
        }
        assert!(matches!(es.append("AFTER", None, json!({})), Err(StoreError::Halted(_))));
        assert!(es.db().set_kill_switch(false).is_err());
    }

    #[test]
    fn a_halted_store_does_not_overwrite_the_first_halt_reason() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let (c, _) = clock(1);
        let es = EventStore::new(Db::open(&p, c));
        es.emit("A", None, json!({}));
        assert!(matches!(es.db().halt_reason(), Some(HaltReason::Corrupt(_))));
    }
}

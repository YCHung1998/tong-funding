//! Funding ledger entries in the append-only `events` table (change funding-pnl, design D1/D2,
//! spec funding-history-fetch "流水寫入不可變事件表並以交易所端 id 去重").
//!
//! One `FUNDING_LEDGER_ENTRY` event per dedupe key, guaranteed by the schema v2 partial unique
//! index; a write is `INSERT ... ON CONFLICT DO NOTHING` and the changed-row count tells whether it
//! was new. The same key with a different amount is never overwritten: a `FUNDING_LEDGER_CONFLICT`
//! event records both values (once per distinct new value).

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use tong_funding_core::pnl::FundingLedgerEntry;
use tong_funding_core::redact::redact_secrets;

use super::db::{Db, StoreError};
use super::events::insert_event_on;
use super::secrets::safe_event_payload;

pub const FUNDING_LEDGER_ENTRY: &str = "FUNDING_LEDGER_ENTRY";
pub const FUNDING_LEDGER_CONFLICT: &str = "FUNDING_LEDGER_CONFLICT";

/// What one write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LedgerWriteReport {
    pub inserted: usize,
    /// Already stored with the same amount (idempotent no-op).
    pub skipped: usize,
    /// Already stored with a different amount: original kept, conflict recorded.
    pub conflicts: usize,
}

/// A stored entry with its event id.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredLedgerEntry {
    pub event_id: i64,
    pub entry: FundingLedgerEntry,
}

impl Db {
    /// Write `entries` in one transaction (all or nothing). A failure halts the store, like every
    /// other event write.
    pub fn write_funding_ledger(&self, entries: &[FundingLedgerEntry]) -> Result<LedgerWriteReport, StoreError> {
        for e in entries {
            if e.dedupe_key.trim().is_empty() {
                return Err(StoreError::Json(serde::de::Error::custom("funding ledger entry without a dedupe key")));
            }
        }
        let ts_ms = self.now_ms();
        self.with_conn(|c| {
            let run = |c: &mut rusqlite::Connection| -> rusqlite::Result<LedgerWriteReport> {
                let tx = c.transaction()?;
                let mut report = LedgerWriteReport::default();
                for e in entries {
                    let payload = safe_event_payload(serde_json::to_value(e).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?);
                    let changed = tx.execute(
                        "INSERT INTO events (ts_ms, event_type, pair_id, payload) VALUES (?1, ?2, NULL, ?3) ON CONFLICT DO NOTHING",
                        params![ts_ms, FUNDING_LEDGER_ENTRY, payload.to_string()],
                    )?;
                    if changed == 1 {
                        report.inserted += 1;
                        continue;
                    }
                    let (stored_id, stored): (i64, String) = tx.query_row(
                        "SELECT id, payload FROM events WHERE event_type = ?1 AND json_extract(payload, '$.dedupe_key') = ?2",
                        params![FUNDING_LEDGER_ENTRY, redact_secrets(&e.dedupe_key)],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    let stored: Value = serde_json::from_str(&stored).unwrap_or(Value::Null);
                    let new_amount = e.amount.normalize().to_string();
                    let stored_amount = stored["amount"].as_str().and_then(|s| s.parse::<rust_decimal::Decimal>().ok()).map(|d| d.normalize().to_string());
                    if stored_amount.as_deref() == Some(new_amount.as_str()) {
                        report.skipped += 1;
                        continue;
                    }
                    report.conflicts += 1;
                    let seen: Option<i64> = tx
                        .query_row(
                            "SELECT id FROM events WHERE event_type = ?1 AND json_extract(payload, '$.dedupe_key') = ?2 AND json_extract(payload, '$.new_amount') = ?3",
                            params![FUNDING_LEDGER_CONFLICT, redact_secrets(&e.dedupe_key), new_amount],
                            |r| r.get(0),
                        )
                        .optional()?;
                    if seen.is_none() {
                        let conflict = json!({
                            "dedupe_key": e.dedupe_key,
                            "exchange": e.exchange.name(),
                            "symbol": e.symbol,
                            "stored_event_id": stored_id,
                            "stored_amount": stored_amount,
                            "new_amount": new_amount,
                            "new_raw": e.raw,
                        });
                        insert_event_on(&tx, ts_ms, FUNDING_LEDGER_CONFLICT, None, &conflict)?;
                    }
                }
                tx.commit()?;
                Ok(report)
            };
            run(c).map_err(|e| self.event_write_failed(FUNDING_LEDGER_ENTRY, &e))
        })
    }

    /// Every stored funding ledger entry, by settlement time then id.
    pub fn funding_ledger_entries(&self) -> Result<Vec<StoredLedgerEntry>, StoreError> {
        self.with_conn(|c| {
            let mut st = c.prepare(
                "SELECT id, payload FROM events WHERE event_type = ?1 ORDER BY json_extract(payload, '$.settled_at_ms'), id",
            )?;
            let rows = st.query_map([FUNDING_LEDGER_ENTRY], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for r in rows {
                let (event_id, payload) = r?;
                // An unreadable stored entry is an error, never silently dropped.
                let entry: FundingLedgerEntry = serde_json::from_str(&payload)?;
                out.push(StoredLedgerEntry { event_id, entry });
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::test_support::open_tmp;
    use tong_funding_core::types::{Decimal, Exchange};

    fn entry(exchange: Exchange, id: &str, amount: &str) -> FundingLedgerEntry {
        let kind = if exchange == Exchange::Binance { "FUNDING_FEE" } else { "SETTLEMENT" };
        FundingLedgerEntry::new(exchange, "BTCUSDT", amount.parse::<Decimal>().unwrap(), "USDT", 1_000, id, kind, json!({"id": id, "funding": amount}))
    }

    fn count(db: &Db, ty: &str) -> i64 {
        db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events WHERE event_type = ?1", [ty], |r| r.get(0))?)).unwrap()
    }

    #[test]
    fn funding_ledger_store_repeated_fetch_does_not_write_twice() {
        let (_d, db, _) = open_tmp();
        let batch = [entry(Exchange::Bybit, "592324_XRPUSDT_161440249321", "-0.003676"), entry(Exchange::Binance, "9689322392", "0.5")];
        assert_eq!(db.write_funding_ledger(&batch).unwrap(), LedgerWriteReport { inserted: 2, skipped: 0, conflicts: 0 });
        assert_eq!(db.write_funding_ledger(&batch).unwrap(), LedgerWriteReport { inserted: 0, skipped: 2, conflicts: 0 });
        assert_eq!(count(&db, FUNDING_LEDGER_ENTRY), 2);
        let stored = db.funding_ledger_entries().unwrap();
        assert_eq!(stored.iter().map(|s| s.entry.dedupe_key.as_str()).collect::<Vec<_>>(), ["bybit:592324_XRPUSDT_161440249321", "binance:FUNDING_FEE:9689322392"]);
        assert_eq!(stored[0].entry, batch[0], "round-trips unchanged (dedupe keys survive redaction)");
    }

    #[test]
    fn funding_ledger_store_entries_cannot_be_updated_or_deleted() {
        let (_d, db, _) = open_tmp();
        db.write_funding_ledger(&[entry(Exchange::Bybit, "a", "1")]).unwrap();
        for sql in ["UPDATE events SET payload = '{}' WHERE event_type = 'FUNDING_LEDGER_ENTRY'", "DELETE FROM events WHERE event_type = 'FUNDING_LEDGER_ENTRY'"] {
            let e = db.with_conn(|c| Ok(c.execute(sql, [])?)).unwrap_err().to_string();
            assert!(e.contains("append-only"), "{sql}: {e}");
        }
        assert_eq!(count(&db, FUNDING_LEDGER_ENTRY), 1);
    }

    #[test]
    fn funding_ledger_store_same_key_different_amount_keeps_the_original_and_records_a_conflict() {
        let (_d, db, _) = open_tmp();
        db.write_funding_ledger(&[entry(Exchange::Binance, "K", "-0.10")]).unwrap();
        let r = db.write_funding_ledger(&[entry(Exchange::Binance, "K", "-0.12")]).unwrap();
        assert_eq!(r, LedgerWriteReport { inserted: 0, skipped: 0, conflicts: 1 });
        let stored = db.funding_ledger_entries().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].entry.amount, "-0.10".parse::<Decimal>().unwrap(), "never overwritten");
        let conflict: Value = db
            .with_conn(|c| Ok(c.query_row("SELECT payload FROM events WHERE event_type = 'FUNDING_LEDGER_CONFLICT'", [], |r| r.get::<_, String>(0))?))
            .map(|s| serde_json::from_str(&s).unwrap())
            .unwrap();
        assert_eq!((conflict["stored_amount"].as_str(), conflict["new_amount"].as_str()), (Some("-0.1"), Some("-0.12")));
        assert_eq!(conflict["dedupe_key"], json!("binance:FUNDING_FEE:K"));
        // Seeing the same conflicting value again does not pile up events; a third value does.
        db.write_funding_ledger(&[entry(Exchange::Binance, "K", "-0.12")]).unwrap();
        assert_eq!(count(&db, FUNDING_LEDGER_CONFLICT), 1);
        db.write_funding_ledger(&[entry(Exchange::Binance, "K", "-0.13")]).unwrap();
        assert_eq!(count(&db, FUNDING_LEDGER_CONFLICT), 2);
        // The same amount written with another scale is not a conflict.
        assert_eq!(db.write_funding_ledger(&[entry(Exchange::Binance, "K", "-0.1000")]).unwrap().skipped, 1);
    }

    #[test]
    fn funding_ledger_store_a_plain_duplicate_insert_is_refused_by_the_database() {
        let (_d, db, _) = open_tmp();
        db.write_funding_ledger(&[entry(Exchange::Bybit, "a", "1")]).unwrap();
        let r = db.with_conn(|c| {
            Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1, 'FUNDING_LEDGER_ENTRY', '{\"dedupe_key\":\"bybit:a\"}')", [])?)
        });
        assert!(r.is_err(), "{r:?}");
        // Other event types with the same payload key are unaffected (partial index).
        db.with_conn(|c| Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1, 'OTHER', '{\"dedupe_key\":\"bybit:a\"}')", [])?)).unwrap();
    }

    #[test]
    fn funding_ledger_store_a_halted_store_writes_nothing() {
        let (_d, db, _) = open_tmp();
        db.halt(crate::store::db::HaltReason::EventWriteFailed("test".into()));
        assert!(matches!(db.write_funding_ledger(&[entry(Exchange::Bybit, "a", "1")]), Err(StoreError::Halted(_))));
    }
}

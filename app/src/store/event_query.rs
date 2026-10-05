//! Read-only, paged access to the `events` table for the system log page (ui-readonly-pages,
//! design D10). Newest first with a `(ts_ms, id)` cursor, an optional type filter, and the totals
//! the page header needs. Never writes.

use std::collections::BTreeSet;

use rusqlite::types::Value as SqlValue;

use super::db::{Db, StoreError};

/// Design D10: 500 rows per page (the Python version's `limit=500`; unverified under GPUI).
pub const PAGE_SIZE: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    /// `None` = every type; `Some(empty)` = nothing.
    pub types: Option<BTreeSet<String>>,
    /// Return only rows strictly older than this `(ts_ms, id)` cursor.
    pub before: Option<(i64, i64)>,
    pub limit: usize,
}

impl Default for EventQuery {
    fn default() -> Self {
        EventQuery { types: None, before: None, limit: PAGE_SIZE }
    }
}

/// One stored event as the page shows it. `payload` is the raw stored text (shown as is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub id: i64,
    pub ts_ms: i64,
    pub event_type: String,
    pub pair_id: Option<String>,
    pub payload: String,
    /// Imported from the legacy `events.jsonl` (its `legacy_hash` is set).
    pub imported: bool,
}

/// One page plus the totals of everything matching the type filter (not just this page).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventPage {
    pub rows: Vec<StoredEvent>,
    /// More (older) rows exist after this page.
    pub has_more: bool,
    /// Every distinct `event_type` in the table with its count (independent of the filter), by name.
    pub type_counts: Vec<(String, i64)>,
    /// Rows matching the filter, and their time range.
    pub matching: i64,
    pub oldest_ts: Option<i64>,
    pub newest_ts: Option<i64>,
}

fn type_clause(types: &Option<BTreeSet<String>>, params: &mut Vec<SqlValue>) -> String {
    match types {
        None => "1=1".into(),
        Some(set) if set.is_empty() => "0=1".into(),
        Some(set) => {
            let marks: Vec<&str> = set.iter().map(|t| {
                params.push(SqlValue::Text(t.clone()));
                "?"
            }).collect();
            format!("event_type IN ({})", marks.join(","))
        }
    }
}

impl Db {
    /// One page of events, newest first, with totals. Read-only.
    pub fn query_events(&self, q: &EventQuery) -> Result<EventPage, StoreError> {
        self.with_conn(|c| {
            let mut type_counts = Vec::new();
            {
                let mut st = c.prepare("SELECT event_type, COUNT(*) FROM events GROUP BY event_type ORDER BY event_type")?;
                let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                for r in rows {
                    type_counts.push(r?);
                }
            }

            let mut params = Vec::new();
            let filter = type_clause(&q.types, &mut params);
            let (matching, oldest_ts, newest_ts) = c.query_row(
                &format!("SELECT COUNT(*), MIN(ts_ms), MAX(ts_ms) FROM events WHERE {filter}"),
                rusqlite::params_from_iter(params.iter()),
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?)),
            )?;

            let mut params = Vec::new();
            let filter = type_clause(&q.types, &mut params);
            let cursor = match q.before {
                Some((ts, id)) => {
                    params.push(SqlValue::Integer(ts));
                    params.push(SqlValue::Integer(ts));
                    params.push(SqlValue::Integer(id));
                    "(ts_ms < ? OR (ts_ms = ? AND id < ?))"
                }
                None => "1=1",
            };
            let limit = q.limit.max(1);
            params.push(SqlValue::Integer(limit as i64 + 1));
            let sql = format!(
                "SELECT id, ts_ms, event_type, pair_id, payload, legacy_hash IS NOT NULL FROM events
                 WHERE {filter} AND {cursor} ORDER BY ts_ms DESC, id DESC LIMIT ?"
            );
            let mut st = c.prepare(&sql)?;
            let mapped = st.query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok(StoredEvent {
                    id: r.get(0)?,
                    ts_ms: r.get(1)?,
                    event_type: r.get(2)?,
                    pair_id: r.get(3)?,
                    payload: r.get(4)?,
                    imported: r.get(5)?,
                })
            })?;
            let mut rows = mapped.collect::<Result<Vec<_>, _>>()?;
            let has_more = rows.len() > limit;
            rows.truncate(limit);
            Ok(EventPage { rows, has_more, type_counts, matching, oldest_ts, newest_ts })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::test_support::open_tmp;
    use crate::store::events::EventStore;
    use serde_json::json;

    fn seeded(n: i64) -> (tempfile::TempDir, Db) {
        let (dir, db, clock) = open_tmp();
        let es = EventStore::new(db.clone());
        for i in 0..n {
            clock.set(1_000 + i);
            let ty = if i % 3 == 0 { "FETCH_ERROR" } else { "ORDER_SUBMITTED" };
            es.append(ty, None, json!({ "i": i })).unwrap();
        }
        (dir, db)
    }

    #[test]
    fn pages_are_newest_first_and_the_cursor_never_overlaps() {
        let (_d, db) = seeded(12);
        let p1 = db.query_events(&EventQuery { limit: 5, ..Default::default() }).unwrap();
        assert_eq!(p1.rows.len(), 5);
        assert!(p1.has_more);
        assert_eq!(p1.rows[0].ts_ms, 1_011);
        let last = p1.rows.last().unwrap();
        let p2 = db.query_events(&EventQuery { limit: 5, before: Some((last.ts_ms, last.id)), ..Default::default() }).unwrap();
        assert_eq!(p2.rows[0].ts_ms, last.ts_ms - 1);
        let p3 = db.query_events(&EventQuery { limit: 5, before: Some((p2.rows[4].ts_ms, p2.rows[4].id)), ..Default::default() }).unwrap();
        assert_eq!(p3.rows.len(), 2);
        assert!(!p3.has_more);
        assert_eq!(p1.matching, 12);
    }

    #[test]
    fn type_filter_applies_to_rows_and_totals_but_not_to_type_counts() {
        let (_d, db) = seeded(9);
        let only = EventQuery { types: Some(["FETCH_ERROR".to_string()].into()), ..Default::default() };
        let p = db.query_events(&only).unwrap();
        assert!(p.rows.iter().all(|r| r.event_type == "FETCH_ERROR"));
        assert_eq!(p.matching, 3);
        assert_eq!((p.oldest_ts, p.newest_ts), (Some(1_000), Some(1_006)));
        assert_eq!(p.type_counts, vec![("FETCH_ERROR".to_string(), 3), ("ORDER_SUBMITTED".to_string(), 6)]);
        let none = db.query_events(&EventQuery { types: Some(BTreeSet::new()), ..Default::default() }).unwrap();
        assert_eq!((none.rows.len(), none.matching), (0, 0));
    }

    #[test]
    fn imported_rows_are_flagged_by_legacy_hash() {
        let (_d, db) = seeded(1);
        db.with_conn(|c| Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (5, 'OLD', '{}', 'h1')", [])?)).unwrap();
        let p = db.query_events(&EventQuery::default()).unwrap();
        let old = p.rows.iter().find(|r| r.event_type == "OLD").unwrap();
        assert!(old.imported);
        assert!(p.rows.iter().filter(|r| r.event_type != "OLD").all(|r| !r.imported));
    }
}

//! The v1 schema as a constant, with tests that pin the database-level guarantees
//! (append-only events, valid JSON, one PREPARED pair per symbol).

pub const SCHEMA_V1: &str = include_str!("schema_v1.sql");
/// funding-pnl: unique dedupe key of `FUNDING_LEDGER_ENTRY` events (partial expression index).
pub const SCHEMA_V2: &str = include_str!("schema_v2.sql");
/// Name of the index created by [`SCHEMA_V2`] (required from schema v2 on).
pub const FUNDING_LEDGER_INDEX: &str = "uniq_funding_ledger_dedupe";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA recursive_triggers = ON; PRAGMA foreign_keys = ON;").unwrap();
        c.execute_batch(SCHEMA_V1).unwrap();
        c
    }

    fn insert_event(c: &Connection, ty: &str, payload: &str) -> rusqlite::Result<usize> {
        c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1, ?1, ?2)", [ty, payload])
    }

    #[test]
    fn schema_version_is_1() {
        let v: i64 = db().query_row("SELECT version FROM schema_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn events_cannot_be_updated_or_deleted() {
        let c = db();
        insert_event(&c, "ORDER_STAGED", "{}").unwrap();
        let upd = c.execute("UPDATE events SET event_type = 'X'", []).unwrap_err().to_string();
        assert!(upd.contains("append-only"), "{upd}");
        let del = c.execute("DELETE FROM events", []).unwrap_err().to_string();
        assert!(del.contains("append-only"), "{del}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn insert_or_replace_cannot_overwrite_an_event() {
        let c = db();
        c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (1,'A','{}','h1')", []).unwrap();
        let r = c.execute("INSERT OR REPLACE INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'B','{}','h1')", []);
        assert_eq!(r.unwrap(), 0, "REPLACE is ignored by the dedupe trigger");
        let ty: String = c.query_row("SELECT event_type FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(ty, "A");
    }

    #[test]
    fn kill_switch_row_is_seeded_off() {
        let v: String = db().query_row("SELECT value FROM system_flags WHERE key = 'kill_switch'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, "OFF");
    }

    #[test]
    fn explicit_id_cannot_overwrite_an_event_even_without_recursive_triggers() {
        // The weakest connection: no recursive_triggers, no foreign_keys.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA_V1).unwrap();
        insert_event(&c, "A", "{}").unwrap();
        let r = c.execute("REPLACE INTO events (id, ts_ms, event_type, payload) VALUES (1, 2, 'FORGED', '{}')", []);
        assert!(r.unwrap_err().to_string().contains("append-only"));
        // Plain AUTOINCREMENT inserts (NEW.id is not an existing id) are unaffected.
        insert_event(&c, "B", "{}").unwrap();
        insert_event(&c, "C", "{}").unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn duplicate_legacy_hash_is_ignored_on_a_plain_connection_whatever_the_conflict_clause() {
        // No recursive_triggers: REPLACE would otherwise delete the old row without any trigger.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA_V1).unwrap();
        c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (1,'ORIGINAL','{\"k\":1}','h1')", []).unwrap();
        for sql in [
            "REPLACE INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'FORGED','{}','h1')",
            "INSERT OR REPLACE INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'FORGED','{}','h1')",
            "INSERT OR IGNORE INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'FORGED','{}','h1')",
            "INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'FORGED','{}','h1') ON CONFLICT(legacy_hash) DO NOTHING",
            "INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'FORGED','{}','h1')",
        ] {
            assert_eq!(c.execute(sql, []).unwrap(), 0, "{sql}");
        }
        let (n, ty, p): (i64, String, String) =
            c.query_row("SELECT COUNT(*), MIN(event_type), MIN(payload) FROM events", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((n, ty.as_str(), p.as_str()), (1, "ORIGINAL", "{\"k\":1}"));
        // A different hash and a NULL hash still insert.
        assert_eq!(c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (3,'B','{}','h2')", []).unwrap(), 1);
        assert_eq!(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (3,'C','{}')", []).unwrap(), 1);
    }

    #[test]
    fn event_payload_must_be_valid_json() {
        assert!(insert_event(&db(), "X", "not json").is_err());
    }

    #[test]
    fn legacy_hash_is_unique_but_null_is_repeatable() {
        let c = db();
        c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'A','{}')", []).unwrap();
        c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (2,'A','{}')", []).unwrap();
        c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (3,'A','{}','h')", []).unwrap();
        // A duplicate legacy_hash is silently ignored (see the dedupe trigger test below).
        assert_eq!(c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (4,'A','{}','h')", []).unwrap(), 0);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    fn add_pair(c: &Connection, uuid: &str, symbol: &str, status: &str) -> rusqlite::Result<usize> {
        c.execute(
            "INSERT INTO pairs (internal_uuid, pair_id, symbol, status, entry_json, created_ms, updated_ms) VALUES (?1, 'p', ?2, ?3, '{}', 1, 1)",
            [uuid, symbol, status],
        )
    }

    #[test]
    fn only_one_prepared_pair_per_symbol() {
        let c = db();
        add_pair(&c, "u1", "BTCUSDT", "PREPARED").unwrap();
        assert!(add_pair(&c, "u2", "BTCUSDT", "PREPARED").is_err());
        add_pair(&c, "u3", "ETHUSDT", "PREPARED").unwrap();
        // once the first is past PREPARED, a new PREPARED one is allowed
        c.execute("UPDATE pairs SET status = 'ORDER_SUBMIT' WHERE internal_uuid = 'u1'", []).unwrap();
        add_pair(&c, "u4", "BTCUSDT", "PREPARED").unwrap();
        // non-PREPARED duplicates are unconstrained
        add_pair(&c, "u5", "BTCUSDT", "FINALIZED").unwrap();
        add_pair(&c, "u6", "BTCUSDT", "FINALIZED").unwrap();
    }

    #[test]
    fn client_order_id_is_unique() {
        let c = db();
        let ins = |id: &str| {
            c.execute(
                "INSERT INTO order_intents (client_order_id, pair_uuid, leg, exchange, symbol, side, quantity, state, created_ms, updated_ms) VALUES (?1,'u','long','Binance','BTCUSDT','BUY','0.019','INTENDED',1,1)",
                [id],
            )
        };
        ins("c1").unwrap();
        assert!(ins("c1").is_err());
        ins("c2").unwrap();
    }
}

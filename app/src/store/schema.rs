//! The v1 schema as a constant, with tests that pin the database-level guarantees
//! (append-only events, valid JSON, one PREPARED pair per symbol).

pub const SCHEMA_V1: &str = include_str!("schema_v1.sql");

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
        assert!(r.is_err(), "REPLACE must be blocked by the delete trigger (recursive_triggers = ON)");
        let ty: String = c.query_row("SELECT event_type FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(ty, "A");
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
        assert!(c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (4,'A','{}','h')", []).is_err());
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

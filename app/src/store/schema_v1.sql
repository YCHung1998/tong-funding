-- Schema v1 (openspec/changes/store-sqlite/design.md "Schema 草案").
-- events is append-only: UPDATE and DELETE are blocked by triggers. The connection must also
-- enable `PRAGMA recursive_triggers = ON`, otherwise `INSERT OR REPLACE` could replace an event
-- without firing the DELETE trigger.

CREATE TABLE schema_version (version INTEGER NOT NULL);
INSERT INTO schema_version (version) VALUES (1);

CREATE TABLE events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms        INTEGER NOT NULL,
    event_type   TEXT    NOT NULL,
    pair_id      TEXT,
    payload      TEXT    NOT NULL CHECK (json_valid(payload)),
    legacy_hash  TEXT UNIQUE
);
CREATE INDEX idx_events_type_ts ON events (event_type, ts_ms);
CREATE INDEX idx_events_pair    ON events (pair_id);

CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only: UPDATE forbidden'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only: DELETE forbidden'); END;

CREATE TABLE pairs (
    internal_uuid TEXT PRIMARY KEY,
    pair_id       TEXT    NOT NULL,
    symbol        TEXT    NOT NULL,
    status        TEXT    NOT NULL,
    entry_json    TEXT    NOT NULL CHECK (json_valid(entry_json)),
    created_ms    INTEGER NOT NULL,
    updated_ms    INTEGER NOT NULL
);
-- At most one PREPARED pair per symbol, enforced by the database (atomic add_if_not_pending).
CREATE UNIQUE INDEX uniq_prepared_symbol ON pairs (symbol) WHERE status = 'PREPARED';

CREATE TABLE order_intents (
    client_order_id   TEXT PRIMARY KEY,
    pair_uuid         TEXT    NOT NULL,
    leg               TEXT    NOT NULL,
    exchange          TEXT    NOT NULL,
    symbol            TEXT    NOT NULL,
    side              TEXT    NOT NULL,
    quantity          TEXT    NOT NULL,
    state             TEXT    NOT NULL,
    exchange_order_id TEXT,
    created_ms        INTEGER NOT NULL,
    updated_ms        INTEGER NOT NULL
);

CREATE TABLE config (
    key        TEXT PRIMARY KEY,
    value_json TEXT    NOT NULL CHECK (json_valid(value_json)),
    version    INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE TABLE system_flags (
    key        TEXT PRIMARY KEY,
    value      TEXT    NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE TABLE portfolio_history (
    ts_ms      INTEGER NOT NULL,
    exchange   TEXT    NOT NULL,
    total_usdt TEXT    NOT NULL,
    PRIMARY KEY (ts_ms, exchange)
);

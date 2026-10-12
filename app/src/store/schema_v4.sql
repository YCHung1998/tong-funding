-- Schema v4 (funding slots): a slot can also be settled as NO_SETTLEMENT (the exchange published no
-- funding for that time: no ledger row and no row in its public funding-rate history). SQLite cannot
-- change a CHECK constraint, so the table is rebuilt with the wider list and the rows are copied.
CREATE TABLE funding_slots_v4 (
    pair_id            TEXT    NOT NULL,
    leg                TEXT    NOT NULL CHECK (leg IN ('long', 'short')),
    exchange           TEXT    NOT NULL,
    symbol             TEXT    NOT NULL,
    settle_ms          INTEGER NOT NULL,
    state              TEXT    NOT NULL CHECK (state IN ('TRACING_1', 'TRACING_2', 'SETTLED', 'MANUAL_CHECK')),
    manual_reason      TEXT    CHECK (manual_reason IS NULL OR manual_reason IN ('NOT_FOUND', 'FETCH_FAILED', 'BACKFILL', 'AMBIGUOUS')),
    resolution         TEXT    CHECK (resolution IS NULL OR resolution IN ('LEDGER', 'ZERO_RATE', 'NO_SETTLEMENT')),
    amount             TEXT,
    ledger_dedupe_key  TEXT    UNIQUE,
    ledger_exchange_id TEXT,
    zero_rate_source   TEXT,
    first_seen_ms      INTEGER,
    next_check_ms      INTEGER NOT NULL,
    checks_ok          INTEGER NOT NULL DEFAULT 0,
    checks_failed      INTEGER NOT NULL DEFAULT 0,
    last_error         TEXT,
    created_ms         INTEGER NOT NULL,
    updated_ms         INTEGER NOT NULL,
    PRIMARY KEY (pair_id, leg, settle_ms)
);
INSERT INTO funding_slots_v4 (pair_id, leg, exchange, symbol, settle_ms, state, manual_reason, resolution, amount, ledger_dedupe_key, ledger_exchange_id, zero_rate_source, first_seen_ms, next_check_ms, checks_ok, checks_failed, last_error, created_ms, updated_ms)
SELECT pair_id, leg, exchange, symbol, settle_ms, state, manual_reason, resolution, amount, ledger_dedupe_key, ledger_exchange_id, zero_rate_source, first_seen_ms, next_check_ms, checks_ok, checks_failed, last_error, created_ms, updated_ms FROM funding_slots;
DROP TABLE funding_slots;
ALTER TABLE funding_slots_v4 RENAME TO funding_slots;
CREATE INDEX idx_funding_slots_due ON funding_slots (state, next_check_ms);

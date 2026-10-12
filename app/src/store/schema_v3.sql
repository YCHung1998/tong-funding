-- Schema v3 (openspec/changes/funding-slot-tracing/design.md D1): one funding slot per
-- (pair, leg, settlement time) the position was held through. The working state of the staged
-- funding tracing lives here; the immutable record is the PAIR_FUNDING_LINKED event written in the
-- same transaction that settles a slot. The events table and its triggers are untouched.
CREATE TABLE funding_slots (
    pair_id            TEXT    NOT NULL,
    leg                TEXT    NOT NULL CHECK (leg IN ('long', 'short')),
    exchange           TEXT    NOT NULL,
    symbol             TEXT    NOT NULL,
    settle_ms          INTEGER NOT NULL,
    state              TEXT    NOT NULL CHECK (state IN ('TRACING_1', 'TRACING_2', 'SETTLED', 'MANUAL_CHECK')),
    manual_reason      TEXT    CHECK (manual_reason IS NULL OR manual_reason IN ('NOT_FOUND', 'FETCH_FAILED', 'BACKFILL', 'AMBIGUOUS')),
    resolution         TEXT    CHECK (resolution IS NULL OR resolution IN ('LEDGER', 'ZERO_RATE')),
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
CREATE INDEX idx_funding_slots_due ON funding_slots (state, next_check_ms);

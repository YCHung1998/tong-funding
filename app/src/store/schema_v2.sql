-- Schema v2 (openspec/changes/funding-pnl/design.md D1): at most one FUNDING_LEDGER_ENTRY event per
-- exchange-side dedupe key, guaranteed by the database. A partial expression index; the events
-- table, its columns and its append-only triggers are unchanged. Writers insert with
-- `INSERT ... ON CONFLICT DO NOTHING` and check the number of changed rows.
-- If a v1 database already holds two such events with the same key, creating the index fails and
-- the whole migration rolls back (the store halts instead of guessing which one to keep).
CREATE UNIQUE INDEX uniq_funding_ledger_dedupe
    ON events (json_extract(payload, '$.dedupe_key'))
    WHERE event_type = 'FUNDING_LEDGER_ENTRY';

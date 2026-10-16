-- Schema v5 (live-process): the database remembers which environment it belongs to.
-- `db_meta` holds exactly one immutable row, `db_venue` = 'demo' | 'live', written in the same
-- transaction as the migration (or at creation) by the opening process identity. It can never be
-- changed, replaced or removed: every path (UPDATE, DELETE, REPLACE, a second INSERT) is refused.
-- Raising the schema version makes a build that only supports v4 refuse the database (SchemaTooNew).
CREATE TABLE db_meta (
    key   TEXT NOT NULL PRIMARY KEY CHECK (key = 'db_venue'),
    value TEXT NOT NULL CHECK (value IN ('demo', 'live'))
) WITHOUT ROWID;

CREATE TRIGGER db_meta_no_update BEFORE UPDATE ON db_meta
BEGIN
    SELECT RAISE(ABORT, 'db_meta is immutable');
END;

CREATE TRIGGER db_meta_no_delete BEFORE DELETE ON db_meta
BEGIN
    SELECT RAISE(ABORT, 'db_meta is immutable');
END;

-- Also catches INSERT OR REPLACE (a second row, or the same key again).
CREATE TRIGGER db_meta_no_second_row BEFORE INSERT ON db_meta
WHEN (SELECT COUNT(*) FROM db_meta) > 0
BEGIN
    SELECT RAISE(ABORT, 'db_meta is immutable');
END;

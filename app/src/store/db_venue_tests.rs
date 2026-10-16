//! live-process 3.1: the database is bound to the process identity (`db_venue`, schema v5).
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::test_support::{clock, tempdir};
use super::*;
use crate::identity::ProcessIdentity::{Demo, Live};
use crate::store::schema::{SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4};

/// What a build that stops at schema v4 (every released version before live-process) knows.
fn up_to_v4() -> Vec<Migration> {
    vec![Migration { version: 1, sql: SCHEMA_V1 }, Migration { version: 2, sql: SCHEMA_V2 }, Migration { version: 3, sql: SCHEMA_V3 }, Migration { version: 4, sql: SCHEMA_V4 }]
}

/// A pre-live-process `funding.db`: schema v4, some data, no `db_meta`.
fn v4_database(path: &Path) {
    let (c, _) = clock(5);
    let db = Db::open_with(path, c, &up_to_v4());
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    db.with_conn(|conn| {
        for i in 0..50 {
            conn.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (?1, 'OLD', '{\"n\":1}')", [i])?;
        }
        conn.execute("INSERT INTO config (key, value_json, version, updated_ms) VALUES ('risk', '{\"a\":1}', 1, 5)", [])?;
        Ok(())
    })
    .unwrap();
}

fn read<T: rusqlite::types::FromSql>(p: &Path, sql: &str) -> T {
    Connection::open(p).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
}
fn venue_of(p: &Path) -> String {
    read(p, "SELECT value FROM db_meta WHERE key = 'db_venue'")
}
fn version_of(p: &Path) -> i64 {
    read(p, "SELECT MAX(version) FROM schema_version")
}
fn bak(p: &Path) -> PathBuf {
    sidecar(p, ".pre-v5.bak")
}

#[test]
fn the_latest_schema_is_5() {
    assert_eq!(LATEST_SCHEMA_VERSION, 5);
    assert_eq!(MIGRATIONS.last().unwrap().version, 5);
}

#[test]
fn default_paths_are_per_identity_in_one_directory() {
    assert_eq!(default_db_path_for(Demo, Some("/Users/a")).unwrap(), PathBuf::from("/Users/a/Library/Application Support/tong-funding/funding.db"));
    assert_eq!(default_db_path_for(Live, Some("/Users/a")).unwrap(), PathBuf::from("/Users/a/Library/Application Support/tong-funding/funding-live.db"));
    assert_eq!(default_db_path_from(Some("/Users/a")).unwrap(), default_db_path_for(Demo, Some("/Users/a")).unwrap());
    assert!(matches!(default_db_path_for(Live, None), Err(StoreError::HomeNotSet)));
}

#[test]
fn a_fresh_database_is_stamped_with_the_identity_that_created_it() {
    for (identity, name) in [(Demo, "demo"), (Live, "live")] {
        let dir = tempdir();
        let p = dir.path().join(identity.db_file_name());
        let (c, _) = clock(1);
        let db = Db::open_for(identity, &p, c);
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        assert_eq!(db.identity(), identity);
        drop(db);
        assert_eq!((version_of(&p), venue_of(&p).as_str()), (5, name));
        assert!(!bak(&p).exists(), "a fresh database needs no backup");
    }
    // `Db::open` (no identity) is the Demo behaviour.
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    let (c, _) = clock(1);
    drop(Db::open(&p, c));
    assert_eq!(venue_of(&p), "demo");
}

#[test]
fn reopening_with_the_same_identity_works_and_keeps_the_venue() {
    for identity in [Demo, Live] {
        let dir = tempdir();
        let p = dir.path().join("x.db");
        let (c, _) = clock(1);
        drop(Db::open_for(identity, &p, c.clone()));
        let db = Db::open_for(identity, &p, c);
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
    }
}

#[test]
fn an_environment_mismatch_halts_without_any_read_or_write() {
    for (made_by, opened_by, made_name) in [(Demo, Live, "demo"), (Live, Demo, "live")] {
        let dir = tempdir();
        let p = dir.path().join("x.db");
        let (c, _) = clock(1);
        {
            let db = Db::open_for(made_by, &p, c.clone());
            assert!(!db.is_halted());
        }
        let before = std::fs::read(&p).unwrap();
        let db = Db::open_for(opened_by, &p, c);
        match db.halt_reason() {
            Some(HaltReason::VenueMismatch(msg)) => assert!(msg.contains(made_name), "{msg}"),
            other => panic!("expected VenueMismatch, got {other:?}"),
        }
        assert!(db.with_conn(|_| Ok(())).is_err(), "a mismatched store refuses all access");
        drop(db);
        assert_eq!(std::fs::read(&p).unwrap(), before, "no byte of the other environment's database is written");
        assert_eq!(venue_of(&p), made_name, "the venue is not rewritten");
    }
}

#[test]
fn an_existing_pre_v5_funding_db_is_upgraded_for_demo_keeping_all_data_and_backed_up() {
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    v4_database(&p);
    assert_eq!(version_of(&p), 4);
    let (c, _) = clock(9);
    let db = Db::open_for(Demo, &p, c);
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    let n: i64 = db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events WHERE event_type = 'OLD'", [], |r| r.get(0))?)).unwrap();
    assert_eq!(n, 50, "existing events survive");
    let cfg: String = db.with_conn(|c| Ok(c.query_row("SELECT value_json FROM config WHERE key = 'risk'", [], |r| r.get(0))?)).unwrap();
    assert_eq!(cfg, "{\"a\":1}");
    drop(db);
    assert_eq!((version_of(&p), venue_of(&p).as_str()), (5, "demo"));
    // The backup is a v4 database with the same data, owner-only.
    let b = bak(&p);
    assert!(b.exists(), "backup missing");
    assert_eq!(version_of(&b), 4);
    assert_eq!(read::<i64>(&b, "SELECT COUNT(*) FROM events WHERE event_type = 'OLD'"), 50);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn an_existing_backup_is_never_overwritten() {
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    v4_database(&p);
    std::fs::write(bak(&p), b"older backup, do not touch").unwrap();
    let (c, _) = clock(9);
    let db = Db::open_for(Demo, &p, c);
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    drop(db);
    assert_eq!(std::fs::read(bak(&p)).unwrap(), b"older backup, do not touch");
    assert_eq!(version_of(&p), 5);
}

#[test]
fn a_database_that_needs_no_upgrade_gets_no_backup() {
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    let (c, _) = clock(1);
    drop(Db::open(&p, c.clone()));
    drop(Db::open(&p, c));
    assert!(!bak(&p).exists());
}

#[test]
fn live_refuses_an_existing_database_without_metadata_and_touches_nothing() {
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    v4_database(&p);
    let before = std::fs::read(&p).unwrap();
    let (c, _) = clock(9);
    let db = Db::open_for(Live, &p, c);
    assert!(matches!(db.halt_reason(), Some(HaltReason::VenueMismatch(_))), "{:?}", db.halt_reason());
    drop(db);
    assert_eq!(std::fs::read(&p).unwrap(), before, "the unmarked database is not upgraded or stamped by a live process");
    assert_eq!(version_of(&p), 4);
    assert!(!bak(&p).exists(), "nothing was upgraded, so nothing was backed up");
}

#[test]
fn a_build_that_only_supports_schema_4_refuses_a_v5_database() {
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    let (c, _) = clock(1);
    drop(Db::open(&p, c.clone()));
    let before = std::fs::read(&p).unwrap();
    let old = Db::open_with(&p, c, &up_to_v4());
    assert_eq!(old.halt_reason(), Some(HaltReason::SchemaTooNew { found: 5, supported: 4 }));
    drop(old);
    assert_eq!(std::fs::read(&p).unwrap(), before);
}

#[test]
fn the_venue_cannot_be_changed_replaced_deleted_or_dropped_through_the_store_connection() {
    let dir = tempdir();
    let p = dir.path().join("funding.db");
    let (c, _) = clock(1);
    let db = Db::open_for(Demo, &p, c);
    for sql in [
        "UPDATE db_meta SET value = 'live'",
        "DELETE FROM db_meta",
        "INSERT OR REPLACE INTO db_meta (key, value) VALUES ('db_venue', 'live')",
        "INSERT INTO db_meta (key, value) VALUES ('db_venue', 'live')",
        "INSERT INTO db_meta (key, value) VALUES ('other', 'live')",
        "DROP TABLE db_meta",
        "DROP TRIGGER db_meta_no_update",
        "ALTER TABLE db_meta RENAME TO gone",
    ] {
        assert!(db.with_conn(|c| Ok(c.execute_batch(sql)?)).is_err(), "{sql} must be refused");
    }
    drop(db);
    assert_eq!(venue_of(&p), "demo");
}

#[test]
fn a_v5_database_whose_venue_row_is_missing_or_whose_guards_are_gone_halts() {
    // Row missing (tampered with a plain connection, guards dropped first).
    let dir = tempdir();
    let p = dir.path().join("a.db");
    let (c, _) = clock(1);
    drop(Db::open_for(Demo, &p, c.clone()));
    let raw = Connection::open(&p).unwrap();
    raw.execute_batch("DROP TRIGGER db_meta_no_delete; DELETE FROM db_meta;").unwrap();
    drop(raw);
    for identity in [Demo, Live] {
        let db = Db::open_for(identity, &p, c.clone());
        assert!(matches!(db.halt_reason(), Some(HaltReason::Corrupt(_)) | Some(HaltReason::VenueMismatch(_))), "{identity:?}: {:?}", db.halt_reason());
    }
    // Guard trigger missing: not trustworthy.
    let p2 = dir.path().join("b.db");
    drop(Db::open_for(Demo, &p2, c.clone()));
    Connection::open(&p2).unwrap().execute_batch("DROP TRIGGER db_meta_no_update;").unwrap();
    let db = Db::open_for(Demo, &p2, c);
    assert!(matches!(db.halt_reason(), Some(HaltReason::Corrupt(_))), "{:?}", db.halt_reason());
}

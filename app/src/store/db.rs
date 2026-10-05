//! Database handle: opening (WAL, pragmas, 0600 permissions), migrations and fail-closed halting.
//!
//! `Db::open` never panics and never returns an error: when the database cannot be used the
//! returned `Db` is in the *halted* state (it carries the reason) and refuses every read and
//! write. An existing database file is never overwritten, recreated or downgraded.

#![allow(dead_code)]

use std::fs::{DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use super::events::insert_event_on;
use super::schema::SCHEMA_V1;
use crate::ports::Clock;

/// One schema migration step. `version` must be strictly increasing within a list.
pub struct Migration {
    pub version: i64,
    pub sql: &'static str,
}

/// Every migration this build knows about; the last version is the highest supported schema.
pub const MIGRATIONS: &[Migration] = &[Migration { version: 1, sql: SCHEMA_V1 }];

const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// Why the store is halted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HaltReason {
    #[error("cannot open database: {0}")]
    CannotOpen(String),
    #[error("database is corrupt: {0}")]
    Corrupt(String),
    #[error("migration failed: {0}")]
    MigrationFailed(String),
    #[error("database schema version {found} is newer than the supported {supported}")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("cannot read config: {0}")]
    ConfigReadFailed(String),
    #[error("cannot read kill switch: {0}")]
    KillSwitchReadFailed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store is halted: {0}")]
    Halted(HaltReason),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config version conflict for {key}: expected {expected:?}, actual {actual:?}")]
    VersionConflict { key: String, expected: Option<i64>, actual: Option<i64> },
    #[error("duplicate client_order_id {0}")]
    DuplicateClientOrderId(String),
    #[error("order intent not found: {0}")]
    IntentNotFound(String),
    #[error("HOME is not set; cannot locate the default database path")]
    HomeNotSet,
}

/// A file whose permissions were wider than 0600 and have been tightened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tightened {
    pub path: PathBuf,
    pub old_mode: u32,
}

struct Inner {
    conn: Mutex<Option<Connection>>,
    halt: Mutex<Option<HaltReason>>,
    clock: Arc<dyn Clock>,
    path: PathBuf,
}

/// Cheap to clone; clones share one connection (serialised by a mutex).
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `$HOME/Library/Application Support/tong-funding/funding.db`.
pub fn default_db_path_from(home: Option<&str>) -> Result<PathBuf, StoreError> {
    let home = home.filter(|h| !h.is_empty()).ok_or(StoreError::HomeNotSet)?;
    Ok(PathBuf::from(home).join("Library/Application Support/tong-funding/funding.db"))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut o = path.as_os_str().to_owned();
    o.push(suffix);
    o.into()
}

/// If `path` exists and is wider than 0600, chmod it to 0600 and return its old mode.
fn tighten(path: &Path) -> std::io::Result<Option<u32>> {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mode = m.permissions().mode() & 0o777;
            if mode & !0o600 != 0 {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                Ok(Some(mode))
            } else {
                Ok(None)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Map an SQLite error met while opening/inspecting to the halt reason.
fn classify(e: rusqlite::Error) -> HaltReason {
    use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
    match &e {
        rusqlite::Error::SqliteFailure(f, _) if matches!(f.code, NotADatabase | DatabaseCorrupt) => HaltReason::Corrupt(e.to_string()),
        _ => HaltReason::CannotOpen(e.to_string()),
    }
}

fn migrate(conn: &mut Connection, migrations: &[Migration], current: i64) -> rusqlite::Result<()> {
    for m in migrations.iter().filter(|m| m.version > current) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(m.sql)?;
        tx.execute("UPDATE schema_version SET version = ?1", [m.version])?;
        tx.commit()?;
    }
    Ok(())
}

/// Open, verify and migrate. Order matters for fail-closed: a non-empty existing file is only
/// ever read (integrity, schema version) before anything that could write to it; WAL mode is
/// switched on last, and a fresh file is created 0600 before SQLite writes a single byte.
fn open_connection(path: &Path, migrations: &[Migration]) -> Result<(Connection, Vec<Tightened>), HaltReason> {
    let io = |e: std::io::Error| HaltReason::CannotOpen(e.to_string());

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(io)?;
    }
    let created = match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(io(e)),
    };
    let existing = !created && std::fs::metadata(path).map_err(io)?.len() > 0;

    let mut tightened = Vec::new();
    for p in [path.to_path_buf(), sidecar(path, "-wal"), sidecar(path, "-shm")] {
        if let Some(old_mode) = tighten(&p).map_err(io)? {
            tightened.push(Tightened { path: p, old_mode });
        }
    }

    let mut conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(classify)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(classify)?;
    conn.pragma_update(None, "foreign_keys", true).map_err(classify)?;
    // Without this, INSERT OR REPLACE can delete an event without firing the DELETE trigger.
    conn.pragma_update(None, "recursive_triggers", true).map_err(classify)?;

    let supported = migrations.last().map_or(0, |m| m.version);
    let mut current = 0;
    if existing {
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(classify)?;
        if check != "ok" {
            return Err(HaltReason::Corrupt(format!("quick_check: {check}")));
        }
        let has_version_table: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'", [], |r| r.get(0))
            .map_err(classify)?;
        if has_version_table == 0 {
            return Err(HaltReason::Corrupt("not a tong-funding database (no schema_version table)".into()));
        }
        current = conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0)).map_err(classify)?;
        if current > supported {
            return Err(HaltReason::SchemaTooNew { found: current, supported });
        }
    }

    migrate(&mut conn, migrations, current).map_err(|e| HaltReason::MigrationFailed(e.to_string()))?;

    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0)).map_err(classify)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(HaltReason::CannotOpen(format!("could not enable WAL (journal_mode = {mode})")));
    }
    // Touch the database once so the -wal/-shm files exist, then make them 0600 as well.
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(classify)?;
    for suffix in ["-wal", "-shm"] {
        tighten(&sidecar(path, suffix)).map_err(io)?;
    }
    Ok((conn, tightened))
}

impl Db {
    /// Open the database at the default path (reads the `HOME` environment variable).
    pub fn open_default(clock: Arc<dyn Clock>) -> Result<Db, StoreError> {
        let home = std::env::var("HOME").ok();
        Ok(Db::open(&default_db_path_from(home.as_deref())?, clock))
    }

    pub fn open(path: &Path, clock: Arc<dyn Clock>) -> Db {
        Db::open_with(path, clock, MIGRATIONS)
    }

    /// Like [`Db::open`] with an explicit migration list (tests inject failing migrations).
    pub fn open_with(path: &Path, clock: Arc<dyn Clock>, migrations: &[Migration]) -> Db {
        match open_connection(path, migrations) {
            Ok((conn, tightened)) => {
                let db = Db::from_parts(Some(conn), None, clock, path);
                for t in tightened {
                    let ts = db.now_ms();
                    let payload = serde_json::json!({
                        "path": t.path.display().to_string(),
                        "old_mode": format!("{:o}", t.old_mode),
                        "new_mode": "600",
                    });
                    let r = db.with_conn(|c| Ok(insert_event_on(c, ts, "PERMISSIONS_TIGHTENED", None, &payload)?));
                    if let Err(e) = r {
                        eprintln!("could not record PERMISSIONS_TIGHTENED: {e}");
                    }
                }
                db
            }
            Err(reason) => Db::from_parts(None, Some(reason), clock, path),
        }
    }

    fn from_parts(conn: Option<Connection>, halt: Option<HaltReason>, clock: Arc<dyn Clock>, path: &Path) -> Db {
        Db { inner: Arc::new(Inner { conn: Mutex::new(conn), halt: Mutex::new(halt), clock, path: path.to_path_buf() }) }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn now_ms(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    /// `Some(reason)` once the store has halted (sticky for the life of this handle).
    pub fn halt_reason(&self) -> Option<HaltReason> {
        lock(&self.inner.halt).clone()
    }

    pub fn is_halted(&self) -> bool {
        self.halt_reason().is_some()
    }

    /// Enter the halted state; the first reason wins.
    pub(crate) fn halt(&self, reason: HaltReason) {
        let mut h = lock(&self.inner.halt);
        if h.is_none() {
            *h = Some(reason);
        }
    }

    /// Run `f` on the connection unless the store is halted.
    pub(crate) fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T, StoreError>) -> Result<T, StoreError> {
        if let Some(r) = self.halt_reason() {
            return Err(StoreError::Halted(r));
        }
        let mut guard = lock(&self.inner.conn);
        match guard.as_mut() {
            Some(c) => f(c),
            None => Err(StoreError::Halted(HaltReason::CannotOpen("no database connection".into()))),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::ports::ManualClock;

    pub fn clock(ms: i64) -> (Arc<dyn Clock>, ManualClock) {
        let c = ManualClock::new(ms);
        (Arc::new(c.clone()), c)
    }

    /// A fresh database inside a temp directory (never a real path).
    pub fn open_tmp() -> (tempfile::TempDir, Db, ManualClock) {
        let dir = tempfile::tempdir().unwrap();
        let (c, m) = clock(1_000_000);
        let db = Db::open(&dir.path().join("funding.db"), c);
        assert!(!db.is_halted(), "fresh db must open: {:?}", db.halt_reason());
        (dir, db, m)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }
    fn sidecar(p: &Path, s: &str) -> PathBuf {
        let mut o = p.as_os_str().to_owned();
        o.push(s);
        o.into()
    }
    fn table_exists(db: &Db, name: &str) -> bool {
        db.with_conn(|c| {
            Ok(c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = ?1", [name], |r| r.get::<_, i64>(0))? > 0)
        })
        .unwrap()
    }
    fn schema_version(path: &Path) -> i64 {
        let c = Connection::open(path).unwrap();
        c.query_row("SELECT version FROM schema_version", [], |r| r.get(0)).unwrap()
    }
    /// A database with some events so the file spans several pages.
    fn populated(path: &Path) {
        let (c, _) = clock(5);
        let db = Db::open(path, c);
        assert!(!db.is_halted());
        db.with_conn(|conn| {
            for i in 0..400 {
                conn.execute(
                    "INSERT INTO events (ts_ms, event_type, payload) VALUES (?1, 'X', ?2)",
                    rusqlite::params![i, format!("{{\"pad\":\"{}\"}}", "x".repeat(100))],
                )?;
            }
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn default_path_is_under_application_support() {
        assert_eq!(
            default_db_path_from(Some("/Users/a")).unwrap(),
            PathBuf::from("/Users/a/Library/Application Support/tong-funding/funding.db")
        );
        assert!(matches!(default_db_path_from(None), Err(StoreError::HomeNotSet)));
    }

    #[test]
    fn fresh_database_is_created_with_schema_v1() {
        let (dir, db, _) = open_tmp();
        assert!(!db.is_halted());
        for t in ["events", "pairs", "order_intents", "config", "system_flags", "portfolio_history", "schema_version"] {
            assert!(table_exists(&db, t), "missing table {t}");
        }
        drop(db);
        assert_eq!(schema_version(&dir.path().join("funding.db")), 1);
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _) = clock(1);
        let db = Db::open(&dir.path().join("a/b/funding.db"), c);
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        assert!(dir.path().join("a/b/funding.db").exists());
    }

    #[test]
    fn connection_uses_wal_foreign_keys_busy_timeout_and_recursive_triggers() {
        let (_d, db, _) = open_tmp();
        let (jm, fk, rt, bt): (String, i64, i64, i64) = db
            .with_conn(|c| {
                Ok((
                    c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
                    c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?,
                    c.query_row("PRAGMA recursive_triggers", [], |r| r.get(0))?,
                    c.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?,
                ))
            })
            .unwrap();
        assert_eq!((jm.as_str(), fk, rt, bt), ("wal", 1, 1, 5_000));
    }

    #[test]
    fn insert_or_replace_cannot_bypass_the_event_delete_trigger() {
        let (_d, db, _) = open_tmp();
        db.with_conn(|c| {
            c.execute("INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (1,'A','{}','h')", [])?;
            Ok(())
        })
        .unwrap();
        let r = db.with_conn(|c| {
            c.execute("INSERT OR REPLACE INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (2,'B','{}','h')", [])?;
            Ok(())
        });
        assert!(r.is_err());
    }

    #[test]
    fn new_database_files_are_0600() {
        let (dir, db, _) = open_tmp();
        let p = dir.path().join("funding.db");
        assert_eq!(mode(&p), 0o600);
        assert_eq!(mode(&sidecar(&p, "-wal")), 0o600);
        assert_eq!(mode(&sidecar(&p, "-shm")), 0o600);
        drop(db);
    }

    #[test]
    fn wide_main_file_permissions_are_tightened_and_an_event_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(!db.is_halted());
        assert_eq!(mode(&p), 0o600);
        let (n, payload): (i64, String) = db
            .with_conn(|c| {
                Ok((
                    c.query_row("SELECT COUNT(*) FROM events WHERE event_type = 'PERMISSIONS_TIGHTENED'", [], |r| r.get(0))?,
                    c.query_row("SELECT payload FROM events WHERE event_type = 'PERMISSIONS_TIGHTENED'", [], |r| r.get(0))?,
                ))
            })
            .unwrap();
        assert_eq!(n, 1);
        assert!(payload.contains("644") && payload.contains("600"), "{payload}");
    }

    #[test]
    fn wide_wal_and_shm_permissions_are_tightened_too() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        // Keep the WAL files alive by holding a second connection across the reopen.
        let keep = Connection::open(&p).unwrap();
        keep.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)).unwrap();
        keep.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'K','{}')", []).unwrap();
        let (wal, shm) = (sidecar(&p, "-wal"), sidecar(&p, "-shm"));
        assert!(wal.exists() && shm.exists());
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o666)).unwrap();
        std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o666)).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(!db.is_halted());
        assert_eq!((mode(&wal), mode(&shm)), (0o600, 0o600));
        drop(keep);
    }

    #[test]
    fn correct_permissions_write_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        let n: i64 = db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events WHERE event_type = 'PERMISSIONS_TIGHTENED'", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn schema_newer_than_the_program_halts_and_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        Connection::open(&p).unwrap().execute("UPDATE schema_version SET version = 2", []).unwrap();
        let before = std::fs::read(&p).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert_eq!(db.halt_reason(), Some(HaltReason::SchemaTooNew { found: 2, supported: 1 }));
        drop(db);
        assert_eq!(std::fs::read(&p).unwrap(), before, "file must not be modified");
        assert_eq!(schema_version(&p), 2, "no downgrade");
    }

    #[test]
    fn truncated_database_halts_and_the_original_bytes_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        let full = std::fs::read(&p).unwrap();
        assert!(full.len() > 20_000, "fixture should span many pages: {}", full.len());
        std::fs::write(&p, &full[..5_000]).unwrap();
        let before = std::fs::read(&p).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(db.is_halted(), "a truncated database must halt");
        assert!(matches!(db.halt_reason(), Some(HaltReason::Corrupt(_)) | Some(HaltReason::CannotOpen(_))), "{:?}", db.halt_reason());
        drop(db);
        assert_eq!(std::fs::read(&p).unwrap(), before, "original bytes must be untouched");
    }

    #[test]
    fn non_sqlite_garbage_halts_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"this is definitely not a sqlite database, just text".repeat(100)).unwrap();
        let before = std::fs::read(&p).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(matches!(db.halt_reason(), Some(HaltReason::Corrupt(_))), "{:?}", db.halt_reason());
        drop(db);
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    #[test]
    fn a_valid_sqlite_file_that_is_not_ours_halts_instead_of_being_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        Connection::open(&p).unwrap().execute_batch("CREATE TABLE other (x INTEGER); INSERT INTO other VALUES (7);").unwrap();
        let before = std::fs::read(&p).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(matches!(db.halt_reason(), Some(HaltReason::Corrupt(_))), "{:?}", db.halt_reason());
        drop(db);
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    #[test]
    fn halted_db_refuses_all_access() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(db.is_halted());
        let r = db.with_conn(|_| Ok(()));
        assert!(matches!(r, Err(StoreError::Halted(_))));
    }

    const GOOD_V2: &str = "CREATE TABLE v2_added (x INTEGER);";
    const BAD_V2: &str = "CREATE TABLE half_applied (x INTEGER); THIS IS NOT SQL;";

    fn migrations(v2: &'static str) -> Vec<Migration> {
        vec![Migration { version: 1, sql: SCHEMA_V1 }, Migration { version: 2, sql: v2 }]
    }

    #[test]
    fn migration_framework_upgrades_an_older_database() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        let (c, _) = clock(9);
        let db = Db::open_with(&p, c, &migrations(GOOD_V2));
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        assert!(table_exists(&db, "v2_added"));
        let n: i64 = db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events WHERE event_type='X'", [], |r| r.get(0))?)).unwrap();
        assert_eq!(n, 400, "existing data preserved");
        drop(db);
        assert_eq!(schema_version(&p), 2);
    }

    #[test]
    fn failed_migration_on_an_existing_database_halts_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        let before = std::fs::read(&p).unwrap();
        let (c, _) = clock(9);
        let db = Db::open_with(&p, c, &migrations(BAD_V2));
        assert!(matches!(db.halt_reason(), Some(HaltReason::MigrationFailed(_))), "{:?}", db.halt_reason());
        drop(db);
        assert_eq!(schema_version(&p), 1);
        let c = Connection::open(&p).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='half_applied'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "no half-applied migration");
        drop(c);
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    #[test]
    fn failed_migration_on_a_fresh_database_halts_and_leaves_it_empty_and_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        let bad = vec![Migration { version: 1, sql: "CREATE TABLE half (x INTEGER); NOT SQL;" }];
        let (c, _) = clock(9);
        let db = Db::open_with(&p, c.clone(), &bad);
        assert!(matches!(db.halt_reason(), Some(HaltReason::MigrationFailed(_))), "{:?}", db.halt_reason());
        drop(db);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0, "fresh file stays empty after a failed migration");
        let db = Db::open(&p, c);
        assert!(!db.is_halted(), "a later start with working migrations succeeds: {:?}", db.halt_reason());
    }

    #[test]
    fn reopening_an_existing_database_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("funding.db");
        populated(&p);
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(!db.is_halted());
        let n: i64 = db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)).unwrap();
        assert_eq!(n, 400);
    }
}

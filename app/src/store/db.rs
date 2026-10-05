//! Database handle: opening (WAL, pragmas, 0600 permissions), migrations and fail-closed halting.
//!
//! `Db::open` never panics and never returns an error: when the database cannot be used the
//! returned `Db` is in the *halted* state (it carries the reason) and refuses every read and
//! write. An existing database file is never overwritten, recreated or downgraded.

#![allow(dead_code)]

use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
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
    #[error("event write failed: {0}")]
    EventWriteFailed(String),
    #[error("database is already open in another instance: {0}")]
    AlreadyOpen(String),
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
    #[error("illegal order intent transition {from} -> {to}")]
    IllegalIntentTransition { from: String, to: String },
    #[error("flag {0:?} is reserved and cannot be written through flag_set")]
    ReservedFlag(String),
}

/// A file whose permissions were wider than 0600 and have been tightened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tightened {
    pub path: PathBuf,
    pub old_mode: u32,
    pub new_mode: u32,
}

struct Inner {
    conn: Mutex<Option<Connection>>,
    halt: Mutex<Option<HaltReason>>,
    clock: Arc<dyn Clock>,
    path: PathBuf,
    /// Single-instance lock; released when the last clone of the handle is dropped.
    _lock: Option<File>,
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

/// If `path` exists and has any permission bit outside `max_mode`, chmod it to `max_mode` and
/// return its old mode. Never follows a symlink (callers reject those first).
fn tighten(path: &Path, max_mode: u32) -> std::io::Result<Option<u32>> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            let mode = m.permissions().mode() & 0o777;
            if mode & !max_mode != 0 {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(max_mode))?;
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

/// Everything `open_connection` hands back on success.
struct Opened {
    conn: Connection,
    tightened: Vec<Tightened>,
    /// Held for the life of the handle: the single-instance lock on `<db>.lock`.
    lock: Option<File>,
}

/// Refuse to follow a symlink (a planted link could redirect writes elsewhere).
fn reject_symlink(path: &Path) -> Result<(), HaltReason> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(HaltReason::CannotOpen(format!("{} is a symlink; refusing to follow it", path.display())))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HaltReason::CannotOpen(e.to_string())),
    }
}

/// Take the exclusive advisory lock on `<db>.lock` (released when the returned file is dropped).
fn acquire_lock(path: &Path) -> Result<File, HaltReason> {
    let lock_path = sidecar(path, ".lock");
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| HaltReason::CannotOpen(format!("cannot open lock file {}: {e}", lock_path.display())))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(HaltReason::AlreadyOpen(format!("{} is locked by another instance", lock_path.display())))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(HaltReason::CannotOpen(format!("cannot lock {}: {e}", lock_path.display()))),
    }
}

/// Triggers and indexes the guarantees rest on; a database missing one is not trustworthy.
const REQUIRED_OBJECTS: &[(&str, &str)] = &[
    ("trigger", "events_no_update"),
    ("trigger", "events_no_delete"),
    ("trigger", "events_no_overwrite"),
    ("index", "uniq_prepared_symbol"),
    ("index", "sqlite_autoindex_events_1"),
];

fn verify_required_objects(conn: &Connection) -> Result<(), HaltReason> {
    for (kind, name) in REQUIRED_OBJECTS {
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2", [kind, name], |r| r.get(0))
            .map_err(classify)?;
        if n == 0 {
            return Err(HaltReason::Corrupt(format!("required {kind} {name} is missing")));
        }
    }
    Ok(())
}

/// What the store's own connection refuses to do: weaken the `events` guarantees.
/// Installed after migrations (which legitimately alter schema) and before any use.
fn authorize(action: AuthAction<'_>) -> Authorization {
    let is_events = |t: &str| t.eq_ignore_ascii_case("events");
    match action {
        AuthAction::DropTable { table_name }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::DropTrigger { table_name, .. }
        | AuthAction::DropIndex { table_name, .. }
            if is_events(table_name) =>
        {
            Authorization::Deny
        }
        AuthAction::DropIndex { index_name, .. } if index_name == "uniq_prepared_symbol" => Authorization::Deny,
        // Reading these pragmas is fine; setting them is not.
        AuthAction::Pragma { pragma_name, pragma_value: Some(_) }
            if ["ignore_check_constraints", "writable_schema", "recursive_triggers"]
                .iter()
                .any(|p| pragma_name.eq_ignore_ascii_case(p)) =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}

/// Open, verify and migrate. Order matters for fail-closed: an existing file is only ever read
/// (read-only connection: integrity, schema version, required triggers) before anything that
/// could write to it; no connection used for inspection checkpoints on close, so a halted open
/// leaves the main file and the `-wal` byte-identical. A fresh file is created 0600 before SQLite
/// writes a byte, and removed again if the open fails.
fn open_connection(path: &Path, migrations: &[Migration], take_lock: bool) -> Result<Opened, HaltReason> {
    let io = |e: std::io::Error| HaltReason::CannotOpen(e.to_string());

    let mut tightened = Vec::new();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(io)?;
        if let Some(old_mode) = tighten(parent, 0o700).map_err(io)? {
            tightened.push(Tightened { path: parent.to_path_buf(), old_mode, new_mode: 0o700 });
        }
    }
    for p in [path.to_path_buf(), sidecar(path, "-wal"), sidecar(path, "-shm"), sidecar(path, ".lock")] {
        reject_symlink(&p)?;
    }
    let lock = if take_lock { Some(acquire_lock(path)?) } else { None };

    let existing = match std::fs::symlink_metadata(path) {
        Ok(m) if !m.is_file() => return Err(HaltReason::CannotOpen(format!("{} is not a regular file", path.display()))),
        Ok(m) if m.len() == 0 => {
            return Err(HaltReason::Corrupt(format!(
                "資料庫檔為空，拒絕初始化（{}）；若確實要重建請手動刪除 (database file is empty, refusing to initialise it)",
                path.display()
            )));
        }
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(io(e)),
    };
    if !existing {
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
            Ok(_) => {}
            Err(e) => return Err(io(e)),
        }
    }

    match open_inner(path, migrations, existing, &mut tightened) {
        Ok(conn) => Ok(Opened { conn, tightened, lock }),
        Err(reason) => {
            if !existing {
                // This open created the file: never leave a zero-byte (or half-built) database behind.
                for p in [path.to_path_buf(), sidecar(path, "-wal"), sidecar(path, "-shm")] {
                    let _ = std::fs::remove_file(p);
                }
            }
            Err(reason)
        }
    }
}

fn open_inner(path: &Path, migrations: &[Migration], existing: bool, tightened: &mut Vec<Tightened>) -> Result<Connection, HaltReason> {
    let io = |e: std::io::Error| HaltReason::CannotOpen(e.to_string());
    let no_ckpt = rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE;

    for p in [path.to_path_buf(), sidecar(path, "-wal"), sidecar(path, "-shm")] {
        if let Some(old_mode) = tighten(&p, 0o600).map_err(io)? {
            tightened.push(Tightened { path: p, old_mode, new_mode: 0o600 });
        }
    }

    let supported = migrations.last().map_or(0, |m| m.version);
    let mut current = 0;
    if existing {
        let ro = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(classify)?;
        ro.set_db_config(no_ckpt, true).map_err(classify)?;
        ro.busy_timeout(BUSY_TIMEOUT).map_err(classify)?;
        let check: String = ro.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(classify)?;
        if check != "ok" {
            return Err(HaltReason::Corrupt(format!("quick_check: {check}")));
        }
        let has_version_table: i64 = ro
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'", [], |r| r.get(0))
            .map_err(classify)?;
        if has_version_table == 0 {
            return Err(HaltReason::Corrupt("not a tong-funding database (no schema_version table)".into()));
        }
        current = ro.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0)).map_err(classify)?;
        if current > supported {
            return Err(HaltReason::SchemaTooNew { found: current, supported });
        }
        verify_required_objects(&ro)?;
    }

    let mut conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(classify)?;
    // Until the open has fully succeeded, closing this connection must not checkpoint.
    conn.set_db_config(no_ckpt, true).map_err(classify)?;
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true).map_err(classify)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(classify)?;
    conn.pragma_update(None, "foreign_keys", true).map_err(classify)?;
    // Belt and braces next to the `events_no_overwrite` trigger: REPLACE must fire the DELETE trigger.
    conn.pragma_update(None, "recursive_triggers", true).map_err(classify)?;
    // On macOS `synchronous` alone does not issue F_FULLFSYNC; without it a power cut can lose
    // an intent that was already reported as persisted.
    conn.pragma_update(None, "fullfsync", true).map_err(classify)?;
    conn.pragma_update(None, "checkpoint_fullfsync", true).map_err(classify)?;

    migrate(&mut conn, migrations, current).map_err(|e| HaltReason::MigrationFailed(e.to_string()))?;
    verify_required_objects(&conn)?;

    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0)).map_err(classify)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(HaltReason::CannotOpen(format!("could not enable WAL (journal_mode = {mode})")));
    }
    // Touch the database once so the -wal/-shm files exist, then make them 0600 as well.
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)).map_err(classify)?;
    for suffix in ["-wal", "-shm"] {
        tighten(&sidecar(path, suffix), 0o600).map_err(io)?;
    }
    conn.authorizer(Some(|ctx: AuthContext<'_>| authorize(ctx.action))).map_err(classify)?;
    // Success: from here on a normal close may checkpoint.
    conn.set_db_config(no_ckpt, false).map_err(classify)?;
    Ok(conn)
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

    /// Test-only: like [`Db::open`] but without the single-instance lock, so a test can hold
    /// two handles on one file (to exercise contention between connections).
    #[cfg(test)]
    pub(crate) fn open_unlocked(path: &Path, clock: Arc<dyn Clock>) -> Db {
        Db::open_impl(path, clock, MIGRATIONS, false)
    }

    /// Like [`Db::open`] with an explicit migration list (tests inject failing migrations).
    pub fn open_with(path: &Path, clock: Arc<dyn Clock>, migrations: &[Migration]) -> Db {
        Db::open_impl(path, clock, migrations, true)
    }

    fn open_impl(path: &Path, clock: Arc<dyn Clock>, migrations: &[Migration], take_lock: bool) -> Db {
        match open_connection(path, migrations, take_lock) {
            Ok(Opened { conn, tightened, lock }) => {
                let db = Db::from_parts(Some(conn), None, clock, path, lock);
                for t in tightened {
                    let ts = db.now_ms();
                    let payload = serde_json::json!({
                        "path": t.path.display().to_string(),
                        "old_mode": format!("{:o}", t.old_mode),
                        "new_mode": format!("{:o}", t.new_mode),
                    });
                    let r = db.with_conn(|c| Ok(insert_event_on(c, ts, "PERMISSIONS_TIGHTENED", None, &payload)?));
                    if let Err(e) = r {
                        eprintln!("could not record PERMISSIONS_TIGHTENED: {e}");
                    }
                }
                db
            }
            Err(reason) => Db::from_parts(None, Some(reason), clock, path, None),
        }
    }

    fn from_parts(conn: Option<Connection>, halt: Option<HaltReason>, clock: Arc<dyn Clock>, path: &Path, lock: Option<File>) -> Db {
        Db {
            inner: Arc::new(Inner { conn: Mutex::new(conn), halt: Mutex::new(halt), clock, path: path.to_path_buf(), _lock: lock }),
        }
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
    ///
    /// RISK: `f` gets the raw connection, so it can run arbitrary SQL against every table
    /// (including `events`). The connection's authorizer refuses the worst (dropping `events`,
    /// its triggers/indexes, `ALTER TABLE events`, `ignore_check_constraints`, ...) but this is
    /// still a back door around the typed API: keep it `pub(crate)`, never expose it outside this
    /// crate, and prefer adding a typed method over calling it from feature code.
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

    /// A temp directory that already has the 0700 mode the store insists on (some platforms
    /// create temp dirs 0755, which would trigger a PERMISSIONS_TIGHTENED event).
    pub fn tempdir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        d
    }

    pub fn clock(ms: i64) -> (Arc<dyn Clock>, ManualClock) {
        let c = ManualClock::new(ms);
        (Arc::new(c.clone()), c)
    }

    /// Every value of every column of every table, as one string (to prove a secret is nowhere).
    pub fn dump_db(db: &Db) -> String {
        db.with_conn(|c| {
            let mut out = String::new();
            let tables: Vec<String> = {
                let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<_, _>>()?
            };
            for t in tables {
                let mut st = c.prepare(&format!("SELECT * FROM \"{t}\""))?;
                let n = st.column_count();
                let mut rows = st.query([])?;
                while let Some(r) = rows.next()? {
                    for i in 0..n {
                        let v = match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                            other => format!("{other:?}"),
                        };
                        out.push_str(&format!("{t}[{i}]={v}\n"));
                    }
                }
            }
            Ok(out)
        })
        .unwrap()
    }

    /// Whether the raw bytes of the main file or its `-wal` contain `needle`.
    pub fn files_contain(path: &Path, needle: &str) -> bool {
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        [path.to_path_buf(), PathBuf::from(wal)]
            .iter()
            .filter_map(|p| std::fs::read(p).ok())
            .any(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
    }

    /// A fresh database inside a temp directory (never a real path).
    pub fn open_tmp() -> (tempfile::TempDir, Db, ManualClock) {
        let dir = tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
        let dir = crate::store::db::test_support::tempdir();
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
    fn failed_migration_on_a_fresh_database_halts_removes_the_file_and_is_retryable() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        let bad = vec![Migration { version: 1, sql: "CREATE TABLE half (x INTEGER); NOT SQL;" }];
        let (c, _) = clock(9);
        let db = Db::open_with(&p, c.clone(), &bad);
        assert!(matches!(db.halt_reason(), Some(HaltReason::MigrationFailed(_))), "{:?}", db.halt_reason());
        drop(db);
        assert!(!p.exists(), "a fresh file made by this open is removed after a failed migration");
        let db = Db::open(&p, c);
        assert!(!db.is_halted(), "a later start with working migrations succeeds: {:?}", db.halt_reason());
    }

    #[test]
    fn reopening_an_existing_database_keeps_its_data() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        populated(&p);
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(!db.is_halted());
        let n: i64 = db.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)).unwrap();
        assert_eq!(n, 400);
    }

    // ---- hardening (review round 1) ----

    fn bytes_of(p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap_or_default()
    }

    /// A v1 database whose newest data sits only in the `-wal` (as after a crash), schema bumped to 2.
    fn crashed_v2_with_wal(p: &Path) {
        populated(p);
        let c = Connection::open(p).unwrap();
        c.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true).unwrap();
        c.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)).unwrap();
        c.execute("UPDATE schema_version SET version = 2", []).unwrap();
        drop(c);
        assert!(std::fs::metadata(sidecar(p, "-wal")).unwrap().len() > 0, "fixture must leave a -wal behind");
    }

    #[test]
    fn halting_on_a_too_new_schema_does_not_rewrite_the_main_file_or_the_wal() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        crashed_v2_with_wal(&p);
        let (main_before, wal_before) = (bytes_of(&p), bytes_of(&sidecar(&p, "-wal")));
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert_eq!(db.halt_reason(), Some(HaltReason::SchemaTooNew { found: 2, supported: 1 }));
        drop(db);
        assert!(bytes_of(&p) == main_before, "main file must be byte-identical");
        assert!(bytes_of(&sidecar(&p, "-wal")) == wal_before, "-wal must be byte-identical (and still exist)");
    }

    #[test]
    fn an_existing_zero_byte_file_halts_and_is_not_initialised() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"").unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        match db.halt_reason() {
            Some(HaltReason::Corrupt(m)) => assert!(m.contains("手動刪除"), "{m}"),
            other => panic!("expected Corrupt, got {other:?}"),
        }
        drop(db);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0, "must not be initialised");
    }

    #[test]
    fn a_failed_migration_on_a_file_created_by_this_open_deletes_it() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        let bad = vec![Migration { version: 1, sql: "CREATE TABLE half (x INTEGER); NOT SQL;" }];
        let (c, _) = clock(9);
        let db = Db::open_with(&p, c.clone(), &bad);
        assert!(matches!(db.halt_reason(), Some(HaltReason::MigrationFailed(_))));
        drop(db);
        assert!(!p.exists(), "no zero-byte leftover");
        let db = Db::open(&p, c);
        assert!(!db.is_halted(), "retry works: {:?}", db.halt_reason());
    }

    #[test]
    fn a_symlinked_database_path_halts_and_the_target_is_untouched() {
        let dir = crate::store::db::test_support::tempdir();
        let target = dir.path().join("elsewhere.bin");
        std::fs::write(&target, b"").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let p = dir.path().join("funding.db");
        std::os::unix::fs::symlink(&target, &p).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&p, c);
        assert!(matches!(db.halt_reason(), Some(HaltReason::CannotOpen(m)) if m.contains("symlink")), "{:?}", db.halt_reason());
        drop(db);
        assert_eq!(bytes_of(&target), b"");
        assert_eq!(mode(&target), 0o644);
    }

    #[test]
    fn a_symlinked_wal_or_shm_halts() {
        for suffix in ["-wal", "-shm"] {
            let dir = crate::store::db::test_support::tempdir();
            let p = dir.path().join("funding.db");
            populated(&p);
            let target = dir.path().join("elsewhere.bin");
            std::fs::write(&target, b"").unwrap();
            let _ = std::fs::remove_file(sidecar(&p, suffix));
            std::os::unix::fs::symlink(&target, sidecar(&p, suffix)).unwrap();
            let (c, _) = clock(9);
            let db = Db::open(&p, c);
            assert!(matches!(db.halt_reason(), Some(HaltReason::CannotOpen(_))), "{suffix}: {:?}", db.halt_reason());
            assert_eq!(bytes_of(&target), b"", "{suffix}: target untouched");
        }
    }

    #[test]
    fn a_too_wide_parent_directory_is_tightened_to_0700_with_an_event() {
        let dir = crate::store::db::test_support::tempdir();
        let parent = dir.path().join("data");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (c, _) = clock(9);
        let db = Db::open(&parent.join("funding.db"), c);
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        assert_eq!(mode(&parent), 0o700);
        let payload: String = db
            .with_conn(|c| {
                Ok(c.query_row("SELECT payload FROM events WHERE event_type = 'PERMISSIONS_TIGHTENED'", [], |r| r.get(0))?)
            })
            .unwrap();
        assert!(payload.contains("755") && payload.contains("data"), "{payload}");
    }

    #[test]
    fn replace_with_an_explicit_id_cannot_overwrite_an_event_even_on_a_plain_connection() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        {
            let (c, _) = clock(1);
            let db = Db::open(&p, c);
            db.with_conn(|c| Ok(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'ORIGINAL','{}')", [])?)).unwrap();
        }
        // No recursive_triggers, no foreign_keys: the weakest possible connection.
        let plain = Connection::open(&p).unwrap();
        let r = plain.execute("REPLACE INTO events (id, ts_ms, event_type, payload) VALUES (1, 2, 'FORGED', '{}')", []);
        assert!(r.is_err(), "REPLACE must fail");
        let ty: String = plain.query_row("SELECT event_type FROM events WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(ty, "ORIGINAL");
        // Normal AUTOINCREMENT inserts still work on the same connection.
        plain.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (3,'NEXT','{}')", []).unwrap();
    }

    #[test]
    fn opening_a_database_missing_a_required_trigger_or_index_halts() {
        for (kind, name) in [
            ("TRIGGER", "events_no_delete"),
            ("TRIGGER", "events_no_update"),
            ("TRIGGER", "events_no_overwrite"),
            ("INDEX", "uniq_prepared_symbol"),
        ] {
            let dir = crate::store::db::test_support::tempdir();
            let p = dir.path().join("funding.db");
            populated(&p);
            Connection::open(&p).unwrap().execute_batch(&format!("DROP {kind} {name}")).unwrap();
            let (c, _) = clock(9);
            let db = Db::open(&p, c);
            assert!(matches!(db.halt_reason(), Some(HaltReason::Corrupt(m)) if m.contains(name)), "{name}: {:?}", db.halt_reason());
        }
    }

    #[test]
    fn the_authorizer_blocks_dropping_or_weakening_the_events_guarantees() {
        let (_d, db, _) = open_tmp();
        for sql in [
            "DROP TRIGGER events_no_delete",
            "DROP TRIGGER events_no_update",
            "DROP TRIGGER events_no_overwrite",
            "DROP TABLE events",
            "DROP INDEX idx_events_type_ts",
            "DROP INDEX uniq_prepared_symbol",
            "ALTER TABLE events ADD COLUMN x TEXT",
            "ALTER TABLE events RENAME TO events_old",
            "PRAGMA ignore_check_constraints = ON",
            "PRAGMA writable_schema = ON",
            "PRAGMA recursive_triggers = OFF",
        ] {
            let r = db.with_conn(|c| Ok(c.execute_batch(sql)?));
            let e = r.expect_err(sql).to_string();
            assert!(e.contains("not authorized") || e.contains("authoriz"), "{sql}: {e}");
        }
        // Everything is still intact.
        assert!(table_exists(&db, "events"));
        let n: i64 = db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='events'", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn the_authorizer_does_not_get_in_the_way_of_normal_work() {
        let (_d, db, _) = open_tmp();
        db.with_conn(|c| {
            c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'A','{}')", [])?;
            c.execute("INSERT INTO pairs (internal_uuid, pair_id, symbol, status, entry_json, created_ms, updated_ms) VALUES ('u','p','BTC','PREPARED','{}',1,1)", [])?;
            c.execute("UPDATE pairs SET status = 'ORDER_SUBMIT' WHERE internal_uuid = 'u'", [])?;
            c.execute("INSERT INTO order_intents (client_order_id, pair_uuid, leg, exchange, symbol, side, quantity, state, created_ms, updated_ms) VALUES ('c','u','long','B','BTC','BUY','1','INTENDED',1,1)", [])?;
            c.execute("UPDATE order_intents SET state = 'SUBMITTED' WHERE client_order_id = 'c'", [])?;
            // Reading pragmas is fine; only changing the protected ones is not.
            let _: i64 = c.query_row("PRAGMA recursive_triggers", [], |r| r.get(0))?;
            // Fault injection by a test (CREATE TRIGGER) must stay possible.
            c.execute_batch("CREATE TRIGGER t_inject BEFORE INSERT ON events WHEN NEW.event_type='BOOM' BEGIN SELECT RAISE(ABORT,'boom'); END")?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migrations_run_before_the_authorizer_is_installed() {
        // A migration that alters events must still work (the authorizer comes after migration).
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        populated(&p);
        let (c, _) = clock(9);
        let db = Db::open_with(&p, c, &migrations("ALTER TABLE events ADD COLUMN extra TEXT;"));
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
    }

    #[test]
    fn durability_pragmas_are_on() {
        let (_d, db, _) = open_tmp();
        let (ff, cff): (i64, i64) = db
            .with_conn(|c| Ok((c.query_row("PRAGMA fullfsync", [], |r| r.get(0))?, c.query_row("PRAGMA checkpoint_fullfsync", [], |r| r.get(0))?)))
            .unwrap();
        assert_eq!((ff, cff), (1, 1));
    }

    #[test]
    fn a_second_instance_on_the_same_path_is_refused_until_the_first_is_dropped() {
        let dir = crate::store::db::test_support::tempdir();
        let p = dir.path().join("funding.db");
        let (c, _) = clock(1);
        let first = Db::open(&p, c.clone());
        assert!(!first.is_halted());
        let second = Db::open(&p, c.clone());
        assert!(matches!(second.halt_reason(), Some(HaltReason::AlreadyOpen(_))), "{:?}", second.halt_reason());
        assert!(second.with_conn(|_| Ok(())).is_err());
        // The refused open must not have damaged the first instance.
        assert!(first.with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))?)).is_ok());
        drop(second);
        drop(first);
        let third = Db::open(&p, c);
        assert!(!third.is_halted(), "released lock lets a new instance in: {:?}", third.halt_reason());
    }

    #[test]
    fn clones_of_one_handle_share_the_lock_and_do_not_conflict() {
        let (_d, db, _) = open_tmp();
        let clone = db.clone();
        assert!(clone.with_conn(|_| Ok(())).is_ok());
    }
}

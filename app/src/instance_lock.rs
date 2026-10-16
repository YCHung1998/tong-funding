//! Single-instance lock (spec live-process, "同一時間只能有一個程式實例").
//!
//! One advisory OS file lock on `tong-funding.lock` in the data directory, taken at the very start
//! of the GUI path, BEFORE the database is opened and before the engine starts. It does not depend
//! on the database and does not know about the process identity: a running Demo process keeps a
//! Live process out and the other way round. The OS drops the lock when the process ends, crash
//! and `kill -9` included, so there is no stale-lock cleanup (and the lock file is never deleted:
//! removing a lock file while others may be opening it is exactly how two instances end up with
//! "their own" lock). Fail closed: any problem other than "free" refuses to start.
//!
//! The lock file is not the database's own per-inode lock (`store::db`); that one protects a single
//! database file against a second connection, this one keeps a second *program* out entirely.
#![allow(dead_code)]

use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// File name inside the data directory.
pub const LOCK_FILE_NAME: &str = "tong-funding.lock";

/// Why the program must not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// Another instance (of either identity) holds the lock.
    AlreadyRunning(PathBuf),
    /// The lock could not be established (HOME unset, directory not creatable, symlinked lock
    /// file, unsupported filesystem...). Treated like "held": no database, no engine.
    Unavailable(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::AlreadyRunning(p) => write!(
                f,
                "另一個 tong-funding 實例已在執行（Demo 或 Live 皆然），本程式不會啟動。請先關閉它。\n\
                 another tong-funding instance is already running (Demo or Live); not starting. Lock file: {}",
                p.display()
            ),
            LockError::Unavailable(why) => write!(f, "無法建立單一實例鎖，不啟動 / cannot take the single-instance lock, not starting: {why}"),
        }
    }
}

impl std::error::Error for LockError {}

/// A held lock. Dropping it (or the process ending) releases it.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Take the lock in `dir` (created 0700 when missing).
    pub fn acquire_in(dir: &Path) -> Result<InstanceLock, LockError> {
        let unavailable = |what: &str, e: std::io::Error| LockError::Unavailable(format!("{what}: {e}"));
        DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| unavailable(&dir.display().to_string(), e))?;
        let path = dir.join(LOCK_FILE_NAME);
        // A planted symlink could point the lock at a file somebody else controls.
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(LockError::Unavailable(format!("{} is a symlink; refusing to follow it", path.display())));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(unavailable(&path.display().to_string(), e)),
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| unavailable(&path.display().to_string(), e))?;
        match file.try_lock() {
            Ok(()) => Ok(InstanceLock { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(LockError::AlreadyRunning(path)),
            Err(TryLockError::Error(e)) => Err(unavailable(&path.display().to_string(), e)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `$HOME/Library/Application Support/tong-funding` (the directory of the databases).
pub fn data_dir_from(home: Option<&str>) -> Result<PathBuf, LockError> {
    let home = home.filter(|h| !h.is_empty()).ok_or_else(|| LockError::Unavailable("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join("Library/Application Support/tong-funding"))
}

static HELD: OnceLock<InstanceLock> = OnceLock::new();

/// Take the lock for the life of this process (default data directory). Call it first thing on the
/// GUI path, before `Db::open_default`; there is no way to give it back except exiting.
/// Calling it again after success is a no-op.
pub fn acquire_for_process() -> Result<(), LockError> {
    if HELD.get().is_some() {
        return Ok(());
    }
    let dir = data_dir_from(std::env::var("HOME").ok().as_deref())?;
    let lock = InstanceLock::acquire_in(&dir)?;
    let _ = HELD.set(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_second_instance_is_refused_and_the_message_says_why() {
        let dir = tmp();
        let first = InstanceLock::acquire_in(dir.path()).expect("first instance");
        let second = InstanceLock::acquire_in(dir.path()).unwrap_err();
        assert_eq!(second, LockError::AlreadyRunning(dir.path().join(LOCK_FILE_NAME)));
        let msg = second.to_string();
        assert!(msg.contains("已在執行") && msg.contains("already running") && msg.contains(LOCK_FILE_NAME), "{msg}");
        // Still refused while the first is alive, however often we try.
        assert!(InstanceLock::acquire_in(dir.path()).is_err());
        drop(first);
    }

    #[test]
    fn releasing_the_lock_lets_a_new_instance_in() {
        let dir = tmp();
        drop(InstanceLock::acquire_in(dir.path()).unwrap());
        let again = InstanceLock::acquire_in(dir.path()).expect("free after drop");
        assert_eq!(again.path(), dir.path().join(LOCK_FILE_NAME));
        // The lock file stays on disk (deleting it would let two instances hold "different" files).
        drop(again);
        assert!(dir.path().join(LOCK_FILE_NAME).exists());
    }

    #[test]
    fn the_lock_is_per_data_directory_and_knows_no_identity() {
        // The API takes a directory and nothing else: Demo and Live share the one lock by
        // construction, and unrelated directories do not interfere.
        let (a, b) = (tmp(), tmp());
        let _x = InstanceLock::acquire_in(a.path()).unwrap();
        let _y = InstanceLock::acquire_in(b.path()).expect("another directory is independent");
    }

    #[test]
    fn directory_and_file_are_owner_only_and_created_on_demand() {
        let dir = tmp();
        let data = dir.path().join("Library/Application Support/tong-funding");
        let _l = InstanceLock::acquire_in(&data).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join(LOCK_FILE_NAME)), 0o600);
    }

    #[test]
    fn a_symlinked_lock_file_is_refused() {
        let dir = tmp();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(LOCK_FILE_NAME)).unwrap();
        let err = InstanceLock::acquire_in(dir.path()).unwrap_err();
        assert!(matches!(err, LockError::Unavailable(ref m) if m.contains("symlink")), "{err:?}");
    }

    #[test]
    fn an_unusable_directory_or_missing_home_fails_closed() {
        let dir = tmp();
        let file = dir.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(InstanceLock::acquire_in(&file.join("sub")), Err(LockError::Unavailable(_))));
        assert!(matches!(data_dir_from(None), Err(LockError::Unavailable(_))));
        assert!(matches!(data_dir_from(Some("")), Err(LockError::Unavailable(_))));
        assert_eq!(data_dir_from(Some("/Users/a")).unwrap(), PathBuf::from("/Users/a/Library/Application Support/tong-funding"));
    }

    /// Helper, run as a child process by the crash test below: holds the lock until killed.
    /// (Does nothing in a normal test run.)
    #[test]
    fn child_holder() {
        let Ok(dir) = std::env::var("TONG_FUNDING_LOCK_TEST_DIR") else { return };
        let _lock = InstanceLock::acquire_in(Path::new(&dir)).expect("child takes the lock");
        println!("LOCKED");
        std::thread::sleep(std::time::Duration::from_secs(120));
    }

    #[test]
    fn a_process_killed_with_sigkill_releases_the_lock() {
        let dir = tmp();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["instance_lock::tests::child_holder", "--exact", "--nocapture", "--test-threads=1"])
            .env("TONG_FUNDING_LOCK_TEST_DIR", dir.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        loop {
            match lines.next() {
                Some(Ok(l)) if l.contains("LOCKED") => break,
                Some(Ok(_)) => continue,
                other => panic!("child never reported the lock: {other:?}"),
            }
        }
        assert!(matches!(InstanceLock::acquire_in(dir.path()), Err(LockError::AlreadyRunning(_))), "held by the child process");
        child.kill().unwrap(); // SIGKILL: no destructor, no cleanup
        child.wait().unwrap();
        InstanceLock::acquire_in(dir.path()).expect("the OS released the crashed process's lock");
    }

    /// The lock must come before the database in the GUI path (structural check of `main.rs`).
    #[test]
    fn main_takes_the_lock_before_it_opens_the_database() {
        let main = include_str!("main.rs");
        let lock = main.find("instance_lock::acquire_for_process").expect("main.rs takes the single-instance lock");
        let db = main.find("Db::open_default").expect("main.rs opens the database");
        let engine = main.find("LiveSource::start").expect("main.rs starts the engine");
        assert!(lock < db && db < engine, "order must be lock -> database -> engine");
    }
}

//! `tong-funding import-legacy-events` subcommand (design D8): runs the legacy importer without
//! opening a window.
//!
//! ```text
//! tong-funding import-legacy-events <events.jsonl> [--db <funding.db>] [--skip-invalid]
//! ```
//!
//! Without `--db` the default database path is used. Exit codes: 0 imported (report identity
//! holds), 1 import refused or failed, 2 usage error.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use tong_funding_core::redact::redact_secrets;

use crate::ports::{Clock, SystemClock};
use crate::store::db::Db;
use crate::store::legacy_import::{run_import, ImportError, ImportOptions};

pub const SUBCOMMAND: &str = "import-legacy-events";

const USAGE: &str = "usage: tong-funding import-legacy-events <events.jsonl> [--db <funding.db>] [--skip-invalid]";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    source: PathBuf,
    db: Option<PathBuf>,
    skip_invalid: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut source = None;
    let mut db = None;
    let mut skip_invalid = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--skip-invalid" => skip_invalid = true,
            "--db" => match it.next() {
                Some(p) if db.is_none() => db = Some(PathBuf::from(p)),
                Some(_) => return Err("--db given twice".into()),
                None => return Err("--db needs a path".into()),
            },
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s if source.is_none() => source = Some(PathBuf::from(s)),
            s => return Err(format!("unexpected argument {s}")),
        }
    }
    Ok(Args { source: source.ok_or("missing <events.jsonl>")?, db, skip_invalid })
}

/// Runs the subcommand with the arguments that follow its name; returns the process exit code.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    run_with_clock(args, Arc::new(SystemClock), out, err)
}

fn run_with_clock(args: &[String], clock: Arc<dyn Clock>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let args = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(err, "{e}\n{USAGE}");
            return 2;
        }
    };
    let db = match &args.db {
        Some(p) => Db::open(p, clock),
        None => match Db::open_default(clock) {
            Ok(db) => db,
            Err(e) => {
                let _ = writeln!(err, "{e}");
                return 1;
            }
        },
    };
    let _ = writeln!(out, "database: {}", db.path().display());
    if let Some(reason) = db.halt_reason() {
        let _ = writeln!(err, "store is halted, nothing was imported: {}", redact_secrets(&reason.to_string()));
        return 1;
    }
    match run_import(&db, &args.source, ImportOptions { skip_invalid: args.skip_invalid }) {
        Ok(report) => {
            let _ = writeln!(out, "{report}");
            if report.identity_holds() {
                0
            } else {
                let _ = writeln!(err, "report identity does not hold (valid = imported + skipped + existing)");
                1
            }
        }
        Err(ImportError::InvalidLines(report)) => {
            let _ = writeln!(out, "{report}");
            let _ = writeln!(err, "{} invalid line(s); aborted, nothing was written (re-run with --skip-invalid to import the rest)", report.invalid.len());
            1
        }
        Err(e) => {
            let _ = writeln!(err, "import failed: {}", redact_secrets(&e.to_string()));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::ManualClock;
    use std::fs;
    use std::path::Path;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn call(args: &[String]) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with_clock(args, Arc::new(ManualClock::new(1)), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    /// Imported rows only (opening may also record e.g. `PERMISSIONS_TIGHTENED`).
    fn event_count(db: &Path) -> i64 {
        let c = rusqlite::Connection::open(db).unwrap();
        c.query_row("SELECT COUNT(*) FROM events WHERE legacy_hash IS NOT NULL", [], |r| r.get(0)).unwrap()
    }

    const GOOD: &str = concat!(
        "{\"ts\": 1791090102.635932, \"event_type\": \"ORDER_STAGED\", \"pair_id\": \"p1\", \"symbol\": \"BTCUSDT\"}\n",
        "{\"ts\": 1791090103.0, \"event_type\": \"SCAN_RUN\", \"n\": 3}\n",
        "{\"ts\": 1791090104.0, \"event_type\": \"ORDER_SUBMITTED\", \"pair_id\": \"p1\"}\n",
    );

    #[test]
    fn parses_flags_in_any_order_and_rejects_bad_usage() {
        assert_eq!(
            parse(&s(&["--skip-invalid", "a.jsonl", "--db", "x.db"])).unwrap(),
            Args { source: "a.jsonl".into(), db: Some("x.db".into()), skip_invalid: true }
        );
        assert_eq!(parse(&s(&["a.jsonl"])).unwrap(), Args { source: "a.jsonl".into(), db: None, skip_invalid: false });
        for bad in [&[][..], &["--db"], &["a", "b"], &["a", "--force"], &["a", "--db", "x", "--db", "y"]] {
            assert!(parse(&s(bad)).is_err(), "{bad:?}");
        }
        let (code, _, err) = call(&s(&["--nope"]));
        assert_eq!(code, 2);
        assert!(err.contains("usage:"), "{err}");
    }

    #[test]
    fn imports_into_the_given_db_prints_the_report_and_is_rerunnable() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("events.jsonl");
        let db = dir.path().join("funding.db");
        fs::write(&src, GOOD).unwrap();
        let before = fs::read(&src).unwrap();

        let (code, out, err) = call(&s(&[src.to_str().unwrap(), "--db", db.to_str().unwrap()]));
        assert_eq!(code, 0, "{out}{err}");
        assert!(out.contains("imported:      2"), "{out}");
        assert!(out.contains("SCAN_RUN: 1"), "{out}");
        assert_eq!(event_count(&db), 2);

        let (code, out, _) = call(&s(&[src.to_str().unwrap(), "--db", db.to_str().unwrap()]));
        assert_eq!(code, 0);
        assert!(out.contains("imported:      0") && out.contains("already there: 2"), "{out}");
        assert_eq!(event_count(&db), 2);
        assert_eq!(fs::read(&src).unwrap(), before, "the source is never modified");
    }

    #[test]
    fn invalid_lines_abort_by_default_and_skip_invalid_imports_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("events.jsonl");
        let db = dir.path().join("funding.db");
        fs::write(&src, format!("{GOOD}not json\n{{\"ts\": 1791090105.0, \"event_type\": \"X\"")).unwrap();

        let (code, out, err) = call(&s(&[src.to_str().unwrap(), "--db", db.to_str().unwrap()]));
        assert_eq!(code, 1);
        assert!(out.contains("line 4") && out.contains("line 5"), "{out}");
        assert!(err.contains("--skip-invalid"), "{err}");
        assert_eq!(event_count(&db), 0, "nothing written on abort");

        let (code, out, _) = call(&s(&[src.to_str().unwrap(), "--db", db.to_str().unwrap(), "--skip-invalid"]));
        assert_eq!(code, 0, "{out}");
        assert_eq!(event_count(&db), 2);
    }

    #[test]
    fn a_halted_store_imports_nothing_and_leaves_the_db_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("events.jsonl");
        let db = dir.path().join("funding.db");
        fs::write(&src, GOOD).unwrap();
        fs::write(&db, b"this is not sqlite").unwrap();
        fs::set_permissions(&db, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();

        let (code, _, err) = call(&s(&[src.to_str().unwrap(), "--db", db.to_str().unwrap()]));
        assert_eq!(code, 1);
        assert!(err.contains("halted"), "{err}");
        assert_eq!(fs::read(&db).unwrap(), b"this is not sqlite");
    }

    #[test]
    fn a_missing_source_fails_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("funding.db");
        let (code, _, err) = call(&s(&[dir.path().join("nope.jsonl").to_str().unwrap(), "--db", db.to_str().unwrap()]));
        assert_eq!(code, 1);
        assert!(err.contains("import failed"), "{err}");
    }
}

//! One-shot, re-runnable import of the old Python `events.jsonl` into the `events` table
//! (spec: legacy-event-import; tasks 5.1, 5.2).
//!
//! The source file is only ever read. Phases: validate every line -> (abort, or skip invalid when
//! asked) -> insert in one transaction with `ON CONFLICT(legacy_hash) DO NOTHING` -> re-hash the
//! source and roll everything back if it changed. Wiring to a CLI subcommand is done elsewhere.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use tong_funding_core::redact::redact_secrets;

use crate::store::db::{Db, StoreError};
use crate::store::secrets::safe_event_payload;

/// Plausible Unix-seconds range for `ts` (2000-01-01 .. 2100-01-01).
pub const TS_MIN_SECS: f64 = 946_684_800.0;
pub const TS_MAX_SECS: f64 = 4_102_444_800.0;

/// Reason key used in `ImportReport::skipped_by_reason` for dropped scan summaries.
pub const SKIP_SCAN_RUN: &str = "SCAN_RUN";

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    /// Import the valid lines even when some lines are invalid (they are listed in the report).
    pub skip_invalid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLine {
    /// 1-based.
    pub line_no: usize,
    pub reason: String,
    /// Lossy-UTF-8 copy of the raw line (without the newline), passed through `redact_secrets`:
    /// a reported line never carries a credential.
    pub content: String,
}

/// What an import did. Note: **idempotent is not line-faithful** — byte-identical lines in the
/// source share one `legacy_hash`, so only the first becomes a row and the rest count as `existing`
/// (re-running the import therefore adds nothing, but duplicate source lines are not preserved).
/// Also, payloads are redacted on the way in (see `payloads_redacted`), while `legacy_hash` is
/// computed over the raw line, so the stored payload may differ from the source on purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub total_lines: u64,
    pub valid_lines: u64,
    pub imported: u64,
    /// Newly imported rows whose payload was changed by secret redaction.
    pub payloads_redacted: u64,
    /// Valid lines already present (same `legacy_hash`).
    pub existing: u64,
    /// Valid lines deliberately not imported, by reason (e.g. `SCAN_RUN`).
    pub skipped_by_reason: BTreeMap<String, u64>,
    pub invalid: Vec<InvalidLine>,
    /// Newly imported rows by `event_type`.
    pub imported_by_type: BTreeMap<String, u64>,
    /// Min/max `ts_ms` over the valid, non-skipped lines (imported or already present).
    pub ts_range_ms: Option<(i64, i64)>,
    pub sha256_before: String,
    pub sha256_after: String,
}

impl ImportReport {
    pub fn skipped(&self) -> u64 {
        self.skipped_by_reason.values().sum()
    }

    /// The spec's identity: valid lines = imported + skipped + already present.
    pub fn identity_holds(&self) -> bool {
        self.valid_lines == self.imported + self.skipped() + self.existing
            && self.total_lines == self.valid_lines + self.invalid.len() as u64
    }
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "total lines:   {}", self.total_lines)?;
        writeln!(f, "valid lines:   {}", self.valid_lines)?;
        writeln!(f, "imported:      {}", self.imported)?;
        writeln!(f, "payloads redacted: {} (of the imported rows)", self.payloads_redacted)?;
        writeln!(f, "already there: {}", self.existing)?;
        writeln!(f, "skipped:       {}", self.skipped())?;
        for (reason, n) in &self.skipped_by_reason {
            writeln!(f, "  - {reason}: {n}")?;
        }
        writeln!(f, "invalid lines: {}", self.invalid.len())?;
        for l in &self.invalid {
            // redacted again at print time: `content` is a public field and may be hand-built
            writeln!(f, "  - line {}: {} | {}", l.line_no, redact_secrets(&l.reason), redact_secrets(&l.content))?;
        }
        writeln!(f, "imported by event_type:")?;
        for (t, n) in &self.imported_by_type {
            writeln!(f, "  - {t}: {n}")?;
        }
        match self.ts_range_ms {
            Some((a, b)) => writeln!(f, "ts range (ms): {a} .. {b}")?,
            None => writeln!(f, "ts range (ms): (none)")?,
        }
        writeln!(f, "sha256 before: {}", self.sha256_before)?;
        write!(f, "sha256 after:  {}", self.sha256_after)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{} invalid line(s); aborted, nothing was written", .0.invalid.len())]
    InvalidLines(Box<ImportReport>),
    #[error("source changed during import (sha256 {before} -> {after}); rolled back")]
    SourceChanged { before: String, after: String },
    #[error("the connection is not configured for import (PRAGMA recursive_triggers must be ON)")]
    ConnectionNotConfigured,
    #[error("unsupported schema version {found} (the importer needs 1)")]
    UnsupportedSchema { found: i64 },
    #[error("source is {size} bytes, over the {limit} byte limit")]
    SourceTooLarge { size: u64, limit: u64 },
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Largest source file the importer will read into memory.
pub const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;

/// Imports `source` into the store's `events` table through `Db::with_conn` (so a halted store
/// refuses). All-or-nothing: one transaction. Payloads are redacted with `safe_event_payload`;
/// `legacy_hash` stays the SHA-256 of the raw line (CR stripped).
///
/// `run_import_on` is the test entry point on a bare connection. It refuses connections without
/// `recursive_triggers = ON` (INSERT OR REPLACE could otherwise bypass the append-only trigger)
/// and schemas other than v1.
pub fn run_import(db: &Db, source: &Path, opts: ImportOptions) -> Result<ImportReport, ImportError> {
    // `with_conn` refuses to run on a halted store; the inner result carries the import's own errors.
    db.with_conn(|conn| Ok(run_import_on(conn, source, opts)))?
}

pub(super) fn run_import_on(conn: &mut Connection, source: &Path, opts: ImportOptions) -> Result<ImportReport, ImportError> {
    run_import_on_with_hook(conn, source, opts, &mut || {})
}

/// Like [`run_import`], calling `before_final_check` after all rows are written but before the
/// source is re-hashed (lets tests simulate a concurrent writer).
pub(super) fn run_import_on_with_hook(
    conn: &mut Connection,
    source: &Path,
    opts: ImportOptions,
    before_final_check: &mut dyn FnMut(),
) -> Result<ImportReport, ImportError> {
    check_connection(conn)?;
    let bytes = read_source(source)?;
    let mut report = ImportReport { sha256_before: sha_hex(&bytes), ..ImportReport::default() };

    // Phase 1: validate every line (no database access).
    let mut candidates: Vec<Candidate> = Vec::new();
    let ends_with_newline = bytes.last().is_none_or(|b| *b == b'\n');
    let mut segments: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if ends_with_newline {
        segments.pop(); // the empty tail after the final newline (or the whole of an empty file)
    }
    let last_idx = segments.len().saturating_sub(1);
    for (i, raw) in segments.iter().enumerate() {
        // CRLF and LF copies of one event are the same event: hash and parse without the CR.
        let mut raw: &[u8] = raw;
        while let Some(rest) = raw.strip_suffix(b"\r") {
            raw = rest;
        }
        report.total_lines += 1;
        let line_no = i + 1;
        let parsed = if !ends_with_newline && i == last_idx {
            Err("last line is not terminated by a newline (file truncated?)".to_string())
        } else {
            parse_line(raw)
        };
        match parsed {
            Ok((p, redacted)) => {
                report.valid_lines += 1;
                if p.event_type == SKIP_SCAN_RUN {
                    *report.skipped_by_reason.entry(SKIP_SCAN_RUN.to_string()).or_default() += 1;
                } else {
                    report.ts_range_ms = Some(match report.ts_range_ms {
                        Some((a, b)) => (a.min(p.ts_ms), b.max(p.ts_ms)),
                        None => (p.ts_ms, p.ts_ms),
                    });
                    candidates.push(Candidate { hash: sha_hex(raw), parsed: p, redacted });
                }
            }
            Err(reason) => report.invalid.push(InvalidLine {
                line_no,
                reason,
                content: redact_secrets(&String::from_utf8_lossy(raw)),
            }),
        }
    }
    if !report.invalid.is_empty() && !opts.skip_invalid {
        report.sha256_after = report.sha256_before.clone();
        return Err(ImportError::InvalidLines(Box::new(report)));
    }

    // Phase 2: one transaction; any early return drops it, which rolls back.
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO events (ts_ms, event_type, pair_id, payload, legacy_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(legacy_hash) DO NOTHING",
        )?;
        for c in &candidates {
            let p = &c.parsed;
            let n = stmt.execute(rusqlite::params![p.ts_ms, p.event_type, p.pair_id, p.payload, c.hash])?;
            if n == 1 {
                report.imported += 1;
                report.payloads_redacted += u64::from(c.redacted);
                *report.imported_by_type.entry(p.event_type.clone()).or_default() += 1;
            } else {
                report.existing += 1;
            }
        }
    }

    before_final_check();
    let after = read_source(source)?;
    report.sha256_after = sha_hex(&after);
    if report.sha256_after != report.sha256_before {
        return Err(ImportError::SourceChanged { before: report.sha256_before, after: report.sha256_after });
    }
    tx.commit()?;
    Ok(report)
}

fn check_connection(conn: &Connection) -> Result<(), ImportError> {
    let recursive: i64 = conn.query_row("PRAGMA recursive_triggers", [], |r| r.get(0))?;
    if recursive != 1 {
        return Err(ImportError::ConnectionNotConfigured);
    }
    let found: i64 = conn.query_row("SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0))?;
    if found != 1 {
        return Err(ImportError::UnsupportedSchema { found });
    }
    Ok(())
}

/// Reads the whole source, refusing anything over [`MAX_SOURCE_BYTES`] (checked on the file size
/// first, and again on what was actually read in case the file grows while we read).
fn read_source(source: &Path) -> Result<Vec<u8>, ImportError> {
    use std::io::Read;
    let io = |e| ImportError::Io { path: source.to_path_buf(), source: e };
    let mut file = std::fs::File::open(source).map_err(io)?;
    let size = file.metadata().map_err(io)?.len();
    if size > MAX_SOURCE_BYTES {
        return Err(ImportError::SourceTooLarge { size, limit: MAX_SOURCE_BYTES });
    }
    let mut bytes = Vec::with_capacity(size as usize);
    (&mut file).take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(ImportError::SourceTooLarge { size: bytes.len() as u64, limit: MAX_SOURCE_BYTES });
    }
    Ok(bytes)
}

struct Candidate {
    hash: String,
    parsed: ParsedLine,
    /// Redaction changed the payload.
    redacted: bool,
}

struct ParsedLine {
    ts_ms: i64,
    event_type: String,
    pair_id: Option<String>,
    payload: String,
}

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Validates one raw line and maps it to an event; `Err` is the human-readable reason.
fn parse_line(raw: &[u8]) -> Result<(ParsedLine, bool), String> {
    let text = std::str::from_utf8(raw).map_err(|_| "not valid UTF-8".to_string())?;
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let serde_json::Value::Object(mut obj) = value else {
        return Err("JSON value is not an object".to_string());
    };
    let ts = match obj.remove("ts") {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(_) => return Err("`ts` is not a number".to_string()),
        None => return Err("missing `ts`".to_string()),
    };
    if !(ts.is_finite() && (TS_MIN_SECS..=TS_MAX_SECS).contains(&ts)) {
        return Err(format!("`ts` {ts} outside the plausible Unix-seconds range"));
    }
    let event_type = match obj.remove("event_type") {
        Some(serde_json::Value::String(s)) if s.is_empty() || !s.chars().all(|c| c.is_ascii_graphic()) => {
            return Err("`event_type` is empty or has characters other than visible ASCII".to_string());
        }
        Some(serde_json::Value::String(s)) => s,
        Some(_) => return Err("`event_type` is not a string".to_string()),
        None => return Err("missing `event_type`".to_string()),
    };
    // Redact first so `pair_id` and `payload` agree and no secret reaches any column.
    let original = serde_json::Value::Object(obj);
    let safe = safe_event_payload(original.clone());
    let redacted = safe != original;
    let pair_id = match safe.get("pair_id") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    Ok((ParsedLine { ts_ms: (ts * 1000.0).round() as i64, event_type, pair_id, payload: safe.to_string() }, redacted))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::store::schema::SCHEMA_V1;

    fn mem_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA recursive_triggers = ON;").unwrap();
        c.execute_batch(SCHEMA_V1).unwrap();
        c
    }

    fn sha_hex(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    fn ev(ts: f64, ty: &str) -> String {
        json!({"ts": ts, "event_type": ty, "symbol": "BTCUSDT"}).to_string()
    }

    /// Writes `lines` joined by '\n' with a trailing newline.
    fn write_src(dir: &tempfile::TempDir, lines: &[String]) -> PathBuf {
        let p = dir.path().join("events.jsonl");
        let mut s = lines.join("\n");
        s.push('\n');
        fs::write(&p, s).unwrap();
        p
    }

    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap()
    }

    fn run(c: &mut Connection, p: &Path) -> ImportReport {
        run_import_on(c, p, ImportOptions::default()).unwrap()
    }

    type Row = (i64, String, Option<String>, String, Option<String>);

    fn rows(c: &Connection) -> Vec<Row> {
        c.prepare("SELECT ts_ms, event_type, pair_id, payload, legacy_hash FROM events ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    // ---- field mapping ----

    #[test]
    fn maps_fields_per_spec() {
        let dir = tempfile::tempdir().unwrap();
        let line = r#"{"ts": 1791090102.635932, "event_type": "ORDER_STAGED", "pair_id": "p1", "symbol": "BTCUSDT"}"#;
        let p = write_src(&dir, &[line.to_string()]);
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!(r.imported, 1);
        let rs = rows(&c);
        assert_eq!(rs.len(), 1);
        let (ts_ms, ty, pair, payload, _) = &rs[0];
        assert_eq!(*ts_ms, 1_791_090_102_636);
        assert_eq!(ty, "ORDER_STAGED");
        assert_eq!(pair.as_deref(), Some("p1"));
        let pv: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(pv["symbol"], "BTCUSDT");
        assert!(pv.get("ts").is_none() && pv.get("event_type").is_none());
    }

    #[test]
    fn payload_keeps_every_other_field_including_pair_id_and_nulls() {
        let dir = tempfile::tempdir().unwrap();
        let line = r#"{"ts": 1791090102.5, "event_type": "ORDER_SUBMITTED", "pair_id": "p", "error": null, "order_id": 269654438, "n": 1.5}"#;
        let p = write_src(&dir, &[line.to_string()]);
        let mut c = mem_db();
        run(&mut c, &p);
        let pv: Value = serde_json::from_str(&rows(&c)[0].3).unwrap();
        assert_eq!(pv, json!({"pair_id": "p", "error": null, "order_id": 269654438, "n": 1.5}));
    }

    #[test]
    fn event_without_pair_id_has_null_pair_id() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "FETCH_ERROR")]);
        let mut c = mem_db();
        run(&mut c, &p);
        assert_eq!(rows(&c)[0].2, None);
    }

    #[test]
    fn integer_ts_and_millisecond_rounding() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(
            &dir,
            &[
                r#"{"ts": 1791090102, "event_type": "A"}"#.to_string(),
                r#"{"ts": 1791090102.0004, "event_type": "B"}"#.to_string(),
                r#"{"ts": 1791090102.0006, "event_type": "C"}"#.to_string(),
            ],
        );
        let mut c = mem_db();
        run(&mut c, &p);
        let ts: Vec<i64> = rows(&c).iter().map(|r| r.0).collect();
        assert_eq!(ts, vec![1_791_090_102_000, 1_791_090_102_000, 1_791_090_102_001]);
    }

    #[test]
    fn unknown_event_type_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "SOMETHING_NEW_FROM_THE_FUTURE")]);
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!(r.imported, 1);
        assert_eq!(rows(&c)[0].1, "SOMETHING_NEW_FROM_THE_FUTURE");
    }

    #[test]
    fn scan_run_is_skipped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(
            &dir,
            &[ev(1791090102.0, "SCAN_RUN"), ev(1791090103.0, "ORDER_STAGED"), ev(1791090104.0, "SCAN_RUN")],
        );
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!(count(&c), 1);
        assert_eq!(r.imported, 1);
        assert_eq!(r.skipped_by_reason.get(SKIP_SCAN_RUN), Some(&2));
        assert_eq!(r.skipped(), 2);
        assert!(r.identity_holds());
    }

    #[test]
    fn source_line_order_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        // timestamps deliberately not monotonic: order must follow the file, not ts
        let p = write_src(&dir, &[ev(1791090105.0, "A"), ev(1791090101.0, "B"), ev(1791090103.0, "C")]);
        let mut c = mem_db();
        run(&mut c, &p);
        let types: Vec<String> = rows(&c).into_iter().map(|r| r.1).collect();
        assert_eq!(types, ["A", "B", "C"]);
    }

    // ---- legacy_hash / idempotence ----

    #[test]
    fn legacy_hash_is_sha256_of_the_raw_line_bytes() {
        let dir = tempfile::tempdir().unwrap();
        // odd spacing on purpose: the hash is of the raw bytes, not of a re-serialization
        let line = r#"{"ts":   1791090102.0,   "event_type":"A"}"#;
        let p = write_src(&dir, &[line.to_string()]);
        let mut c = mem_db();
        run(&mut c, &p);
        assert_eq!(rows(&c)[0].4.as_deref(), Some(sha_hex(line.as_bytes()).as_str()));
    }

    #[test]
    fn running_twice_adds_nothing_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A"), ev(1791090103.0, "SCAN_RUN"), ev(1791090104.0, "B")]);
        let mut c = mem_db();
        let first = run(&mut c, &p);
        let after_first = rows(&c);
        let second = run(&mut c, &p);
        assert_eq!((first.imported, first.existing), (2, 0));
        assert_eq!((second.imported, second.existing), (0, 2));
        assert_eq!(rows(&c), after_first);
        assert!(first.identity_holds() && second.identity_holds());
    }

    #[test]
    fn incremental_import_adds_only_the_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = vec![ev(1791090102.0, "A"), ev(1791090103.0, "B")];
        let p = write_src(&dir, &lines);
        let mut c = mem_db();
        run(&mut c, &p);
        lines.extend([ev(1791090104.0, "C"), ev(1791090105.0, "D"), ev(1791090106.0, "E")]);
        write_src(&dir, &lines);
        let r = run(&mut c, &p);
        assert_eq!((r.imported, r.existing), (3, 2));
        assert_eq!(count(&c), 5);
        assert_eq!(r.imported_by_type.keys().cloned().collect::<Vec<_>>(), ["C", "D", "E"]);
    }

    #[test]
    fn identical_lines_in_the_source_count_as_existing_and_identity_holds() {
        let dir = tempfile::tempdir().unwrap();
        let l = ev(1791090102.0, "A");
        let p = write_src(&dir, &[l.clone(), l.clone(), ev(1791090103.0, "SCAN_RUN")]);
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!((r.valid_lines, r.imported, r.existing, r.skipped()), (3, 1, 1, 1));
        assert!(r.identity_holds());
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn never_replaces_an_existing_event_with_the_same_hash() {
        let dir = tempfile::tempdir().unwrap();
        let line = ev(1791090102.0, "A");
        let p = write_src(&dir, &[line.clone()]);
        let mut c = mem_db();
        c.execute(
            "INSERT INTO events (ts_ms, event_type, payload, legacy_hash) VALUES (1, 'ORIGINAL', '{}', ?1)",
            [sha_hex(line.as_bytes())],
        )
        .unwrap();
        let r = run(&mut c, &p);
        assert_eq!((r.imported, r.existing), (0, 1));
        let rs = rows(&c);
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].1, "ORIGINAL");
    }

    // ---- source is read-only ----

    #[test]
    fn source_file_is_not_modified_and_report_carries_both_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A"), ev(1791090103.0, "SCAN_RUN")]);
        let before = sha_hex(&fs::read(&p).unwrap());
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!(sha_hex(&fs::read(&p).unwrap()), before);
        assert_eq!(r.sha256_before, before);
        assert_eq!(r.sha256_after, before);
    }

    #[test]
    fn source_changing_mid_import_aborts_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A"), ev(1791090103.0, "B")]);
        let mut c = mem_db();
        let p2 = p.clone();
        let mut hook = || {
            let mut f = fs::OpenOptions::new().append(true).open(&p2).unwrap();
            writeln!(f, "{}", ev(1791090104.0, "LATE")).unwrap();
        };
        let err = run_import_on_with_hook(&mut c, &p, ImportOptions::default(), &mut hook).unwrap_err();
        assert!(matches!(err, ImportError::SourceChanged { .. }), "{err:?}");
        assert_eq!(count(&c), 0, "rows written before the check must be rolled back");
    }

    #[test]
    fn missing_source_is_an_io_error() {
        let mut c = mem_db();
        let err = run_import_on(&mut c, Path::new("/nonexistent/events.jsonl"), ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::Io { .. }));
    }

    // ---- validation ----

    #[test]
    fn truncated_last_line_aborts_by_default_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        let full = ev(1791090102.0, "A");
        fs::write(&p, format!("{full}\n{{\"ts\": 1791090103.0, \"event_ty")).unwrap();
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        let ImportError::InvalidLines(rep) = err else { panic!("expected InvalidLines, got {err:?}") };
        assert_eq!(rep.invalid.len(), 1);
        assert_eq!(rep.invalid[0].line_no, 2);
        assert!(rep.invalid[0].content.contains("event_ty"));
        assert_eq!((rep.total_lines, rep.valid_lines), (2, 1));
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn complete_json_without_trailing_newline_is_still_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        fs::write(&p, format!("{}\n{}", ev(1791090102.0, "A"), ev(1791090103.0, "B"))).unwrap();
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        let ImportError::InvalidLines(rep) = err else { panic!("{err:?}") };
        assert_eq!(rep.invalid[0].line_no, 2);
        assert!(rep.invalid[0].reason.contains("newline"), "{}", rep.invalid[0].reason);
        assert_eq!(count(&c), 0);
    }

    fn invalid_reason(line: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[line.to_string()]);
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        let ImportError::InvalidLines(rep) = err else { panic!("{line}: {err:?}") };
        assert_eq!(rep.invalid.len(), 1, "{line}");
        assert_eq!(count(&c), 0);
        rep.invalid[0].reason.clone()
    }

    #[test]
    fn each_kind_of_bad_line_is_rejected_with_a_reason() {
        assert!(invalid_reason("not json at all").contains("JSON"));
        assert!(invalid_reason("").contains("JSON"), "blank line");
        assert!(invalid_reason("[1,2]").contains("object"));
        assert!(invalid_reason(r#"{"event_type":"A"}"#).contains("ts"));
        assert!(invalid_reason(r#"{"ts":"1791090102","event_type":"A"}"#).contains("ts"));
        assert!(invalid_reason(r#"{"ts":1791090102.0}"#).contains("event_type"));
        assert!(invalid_reason(r#"{"ts":1791090102.0,"event_type":5}"#).contains("event_type"));
        assert!(invalid_reason(r#"{"ts":1791090102000,"event_type":"A"}"#).contains("range"), "ms mistaken for s");
        assert!(invalid_reason(r#"{"ts":-5,"event_type":"A"}"#).contains("range"));
    }

    #[test]
    fn non_utf8_line_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        fs::write(&p, b"\xff\xfe{\n").unwrap();
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::InvalidLines(_)));
    }

    fn two_bad_source(dir: &tempfile::TempDir) -> PathBuf {
        write_src(
            dir,
            &[
                ev(1791090102.0, "A"),
                "garbage line".to_string(),
                ev(1791090103.0, "SCAN_RUN"),
                r#"{"ts":1791090104.0}"#.to_string(),
                ev(1791090105.0, "B"),
            ],
        )
    }

    #[test]
    fn invalid_lines_abort_by_default_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let p = two_bad_source(&dir);
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        let ImportError::InvalidLines(rep) = err else { panic!("{err:?}") };
        let nums: Vec<usize> = rep.invalid.iter().map(|l| l.line_no).collect();
        assert_eq!(nums, [2, 4]);
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn skip_invalid_imports_the_rest_and_lists_the_skipped_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = two_bad_source(&dir);
        let mut c = mem_db();
        let r = run_import_on(&mut c, &p, ImportOptions { skip_invalid: true }).unwrap();
        assert_eq!(count(&c), 2);
        assert_eq!((r.total_lines, r.valid_lines, r.imported), (5, 3, 2));
        assert_eq!(r.invalid.len(), 2);
        assert_eq!((r.invalid[0].line_no, r.invalid[0].content.as_str()), (2, "garbage line"));
        assert_eq!((r.invalid[1].line_no, r.invalid[1].content.as_str()), (4, r#"{"ts":1791090104.0}"#));
        assert!(r.identity_holds());
    }

    #[test]
    fn empty_file_imports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        fs::write(&p, b"").unwrap();
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!((r.total_lines, r.imported), (0, 0));
        assert!(r.identity_holds());
        assert_eq!(r.ts_range_ms, None);
    }

    // ---- report ----

    #[test]
    fn report_counts_types_and_time_range() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(
            &dir,
            &[
                ev(1791090105.0, "B"),
                ev(1791090100.0, "A"),
                ev(1791090102.0, "A"),
                ev(1791099999.0, "SCAN_RUN"), // skipped lines do not widen the range
            ],
        );
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!(r.imported_by_type.get("A"), Some(&2));
        assert_eq!(r.imported_by_type.get("B"), Some(&1));
        assert_eq!(r.ts_range_ms, Some((1_791_090_100_000, 1_791_090_105_000)));
        assert!(r.identity_holds());
        let text = r.to_string();
        assert!(text.contains("SCAN_RUN: 1") && text.contains("A: 2"), "{text}");
    }

    // ---- database guarantees still apply to imported rows ----

    #[test]
    fn imported_events_are_append_only_and_payload_is_valid_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A")]);
        let mut c = mem_db();
        run(&mut c, &p);
        assert!(c.execute("UPDATE events SET event_type = 'X'", []).unwrap_err().to_string().contains("append-only"));
        assert!(c.execute("DELETE FROM events", []).unwrap_err().to_string().contains("append-only"));
        assert!(c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'X','nope')", []).is_err());
        let valid: i64 = c.query_row("SELECT json_valid(payload) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(valid, 1);
    }

    #[test]
    fn a_database_failure_midway_rolls_back_the_whole_import() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A"), ev(1791090103.0, "POISON"), ev(1791090104.0, "B")]);
        let mut c = mem_db();
        c.execute_batch(
            "CREATE TRIGGER poison BEFORE INSERT ON events WHEN NEW.event_type = 'POISON'
             BEGIN SELECT RAISE(ABORT, 'poisoned'); END;",
        )
        .unwrap();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::Db(_)), "{err:?}");
        assert_eq!(count(&c), 0);
    }

    // ---- review fixes ----

    fn dump_events(c: &Connection) -> String {
        c.prepare("SELECT ts_ms || '|' || event_type || '|' || COALESCE(pair_id,'') || '|' || payload || '|' || COALESCE(legacy_hash,'') FROM events")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
    }

    #[test]
    fn imported_payloads_are_redacted_but_the_hash_is_of_the_raw_line() {
        let dir = tempfile::tempdir().unwrap();
        let raw = r#"{"ts": 1791090102.0, "event_type": "FETCH_ERROR", "pair_id": "p1", "error": "GET https://x?symbol=B&signature=LEGSIG", "apiKey": "LEGKEY", "headers": [["X-MBX-APIKEY", "LEGHDR"]]}"#;
        let p = write_src(&dir, &[raw.to_string(), ev(1791090103.0, "PLAIN")]);
        let mut c = mem_db();
        let r = run(&mut c, &p);
        let dump = dump_events(&c);
        for leaked in ["LEGSIG", "LEGKEY", "LEGHDR"] {
            assert!(!dump.contains(leaked), "{leaked} in db: {dump}");
        }
        assert!(dump.contains("symbol=B"), "{dump}");
        assert_eq!(rows(&c)[0].4.as_deref(), Some(sha_hex(raw.as_bytes()).as_str()), "hash stays over the raw bytes");
        assert_eq!((r.imported, r.payloads_redacted), (2, 1));
    }

    #[test]
    fn an_ordinary_payload_is_unchanged_and_not_counted_as_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A")]);
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!(r.payloads_redacted, 0);
        let pv: Value = serde_json::from_str(&rows(&c)[0].3).unwrap();
        assert_eq!(pv, json!({"symbol": "BTCUSDT"}));
    }

    #[test]
    fn invalid_line_report_never_prints_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &["garbage https://x?symbol=B&signature=INVSIG tail".to_string()]);
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        let ImportError::InvalidLines(rep) = &err else { panic!("{err:?}") };
        assert!(rep.invalid[0].content.contains("symbol=B"), "{}", rep.invalid[0].content);
        for text in [rep.to_string(), format!("{rep:?}"), format!("{err:?}"), err.to_string(), format!("{:?}", rep.invalid)] {
            assert!(!text.contains("INVSIG"), "{text}");
        }
        let r = run_import_on(&mut c, &p, ImportOptions { skip_invalid: true }).unwrap();
        assert!(!r.to_string().contains("INVSIG") && !format!("{r:?}").contains("INVSIG"));
    }

    fn configured_db() -> (tempfile::TempDir, Db) {
        let (dir, db, _) = crate::store::db::test_support::open_tmp();
        (dir, db)
    }

    fn count_in_file(db: &Db) -> i64 {
        let c = Connection::open_with_flags(db.path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn run_import_goes_through_db_and_is_idempotent() {
        let (dir, db) = configured_db();
        let p = write_src(&dir, &[ev(1791090102.0, "A"), ev(1791090103.0, "B")]);
        let r = run_import(&db, &p, ImportOptions::default()).unwrap();
        assert_eq!((r.imported, r.existing), (2, 0));
        let r2 = run_import(&db, &p, ImportOptions::default()).unwrap();
        assert_eq!((r2.imported, r2.existing), (0, 2));
        assert_eq!(count_in_file(&db), 2);
    }

    #[test]
    fn run_import_on_a_halted_store_errors_and_writes_nothing() {
        let (dir, db) = configured_db();
        let p = write_src(&dir, &[ev(1791090102.0, "A")]);
        db.halt(crate::store::db::HaltReason::Corrupt("test".into()));
        let err = run_import(&db, &p, ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::Store(StoreError::Halted(_))), "{err:?}");
        assert_eq!(count_in_file(&db), 0);
    }

    #[test]
    fn unconfigured_connection_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A")]);
        let mut c = Connection::open_in_memory().unwrap(); // recursive_triggers left at its default (off)
        c.execute_batch(SCHEMA_V1).unwrap();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::ConnectionNotConfigured), "{err:?}");
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn wrong_schema_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "A")]);
        let mut c = mem_db();
        c.execute("UPDATE schema_version SET version = 2", []).unwrap();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::UnsupportedSchema { found: 2 }), "{err:?}");
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn crlf_and_lf_versions_of_the_same_event_are_one_event() {
        let dir = tempfile::tempdir().unwrap();
        let line = ev(1791090102.0, "A");
        let p = dir.path().join("events.jsonl");
        fs::write(&p, format!("{line}\n")).unwrap();
        let mut c = mem_db();
        assert_eq!(run(&mut c, &p).imported, 1);
        fs::write(&p, format!("{line}\r\n")).unwrap();
        let r = run(&mut c, &p);
        assert_eq!((r.imported, r.existing), (0, 1));
        assert_eq!(rows(&c)[0].4.as_deref(), Some(sha_hex(line.as_bytes()).as_str()));
    }

    #[test]
    fn every_trailing_cr_is_stripped_before_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let line = ev(1791090102.0, "A");
        let p = dir.path().join("events.jsonl");
        fs::write(&p, format!("{line}\n")).unwrap();
        let mut c = mem_db();
        assert_eq!(run(&mut c, &p).imported, 1);
        for ending in ["\r\n", "\r\r\n", "\r\r\r\n"] {
            fs::write(&p, format!("{line}{ending}")).unwrap();
            let r = run(&mut c, &p);
            assert_eq!((r.imported, r.existing), (0, 1), "ending {ending:?}");
        }
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn event_type_must_be_visible_ascii() {
        for bad in ["A\u{200B}B", "ＡＢ", "A B", "A\tB", "A\u{7}B", "é", "A\u{FEFF}"] {
            let line = json!({"ts": 1791090102.0, "event_type": bad}).to_string();
            assert!(invalid_reason(&line).contains("event_type"), "{bad:?}");
        }
        let dir = tempfile::tempdir().unwrap();
        let p = write_src(&dir, &[ev(1791090102.0, "ORDER_SUBMIT-1.v2")]);
        assert_eq!(run(&mut mem_db(), &p).imported, 1);
    }

    #[test]
    fn blank_event_type_is_an_invalid_line() {
        assert!(invalid_reason(r#"{"ts":1791090102.0,"event_type":""}"#).contains("event_type"));
        assert!(invalid_reason(r#"{"ts":1791090102.0,"event_type":"  \t"}"#).contains("event_type"));
    }

    #[test]
    fn idempotent_does_not_mean_line_faithful_for_identical_source_lines() {
        let dir = tempfile::tempdir().unwrap();
        let l = ev(1791090102.0, "A");
        let p = write_src(&dir, &[l.clone(), l.clone(), l]);
        let mut c = mem_db();
        let r = run(&mut c, &p);
        assert_eq!((r.valid_lines, r.imported, r.existing), (3, 1, 2));
        assert_eq!(count(&c), 1, "three identical source lines become one row");
    }

    #[test]
    fn oversized_source_is_refused_without_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("events.jsonl");
        let f = fs::File::create(&p).unwrap();
        f.set_len(MAX_SOURCE_BYTES + 1).unwrap(); // sparse: no real 256 MiB written
        let mut c = mem_db();
        let err = run_import_on(&mut c, &p, ImportOptions::default()).unwrap_err();
        assert!(matches!(err, ImportError::SourceTooLarge { size, limit } if size == MAX_SOURCE_BYTES + 1 && limit == MAX_SOURCE_BYTES), "{err:?}");
        assert_eq!(count(&c), 0);
    }

    // ---- real data (manual) ----

    /// Runs the importer against a COPY of the real events.jsonl and prints the report.
    /// `cp /Users/eason.hung/orca/workspaces/funding-analysis/mvp-python/data/events.jsonl <copy>`
    /// then `LEGACY_EVENTS_COPY=<copy> cargo test -p tong-funding store::legacy_import::tests::real_events_copy -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a copy of the real events.jsonl (LEGACY_EVENTS_COPY)"]
    fn real_events_copy() {
        let path = std::env::var("LEGACY_EVENTS_COPY").expect("set LEGACY_EVENTS_COPY to the copy's path");
        let p = PathBuf::from(path);
        let before = sha_hex(&fs::read(&p).unwrap());
        let mut c = mem_db();
        let r = run(&mut c, &p);
        println!("{r}");
        let after = sha_hex(&fs::read(&p).unwrap());
        println!("independent sha256 before: {before}\nindependent sha256 after:  {after}");
        assert_eq!(before, after);
        assert!(r.identity_holds());
        assert_eq!(count(&c) as u64, r.imported);
        let again = run(&mut c, &p);
        assert_eq!((again.imported, again.existing), (0, r.imported));
    }
}

//! `tong-funding config` subcommand: the developer-only way to enter fees and thresholds until the
//! risk settings page exists (ui-readonly-pages design.md, 決定紀錄 2026-10-05). Not part of any
//! spec. Headless: runs before any window is opened.
//!
//! ```text
//! tong-funding config show [--db <funding.db>]
//! tong-funding config set <risk|risk_overrides> '<json>' [--db <funding.db>]
//! ```
//!
//! `set` validates with core (`RiskConfig::from_json` / `parse_overrides`) before writing, writes
//! with optimistic locking and records a `CONFIG_UPDATED` event (`source: "cli"`). The value
//! replaces the whole key. The database is single-instance: quit the app first.
//! Exit codes: 0 ok, 1 refused or store error, 2 usage error.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tong_funding_core::redact::redact_secrets;
use tong_funding_core::risk::{RiskConfig, parse_overrides};

use crate::ports::{Clock, SystemClock};
use crate::store::db::Db;
use crate::store::events::EventStore;

pub const SUBCOMMAND: &str = "config";

/// The keys the engine reads (`engine::actor::CONFIG_RISK` / `CONFIG_RISK_OVERRIDES`).
pub const KEY_RISK: &str = "risk";
pub const KEY_RISK_OVERRIDES: &str = "risk_overrides";
pub const CONFIG_UPDATED: &str = "CONFIG_UPDATED";

const USAGE: &str = "usage:\n  tong-funding config show [--db <funding.db>]\n  tong-funding config set <risk|risk_overrides> '<json>' [--db <funding.db>]\n\nexample:\n  tong-funding config set risk '{\"net_edge_threshold_pct\":\"0.01\",\"est_slippage_pct\":\"0.02\",\"taker_fee_pct\":{\"Binance\":\"0.05\",\"Bybit\":\"0.055\",\"Okx\":\"0.05\"}}'\n  tong-funding config set risk_overrides '{\"Bybit\":{\"est_slippage_pct\":\"0.03\"}}'";

#[derive(Debug, PartialEq)]
enum Cmd {
    Show,
    Set { key: String, value: Value },
}

fn parse(args: &[String]) -> Result<(Cmd, Option<PathBuf>), String> {
    let mut db = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => match it.next() {
                Some(p) if db.is_none() => db = Some(PathBuf::from(p)),
                Some(_) => return Err("--db given twice".into()),
                None => return Err("--db needs a path".into()),
            },
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => rest.push(s.to_string()),
        }
    }
    let cmd = match rest.as_slice() {
        [c] if c == "show" => Cmd::Show,
        [c, key, json] if c == "set" => {
            if key != KEY_RISK && key != KEY_RISK_OVERRIDES {
                return Err(format!("unknown key {key} (expected {KEY_RISK} or {KEY_RISK_OVERRIDES})"));
            }
            let value: Value = serde_json::from_str(json).map_err(|e| format!("value is not JSON: {e}"))?;
            Cmd::Set { key: key.clone(), value }
        }
        [c, ..] if c == "set" => return Err("expected: set <key> <json>".into()),
        [] => return Err("missing command".into()),
        [c, ..] => return Err(format!("unknown command {c}")),
    };
    Ok((cmd, db))
}

/// Validates a value for `key` with core; the error names the offending field.
pub fn validate(key: &str, value: &Value) -> Result<(), String> {
    match key {
        KEY_RISK => RiskConfig::from_json(&value.to_string()).map(|_| ()).map_err(|e| e.to_string()),
        KEY_RISK_OVERRIDES => parse_overrides(value).map(|_| ()).map_err(|e| e.to_string()),
        other => Err(format!("unknown key {other}")),
    }
}

/// Runs the subcommand on the default (or `--db`) database; returns the process exit code.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    run_with_clock(args, Arc::new(SystemClock), out, err)
}

fn run_with_clock(args: &[String], clock: Arc<dyn Clock>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (cmd, path) = match parse(args) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(err, "{e}\n{USAGE}");
            return 2;
        }
    };
    let db = match &path {
        Some(p) => Db::open(p, clock),
        None => match Db::open_default(clock) {
            Ok(db) => db,
            Err(e) => {
                let _ = writeln!(err, "{e}");
                return 1;
            }
        },
    };
    if let Some(reason) = db.halt_reason() {
        let _ = writeln!(err, "store is halted, nothing was changed: {}", redact_secrets(&reason.to_string()));
        return 1;
    }
    match cmd {
        Cmd::Show => show(&db, out, err),
        Cmd::Set { key, value } => set(&db, &key, value, out, err),
    }
}

fn show(db: &Db, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let _ = writeln!(out, "database: {}", db.path().display());
    let mut risk = RiskConfig::default();
    for key in [KEY_RISK, KEY_RISK_OVERRIDES] {
        match db.config_get(key) {
            Ok(None) => {
                let _ = writeln!(out, "{key}: (not set)");
            }
            Ok(Some(e)) => {
                let pretty = serde_json::to_string_pretty(&e.value).unwrap_or_else(|_| e.value.to_string());
                let _ = writeln!(out, "{key} (version {}): {pretty}", e.version);
                let verdict = validate(key, &e.value);
                if let Err(why) = &verdict {
                    let _ = writeln!(out, "  INVALID: {why}");
                }
                if key == KEY_RISK && verdict.is_ok() {
                    risk = RiskConfig::from_json(&e.value.to_string()).unwrap_or_default();
                }
            }
            Err(e) => {
                let _ = writeln!(err, "cannot read {key}: {}", redact_secrets(&e.to_string()));
                return 1;
            }
        }
    }
    let missing = risk.missing_fields();
    if missing.is_empty() {
        let _ = writeln!(out, "required fields: complete");
    } else {
        let _ = writeln!(out, "required fields still missing (Net Edge shows 未設定): {}", missing.join(", "));
    }
    0
}

fn set(db: &Db, key: &str, value: Value, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if let Err(why) = validate(key, &value) {
        let _ = writeln!(err, "refused, nothing was written: {why}");
        return 1;
    }
    let version = match db.config_get(key) {
        Ok(e) => e.map(|e| e.version),
        Err(e) => {
            let _ = writeln!(err, "cannot read {key}: {}", redact_secrets(&e.to_string()));
            return 1;
        }
    };
    let new_version = match db.config_set(key, &value, version) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(err, "cannot save {key}: {}", redact_secrets(&e.to_string()));
            return 1;
        }
    };
    if let Err(e) = EventStore::new(db.clone()).append(CONFIG_UPDATED, None, json!({ "key": key, "version": new_version, "source": "cli" })) {
        let _ = writeln!(err, "saved {key} but could not record the event: {}", redact_secrets(&e.to_string()));
        return 1;
    }
    let _ = writeln!(out, "saved {key} (version {new_version})");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::ManualClock;
    use crate::store::db::test_support::tempdir;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn call(db: &std::path::Path, args: &[&str]) -> (i32, String, String) {
        let mut full = s(args);
        full.push("--db".into());
        full.push(db.display().to_string());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with_clock(&full, Arc::new(ManualClock::new(1_000)), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn stored(db: &std::path::Path, key: &str) -> Option<(Value, i64)> {
        let db = Db::open(db, Arc::new(ManualClock::new(1)));
        db.config_get(key).unwrap().map(|e| (e.value, e.version))
    }

    const FULL: &str = r#"{"net_edge_threshold_pct":"0.01","est_slippage_pct":"0.02","taker_fee_pct":{"Binance":"0.05","Bybit":"0.055","Okx":"0.05"}}"#;

    #[test]
    fn set_risk_validates_and_writes_the_key_the_engine_reads() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        let (code, out, err) = call(&p, &["set", "risk", FULL]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("saved risk (version 1)"), "{out}");
        let (v, ver) = stored(&p, "risk").unwrap();
        assert_eq!(ver, 1);
        let cfg = RiskConfig::from_json(&v.to_string()).unwrap();
        assert!(cfg.is_complete());
        // second write bumps the version (optimistic locking with the current version)
        assert_eq!(call(&p, &["set", "risk", r#"{"net_edge_threshold_pct":"0.02"}"#]).0, 0);
        assert_eq!(stored(&p, "risk").unwrap().1, 2);
    }

    #[test]
    fn invalid_values_are_refused_naming_the_field_and_nothing_is_written() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        let (code, _, err) = call(&p, &["set", "risk", r#"{"max_leverage":"0"}"#]);
        assert_eq!(code, 1);
        assert!(err.contains("max_leverage"), "{err}");
        let (code, _, err) = call(&p, &["set", "risk", r#"{"no_such_field":1}"#]);
        assert_eq!(code, 1);
        assert!(err.contains("no_such_field"), "{err}");
        let (code, _, err) = call(&p, &["set", "risk_overrides", r#"{"Bybit":{"allowed_coins":["BTC"]}}"#]);
        assert_eq!(code, 1);
        assert!(err.contains("allowed_coins"), "{err}");
        assert_eq!(stored(&p, "risk"), None);
        assert_eq!(stored(&p, "risk_overrides"), None);
    }

    #[test]
    fn overrides_are_validated_by_core_and_stored() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        assert_eq!(call(&p, &["set", "risk_overrides", r#"{"Bybit":{"est_slippage_pct":"0.03"}}"#]).0, 0);
        let (v, _) = stored(&p, "risk_overrides").unwrap();
        assert!(parse_overrides(&v).is_ok());
    }

    #[test]
    fn set_records_a_config_updated_event() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        call(&p, &["set", "risk", FULL]);
        let db = Db::open(&p, Arc::new(ManualClock::new(1)));
        let rows = EventStore::new(db).list(10).unwrap();
        let ev = rows.iter().find(|r| r.event_type == CONFIG_UPDATED).expect("event");
        assert_eq!(ev.payload, json!({"key": "risk", "version": 1, "source": "cli"}));
    }

    #[test]
    fn show_lists_values_and_missing_required_fields() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        let (code, out, _) = call(&p, &["show"]);
        assert_eq!(code, 0);
        assert!(out.contains("risk: (not set)"), "{out}");
        assert!(out.contains("net_edge_threshold_pct") && out.contains("taker_fee_pct.Bybit"), "{out}");
        call(&p, &["set", "risk", FULL]);
        let (_, out, _) = call(&p, &["show"]);
        assert!(out.contains("risk (version 1)") && out.contains("required fields: complete"), "{out}");
    }

    #[test]
    fn usage_errors_exit_2() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        assert_eq!(call(&p, &[]).0, 2);
        assert_eq!(call(&p, &["set", "max_leverage", "3"]).0, 2);
        assert_eq!(call(&p, &["set", "risk", "{not json"]).0, 2);
        assert_eq!(call(&p, &["frobnicate"]).0, 2);
    }

    #[test]
    fn a_halted_store_changes_nothing() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
        let before = std::fs::read(&p).unwrap();
        let (code, _, err) = call(&p, &["set", "risk", FULL]);
        assert_eq!(code, 1);
        assert!(err.contains("halted"), "{err}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }
}

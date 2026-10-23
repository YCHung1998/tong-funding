//! `tong-funding config` subcommand: the developer-only way to enter fees and thresholds until the
//! risk settings page exists (ui-readonly-pages design.md, 決定紀錄 2026-10-05). Not part of any
//! spec. Headless: runs before any window is opened.
//!
//! ```text
//! tong-funding config show [--live] [--db <funding.db>]
//! tong-funding config set <risk|risk_overrides> '<json>' [--live] [--db <funding.db>]
//! ```
//!
//! `--live` selects the Live identity (default database `funding-live.db`). The database must
//! belong to that environment (`db_venue`): a Live tool cannot open a demo database and the
//! reverse (live-process 3.2). Without `--live` nothing changes (Demo).
//!
//! `set` validates with core (`RiskConfig::from_json` / `parse_overrides`) before writing, writes
//! with optimistic locking and records a `CONFIG_UPDATED` event (`source: "cli"`). The value
//! replaces the whole key. `set` takes the app's single-instance lock (review round 2, B3): while the app runs
//! it is refused (exit 1) and says to close the app first; `show` is read-only and needs no lock.
//! Exit codes: 0 ok, 1 refused or store error, 2 usage error.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tong_funding_core::redact::redact_secrets;
use tong_funding_core::risk::{RiskConfig, parse_overrides};

use crate::identity::ProcessIdentity;
use crate::ports::{Clock, SystemClock};
use crate::store::db::Db;
use crate::store::events::EventStore;

pub const SUBCOMMAND: &str = "config";

/// The keys the engine reads (`engine::actor::CONFIG_RISK` / `CONFIG_RISK_OVERRIDES`).
pub const KEY_RISK: &str = "risk";
pub const KEY_RISK_OVERRIDES: &str = "risk_overrides";
pub const CONFIG_UPDATED: &str = "CONFIG_UPDATED";

const USAGE: &str = "usage:\n  tong-funding config show [--live] [--db <funding.db>]\n  tong-funding config set <risk|risk_overrides> '<json>' [--live] [--db <funding.db>]\n\nexample:\n  tong-funding config set risk '{\"net_edge_threshold_pct\":\"0.01\",\"est_slippage_pct\":\"0.02\",\"taker_fee_pct\":{\"Binance\":\"0.05\",\"Bybit\":\"0.055\",\"Okx\":\"0.05\"}}'\n  tong-funding config set risk_overrides '{\"Bybit\":{\"est_slippage_pct\":\"0.03\"}}'";

#[derive(Debug, PartialEq)]
enum Cmd {
    Show,
    Set { key: String, value: Value },
}

fn parse(args: &[String]) -> Result<(Cmd, Option<PathBuf>, ProcessIdentity), String> {
    let mut db = None;
    let mut live = false;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => match it.next() {
                Some(p) if db.is_none() => db = Some(PathBuf::from(p)),
                Some(_) => return Err("--db given twice".into()),
                None => return Err("--db needs a path".into()),
            },
            "--live" if live => return Err("--live given twice".into()),
            "--live" => live = true,
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
    Ok((cmd, db, if live { ProcessIdentity::Live } else { ProcessIdentity::Demo }))
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
    let lock_dir = crate::instance_lock::data_dir_from(std::env::var("HOME").ok().as_deref());
    run_with_clock(args, Arc::new(SystemClock), lock_dir, out, err)
}

/// `lock_dir` = where the single-instance lock of the app lives (`Err` = it cannot be told: `set` refuses).
fn run_with_clock(args: &[String], clock: Arc<dyn Clock>, lock_dir: Result<PathBuf, crate::instance_lock::LockError>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (cmd, path, identity) = match parse(args) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(err, "{e}\n{USAGE}");
            return 2;
        }
    };
    // `set` changes settings the running app reads and checks (frozen caps, the breaker): one program at a
    // time, the same lock the app takes before it opens the database (review round 2, B3). `show` only reads.
    let _lock = if matches!(cmd, Cmd::Set { .. }) {
        match lock_dir.and_then(|d| crate::instance_lock::InstanceLock::acquire_in(&d)) {
            Ok(lock) => Some(lock),
            Err(e) => {
                let _ = writeln!(err, "{e}\nconfig set was refused, nothing was written: close the app first (it holds the database and checks these settings while it runs), then run it again");
                return 1;
            }
        }
    } else {
        None
    };
    let db = match &path {
        Some(p) => Db::open_for(identity, p, clock),
        None => match Db::open_default_for(identity, clock) {
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
    // live-guardrails 3.5: a database this tool just created in the live world starts with the safe defaults
    if let Err(why) = crate::store::live_init::apply_if_fresh(&db) {
        let _ = writeln!(err, "{why}");
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

/// live-guardrails 3.6: the `risk` fields the command line may not write on a LIVE database. They
/// are changed only through the app (frozen at arming, audited).
pub const LIVE_PROTECTED_FIELDS: [&str; 5] = ["live_max_leg_notional_usdt", "live_max_pairs_per_arming", "max_leverage", "max_concurrent_pairs", "trigger_mode"];

/// On a live database: refuses a value that carries a protected field, and keeps the stored values of
/// those fields when `risk` is replaced (a replace must not reset caps or the trigger mode).
fn guard_live_value(db: &Db, key: &str, value: Value, stored: Option<&Value>) -> Result<Value, String> {
    let refuse = |field: &str| format!("refused, nothing was written: `{field}` cannot be set from the command line on a live database (change it in the app)");
    match (key, value) {
        (KEY_RISK, Value::Object(mut incoming)) => {
            if let Some(f) = LIVE_PROTECTED_FIELDS.iter().find(|f| incoming.contains_key(**f)) {
                return Err(refuse(f));
            }
            if let Some(Value::Object(old)) = stored {
                for f in LIVE_PROTECTED_FIELDS {
                    if let Some(v) = old.get(f) {
                        incoming.insert(f.to_string(), v.clone());
                    }
                }
            }
            Ok(Value::Object(incoming))
        }
        (KEY_RISK_OVERRIDES, Value::Object(map)) => {
            if map.values().any(|o| o.get("max_leverage").is_some()) {
                return Err(refuse("max_leverage"));
            }
            Ok(Value::Object(map))
        }
        (_, v) => {
            let _ = db;
            Ok(v)
        }
    }
}

fn set(db: &Db, key: &str, value: Value, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let current = match db.config_get(key) {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(err, "cannot read {key}: {}", redact_secrets(&e.to_string()));
            return 1;
        }
    };
    let version = current.as_ref().map(|e| e.version);
    let value = if db.identity().is_live() {
        match guard_live_value(db, key, value, current.as_ref().map(|e| &e.value)) {
            Ok(v) => v,
            Err(why) => {
                let _ = writeln!(err, "{why}");
                return 1;
            }
        }
    } else {
        value
    };
    if let Err(why) = validate(key, &value) {
        let _ = writeln!(err, "refused, nothing was written: {why}");
        return 1;
    }
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
        let code = run_with_clock(&full, Arc::new(ManualClock::new(1_000)), Ok(db.parent().unwrap().to_path_buf()), &mut out, &mut err);
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

    // ---- live-process 3.2: `--db` follows the same environment rule as the app ----

    fn call_live(db: &std::path::Path, args: &[&str]) -> (i32, String, String) {
        let mut full = s(args);
        full.push("--live".into());
        full.push("--db".into());
        full.push(db.display().to_string());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with_clock(&full, Arc::new(ManualClock::new(1_000)), Ok(db.parent().unwrap().to_path_buf()), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn the_live_flag_is_optional_and_parsed_anywhere() {
        assert_eq!(parse(&s(&["show"])).unwrap().2, ProcessIdentity::Demo);
        assert_eq!(parse(&s(&["show", "--live"])).unwrap().2, ProcessIdentity::Live);
        assert_eq!(parse(&s(&["--live", "set", "risk", "{}"])).unwrap().2, ProcessIdentity::Live);
        assert!(parse(&s(&["show", "--live", "--live"])).is_err(), "given twice");
    }

    #[test]
    fn a_live_tool_cannot_open_a_demo_database_and_writes_nothing() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        assert_eq!(call(&p, &["set", "risk", FULL]).0, 0, "creates a demo database");
        let before = std::fs::read(&p).unwrap();
        for args in [&["show"][..], &["set", "risk", r#"{"net_edge_threshold_pct":"0.02"}"#]] {
            let (code, _, err) = call_live(&p, args);
            assert_eq!(code, 1, "{args:?}");
            assert!(err.contains("environment mismatch") && err.contains("demo"), "{err}");
        }
        assert_eq!(std::fs::read(&p).unwrap(), before, "the demo database is untouched");
    }

    #[test]
    fn a_demo_tool_cannot_open_a_live_database_and_writes_nothing() {
        let dir = tempdir();
        let p = dir.path().join("funding-live.db");
        let (code, _, err) = call_live(&p, &["set", "risk", FULL]);
        assert_eq!(code, 0, "{err}");
        let before = std::fs::read(&p).unwrap();
        for args in [&["show"][..], &["set", "risk", r#"{"net_edge_threshold_pct":"0.02"}"#]] {
            let (code, _, err) = call(&p, args);
            assert_eq!(code, 1, "{args:?}");
            assert!(err.contains("environment mismatch") && err.contains("live"), "{err}");
        }
        assert_eq!(std::fs::read(&p).unwrap(), before, "the live database is untouched");
        // And the live tool still works on its own database.
        let (code, out, _) = call_live(&p, &["show"]);
        assert_eq!(code, 0);
        assert!(out.contains("risk (version 2)"), "defaults (v1) then the user set (v2): {out}");
    }

    #[test]
    fn a_live_tool_refuses_an_old_demo_database_without_metadata() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        {
            let migrations = [
                crate::store::db::Migration { version: 1, sql: crate::store::schema::SCHEMA_V1 },
                crate::store::db::Migration { version: 2, sql: crate::store::schema::SCHEMA_V2 },
                crate::store::db::Migration { version: 3, sql: crate::store::schema::SCHEMA_V3 },
                crate::store::db::Migration { version: 4, sql: crate::store::schema::SCHEMA_V4 },
            ];
            let db = Db::open_with(&p, Arc::new(ManualClock::new(1)), &migrations);
            assert!(!db.is_halted());
        }
        let before = std::fs::read(&p).unwrap();
        let (code, _, err) = call_live(&p, &["show"]);
        assert_eq!(code, 1);
        assert!(err.contains("environment"), "{err}");
        assert_eq!(std::fs::read(&p).unwrap(), before);
    }

    // ---- live-guardrails 3.6: the command line cannot move the live caps ------------------------

    fn live_stored(db: &std::path::Path, key: &str) -> Option<Value> {
        let db = Db::open_for(ProcessIdentity::Live, db, Arc::new(ManualClock::new(1)));
        db.config_get(key).unwrap().map(|e| e.value)
    }

    #[test]
    fn on_a_live_database_the_cap_and_mode_fields_cannot_be_written_from_the_command_line() {
        for field in ["live_max_leg_notional_usdt", "live_max_pairs_per_arming", "max_leverage", "max_concurrent_pairs", "trigger_mode"] {
            let dir = tempdir();
            let p = dir.path().join("funding-live.db");
            let mut v: Value = serde_json::from_str(FULL).unwrap();
            v[field] = json!("3");
            let (code, _, err) = call_live(&p, &["set", "risk", &v.to_string()]);
            assert_eq!(code, 1, "{field}");
            assert!(err.contains(field) && err.contains("live"), "{field}: {err}");
            // nothing was written
            assert!(live_stored(&p, KEY_RISK).is_none_or(|r| r.get(field) != Some(&json!("3"))), "{field}");
        }
        // risk_overrides may not carry a leverage either
        let dir = tempdir();
        let p = dir.path().join("funding-live.db");
        let (code, _, err) = call_live(&p, &["set", "risk_overrides", r#"{"Bybit":{"max_leverage":"20"}}"#]);
        assert_eq!(code, 1, "{err}");
        assert!(err.contains("max_leverage"), "{err}");
    }

    #[test]
    fn a_live_set_of_fees_keeps_the_stored_caps_and_mode_instead_of_resetting_them() {
        let dir = tempdir();
        let p = dir.path().join("funding-live.db");
        {
            let db = Db::open_for(ProcessIdentity::Live, &p, Arc::new(ManualClock::new(1)));
            let mut stored: Value = serde_json::from_str(FULL).unwrap();
            stored["live_max_leg_notional_usdt"] = json!("10");
            stored["live_max_pairs_per_arming"] = json!(2);
            stored["trigger_mode"] = json!("MANUAL");
            stored["max_leverage"] = json!("3");
            db.config_set(KEY_RISK, &stored, None).unwrap();
        }
        let (code, _, err) = call_live(&p, &["set", "risk", &FULL.replace("0.01", "0.02")]);
        assert_eq!(code, 0, "{err}");
        let after = live_stored(&p, KEY_RISK).unwrap();
        assert_eq!(after["net_edge_threshold_pct"], json!("0.02"), "the fee edit went through");
        assert_eq!((after["live_max_leg_notional_usdt"].clone(), after["live_max_pairs_per_arming"].clone(), after["trigger_mode"].clone(), after["max_leverage"].clone()), (json!("10"), json!(2), json!("MANUAL"), json!("3")));
    }

    #[test]
    fn the_same_fields_stay_writable_on_a_demo_database() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        let mut v: Value = serde_json::from_str(FULL).unwrap();
        v["max_leverage"] = json!("3");
        v["trigger_mode"] = json!("MANUAL");
        let (code, _, err) = call(&p, &["set", "risk", &v.to_string()]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(stored(&p, KEY_RISK).unwrap().0["trigger_mode"], json!("MANUAL"));
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

    // ---- review round 2, B3: `set` takes the single-instance lock, a running app keeps it out ----

    #[test]
    fn set_is_refused_while_the_app_holds_the_instance_lock_and_nothing_is_written() {
        for live in [false, true] {
            let dir = tempdir();
            let p = dir.path().join(if live { "funding-live.db" } else { "funding.db" });
            let app = crate::instance_lock::InstanceLock::acquire_in(dir.path()).expect("the running app");
            let (code, _, err) = if live { call_live(&p, &["set", "risk", FULL]) } else { call(&p, &["set", "risk", FULL]) };
            assert_eq!(code, 1, "live = {live}: {err}");
            assert!(err.contains("already running") && err.contains("config set"), "says why and what to do: {err}");
            assert!(!p.exists() || stored(&p, "risk").is_none(), "nothing was written");
            // reading stays possible
            let (code, out, _) = if live { call_live(&p, &["show"]) } else { call(&p, &["show"]) };
            assert_eq!(code, 0, "show is read-only and needs no lock");
            assert!(!out.contains("net_edge_threshold_pct\": \"0.01"), "the refused value is not there: {out}");
            drop(app);
            let (code, _, err) = if live { call_live(&p, &["set", "risk", FULL]) } else { call(&p, &["set", "risk", FULL]) };
            assert_eq!(code, 0, "free again: {err}");
        }
    }

    #[test]
    fn set_cannot_tell_where_the_lock_is_so_it_refuses() {
        let dir = tempdir();
        let p = dir.path().join("funding.db");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = s(&["set", "risk", FULL, "--db", &p.display().to_string()]);
        let unavailable = Err(crate::instance_lock::LockError::Unavailable("HOME is not set".into()));
        assert_eq!(run_with_clock(&args, Arc::new(ManualClock::new(1_000)), unavailable, &mut out, &mut err), 1);
        assert!(String::from_utf8(err).unwrap().contains("HOME is not set"));
    }
}

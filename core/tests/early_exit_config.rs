//! leg-early-exit (risk-config): `early_exit_min_move_pct` — global only, default 0.3, 0 disables,
//! negative rejected by name, missing in stored config => default, never makes the config incomplete.
use std::str::FromStr;
use tong_funding_core::risk::*;
use tong_funding_core::types::Decimal;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn default_is_0_3_and_missing_field_in_stored_config_gets_it() {
    assert_eq!(RiskConfig::default().early_exit_min_move_pct, d("0.3"));
    let stored = RiskConfig::from_json(r#"{"max_leverage":"4"}"#).unwrap();
    assert_eq!(stored.early_exit_min_move_pct, d("0.3"));
    let blank: RiskConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(blank.early_exit_min_move_pct, d("0.3"));
}

#[test]
fn does_not_affect_completeness() {
    let c = RiskConfig::default();
    assert!(!c.missing_fields().iter().any(|f| f.contains("early_exit")));
    let mut z = RiskConfig::default();
    z.early_exit_min_move_pct = d("0");
    assert_eq!(z.missing_fields(), c.missing_fields());
}

#[test]
fn valid_value_round_trips_as_decimal_without_rounding() {
    let mut c = RiskConfig::default();
    c.try_update(|c| c.early_exit_min_move_pct = d("0.05")).unwrap();
    assert_eq!(c.early_exit_min_move_pct, d("0.05"));
    let back = RiskConfig::from_json(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back.early_exit_min_move_pct, d("0.05"));
    c.try_update(|c| c.early_exit_min_move_pct = d("0")).unwrap(); // 0 = disabled, allowed
}

#[test]
fn negative_is_rejected_by_name_and_config_untouched() {
    let mut c = RiskConfig::default();
    let before = c.clone();
    let err = c.try_update(|c| c.early_exit_min_move_pct = d("-0.01")).unwrap_err();
    assert_eq!(err.field(), Some("early_exit_min_move_pct"));
    assert_eq!(c, before);
    let e = RiskConfig::from_json(r#"{"early_exit_min_move_pct":"-1"}"#).unwrap_err();
    assert_eq!(e.field(), Some("early_exit_min_move_pct"));
}

#[test]
fn global_only_not_overridable() {
    let err = parse_overrides(&serde_json::json!({"Bybit": {"early_exit_min_move_pct": "0.1"}})).unwrap_err();
    assert_eq!(err.field(), Some("early_exit_min_move_pct"));
}

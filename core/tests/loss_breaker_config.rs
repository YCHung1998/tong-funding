//! loss-circuit-breaker 1.2 (risk-config): `loss_breaker_max_loss_usdt` (default 15, > 0) and
//! `loss_breaker_max_consecutive_losses` (default 3, integer >= 1). Global only; an older stored
//! config without them loads with the defaults and is not made incomplete.
use std::str::FromStr;
use tong_funding_core::risk::*;
use tong_funding_core::types::{Decimal, Exchange};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn complete() -> RiskConfig {
    RiskConfig {
        net_edge_threshold_pct: Some(d("0.05")),
        est_slippage_pct: Some(d("0.01")),
        taker_fee_pct: Exchange::ALL.into_iter().map(|e| (e, d("0.02"))).collect(),
        ..RiskConfig::default()
    }
}

#[test]
fn defaults_are_15_and_3() {
    let c = RiskConfig::default();
    assert_eq!((c.loss_breaker_max_loss_usdt, c.loss_breaker_max_consecutive_losses), (d("15"), 3));
}

#[test]
fn an_old_stored_config_without_the_fields_loads_with_defaults_and_stays_complete() {
    // spec scenario: load an existing config that has no breaker fields
    let old = r#"{"max_leverage":"4","net_edge_threshold_pct":"0.05","est_slippage_pct":"0.01",
        "taker_fee_pct":{"Binance":"0.02","Bybit":"0.02","Okx":"0.02"}}"#;
    let c = RiskConfig::from_json(old).unwrap();
    assert_eq!((c.loss_breaker_max_loss_usdt, c.loss_breaker_max_consecutive_losses), (d("15"), 3));
    assert!(c.is_complete() && c.missing_fields().is_empty());
    assert!(!c.missing_live_fields().iter().any(|f| f.contains("loss_breaker")));
    let blank = RiskConfig::from_json("{}").unwrap();
    assert_eq!(blank.loss_breaker_max_loss_usdt, d("15"));
}

#[test]
fn the_fields_never_make_the_config_incomplete() {
    let c = complete();
    assert!(c.is_complete());
    let mut tight = complete();
    tight.loss_breaker_max_loss_usdt = d("0.01");
    tight.loss_breaker_max_consecutive_losses = 1;
    assert_eq!(tight.missing_fields(), c.missing_fields());
    assert_eq!(tight.missing_live_fields(), c.missing_live_fields());
}

#[test]
fn max_loss_must_be_positive_and_is_named_on_failure() {
    for bad in ["0", "-1", "-0.01"] {
        let mut c = RiskConfig::default();
        let before = c.clone();
        let e = c.try_update(|c| c.loss_breaker_max_loss_usdt = d(bad)).unwrap_err();
        assert_eq!(e.field(), Some("loss_breaker_max_loss_usdt"), "{bad}");
        assert_eq!(c, before, "a failed update leaves the config unchanged");
        let e = RiskConfig::from_json(&format!(r#"{{"loss_breaker_max_loss_usdt":"{bad}"}}"#)).unwrap_err();
        assert_eq!(e.field(), Some("loss_breaker_max_loss_usdt"));
    }
    let mut c = RiskConfig::default();
    c.try_update(|c| c.loss_breaker_max_loss_usdt = d("0.01")).unwrap();
    assert_eq!(c.loss_breaker_max_loss_usdt, d("0.01"));
}

#[test]
fn max_consecutive_must_be_an_integer_of_at_least_one() {
    // spec scenario: setting it to 0 fails validation naming the field and leaves the config as it was
    let mut c = RiskConfig::default();
    let before = c.clone();
    let e = c.try_update(|c| c.loss_breaker_max_consecutive_losses = 0).unwrap_err();
    assert_eq!(e.field(), Some("loss_breaker_max_consecutive_losses"));
    assert_eq!(c, before);
    let e = RiskConfig::from_json(r#"{"loss_breaker_max_consecutive_losses":0}"#).unwrap_err();
    assert_eq!(e.field(), Some("loss_breaker_max_consecutive_losses"));
    c.try_update(|c| c.loss_breaker_max_consecutive_losses = 1).unwrap();
    // a fraction or a negative number is not an integer >= 1 either (refused on load)
    assert!(RiskConfig::from_json(r#"{"loss_breaker_max_consecutive_losses":1.5}"#).is_err());
    assert!(RiskConfig::from_json(r#"{"loss_breaker_max_consecutive_losses":-1}"#).is_err());
}

#[test]
fn values_round_trip_through_json_without_rounding() {
    let mut c = complete();
    c.loss_breaker_max_loss_usdt = d("5.25");
    c.loss_breaker_max_consecutive_losses = 2;
    let back = RiskConfig::from_json(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back, c);
}

#[test]
fn global_only_not_overridable_per_exchange() {
    for key in ["loss_breaker_max_loss_usdt", "loss_breaker_max_consecutive_losses"] {
        let err = parse_overrides(&serde_json::json!({"Bybit": {key: "1"}})).unwrap_err();
        assert_eq!(err.field(), Some(key), "{key}");
    }
}

#[test]
fn effective_for_pair_does_not_carry_them() {
    // Global only: there is nothing to merge per pair, so the effective config never changes with them.
    let mut g = complete();
    let base = effective_for_pair(&g, &RiskOverrides::new(), Exchange::Binance, Exchange::Bybit);
    g.loss_breaker_max_loss_usdt = d("1");
    g.loss_breaker_max_consecutive_losses = 1;
    assert_eq!(effective_for_pair(&g, &RiskOverrides::new(), Exchange::Binance, Exchange::Bybit), base);
}

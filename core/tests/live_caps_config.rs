//! live-guardrails (risk-config): `live_max_leg_notional_usdt` / `live_max_pairs_per_arming` —
//! live-only required fields, no default, hard ceilings baked into the program.
use std::str::FromStr;
use tong_funding_core::risk::*;
use tong_funding_core::types::Decimal;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn complete_demo() -> RiskConfig {
    RiskConfig {
        net_edge_threshold_pct: Some(d("0.05")),
        est_slippage_pct: Some(d("0.01")),
        taker_fee_pct: tong_funding_core::types::Exchange::ALL.into_iter().map(|e| (e, d("0.02"))).collect(),
        ..RiskConfig::default()
    }
}

#[test]
fn ceilings_are_the_documented_constants() {
    assert_eq!(LIVE_CAP_CEILING_LEG_NOTIONAL_USDT, d("100"));
    assert_eq!(LIVE_CAP_CEILING_PAIRS_PER_ARMING, 5);
}

#[test]
fn no_defaults_and_demo_completeness_unchanged() {
    let c = RiskConfig::default();
    assert_eq!(c.live_max_leg_notional_usdt, None);
    assert_eq!(c.live_max_pairs_per_arming, None);
    // Demo: neither field is in missing_fields, completeness unaffected.
    assert!(complete_demo().is_complete());
    assert!(!complete_demo().missing_fields().iter().any(|f| f.starts_with("live_")));
}

#[test]
fn live_incomplete_until_both_caps_set() {
    let c = complete_demo();
    assert!(!c.is_live_complete());
    assert_eq!(c.missing_live_fields(), vec!["live_max_leg_notional_usdt", "live_max_pairs_per_arming"]);
    let c = RiskConfig { live_max_leg_notional_usdt: Some(d("10")), ..c };
    assert_eq!(c.missing_live_fields(), vec!["live_max_pairs_per_arming"]);
    let c = RiskConfig { live_max_pairs_per_arming: Some(2), ..c };
    assert!(c.is_live_complete());
    // Live completeness also requires the base fields.
    let only_caps = RiskConfig {
        live_max_leg_notional_usdt: Some(d("10")),
        live_max_pairs_per_arming: Some(2),
        ..RiskConfig::default()
    };
    assert!(!only_caps.is_live_complete());
    assert!(only_caps.missing_live_fields().contains(&"net_edge_threshold_pct".to_string()));
}

#[test]
fn ceiling_exceeded_names_the_field() {
    let mut c = RiskConfig::default();
    let e = c.try_update(|c| c.live_max_leg_notional_usdt = Some(d("500"))).unwrap_err();
    assert_eq!(e.field(), Some("live_max_leg_notional_usdt"));
    assert!(e.to_string().contains("100"), "{e}");
    let e = c.try_update(|c| c.live_max_pairs_per_arming = Some(9)).unwrap_err();
    assert_eq!(e.field(), Some("live_max_pairs_per_arming"));
    assert!(e.to_string().contains('5'), "{e}");
    assert_eq!(c, RiskConfig::default(), "failed update leaves config untouched");
}

#[test]
fn exactly_at_ceiling_is_allowed_and_zero_negative_rejected() {
    let mut c = RiskConfig::default();
    c.try_update(|c| {
        c.live_max_leg_notional_usdt = Some(d("100"));
        c.live_max_pairs_per_arming = Some(5);
    })
    .unwrap();
    for bad in ["0", "-1"] {
        let e = c.try_update(|c| c.live_max_leg_notional_usdt = Some(d(bad))).unwrap_err();
        assert_eq!(e.field(), Some("live_max_leg_notional_usdt"), "{bad}");
    }
    let e = c.try_update(|c| c.live_max_pairs_per_arming = Some(0)).unwrap_err();
    assert_eq!(e.field(), Some("live_max_pairs_per_arming"));
}

#[test]
fn json_roundtrip_and_stored_config_without_fields_loads() {
    let blank = RiskConfig::from_json("{}").unwrap();
    assert_eq!(blank.live_max_leg_notional_usdt, None);
    let c = RiskConfig::from_json(r#"{"live_max_leg_notional_usdt":"10","live_max_pairs_per_arming":2}"#).unwrap();
    assert_eq!(c.live_max_leg_notional_usdt, Some(d("10")));
    assert_eq!(c.live_max_pairs_per_arming, Some(2));
    let back = RiskConfig::from_json(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back, c);
    let e = RiskConfig::from_json(r#"{"live_max_pairs_per_arming":9}"#).unwrap_err();
    assert_eq!(e.field(), Some("live_max_pairs_per_arming"));
}

#[test]
fn caps_are_global_only() {
    for f in ["live_max_leg_notional_usdt", "live_max_pairs_per_arming"] {
        let err = parse_overrides(&serde_json::json!({"Bybit": {f: "1"}})).unwrap_err();
        assert_eq!(err.field(), Some(f));
    }
}

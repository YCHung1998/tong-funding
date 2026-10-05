//! risk-config: per-exchange overrides and the conservative merge.
use serde_json::json;
use tong_funding_core::risk::*;
use tong_funding_core::types::{Decimal, Exchange::*};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn global() -> RiskConfig {
    let mut c = RiskConfig::default();
    c.net_edge_threshold_pct = Some(d("0.02"));
    c.est_slippage_pct = Some(d("0.01"));
    for e in tong_funding_core::types::Exchange::ALL {
        c.taker_fee_pct.insert(e, d("0.05"));
    }
    c
}

fn ov(f: impl FnOnce(&mut RiskOverride)) -> RiskOverride {
    let mut o = RiskOverride::default();
    f(&mut o);
    o
}

#[test]
fn no_overrides_equals_global() {
    let g = global();
    let e = effective_for_pair(&g, &RiskOverrides::new(), Binance, Bybit);
    assert_eq!(e.max_leverage, g.max_leverage);
    assert_eq!(e.max_price_drift_pct, g.max_price_drift_pct);
    assert_eq!(e.stale_data_threshold_ms, g.stale_data_threshold_ms);
    assert_eq!(e.order_timeout_seconds, g.order_timeout_seconds);
    assert_eq!(e.max_leg_imbalance_pct, g.max_leg_imbalance_pct);
    assert_eq!(e.min_24h_volume_usdt, g.min_24h_volume_usdt);
    assert_eq!(e.safety_margin_pct, g.safety_margin_pct);
    assert_eq!(e.net_edge_threshold_pct, g.net_edge_threshold_pct);
    assert_eq!(e.est_slippage_pct, g.est_slippage_pct);
    assert_eq!(e.long_taker_fee_pct, Some(d("0.05")));
    assert!(e.is_complete());
}

#[test]
fn leverage_takes_smaller_spec_scenario() {
    let mut o = RiskOverrides::new();
    o.insert(Bybit, ov(|o| o.max_leverage = Some(d("4"))));
    let e = effective_for_pair(&global(), &o, Binance, Bybit);
    assert_eq!(e.max_leverage, d("4"));
    // symmetric in which leg is long/short
    assert_eq!(effective_for_pair(&global(), &o, Bybit, Binance).max_leverage, d("4"));
}

#[test]
fn slippage_takes_larger_spec_scenario() {
    let mut g = global();
    g.est_slippage_pct = Some(d("0.01"));
    let mut o = RiskOverrides::new();
    o.insert(Binance, ov(|o| o.est_slippage_pct = Some(d("0.03"))));
    let e = effective_for_pair(&g, &o, Binance, Bybit);
    assert_eq!(e.est_slippage_pct, Some(d("0.03")));
}

#[test]
fn override_larger_than_global_does_not_loosen_min_fields() {
    // override leverage 10 > global 5: Bybit leg is 10, Binance leg is 5 -> min = 5
    let mut o = RiskOverrides::new();
    o.insert(Bybit, ov(|o| o.max_leverage = Some(d("10"))));
    assert_eq!(effective_for_pair(&global(), &o, Binance, Bybit).max_leverage, d("5"));
}

#[test]
fn override_smaller_than_global_does_not_loosen_max_fields() {
    let mut o = RiskOverrides::new();
    o.insert(Bybit, ov(|o| o.net_edge_threshold_pct = Some(d("0.001"))));
    // Binance leg stays 0.02 (global) -> max = 0.02
    let e = effective_for_pair(&global(), &o, Binance, Bybit);
    assert_eq!(e.net_edge_threshold_pct, Some(d("0.02")));
}

#[test]
fn all_min_fields_take_smaller() {
    let mut o = RiskOverrides::new();
    o.insert(
        Binance,
        ov(|o| {
            o.max_leverage = Some(d("2"));
            o.max_price_drift_pct = Some(d("0.01"));
            o.stale_data_threshold_ms = Some(500);
            o.order_timeout_seconds = Some(5);
            o.max_leg_imbalance_pct = Some(d("0.5"));
        }),
    );
    let e = effective_for_pair(&global(), &o, Binance, Okx);
    assert_eq!(e.max_leverage, d("2"));
    assert_eq!(e.max_price_drift_pct, d("0.01"));
    assert_eq!(e.stale_data_threshold_ms, 500);
    assert_eq!(e.order_timeout_seconds, 5);
    assert_eq!(e.max_leg_imbalance_pct, d("0.5"));
}

#[test]
fn all_max_fields_take_larger() {
    let mut o = RiskOverrides::new();
    o.insert(
        Okx,
        ov(|o| {
            o.min_24h_volume_usdt = Some(d("100000"));
            o.net_edge_threshold_pct = Some(d("0.05"));
            o.est_slippage_pct = Some(d("0.04"));
            o.safety_margin_pct = Some(d("0.02"));
        }),
    );
    let e = effective_for_pair(&global(), &o, Binance, Okx);
    assert_eq!(e.min_24h_volume_usdt, d("100000"));
    assert_eq!(e.net_edge_threshold_pct, Some(d("0.05")));
    assert_eq!(e.est_slippage_pct, Some(d("0.04")));
    assert_eq!(e.safety_margin_pct, d("0.02"));
}

#[test]
fn both_legs_override_picks_conservative_of_the_two() {
    let mut o = RiskOverrides::new();
    o.insert(Binance, ov(|o| o.max_leverage = Some(d("4"))));
    o.insert(Bybit, ov(|o| o.max_leverage = Some(d("3"))));
    assert_eq!(effective_for_pair(&global(), &o, Binance, Bybit).max_leverage, d("3"));
}

#[test]
fn same_exchange_on_both_legs_is_fine() {
    let mut o = RiskOverrides::new();
    o.insert(Binance, ov(|o| o.max_leverage = Some(d("2"))));
    let e = effective_for_pair(&global(), &o, Binance, Binance);
    assert_eq!(e.max_leverage, d("2"));
    assert_eq!(e.long_taker_fee_pct, e.short_taker_fee_pct);
}

#[test]
fn override_only_affects_involved_exchanges() {
    let mut o = RiskOverrides::new();
    o.insert(
        Okx,
        ov(|o| {
            o.max_leverage = Some(d("1"));
            o.est_slippage_pct = Some(d("0.5"));
        }),
    );
    let e = effective_for_pair(&global(), &o, Binance, Bybit);
    assert_eq!(e.max_leverage, d("5"));
    assert_eq!(e.est_slippage_pct, Some(d("0.01")));
}

#[test]
fn override_can_supply_a_missing_required_field_for_its_leg_only() {
    let mut g = global();
    g.est_slippage_pct = None;
    let mut o = RiskOverrides::new();
    o.insert(Binance, ov(|o| o.est_slippage_pct = Some(d("0.03"))));
    // Bybit leg has no value anywhere -> pair stays incomplete (not silently 0.03)
    let e = effective_for_pair(&g, &o, Binance, Bybit);
    assert_eq!(e.est_slippage_pct, None);
    assert!(!e.is_complete());
    // both legs covered by overrides -> set
    o.insert(Bybit, ov(|o| o.est_slippage_pct = Some(d("0.02"))));
    let e = effective_for_pair(&g, &o, Binance, Bybit);
    assert_eq!(e.est_slippage_pct, Some(d("0.03")));
    assert!(e.is_complete());
}

#[test]
fn missing_global_required_with_no_overrides_stays_missing() {
    let e = effective_for_pair(&RiskConfig::default(), &RiskOverrides::new(), Binance, Bybit);
    assert_eq!(e.net_edge_threshold_pct, None);
    assert_eq!(e.est_slippage_pct, None);
    assert_eq!(e.long_taker_fee_pct, None);
    assert!(!e.is_complete());
}

#[test]
fn missing_fee_on_one_leg_makes_effective_incomplete() {
    let mut g = global();
    g.taker_fee_pct.remove(&Okx);
    assert!(effective_for_pair(&g, &RiskOverrides::new(), Binance, Bybit).is_complete());
    let e = effective_for_pair(&g, &RiskOverrides::new(), Binance, Okx);
    assert_eq!(e.short_taker_fee_pct, None);
    assert!(!e.is_complete());
}

#[test]
fn merge_is_pure_does_not_mutate_inputs() {
    let g = global();
    let mut o = RiskOverrides::new();
    o.insert(Bybit, ov(|o| o.max_leverage = Some(d("4"))));
    let (g0, o0) = (g.clone(), o.clone());
    let _ = effective_for_pair(&g, &o, Binance, Bybit);
    assert_eq!(g, g0);
    assert_eq!(o, o0);
}

#[test]
fn non_overridable_fields_are_rejected_as_global_only() {
    for f in ["max_concurrent_pairs", "allowed_exchanges", "allowed_coins", "execution_mode", "trigger_mode"] {
        let err = RiskOverride::from_json_value(&json!({ f: "x" })).unwrap_err();
        assert!(matches!(err, RiskError::GlobalOnlyField { .. }), "{f}: {err:?}");
        assert_eq!(err.field(), Some(f));
        assert!(err.to_string().contains("global"), "{err}");
    }
}

#[test]
fn spec_scenario_override_execution_mode_on_bybit_rejected() {
    let err = parse_overrides(&json!({ "Bybit": { "execution_mode": "SIMULATION" } })).unwrap_err();
    assert_eq!(err.field(), Some("execution_mode"));
    assert!(matches!(err, RiskError::GlobalOnlyField { .. }));
}

#[test]
fn taker_fee_is_not_overridable_and_unknown_fields_rejected() {
    let err = RiskOverride::from_json_value(&json!({ "taker_fee_pct": "0.1" })).unwrap_err();
    assert!(matches!(err, RiskError::UnknownField { .. }));
    assert_eq!(err.field(), Some("taker_fee_pct"));
    let err = RiskOverride::from_json_value(&json!({ "hedge_threshold_pct": 1 })).unwrap_err();
    assert!(matches!(err, RiskError::UnknownField { .. }));
}

#[test]
fn all_nine_overridable_fields_parse() {
    let o = RiskOverride::from_json_value(&json!({
        "max_leverage": "4", "max_price_drift_pct": "0.04", "stale_data_threshold_ms": 900,
        "order_timeout_seconds": 10, "max_leg_imbalance_pct": "0.5", "min_24h_volume_usdt": "70000",
        "net_edge_threshold_pct": "0.03", "est_slippage_pct": "0.02", "safety_margin_pct": "0.02"
    }))
    .unwrap();
    assert_eq!(o.max_leverage, Some(d("4")));
    assert_eq!(o.stale_data_threshold_ms, Some(900));
    assert_eq!(o.order_timeout_seconds, Some(10));
    assert_eq!(o.safety_margin_pct, Some(d("0.02")));
    assert!(o.validate(Bybit).is_ok());
}

#[test]
fn parse_overrides_builds_map_and_rejects_bad_exchange() {
    let m = parse_overrides(&json!({ "Bybit": { "max_leverage": "4" } })).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[&Bybit].max_leverage, Some(d("4")));
    assert!(matches!(parse_overrides(&json!({ "Kraken": {} })).unwrap_err(), RiskError::Malformed(_)));
    assert!(matches!(parse_overrides(&json!([1])).unwrap_err(), RiskError::Malformed(_)));
}

#[test]
fn override_values_are_validated_with_field_names() {
    let cases: Vec<(&str, RiskOverride)> = vec![
        ("max_leverage", ov(|o| o.max_leverage = Some(d("0")))),
        ("max_price_drift_pct", ov(|o| o.max_price_drift_pct = Some(d("0")))),
        ("stale_data_threshold_ms", ov(|o| o.stale_data_threshold_ms = Some(0))),
        ("order_timeout_seconds", ov(|o| o.order_timeout_seconds = Some(0))),
        ("max_leg_imbalance_pct", ov(|o| o.max_leg_imbalance_pct = Some(d("-1")))),
        ("min_24h_volume_usdt", ov(|o| o.min_24h_volume_usdt = Some(d("-1")))),
        ("net_edge_threshold_pct", ov(|o| o.net_edge_threshold_pct = Some(d("-1")))),
        ("est_slippage_pct", ov(|o| o.est_slippage_pct = Some(d("-1")))),
        ("safety_margin_pct", ov(|o| o.safety_margin_pct = Some(d("-1")))),
    ];
    for (f, o) in cases {
        let err = o.validate(Bybit).unwrap_err();
        assert!(err.field().unwrap().ends_with(f), "{f}: {err}");
        assert!(err.field().unwrap().contains("Bybit"), "{err}");
    }
    // parse_overrides also validates
    let err = parse_overrides(&json!({ "Okx": { "max_leverage": "0" } })).unwrap_err();
    assert!(err.field().unwrap().ends_with("max_leverage"));
}

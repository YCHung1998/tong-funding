//! risk-config: defaults, validation, completeness, removed fields.
use std::collections::BTreeMap;
use tong_funding_core::risk::*;
use tong_funding_core::types::{Decimal, Exchange};

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn complete() -> RiskConfig {
    let mut c = RiskConfig::default();
    c.net_edge_threshold_pct = Some(d("0.02"));
    c.est_slippage_pct = Some(d("0.01"));
    for e in Exchange::ALL {
        c.taker_fee_pct.insert(e, d("0.05"));
    }
    c
}

#[test]
fn defaults_match_spec_table() {
    let c = RiskConfig::default();
    assert_eq!(c.max_leverage, d("5"));
    assert_eq!(c.max_concurrent_pairs, 3);
    assert_eq!(c.max_price_drift_pct, d("0.05"));
    assert_eq!(c.stale_data_threshold_ms, 1000);
    assert_eq!(c.safety_margin_pct, d("0.01"));
    assert_eq!(c.order_timeout_seconds, 15);
    assert_eq!(c.max_leg_imbalance_pct, d("1.0"));
    assert_eq!(c.min_24h_volume_usdt, d("50000"));
    assert_eq!(c.allowed_exchanges, vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx]);
    assert!(c.allowed_coins.is_empty());
    assert_eq!(c.execution_mode, ExecutionMode::Simulation);
    assert_eq!(c.trigger_mode, TriggerMode::Auto);
    assert_eq!(c.net_edge_threshold_pct, None);
    assert_eq!(c.est_slippage_pct, None);
    assert!(c.taker_fee_pct.is_empty());
    assert!(c.validate().is_ok());
}

#[test]
fn blank_config_loads_simulation_and_incomplete() {
    let c: RiskConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(c.execution_mode, ExecutionMode::Simulation);
    assert!(!c.is_complete());
    assert_eq!(c, RiskConfig::default());
}

#[test]
fn execution_mode_strings() {
    assert_eq!("SIMULATION".parse::<ExecutionMode>().unwrap(), ExecutionMode::Simulation);
    assert_eq!("EXCHANGE_DEMO".parse::<ExecutionMode>().unwrap(), ExecutionMode::ExchangeDemo);
    assert_eq!(serde_json::to_string(&ExecutionMode::ExchangeDemo).unwrap(), "\"EXCHANGE_DEMO\"");
}

#[test]
fn execution_mode_live_is_rejected_everywhere() {
    let e = "LIVE".parse::<ExecutionMode>().unwrap_err();
    assert_eq!(e.field(), Some("execution_mode"));
    assert!(serde_json::from_str::<RiskConfig>(r#"{"execution_mode":"LIVE"}"#).is_err());
    assert!(serde_json::from_str::<ExecutionMode>("\"LIVE\"").is_err());
}

#[test]
fn trigger_mode_strings() {
    assert_eq!("AUTO".parse::<TriggerMode>().unwrap(), TriggerMode::Auto);
    assert_eq!("MANUAL".parse::<TriggerMode>().unwrap(), TriggerMode::Manual);
    assert_eq!("SEMI".parse::<TriggerMode>().unwrap_err().field(), Some("trigger_mode"));
}

fn assert_rejects(field: &str, f: impl FnOnce(&mut RiskConfig)) {
    let mut c = complete();
    let before = c.clone();
    let err = c.try_update(f).unwrap_err();
    assert_eq!(err.field(), Some(field), "{err}");
    assert!(err.to_string().contains(field));
    assert_eq!(c, before, "config must not change on failure");
}

#[test]
fn rejects_max_leverage_zero_and_negative() {
    assert_rejects("max_leverage", |c| c.max_leverage = d("0"));
    assert_rejects("max_leverage", |c| c.max_leverage = d("-1"));
}
#[test]
fn rejects_max_concurrent_pairs_zero() {
    assert_rejects("max_concurrent_pairs", |c| c.max_concurrent_pairs = 0);
}
#[test]
fn rejects_max_price_drift_non_positive() {
    assert_rejects("max_price_drift_pct", |c| c.max_price_drift_pct = d("0"));
    assert_rejects("max_price_drift_pct", |c| c.max_price_drift_pct = d("-0.1"));
}
#[test]
fn rejects_stale_threshold_zero() {
    assert_rejects("stale_data_threshold_ms", |c| c.stale_data_threshold_ms = 0);
}
#[test]
fn rejects_negative_safety_margin_but_allows_zero() {
    assert_rejects("safety_margin_pct", |c| c.safety_margin_pct = d("-0.01"));
    let mut c = complete();
    c.try_update(|c| c.safety_margin_pct = d("0")).unwrap();
}
#[test]
fn rejects_order_timeout_zero() {
    assert_rejects("order_timeout_seconds", |c| c.order_timeout_seconds = 0);
}
#[test]
fn rejects_negative_leg_imbalance_but_allows_zero() {
    assert_rejects("max_leg_imbalance_pct", |c| c.max_leg_imbalance_pct = d("-1"));
    let mut c = complete();
    c.try_update(|c| c.max_leg_imbalance_pct = d("0")).unwrap();
}
#[test]
fn rejects_negative_min_volume_but_allows_zero() {
    assert_rejects("min_24h_volume_usdt", |c| c.min_24h_volume_usdt = d("-1"));
    let mut c = complete();
    c.try_update(|c| c.min_24h_volume_usdt = d("0")).unwrap();
}
#[test]
fn rejects_empty_allowed_exchanges() {
    assert_rejects("allowed_exchanges", |c| c.allowed_exchanges.clear());
}
#[test]
fn rejects_negative_required_fields() {
    assert_rejects("net_edge_threshold_pct", |c| c.net_edge_threshold_pct = Some(d("-0.01")));
    assert_rejects("est_slippage_pct", |c| c.est_slippage_pct = Some(d("-0.01")));
    assert_rejects("taker_fee_pct", |c| {
        c.taker_fee_pct.insert(Exchange::Okx, d("-0.01"));
    });
}

#[test]
fn successful_update_is_committed() {
    let mut c = complete();
    c.try_update(|c| c.max_leverage = d("3")).unwrap();
    assert_eq!(c.max_leverage, d("3"));
}

#[test]
fn validate_names_field_on_directly_built_bad_config() {
    let mut c = RiskConfig::default();
    c.max_leverage = d("0");
    assert_eq!(c.validate().unwrap_err().field(), Some("max_leverage"));
}

#[test]
fn incomplete_lists_every_missing_required_field() {
    let c = RiskConfig::default();
    assert!(!c.is_complete());
    let m = c.missing_fields();
    for f in [
        "net_edge_threshold_pct",
        "est_slippage_pct",
        "taker_fee_pct.Binance",
        "taker_fee_pct.Bybit",
        "taker_fee_pct.Okx",
    ] {
        assert!(m.contains(&f.to_string()), "missing {f} in {m:?}");
    }
    assert_eq!(m.len(), 5);
}

#[test]
fn each_missing_required_field_alone_makes_config_incomplete() {
    let mut c = complete();
    assert!(c.is_complete());
    assert!(c.missing_fields().is_empty());

    c.net_edge_threshold_pct = None;
    assert!(!c.is_complete());
    assert_eq!(c.missing_fields(), vec!["net_edge_threshold_pct"]);
    c.net_edge_threshold_pct = Some(d("0.02"));

    c.est_slippage_pct = None;
    assert!(!c.is_complete());
    assert_eq!(c.missing_fields(), vec!["est_slippage_pct"]);
    c.est_slippage_pct = Some(d("0.01"));

    c.taker_fee_pct.remove(&Exchange::Bybit);
    assert!(!c.is_complete());
    assert_eq!(c.missing_fields(), vec!["taker_fee_pct.Bybit"]);
}

#[test]
fn zero_values_count_as_set_not_missing() {
    let mut c = complete();
    c.net_edge_threshold_pct = Some(d("0"));
    c.est_slippage_pct = Some(d("0"));
    c.taker_fee_pct = BTreeMap::from_iter(Exchange::ALL.map(|e| (e, d("0"))));
    assert!(c.is_complete());
}

#[test]
fn serialized_json_has_no_removed_python_fields() {
    let v = serde_json::to_value(complete()).unwrap();
    let obj = v.as_object().unwrap();
    for k in ["hedge_threshold_pct", "funding_threshold_pct", "max_concurrent_trades"] {
        assert!(!obj.contains_key(k), "{k} must not exist");
    }
    assert!(obj.contains_key("max_price_drift_pct"));
    assert!(obj.contains_key("est_slippage_pct"));
    assert!(!obj.contains_key("max_slippage_pct"));
}

#[test]
fn loading_removed_python_fields_fails() {
    for k in ["hedge_threshold_pct", "funding_threshold_pct", "max_concurrent_trades"] {
        let json = format!("{{\"{k}\": 1}}");
        assert!(serde_json::from_str::<RiskConfig>(&json).is_err(), "{k}");
    }
}

#[test]
fn json_roundtrip_keeps_decimal_exact() {
    let c = complete();
    let s = serde_json::to_string(&c).unwrap();
    assert_eq!(serde_json::from_str::<RiskConfig>(&s).unwrap(), c);
    assert!(s.contains("\"0.05\"")); // Decimal serialized as string
}

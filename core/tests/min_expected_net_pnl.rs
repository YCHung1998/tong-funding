//! ui-trading-pages (MODIFIED risk-config / pretrade-validation): `min_expected_net_pnl_pct` is a
//! second, independent entry threshold next to `net_edge_threshold_pct` (user decision
//! 2026-10-05 evening: keep both). Global only, documented default 0.03 (Python
//! `risk_config.py`), validated `>= 0`; it is not one of the nine overridable fields and does not
//! add an eleventh pre-trade check (it is part of `NetEdgeQualified`).
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::net_edge::*;
use tong_funding_core::risk::*;
use tong_funding_core::types::{Decimal, Exchange};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn default_is_the_documented_conservative_value() {
    assert_eq!(RiskConfig::default().min_expected_net_pnl_pct, d("0.03"));
    let blank: RiskConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(blank.min_expected_net_pnl_pct, d("0.03"));
    // It has a default, so it never makes the config incomplete (the missing list stays exact).
    assert!(!RiskConfig::default().missing_fields().iter().any(|f| f.contains("min_expected")));
}

#[test]
fn negative_is_rejected_by_name_and_zero_is_allowed() {
    let mut c = RiskConfig::default();
    let before = c.clone();
    let err = c.try_update(|c| c.min_expected_net_pnl_pct = d("-0.01")).unwrap_err();
    assert_eq!(err.field(), Some("min_expected_net_pnl_pct"));
    assert_eq!(c, before);
    c.try_update(|c| c.min_expected_net_pnl_pct = d("0")).unwrap();
    let e = RiskConfig::from_json(r#"{"min_expected_net_pnl_pct":"-1"}"#).unwrap_err();
    assert_eq!(e.field(), Some("min_expected_net_pnl_pct"));
}

#[test]
fn it_is_global_only_and_reaches_the_effective_config_unchanged() {
    let mut g = RiskConfig::default();
    g.min_expected_net_pnl_pct = d("0.07");
    let e = effective_for_pair(&g, &RiskOverrides::new(), Exchange::Binance, Exchange::Bybit);
    assert_eq!(e.min_expected_net_pnl_pct, d("0.07"));
    // Not one of the nine overridable fields: an override carrying it is refused by name.
    let err = parse_overrides(&serde_json::json!({"Bybit": {"min_expected_net_pnl_pct": "0.1"}})).unwrap_err();
    assert_eq!(err.field(), Some("min_expected_net_pnl_pct"));
}

fn listed(ex: Exchange, rate: &str) -> FundingObservation {
    FundingObservation {
        exchange: ex,
        symbol: "BTCUSDT".into(),
        funding_rate: d(rate),
        funding_interval_secs: Some(28800),
        next_funding_time: 8 * 3_600_000,
        mark_price: d("100"),
        volume_24h_quote: Some(d("10000000")),
        exchange_timestamp: 0,
        observed_at: 0,
        data_status: DataStatus::Listed,
    }
}

fn params(margin: &str) -> NetEdgeParams {
    NetEdgeParams {
        taker_fee_pct: Exchange::ALL.iter().map(|e| (*e, d("0.02"))).collect::<BTreeMap<_, _>>(),
        est_slippage_pct: Some(d("0.01")),
        safety_margin_pct: d(margin),
        net_edge_threshold_pct: Some(d("0")),
        min_24h_volume_usdt: d("0"),
        allowed_exchanges: Exchange::ALL.iter().copied().collect(),
        allowed_coins: BTreeSet::new(),
    }
}

#[test]
fn expected_net_pnl_is_net_edge_before_the_safety_margin_hand_calc() {
    // income 1000 × 0.0015 = 1.5; fees 1000 × 2 × 0.04 / 100 = 0.8; slippage 1000 × 4 × 0.01 / 100 = 0.4
    // expected net PnL = 1.5 − 0.8 − 0.4 = 0.3 USDT = 0.03 %; safety margin 0.05 % = 0.5 USDT
    let e = compute_net_edge(&listed(Exchange::Binance, "-0.0005"), &listed(Exchange::Bybit, "0.001"), d("1000"), &params("0.05")).unwrap();
    assert_eq!(e.net_edge_pct, d("-0.02"));
    assert_eq!(expected_net_pnl_pct(&e, d("1000")), d("0.03"));
    assert!(meets_min_expected_net_pnl(&e, d("1000"), d("0.03")), "exactly equal passes");
    assert!(!meets_min_expected_net_pnl(&e, d("1000"), d("0.0300001")));
    assert!(!meets_min_expected_net_pnl(&e, d("0"), d("0")), "no notional: cannot qualify (fail closed)");
}

use rust_decimal::Decimal;
use std::str::FromStr;
use tong_funding_core::pretrade::*;
use tong_funding_core::types::Exchange;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn leg(exchange: Exchange) -> LegInput {
    LegInput {
        exchange,
        baseline_price: Some(d("100")),
        scan_price: d("100"),
        latest_price: d("100"),
        price_observed_at_ms: 99_500,
        funding_observed_at_ms: 99_500,
        available_margin: d("1000"),
        listed: true,
        exchange_allowed: true,
        volume_24h_quote: Some(d("1000000")),
        has_foreign_exposure: false,
    }
}

fn input() -> PretradeInput {
    PretradeInput {
        now_ms: 100_000,
        net_edge_qualified: true,
        long: leg(Exchange::Binance),
        short: leg(Exchange::Bybit),
        margin_needed: d("500"),
        leverage: d("3"),
        open_pair_count: 0,
    }
}

fn limits() -> PretradeLimits {
    PretradeLimits {
        max_price_drift_pct: d("0.05"),
        stale_data_threshold_ms: 5000,
        max_leverage: d("4"),
        max_concurrent_pairs: 3,
        min_24h_volume_usdt: d("50000"),
    }
}

fn failed(i: &PretradeInput) -> Vec<Check> {
    evaluate_pretrade(i, &limits()).failed().to_vec()
}

#[test]
fn all_pass() {
    let v = evaluate_pretrade(&input(), &limits());
    assert_eq!(v, PretradeVerdict::Pass);
    assert!(v.failed().is_empty());
}

#[test]
fn data_fresh_fails_only_when_strictly_older_than_threshold() {
    let mut i = input();
    i.long.price_observed_at_ms = 100_000 - 6000;
    assert_eq!(failed(&i), [Check::DataFresh]);
    let mut i = input();
    i.short.price_observed_at_ms = 100_000 - 5000;
    assert!(failed(&i).is_empty());
}

#[test]
fn data_fresh_also_covers_funding_observation_of_each_leg() {
    let mut i = input();
    i.short.funding_observed_at_ms = 100_000 - 5001;
    assert_eq!(failed(&i), [Check::DataFresh]);
    let mut i = input();
    i.long.funding_observed_at_ms = 100_000 - 5001;
    assert_eq!(failed(&i), [Check::DataFresh]);
}

#[test]
fn coin_listed_fails_alone() {
    let mut i = input();
    i.short.listed = false;
    assert_eq!(failed(&i), [Check::CoinListed]);
}

#[test]
fn exchange_allowed_fails_alone() {
    let mut i = input();
    i.long.exchange_allowed = false;
    assert_eq!(failed(&i), [Check::ExchangeAllowed]);
}

#[test]
fn net_edge_fails_alone() {
    let mut i = input();
    i.net_edge_qualified = false;
    assert_eq!(failed(&i), [Check::NetEdgeQualified]);
}

#[test]
fn price_drift_over_limit_fails() {
    let mut i = input();
    i.long.latest_price = d("100.06");
    assert_eq!(failed(&i), [Check::PriceDrift]);
}

#[test]
fn price_drift_equal_to_limit_passes_both_directions() {
    let mut i = input();
    i.long.latest_price = d("100.05");
    i.short.latest_price = d("99.95");
    assert!(failed(&i).is_empty());
}

#[test]
fn price_drift_checked_per_leg_and_downward() {
    let mut i = input();
    i.short.latest_price = d("99.94");
    assert_eq!(failed(&i), [Check::PriceDrift]);
}

#[test]
fn price_drift_uses_baseline_not_scan_price() {
    let mut i = input();
    i.long.scan_price = d("90");
    i.long.baseline_price = Some(d("100"));
    assert!(failed(&i).is_empty());
}

#[test]
fn price_drift_falls_back_to_scan_price_only_without_baseline() {
    let mut i = input();
    i.long.baseline_price = None;
    i.long.scan_price = d("90");
    assert_eq!(failed(&i), [Check::PriceDrift]);
    i.long.scan_price = d("100");
    assert!(failed(&i).is_empty());
}

#[test]
fn price_drift_nonpositive_baseline_is_a_failure_not_a_skip() {
    let mut i = input();
    i.long.baseline_price = Some(d("0"));
    assert_eq!(failed(&i), [Check::PriceDrift]);
}

#[test]
fn liquidity_fails_alone_when_a_leg_is_below_the_volume_floor() {
    let mut i = input();
    i.short.volume_24h_quote = Some(d("49999.99"));
    assert_eq!(failed(&i), [Check::Liquidity]);
}

#[test]
fn liquidity_passes_when_volume_equals_the_floor() {
    let mut i = input();
    i.long.volume_24h_quote = Some(d("50000"));
    assert!(failed(&i).is_empty());
}

#[test]
fn liquidity_missing_volume_fails_closed() {
    let mut i = input();
    i.long.volume_24h_quote = None;
    assert_eq!(failed(&i), [Check::Liquidity]);
}

#[test]
fn margin_fails_alone_on_either_leg() {
    let mut i = input();
    i.long.available_margin = d("499.99");
    assert_eq!(failed(&i), [Check::Margin]);
    let mut i = input();
    i.short.available_margin = d("499.99");
    assert_eq!(failed(&i), [Check::Margin]);
    let mut i = input();
    i.short.available_margin = d("500");
    assert!(failed(&i).is_empty());
}

#[test]
fn leverage_over_limit_fails_without_adjusted_value() {
    let mut i = input();
    i.leverage = d("5");
    let v = evaluate_pretrade(&i, &limits());
    assert_eq!(v, PretradeVerdict::Block { failed: vec![Check::Leverage] });
    i.leverage = d("4");
    assert!(failed(&i).is_empty());
}

#[test]
fn existing_exposure_fails_alone() {
    let mut i = input();
    i.short.has_foreign_exposure = true;
    assert_eq!(failed(&i), [Check::ExistingExposure]);
}

#[test]
fn risk_limits_fail_when_open_plus_one_exceeds_max() {
    let mut i = input();
    i.open_pair_count = 3;
    assert_eq!(failed(&i), [Check::RiskLimits]);
    i.open_pair_count = 2;
    assert!(failed(&i).is_empty());
}

#[test]
fn multiple_failures_are_all_listed() {
    let mut i = input();
    i.long.latest_price = d("101");
    i.short.available_margin = d("1");
    assert_eq!(failed(&i), [Check::PriceDrift, Check::Margin]);
}

#[test]
fn everything_failing_lists_all_ten() {
    let mut i = input();
    i.long.price_observed_at_ms = 0;
    i.long.listed = false;
    i.long.exchange_allowed = false;
    i.net_edge_qualified = false;
    i.long.latest_price = d("200");
    i.long.volume_24h_quote = Some(d("1"));
    i.long.available_margin = d("1");
    i.leverage = d("10");
    i.long.has_foreign_exposure = true;
    i.open_pair_count = 3;
    use Check::*;
    assert_eq!(
        failed(&i),
        [DataFresh, CoinListed, ExchangeAllowed, NetEdgeQualified, PriceDrift, Liquidity, Margin, Leverage, ExistingExposure, RiskLimits]
    );
}

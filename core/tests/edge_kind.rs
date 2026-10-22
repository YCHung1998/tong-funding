//! cost-attribution 6.3 (spec edge-kind): the opportunity kind and the hourly rate spread. The
//! numbers are the spec's scenarios. Run: `cargo test -p tong-funding-core --test edge_kind`.

use std::str::FromStr;
use tong_funding_core::edge_kind::*;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::net_edge::expected_income_rate;
use tong_funding_core::types::{Decimal, Exchange};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn ob(ex: Exchange, rate: &str, interval_h: Option<i64>, next: i64) -> FundingObservation {
    FundingObservation {
        exchange: ex,
        symbol: "BTCUSDT".into(),
        funding_rate: d(rate),
        funding_interval_secs: interval_h.map(|h| h * 3600),
        next_funding_time: next,
        mark_price: d("100"),
        volume_24h_quote: Some(d("10000000")),
        exchange_timestamp: 0,
        observed_at: 0,
        data_status: DataStatus::Listed,
    }
}

const T: i64 = 3_600_000;

/// The spec's "模型收入": Net Edge's expected funding income per notional, as a percentage.
fn model_income_pct(long: &FundingObservation, short: &FundingObservation) -> Decimal {
    expected_income_rate(long, short) * Decimal::ONE_HUNDRED
}

/// The spec's "費率差收入": the hourly spread held for the longer of the two settlement periods.
fn rate_gap_income_pct(c: &EdgeClassification, hours_long: i64, hours_short: i64) -> Decimal {
    c.hourly_spread_pct_per_hour * Decimal::from(hours_long.max(hours_short))
}

#[test]
fn interval_mismatch_when_income_comes_from_different_periods() {
    let long = ob(Exchange::Bybit, "-0.007337", Some(8), T);
    let short = ob(Exchange::Binance, "-0.003346", Some(4), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(model_income_pct(&long, &short), d("0.3991"));
    assert_eq!(c.hourly_spread_pct_per_hour, d("0.0080625"));
    assert_eq!(rate_gap_income_pct(&c, 8, 4), d("0.0645"), "held for the longer period (8 h)");
    assert!(rate_gap_income_pct(&c, 8, 4) < d("0.19955"), "below 0.5 x 0.3991");
    assert_eq!(c.kind, Some(EdgeKind::IntervalMismatch));
}

#[test]
fn rate_gap_when_intervals_are_equal() {
    let long = ob(Exchange::Bybit, "-0.002", Some(8), T);
    let short = ob(Exchange::Binance, "0.001", Some(8), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(c.kind, Some(EdgeKind::RateGap));
    assert_eq!(c.hourly_spread_pct_per_hour, d("0.0375"));
}

#[test]
fn equal_intervals_are_a_rate_gap_whatever_the_settlement_times() {
    // the legs do not settle together: only the long leg earns, so the "rate gap income" is far
    // below the model income, but the periods are equal: not an interval mismatch
    let long = ob(Exchange::Bybit, "-0.002", Some(8), T);
    let short = ob(Exchange::Binance, "-0.002", Some(8), T + 4 * T);
    assert_eq!(model_income_pct(&long, &short), d("0.2"));
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(c.hourly_spread_pct_per_hour, d("0"));
    assert_eq!(c.kind, Some(EdgeKind::RateGap));
}

#[test]
fn rate_gap_when_intervals_differ_but_spread_is_large() {
    let long = ob(Exchange::Bybit, "-0.0040", Some(8), T);
    let short = ob(Exchange::Binance, "0.0010", Some(4), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(model_income_pct(&long, &short), d("0.5000"));
    assert_eq!(c.hourly_spread_pct_per_hour, d("0.075"));
    assert_eq!(rate_gap_income_pct(&c, 8, 4), d("0.6000"));
    assert!(rate_gap_income_pct(&c, 8, 4) >= d("0.2500"), "at least 0.5 x 0.5000");
    assert_eq!(c.kind, Some(EdgeKind::RateGap));
}

#[test]
fn rate_gap_when_the_long_period_exchange_carries_the_rate_and_the_short_period_one_is_near_zero() {
    // 4 h leg pays -0.951% per period, the 1 h leg -0.0044%: holding one full 4 h period keeps almost all of it
    let long = ob(Exchange::Binance, "-0.00951", Some(4), T);
    let short = ob(Exchange::Bybit, "-0.000044", Some(1), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(model_income_pct(&long, &short), d("0.9466"));
    assert_eq!(c.hourly_spread_pct_per_hour, d("0.233350"));
    assert_eq!(rate_gap_income_pct(&c, 4, 1), d("0.9334"), "the longer period (4 h), not the shorter one (1 h)");
    assert!(rate_gap_income_pct(&c, 4, 1) >= d("0.4733"), "at least 0.5 x 0.9466");
    assert_eq!(c.kind, Some(EdgeKind::RateGap));
}

#[test]
fn the_share_is_a_strict_threshold_equal_is_a_rate_gap() {
    // 8 h long at -0.75%, 4 h short at -0.25%: model income 0.5000%, rate gap income exactly 0.25 (= 0.5 x 0.5000)
    let long = ob(Exchange::Bybit, "-0.0075", Some(8), T);
    let at = ob(Exchange::Binance, "-0.0025", Some(4), T);
    let c = classify_edge(&long, &at).unwrap();
    assert_eq!(model_income_pct(&long, &at), d("0.5000"));
    assert_eq!(rate_gap_income_pct(&c, 8, 4), d("0.25"));
    assert_eq!(c.kind, Some(EdgeKind::RateGap), "not below the threshold");
    // short at -0.26%: model income 0.4900%, rate gap income 0.23, below 0.245
    let below = ob(Exchange::Binance, "-0.0026", Some(4), T);
    let c = classify_edge(&long, &below).unwrap();
    assert_eq!(model_income_pct(&long, &below), d("0.4900"));
    assert_eq!(rate_gap_income_pct(&c, 8, 4), d("0.23"));
    assert_eq!(c.kind, Some(EdgeKind::IntervalMismatch));
}

#[test]
fn no_kind_without_income_but_spread_still_reported() {
    let long = ob(Exchange::Bybit, "0.001", Some(8), T);
    let short = ob(Exchange::Binance, "0.001", Some(8), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(c.kind, None);
    assert_eq!(c.hourly_spread_pct_per_hour, d("0"));
    let long = ob(Exchange::Bybit, "0.002", Some(8), T);
    let short = ob(Exchange::Binance, "0.001", Some(8), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(c.kind, None);
    assert_eq!(c.hourly_spread_pct_per_hour, d("-0.0125"));
}

#[test]
fn unknown_interval_cannot_be_classified() {
    let known = ob(Exchange::Binance, "0.001", Some(8), T);
    // either leg, and an interval that is not positive counts as unknown
    assert_eq!(classify_edge(&ob(Exchange::Bybit, "-0.002", None, T), &known), None);
    assert_eq!(classify_edge(&known, &ob(Exchange::Bybit, "-0.002", None, T)), None);
    assert_eq!(classify_edge(&ob(Exchange::Bybit, "-0.002", Some(0), T), &known), None);
    assert_eq!(classify_edge(&ob(Exchange::Bybit, "-0.002", Some(-8), T), &known), None);
}

#[test]
fn overflow_is_not_classified_and_never_panics() {
    let normal = ob(Exchange::Binance, "0.001", Some(8), T);
    let huge = ob(Exchange::Bybit, &Decimal::MAX.to_string(), Some(8), T);
    // the hourly spread x 100 does not fit (either leg)
    assert_eq!(classify_edge(&normal, &huge), None, "short rate");
    assert_eq!(classify_edge(&huge, &normal), None, "long rate");

    // a rate per hour does not fit: 1e27 over a one second period is 3.6e30
    let mut one_second = ob(Exchange::Bybit, "1000000000000000000000000000", Some(8), T);
    one_second.funding_interval_secs = Some(1);
    assert_eq!(classify_edge(&normal, &one_second), None, "rate / hours");

    // the spread is tiny over a very long period, but the expected income of opposite huge rates does not fit
    let mut long = ob(Exchange::Binance, &(-Decimal::MAX).to_string(), Some(8), T);
    let mut short = ob(Exchange::Bybit, &Decimal::MAX.to_string(), Some(8), T);
    (long.funding_interval_secs, short.funding_interval_secs) = (Some(i64::MAX), Some(i64::MAX));
    assert_eq!(classify_edge(&long, &short), None, "income of the pair");

    // the income fits (1e27) but not as a percentage (1e29); the spread over that long period is small
    let zero = ob(Exchange::Binance, "0", Some(8), T);
    let mut short = ob(Exchange::Bybit, "1000000000000000000000000000", Some(8), T);
    short.funding_interval_secs = Some(i64::MAX);
    assert_eq!(classify_edge(&zero, &short), None, "income as a percentage");

    // the spread fits (7.2e28) but not held for the longer period (8 h): 5.8e29
    let mut short = ob(Exchange::Bybit, "200000000000000000000000", Some(8), T);
    short.funding_interval_secs = Some(1);
    assert_eq!(classify_edge(&zero, &short), None, "rate gap income");
}

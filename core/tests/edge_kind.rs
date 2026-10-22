use std::str::FromStr;
use tong_funding_core::edge_kind::*;
use tong_funding_core::funding::{DataStatus, FundingObservation};
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

#[test]
fn interval_mismatch_when_income_comes_from_different_periods() {
    let long = ob(Exchange::Bybit, "-0.007337", Some(8), T);
    let short = ob(Exchange::Binance, "-0.003346", Some(4), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(c.kind, Some(EdgeKind::IntervalMismatch));
    assert_eq!(c.hourly_spread_pct_per_hour, d("0.0080625"));
}

#[test]
fn rate_gap_when_intervals_are_equal() {
    let long = ob(Exchange::Bybit, "-0.002", Some(8), T);
    let short = ob(Exchange::Binance, "0.001", Some(8), T);
    assert_eq!(classify_edge(&long, &short).unwrap().kind, Some(EdgeKind::RateGap));
}

#[test]
fn rate_gap_when_intervals_differ_but_spread_is_large() {
    let long = ob(Exchange::Bybit, "-0.0040", Some(8), T);
    let short = ob(Exchange::Binance, "0.0010", Some(4), T);
    let c = classify_edge(&long, &short).unwrap();
    assert_eq!(c.kind, Some(EdgeKind::RateGap));
    assert_eq!(c.hourly_spread_pct_per_hour, d("0.075"));
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
    let long = ob(Exchange::Bybit, "-0.002", None, T);
    let short = ob(Exchange::Binance, "0.001", Some(8), T);
    assert_eq!(classify_edge(&long, &short), None);
}

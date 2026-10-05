use std::str::FromStr;
use tong_funding_core::funding::*;
use tong_funding_core::types::{Decimal, Exchange};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn obs(ex: Exchange, rate: &str, interval: Option<i64>, next: i64, observed_at: i64, status: DataStatus) -> FundingObservation {
    FundingObservation::new(ex, "BTCUSDT", d(rate), interval, next, d("100"), Some(d("1000000")), 1_000, observed_at, status)
}

#[test]
fn dual_timestamps_kept_independent() {
    let o = FundingObservation::new(Exchange::Binance, "BTCUSDT", d("0.0001"), Some(28800), 5, d("1"), None, 1_000_000, 1_000_400, DataStatus::Listed);
    assert_eq!(o.exchange_timestamp, 1_000_000);
    assert_eq!(o.observed_at, 1_000_400);
}

#[test]
fn three_exchange_interval_derivation() {
    assert_eq!(binance_interval_secs(Some(4)), Some(14400));
    assert_eq!(bybit_interval_secs(Some(480)), Some(28800));
    assert_eq!(okx_interval_secs(Some(1791187200000), Some(1791216000000)), Some(28800));
}

#[test]
fn missing_zero_negative_interval_is_none() {
    assert_eq!(binance_interval_secs(None), None);
    assert_eq!(binance_interval_secs(Some(0)), None);
    assert_eq!(binance_interval_secs(Some(-4)), None);
    assert_eq!(bybit_interval_secs(None), None);
    assert_eq!(bybit_interval_secs(Some(0)), None);
    assert_eq!(bybit_interval_secs(Some(-1)), None);
    assert_eq!(okx_interval_secs(None, Some(5)), None);
    assert_eq!(okx_interval_secs(Some(5), None), None);
    assert_eq!(okx_interval_secs(Some(5), Some(5)), None);
    assert_eq!(okx_interval_secs(Some(10), Some(5)), None);
    assert_eq!(binance_interval_secs(Some(i64::MAX)), None, "overflow must not wrap");
}

#[test]
fn missing_interval_gives_data_error_not_8h() {
    let o = obs(Exchange::Binance, "0.0001", binance_interval_secs(None), 0, 0, DataStatus::Listed);
    assert_eq!(o.data_status, DataStatus::DataError);
    assert_eq!(o.funding_interval_secs, None);
}

#[test]
fn zero_interval_given_to_new_is_data_error_and_not_stored() {
    let o = obs(Exchange::Okx, "0.0001", Some(0), 0, 0, DataStatus::Listed);
    assert_eq!(o.data_status, DataStatus::DataError);
    assert_eq!(o.funding_interval_secs, None);
}

#[test]
fn valid_interval_keeps_listed() {
    let o = obs(Exchange::Okx, "0.0001", Some(28800), 0, 0, DataStatus::Listed);
    assert_eq!(o.data_status, DataStatus::Listed);
    assert_eq!(o.funding_interval_secs, Some(28800));
}

#[test]
fn not_listed_status_is_preserved() {
    let o = obs(Exchange::Okx, "0.0001", None, 0, 0, DataStatus::NotListed);
    assert_eq!(o.data_status, DataStatus::NotListed);
}

#[test]
fn stale_boundary() {
    let o = obs(Exchange::Binance, "0.0001", Some(28800), 0, 10_000, DataStatus::Listed);
    assert!(is_stale(&o, 15_001, 5000));
    assert!(!is_stale(&o, 15_000, 5000));
    assert!(!is_stale(&o, 10_000, 5000));
}

#[test]
fn effective_status_stale_only_overrides_listed() {
    let listed = obs(Exchange::Binance, "0.0001", Some(28800), 0, 0, DataStatus::Listed);
    assert_eq!(effective_status(&listed, 5001, 5000), DataStatus::Stale);
    assert_eq!(effective_status(&listed, 5000, 5000), DataStatus::Listed);
    let err = obs(Exchange::Binance, "0.0001", None, 0, 0, DataStatus::Listed);
    assert_eq!(effective_status(&err, 99_999, 5000), DataStatus::DataError);
}

#[test]
fn equivalent_8h_rate_cases() {
    assert_eq!(equivalent_8h_rate(d("0.0005"), Some(14400)), Some(d("0.0010")));
    assert_eq!(equivalent_8h_rate(d("0.0005"), Some(28800)), Some(d("0.0005")));
    assert_eq!(equivalent_8h_rate(d("0.0005"), None), None);
    assert_eq!(equivalent_8h_rate(d("0.0005"), Some(0)), None);
}

#[test]
fn settlement_different_cycle_only_short_settles() {
    let long = obs(Exchange::Bybit, "0", Some(28800), 8 * 3_600_000, 0, DataStatus::Listed);
    let short = obs(Exchange::Binance, "0.0005", Some(14400), 4 * 3_600_000, 0, DataStatus::Listed);
    let s = pair_settlement(&long, &short);
    assert_eq!(s, PairSettlement { time: 4 * 3_600_000, long_settles: false, short_settles: true });
}

#[test]
fn settlement_same_time_both_settle() {
    let long = obs(Exchange::Bybit, "0", Some(28800), 8 * 3_600_000, 0, DataStatus::Listed);
    let short = obs(Exchange::Binance, "0.0005", Some(28800), 8 * 3_600_000, 0, DataStatus::Listed);
    let s = pair_settlement(&long, &short);
    assert_eq!(s, PairSettlement { time: 8 * 3_600_000, long_settles: true, short_settles: true });
}

#[test]
fn settlement_long_earlier_only_long_settles() {
    let long = obs(Exchange::Bybit, "0", Some(14400), 4 * 3_600_000, 0, DataStatus::Listed);
    let short = obs(Exchange::Binance, "0.0005", Some(28800), 8 * 3_600_000, 0, DataStatus::Listed);
    let s = pair_settlement(&long, &short);
    assert_eq!(s, PairSettlement { time: 4 * 3_600_000, long_settles: true, short_settles: false });
}

#[test]
fn effective_status_downgrades_an_inconsistent_listed_observation() {
    // Built by struct literal (bypassing `new`): Listed but no interval is a contradiction.
    let mut o = tong_funding_core::funding::FundingObservation::new(
        tong_funding_core::types::Exchange::Binance,
        "BTCUSDT",
        rust_decimal::Decimal::ZERO,
        Some(28800),
        0,
        rust_decimal::Decimal::ONE,
        None,
        0,
        0,
        DataStatus::Listed,
    );
    o.funding_interval_secs = None;
    assert_eq!(effective_status(&o, 0, 5000), DataStatus::DataError);
    o.funding_interval_secs = Some(0);
    assert_eq!(effective_status(&o, 0, 5000), DataStatus::DataError);
}

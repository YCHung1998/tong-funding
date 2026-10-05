use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::net_edge::*;
use tong_funding_core::types::{Decimal, Exchange};

const H: i64 = 3_600_000;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn ob(ex: Exchange, symbol: &str, rate: &str, volume: Option<&str>, status: DataStatus) -> FundingObservation {
    FundingObservation {
        exchange: ex,
        symbol: symbol.into(),
        funding_rate: d(rate),
        funding_interval_secs: Some(28800),
        next_funding_time: 8 * H,
        mark_price: d("100"),
        volume_24h_quote: volume.map(d),
        exchange_timestamp: 0,
        observed_at: 0,
        data_status: status,
    }
}

/// Pair with net edge exactly +0.06 pct on N=1000 (income 0.6, no costs).
fn good_pair() -> (FundingObservation, FundingObservation) {
    (
        ob(Exchange::Binance, "BTCUSDT", "-0.0002", Some("2000000"), DataStatus::Listed),
        ob(Exchange::Bybit, "BTCUSDT", "0.0004", Some("2000000"), DataStatus::Listed),
    )
}

fn params(threshold: &str) -> NetEdgeParams {
    NetEdgeParams {
        taker_fee_pct: Exchange::ALL.iter().map(|e| (*e, d("0"))).collect::<BTreeMap<_, _>>(),
        est_slippage_pct: Some(d("0")),
        safety_margin_pct: d("0"),
        net_edge_threshold_pct: Some(d(threshold)),
        min_24h_volume_usdt: d("1000000"),
        allowed_exchanges: Exchange::ALL.iter().copied().collect(),
        allowed_coins: BTreeSet::new(),
    }
}

fn qualifies(a: &FundingObservation, b: &FundingObservation, p: &NetEdgeParams) -> bool {
    evaluate_pair(a, b, d("1000"), p).unwrap().qualifies
}

#[test]
fn baseline_qualifies_and_threshold_is_inclusive() {
    let (a, b) = good_pair();
    let o = evaluate_pair(&a, &b, d("1000"), &params("0.06")).unwrap();
    assert_eq!(o.edge.net_edge_pct, d("0.06"));
    assert!(o.qualifies, "net_edge_pct == threshold qualifies");
    assert!(!qualifies(&a, &b, &params("0.061")));
}

#[test]
fn gross_spread_does_not_help() {
    let (a, b) = good_pair();
    let mut p = params("0");
    p.taker_fee_pct.insert(Exchange::Binance, d("0.2")); // 1000*2*0.2/100 = 4 > 0.6
    let o = evaluate_pair(&a, &b, d("1000"), &p).unwrap();
    assert!(o.edge.gross_spread > d("0.0005"));
    assert!(o.edge.net_edge_pct < d("0"));
    assert!(!o.qualifies);
}

#[test]
fn volume_one_leg_low_fails_and_exact_minimum_passes() {
    let (a, mut b) = good_pair();
    b.volume_24h_quote = Some(d("999999"));
    assert!(!qualifies(&a, &b, &params("0")));
    b.volume_24h_quote = Some(d("1000000"));
    assert!(qualifies(&a, &b, &params("0")));
    let (mut a, b) = good_pair();
    a.volume_24h_quote = Some(d("999999"));
    assert!(!qualifies(&a, &b, &params("0")));
}

#[test]
fn missing_volume_is_zero() {
    let (a, mut b) = good_pair();
    b.volume_24h_quote = None;
    assert!(!qualifies(&a, &b, &params("0")));
    let (mut a, b) = good_pair();
    a.volume_24h_quote = None;
    assert!(!qualifies(&a, &b, &params("0")));
}

#[test]
fn non_listed_status_fails() {
    for st in [DataStatus::NotListed, DataStatus::DataError, DataStatus::Stale] {
        let (mut a, b) = good_pair();
        a.data_status = st;
        assert!(!qualifies(&a, &b, &params("0")), "{st:?} on one leg");
        let (a, mut b) = good_pair();
        b.data_status = st;
        assert!(!qualifies(&a, &b, &params("0")), "{st:?} on other leg");
    }
}

#[test]
fn exchange_must_be_allowed() {
    let (a, b) = good_pair();
    let mut p = params("0");
    p.allowed_exchanges = [Exchange::Binance].into_iter().collect();
    assert!(!qualifies(&a, &b, &p));
    p.allowed_exchanges = [Exchange::Bybit].into_iter().collect();
    assert!(!qualifies(&a, &b, &p));
    p.allowed_exchanges = [Exchange::Binance, Exchange::Bybit].into_iter().collect();
    assert!(qualifies(&a, &b, &p));
    p.allowed_exchanges = BTreeSet::new();
    assert!(!qualifies(&a, &b, &p), "empty allowed_exchanges allows none");
}

#[test]
fn coin_filter_only_when_non_empty() {
    let (a, b) = good_pair();
    let mut p = params("0");
    assert!(qualifies(&a, &b, &p), "empty means unrestricted");
    p.allowed_coins = ["ETHUSDT".to_string()].into_iter().collect();
    assert!(!qualifies(&a, &b, &p));
    p.allowed_coins.insert("BTCUSDT".into());
    assert!(qualifies(&a, &b, &p));
}

#[test]
fn missing_threshold_is_error_not_zero() {
    let (a, b) = good_pair();
    let mut p = params("0");
    p.net_edge_threshold_pct = None;
    assert_eq!(evaluate_pair(&a, &b, d("1000"), &p), Err(NetEdgeError::MissingThreshold));
}

#[test]
fn best_opportunity_reports_qualification_of_best() {
    let obs = vec![
        ob(Exchange::Binance, "BTCUSDT", "-0.0002", Some("2000000"), DataStatus::Listed),
        ob(Exchange::Bybit, "BTCUSDT", "0.0004", Some("2000000"), DataStatus::Listed),
        ob(Exchange::Okx, "BTCUSDT", "0.0001", None, DataStatus::Listed),
    ];
    let best = best_opportunity(&obs, d("1000"), &params("0.06")).unwrap().unwrap();
    assert_eq!((best.edge.long, best.edge.short), (Exchange::Binance, Exchange::Bybit));
    assert!(best.qualifies);
}

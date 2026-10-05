use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::net_edge::*;
use tong_funding_core::types::{Decimal, Exchange};

const H: i64 = 3_600_000;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn ob(ex: Exchange, rate: &str, next_h: i64, volume: Option<&str>, status: DataStatus) -> FundingObservation {
    FundingObservation {
        exchange: ex,
        symbol: "BTCUSDT".into(),
        funding_rate: d(rate),
        funding_interval_secs: Some(28800),
        next_funding_time: next_h * H,
        mark_price: d("100"),
        volume_24h_quote: volume.map(d),
        exchange_timestamp: 0,
        observed_at: 0,
        data_status: status,
    }
}

fn listed(ex: Exchange, rate: &str, next_h: i64) -> FundingObservation {
    ob(ex, rate, next_h, Some("10000000"), DataStatus::Listed)
}

fn params(fee: &str, slip: &str, margin: &str, threshold: &str) -> NetEdgeParams {
    NetEdgeParams {
        taker_fee_pct: Exchange::ALL.iter().map(|e| (*e, d(fee))).collect::<BTreeMap<_, _>>(),
        est_slippage_pct: Some(d(slip)),
        safety_margin_pct: d(margin),
        net_edge_threshold_pct: Some(d(threshold)),
        min_24h_volume_usdt: d("1000000"),
        allowed_exchanges: Exchange::ALL.iter().copied().collect(),
        allowed_coins: BTreeSet::new(),
    }
}

#[test]
fn both_settle_positive_funding_hand_calc() {
    let long = listed(Exchange::Binance, "-0.0001", 8);
    let short = listed(Exchange::Bybit, "0.0003", 8);
    let e = compute_net_edge(&long, &short, d("1000"), &params("0.02", "0", "0", "0")).unwrap();
    assert_eq!(e.funding_income_usdt, d("0.4"));
    assert_eq!(e.fee_usdt, d("0.8"));
    assert_eq!(e.net_edge_usdt, d("-0.4"));
    assert_eq!(e.net_edge_pct, d("-0.04"));
    assert_eq!(e.gross_spread, d("0.0004"));
    assert_eq!((e.long, e.short), (Exchange::Binance, Exchange::Bybit));
}

#[test]
fn safety_margin_is_percentage_number() {
    let long = listed(Exchange::Binance, "-0.0001", 8);
    let short = listed(Exchange::Bybit, "0.0003", 8);
    let e = compute_net_edge(&long, &short, d("1000"), &params("0", "0", "0.01", "0")).unwrap();
    assert_eq!(e.safety_margin_usdt, d("0.1"));
    assert_eq!(e.net_edge_usdt, d("0.3"));
}

#[test]
fn different_cycle_only_settling_leg_counts() {
    let long = listed(Exchange::Bybit, "0", 8);
    let short = listed(Exchange::Binance, "0.0005", 4);
    let e = compute_net_edge(&long, &short, d("1000"), &params("0", "0", "0", "0")).unwrap();
    assert_eq!(e.funding_income_usdt, d("0.5"));
    // and the long-only-settles mirror: long pays its positive rate
    let long = listed(Exchange::Bybit, "0.0002", 4);
    let short = listed(Exchange::Binance, "0.0005", 8);
    let e = compute_net_edge(&long, &short, d("1000"), &params("0", "0", "0", "0")).unwrap();
    assert_eq!(e.funding_income_usdt, d("-0.2"));
}

#[test]
fn same_sign_rates_cancel() {
    let long = listed(Exchange::Bybit, "0.0002", 8);
    let short = listed(Exchange::Binance, "0.0002", 8);
    let e = compute_net_edge(&long, &short, d("1000"), &params("0", "0", "0", "0")).unwrap();
    assert_eq!(e.funding_income_usdt, d("0"));
    assert_eq!(e.net_edge_usdt, d("0"));
}

#[test]
fn per_exchange_fees_and_slippage_sum() {
    let long = listed(Exchange::Binance, "0", 8);
    let short = listed(Exchange::Okx, "0", 8);
    let mut p = params("0", "0.01", "0", "0");
    p.taker_fee_pct.insert(Exchange::Binance, d("0.02"));
    p.taker_fee_pct.insert(Exchange::Okx, d("0.05"));
    let e = compute_net_edge(&long, &short, d("1000"), &p).unwrap();
    assert_eq!(e.fee_usdt, d("1.4")); // 1000 * 2 * 0.07 / 100
    assert_eq!(e.slippage_usdt, d("0.4")); // 1000 * 4 * 0.01 / 100
    assert_eq!(e.net_edge_usdt, d("-1.8"));
    assert_eq!(e.net_edge_pct, d("-0.18"));
}

#[test]
fn missing_fee_is_error_for_each_leg() {
    let long = listed(Exchange::Binance, "0", 8);
    let short = listed(Exchange::Okx, "0", 8);
    let mut p = params("0.02", "0", "0", "0");
    p.taker_fee_pct.remove(&Exchange::Okx);
    assert_eq!(compute_net_edge(&long, &short, d("1000"), &p), Err(NetEdgeError::MissingTakerFee(Exchange::Okx)));
    let mut p = params("0.02", "0", "0", "0");
    p.taker_fee_pct.remove(&Exchange::Binance);
    assert_eq!(compute_net_edge(&long, &short, d("1000"), &p), Err(NetEdgeError::MissingTakerFee(Exchange::Binance)));
}

#[test]
fn missing_slippage_is_error() {
    let long = listed(Exchange::Binance, "0", 8);
    let short = listed(Exchange::Okx, "0", 8);
    let mut p = params("0.02", "0", "0", "0");
    p.est_slippage_pct = None;
    assert_eq!(compute_net_edge(&long, &short, d("1000"), &p), Err(NetEdgeError::MissingEstSlippage));
}

#[test]
fn non_positive_notional_is_error() {
    let long = listed(Exchange::Binance, "0", 8);
    let short = listed(Exchange::Okx, "0", 8);
    let p = params("0", "0", "0", "0");
    assert_eq!(compute_net_edge(&long, &short, d("0"), &p), Err(NetEdgeError::NonPositiveNotional));
}

#[test]
fn legs_assigned_by_rate_and_tie_keeps_first_as_long() {
    let a = listed(Exchange::Binance, "0.0003", 8);
    let b = listed(Exchange::Bybit, "-0.0001", 8);
    let (l, s) = assign_legs(&a, &b);
    assert_eq!((l.exchange, s.exchange), (Exchange::Bybit, Exchange::Binance));
    let (l, s) = assign_legs(&b, &a);
    assert_eq!((l.exchange, s.exchange), (Exchange::Bybit, Exchange::Binance));
    let c = listed(Exchange::Okx, "0.0003", 8);
    let (l, s) = assign_legs(&a, &c);
    assert_eq!((l.exchange, s.exchange), (Exchange::Binance, Exchange::Okx));
}

#[test]
fn evaluate_pair_orients_legs_regardless_of_argument_order() {
    let a = listed(Exchange::Binance, "0.0003", 8);
    let b = listed(Exchange::Bybit, "-0.0001", 8);
    let o = evaluate_pair(&a, &b, d("1000"), &params("0", "0", "0", "0")).unwrap();
    assert_eq!((o.edge.long, o.edge.short), (Exchange::Bybit, Exchange::Binance));
    assert_eq!(o.edge.net_edge_usdt, d("0.4"));
}

#[test]
fn best_of_three_by_net_edge_not_gross_spread() {
    // Binance has the extreme rate but settles later than the other two.
    let obs = vec![
        listed(Exchange::Binance, "0.0010", 8),
        listed(Exchange::Bybit, "0.0004", 4),
        listed(Exchange::Okx, "-0.0002", 4),
    ];
    let best = best_opportunity(&obs, d("1000"), &params("0", "0", "0", "0")).unwrap().unwrap();
    assert_eq!((best.edge.long, best.edge.short), (Exchange::Okx, Exchange::Bybit));
    assert_eq!(best.edge.net_edge_usdt, d("0.6"));
    // the largest gross spread pair (Okx/Binance) would only be worth 0.2
    let alt = evaluate_pair(&obs[0], &obs[2], d("1000"), &params("0", "0", "0", "0")).unwrap();
    assert_eq!(alt.edge.net_edge_usdt, d("0.2"));
    assert!(alt.edge.gross_spread > best.edge.gross_spread);
}

#[test]
fn best_opportunity_edge_cases() {
    let p = params("0", "0", "0", "0");
    assert_eq!(best_opportunity(&[], d("1000"), &p).unwrap(), None);
    let one = vec![listed(Exchange::Binance, "0.0001", 8)];
    assert_eq!(best_opportunity(&one, d("1000"), &p).unwrap(), None);
    // same exchange twice or different symbols are not a pair
    let mut other = listed(Exchange::Bybit, "0.0009", 8);
    other.symbol = "ETHUSDT".into();
    let obs = vec![listed(Exchange::Binance, "0.0001", 8), other];
    assert_eq!(best_opportunity(&obs, d("1000"), &p).unwrap(), None);
}

#[test]
fn best_opportunity_propagates_missing_fee() {
    let obs = vec![listed(Exchange::Binance, "0.0001", 8), listed(Exchange::Okx, "0.0003", 8)];
    let mut p = params("0.02", "0", "0", "0");
    p.taker_fee_pct.remove(&Exchange::Okx);
    assert_eq!(best_opportunity(&obs, d("1000"), &p), Err(NetEdgeError::MissingTakerFee(Exchange::Okx)));
}

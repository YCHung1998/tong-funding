//! cost-attribution task 2.1: the split of a pair's price PnL into entry slippage, holding drift and
//! exit slippage (spec cost-attribution). Hand-computed cases; identities are pinned against `compute_pnl`.
//! Run: `cargo test -p tong-funding-core --test cost_attribution`.

use std::str::FromStr;

use serde_json::json;
use tong_funding_core::cost_attribution::{attribute_costs, CostAttribution};
use tong_funding_core::pnl::{
    compute_pnl, FillAction, FillRecord, FundingFetchState, FundingLedgerEntry, LegInput, PairInput,
};
use tong_funding_core::types::{Decimal, Exchange, Side};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn fill(id: &str, action: FillAction, qty: &str, expected: Option<&str>, actual: &str, fee: &str) -> FillRecord {
    FillRecord {
        id: id.into(),
        action,
        quantity: d(qty),
        expected_price: expected.map(d),
        actual_price: Some(d(actual)),
        fee: Some(d(fee)),
        fee_asset: Some("USDT".into()),
        filled_at_ms: 0,
        contract_value_missing: false,
    }
}

fn funding(exchange: Exchange, id: &str, amount: &str) -> FundingLedgerEntry {
    FundingLedgerEntry::new(exchange, "BTCUSDT", d(amount), "USDT", 10, id, "FUNDING_FEE", json!({}))
}

fn leg(exchange: Exchange, side: Side, fills: Vec<FillRecord>, funding: Vec<FundingLedgerEntry>) -> LegInput {
    let expected = funding.len();
    LegInput { exchange, symbol: "BTCUSDT".into(), side, fills, funding, expected_settlements: Some(expected), funding_fetch: FundingFetchState::Fetched }
}

fn pair(legs: Vec<LegInput>) -> PairInput {
    PairInput { legs, ambiguous_attribution: false, reconciliation_mismatch: false }
}

/// The spec's 100 / 100.1 / 100.5 / 100.4 case; fees 0.1 per fill, funding +0.5 on the long leg.
fn spec_pair() -> PairInput {
    pair(vec![
        leg(
            Exchange::Binance,
            Side::Long,
            vec![
                fill("l-open", FillAction::Open, "10", Some("100"), "100.1", "0.1"),
                fill("l-close", FillAction::Close, "10", Some("100.5"), "100.4", "0.1"),
            ],
            vec![funding(Exchange::Binance, "f1", "0.5")],
        ),
        leg(
            Exchange::Bybit,
            Side::Short,
            vec![
                fill("s-open", FillAction::Open, "10", Some("100.2"), "100.1", "0.1"),
                fill("s-close", FillAction::Close, "10", Some("100.6"), "100.7", "0.1"),
            ],
            vec![],
        ),
    ])
}

fn usdt(a: &Option<tong_funding_core::cost_attribution::Amount>) -> Decimal {
    a.as_ref().expect("known").usdt
}

#[test]
fn cost_attribution_spec_numbers_split_entry_and_exit_per_leg_and_in_total() {
    let a = attribute_costs(&spec_pair());
    assert_eq!(usdt(&a.entry_slippage), d("2.0"), "long 1.0 + short 1.0");
    assert_eq!(usdt(&a.exit_slippage), d("2.0"), "long 1.0 + short 1.0");
    assert!(a.unknown.is_empty(), "{:?}", a.unknown);
}

#[test]
fn cost_attribution_identities_hold_against_compute_pnl() {
    let input = spec_pair();
    let (a, b) = (attribute_costs(&input), compute_pnl(&input));
    assert_eq!(usdt(&a.entry_slippage) + usdt(&a.exit_slippage), b.total.slippage, "entry + exit = slippage");
    assert_eq!(usdt(&a.drift), b.total.price_ref, "drift = price_ref");
    assert_eq!(usdt(&a.funding), b.total.funding);
    assert_eq!(usdt(&a.fees), b.total.opening_fee + b.total.closing_fee);
    let net = usdt(&a.funding) + usdt(&a.drift) - usdt(&a.entry_slippage) - usdt(&a.exit_slippage) - usdt(&a.fees);
    assert_eq!(net, b.total.net, "funding + drift - entry - exit - fees = net");
    assert_eq!(usdt(&a.net), b.total.net);
}

#[test]
fn cost_attribution_pct_is_of_the_average_open_notional_per_leg() {
    // open notionals 10 x 100.1 = 1001 and 10 x 100.1 = 1001; entry slippage 2.0
    let a = attribute_costs(&spec_pair());
    assert_eq!(a.notional_per_leg, Some(d("1001")));
    assert_eq!(a.entry_slippage.as_ref().unwrap().pct, Some(d("2.0") / d("1001") * d("100")));

    // spec scenario: notionals 1000 and 990 -> 995; the percentage is the drift over 995 (full precision, no rounding)
    let input = pair(vec![
        leg(
            Exchange::Binance,
            Side::Long,
            vec![fill("l-open", FillAction::Open, "10", Some("100"), "100", "0"), fill("l-close", FillAction::Close, "10", Some("99.9"), "99.9", "0")],
            vec![],
        ),
        leg(
            Exchange::Bybit,
            Side::Short,
            vec![fill("s-open", FillAction::Open, "10", Some("99"), "99", "0"), fill("s-close", FillAction::Close, "10", Some("99"), "99", "0")],
            vec![],
        ),
    ]);
    let a = attribute_costs(&input);
    assert_eq!(a.notional_per_leg, Some(d("995")));
    // long reference PnL (99.9 - 100) x 10 = -1, short 0
    let drift = a.drift.as_ref().unwrap();
    assert_eq!(drift.usdt, d("-1"));
    assert_eq!(drift.pct, Some(d("-1") / d("995") * d("100")));
}

#[test]
fn cost_attribution_missing_close_reference_makes_exit_and_drift_unknown_not_zero() {
    let mut input = spec_pair();
    input.legs[0].fills[1].expected_price = None;
    let a = attribute_costs(&input);
    assert!(a.exit_slippage.is_none() && a.drift.is_none(), "{a:?}");
    assert!(a.unknown.contains(&"exit_slippage".to_string()) && a.unknown.contains(&"drift".to_string()), "{:?}", a.unknown);
    assert_eq!(usdt(&a.entry_slippage), d("2.0"), "entry is unaffected");
    assert!(a.funding.is_some() && a.fees.is_some(), "funding and fees still shown");
    assert!(a.net.is_none(), "net is incomplete when a component is missing");
    // the existing breakdown is unchanged by the missing close reference: slippage stays flagged, not zeroed
    let b = compute_pnl(&input);
    assert!(b.missing.contains(&tong_funding_core::pnl::Component::Slippage));
}

#[test]
fn cost_attribution_missing_open_reference_makes_entry_and_drift_unknown() {
    let mut input = spec_pair();
    input.legs[1].fills[0].expected_price = None;
    let a = attribute_costs(&input);
    assert!(a.entry_slippage.is_none() && a.drift.is_none());
    assert_eq!(usdt(&a.exit_slippage), d("2.0"));
}

#[test]
fn cost_attribution_missing_fee_makes_only_fees_and_net_unknown() {
    let mut input = spec_pair();
    input.legs[0].fills[0].fee = None;
    let a = attribute_costs(&input);
    assert!(a.fees.is_none() && a.net.is_none());
    assert_eq!(usdt(&a.entry_slippage), d("2.0"));
    assert_eq!(usdt(&a.drift), compute_pnl(&input).total.price_ref);
}

#[test]
fn cost_attribution_without_open_fill_prices_has_no_notional_and_no_pct() {
    let mut input = spec_pair();
    input.legs[0].fills[0].actual_price = None;
    let a = attribute_costs(&input);
    assert!(a.notional_per_leg.is_none());
    assert!(a.unknown.contains(&"notional_per_leg".to_string()));
    assert_eq!(a.exit_slippage.as_ref().unwrap().pct, None, "pct unknown without a notional, usdt still shown");
    assert_eq!(usdt(&a.exit_slippage), d("2.0"));
}

#[test]
fn cost_attribution_no_fills_is_all_unknown() {
    let input = pair(vec![leg(Exchange::Binance, Side::Long, vec![], vec![]), leg(Exchange::Bybit, Side::Short, vec![], vec![])]);
    let a = attribute_costs(&input);
    assert!(a.entry_slippage.is_none() && a.exit_slippage.is_none() && a.drift.is_none() && a.fees.is_none() && a.net.is_none());
}

#[test]
fn cost_attribution_unconverted_okx_fill_is_unknown_not_zero() {
    let mut input = spec_pair();
    input.legs[1].fills[1].contract_value_missing = true;
    let a = attribute_costs(&input);
    assert!(a.exit_slippage.is_none() && a.drift.is_none());
    assert_eq!(usdt(&a.entry_slippage), d("2.0"));
}

#[test]
fn cost_attribution_serialises_decimals_as_strings_and_unknown_as_null() {
    let mut input = spec_pair();
    input.legs[0].fills[1].expected_price = None;
    let v = serde_json::to_value(attribute_costs(&input)).unwrap();
    assert_eq!(v["entry_slippage"]["usdt"], json!("2"));
    assert!(v["exit_slippage"].is_null() && v["drift"].is_null());
    assert_eq!(v["unknown"], json!(["drift", "exit_slippage", "net"]));
    let _: CostAttribution = serde_json::from_value(v).unwrap();
}

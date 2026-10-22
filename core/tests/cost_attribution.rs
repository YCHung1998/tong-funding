//! cost-attribution 6.1 (spec cost-attribution): the two values the PnL `breakdown` does not
//! carry, `entry_slippage` and `notional_per_leg`. Hand-computed cases; the parts the spec derives
//! from `breakdown` (exit slippage, drift) are pinned against `compute_pnl`.
//! Run: `cargo test -p tong-funding-core --test cost_attribution`.

use std::str::FromStr;

use serde_json::json;
use tong_funding_core::cost_attribution::{entry_slippage, notional_per_leg};
use tong_funding_core::pnl::{
    compute_pnl, Component, FillAction, FillRecord, FundingFetchState, FundingLedgerEntry, LegInput, PairInput,
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

/// A leg with nothing but the given opening fills (the closing side does not matter to the entry values).
fn opening(exchange: Exchange, side: Side, fills: Vec<FillRecord>) -> LegInput {
    leg(exchange, side, fills, vec![])
}

fn open_fill(id: &str, qty: &str, expected: &str, actual: &str) -> FillRecord {
    fill(id, FillAction::Open, qty, Some(expected), actual, "0")
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

#[test]
fn cost_attribution_spec_numbers_entry_slippage_notional_and_the_derived_exit_slippage() {
    let input = spec_pair();
    let entry = entry_slippage(&input).expect("known");
    assert_eq!(entry, d("2.0"), "long 1.0 + short 1.0");
    assert_eq!(notional_per_leg(&input), Some(d("1001")), "10 x 100.1 on both legs");
    assert_eq!((entry.to_string(), notional_per_leg(&input).unwrap().to_string()), ("2".to_string(), "1001".to_string()), "normalised");

    // the rest is derived from the breakdown, not stored (spec): exit slippage and drift
    let b = compute_pnl(&input);
    assert_eq!(b.total.slippage, d("4.0"));
    assert_eq!(b.total.slippage - entry, d("2.0"), "exit slippage = slippage - entry slippage");
    assert_eq!(b.total.price_ref, d("1.0"), "drift = price_ref");
}

#[test]
fn cost_attribution_entry_slippage_is_positive_when_adverse_for_both_sides() {
    let one = |side: Side, expected: &str, actual: &str| entry_slippage(&pair(vec![opening(Exchange::Binance, side, vec![open_fill("o", "10", expected, actual)])]));
    // buying the long leg above the reference, selling the short leg below it: adverse = a cost
    assert_eq!(one(Side::Long, "100", "100.1"), Some(d("1.0")));
    assert_eq!(one(Side::Short, "100", "99.9"), Some(d("1.0")));
    // the other way round is a gain
    assert_eq!(one(Side::Long, "100", "99.9"), Some(d("-1.0")));
    assert_eq!(one(Side::Short, "100", "100.1"), Some(d("-1.0")));
}

#[test]
fn cost_attribution_adds_up_several_opening_orders_of_a_leg() {
    // long: 4 @ 100.1 and 6 @ 100.3 against the reference 100 -> 0.4 + 1.8 = 2.2; value 400.4 + 601.8 = 1002.2
    // short: 10 @ 100.1 against 100.2 -> 1.0; value 1001
    let input = pair(vec![
        opening(Exchange::Binance, Side::Long, vec![open_fill("l1", "4", "100", "100.1"), open_fill("l2", "6", "100", "100.3")]),
        opening(Exchange::Bybit, Side::Short, vec![open_fill("s1", "10", "100.2", "100.1")]),
    ]);
    assert_eq!(entry_slippage(&input), Some(d("3.2")));
    assert_eq!(notional_per_leg(&input), Some(d("1001.6")), "(1002.2 + 1001) / 2");
}

#[test]
fn cost_attribution_partial_close_keeps_the_entry_values_known() {
    // the long leg opened 10 but only 6 were closed: the breakdown flags slippage and price_ref as missing
    let mut input = spec_pair();
    input.legs[0].fills[1].quantity = d("6");
    assert_eq!(entry_slippage(&input), Some(d("2.0")));
    assert_eq!(notional_per_leg(&input), Some(d("1001")));
    let missing = compute_pnl(&input).missing;
    assert!(missing.contains(&Component::Slippage) && missing.contains(&Component::PriceRef), "{missing:?}");
}

#[test]
fn cost_attribution_a_leg_that_never_opened_contributes_zero_and_is_not_averaged() {
    // the short leg's order failed: no fill at all
    let mut input = pair(vec![
        opening(Exchange::Binance, Side::Long, vec![open_fill("l-open", "10", "100", "100.1")]),
        opening(Exchange::Bybit, Side::Short, vec![]),
    ]);
    assert_eq!(entry_slippage(&input), Some(d("1.0")), "only the leg that traded");
    assert_eq!(notional_per_leg(&input), Some(d("1001")), "that leg's opening value, not halved");

    // a fill record with nothing filled (zero quantity, no prices) is no opening fill either
    let mut nothing = fill("s-open", FillAction::Open, "0", None, "0", "0");
    nothing.actual_price = None;
    input.legs[1].fills = vec![nothing];
    assert_eq!(entry_slippage(&input), Some(d("1.0")));
    assert_eq!(notional_per_leg(&input), Some(d("1001")));
}

#[test]
fn cost_attribution_no_opening_fill_on_any_leg_is_unknown_not_zero() {
    assert_eq!((entry_slippage(&pair(vec![])), notional_per_leg(&pair(vec![]))), (None, None), "no legs");
    let close = |id: &str| fill(id, FillAction::Close, "10", Some("100"), "100", "0");
    let only_closes = pair(vec![
        opening(Exchange::Binance, Side::Long, vec![close("l-close")]),
        opening(Exchange::Bybit, Side::Short, vec![close("s-close")]),
    ]);
    assert_eq!((entry_slippage(&only_closes), notional_per_leg(&only_closes)), (None, None), "closing fills are no entry");
}

#[test]
fn cost_attribution_missing_opening_reference_makes_both_unknown_and_leaves_the_breakdown_as_it_is() {
    let mut input = spec_pair();
    input.legs[1].fills[0].expected_price = None;
    assert_eq!((entry_slippage(&input), notional_per_leg(&input)), (None, None));
    let missing = compute_pnl(&input).missing;
    assert!(missing.contains(&Component::PriceRef) && missing.contains(&Component::Slippage), "{missing:?}");

    // any opening fill of a leg counts, not only the first
    let mut input = pair(vec![
        opening(Exchange::Binance, Side::Long, vec![open_fill("l1", "4", "100", "100.1"), open_fill("l2", "6", "100", "100.3")]),
        opening(Exchange::Bybit, Side::Short, vec![open_fill("s1", "10", "100.2", "100.1")]),
    ]);
    input.legs[0].fills[1].expected_price = None;
    assert_eq!((entry_slippage(&input), notional_per_leg(&input)), (None, None));
}

#[test]
fn cost_attribution_missing_opening_fill_price_makes_both_unknown() {
    let mut input = spec_pair();
    input.legs[0].fills[0].actual_price = None;
    assert_eq!((entry_slippage(&input), notional_per_leg(&input)), (None, None));
}

#[test]
fn cost_attribution_unconverted_okx_opening_fill_makes_both_unknown() {
    // the quantity is in contracts, not coins: nothing built on it is a number of USDT
    let mut input = spec_pair();
    input.legs[1].fills[0].contract_value_missing = true;
    assert_eq!((entry_slippage(&input), notional_per_leg(&input)), (None, None));
}

#[test]
fn cost_attribution_closing_fills_do_not_change_the_entry_values() {
    let mut input = spec_pair();
    input.legs[0].fills[1].expected_price = None; // close reference missing
    input.legs[1].fills[1].actual_price = None; // close price missing
    assert_eq!(entry_slippage(&input), Some(d("2.0")));
    assert_eq!(notional_per_leg(&input), Some(d("1001")));
    input.legs[0].fills[1].contract_value_missing = true; // an unconverted close
    assert_eq!(entry_slippage(&input), Some(d("2.0")));
    assert_eq!(notional_per_leg(&input), Some(d("1001")));
}

#[test]
fn cost_attribution_overflow_is_unknown_and_never_panics() {
    let max = Decimal::MAX.to_string();
    let one = |side: Side, qty: &str, expected: &str, actual: &str| opening(Exchange::Binance, side, vec![open_fill("o", qty, expected, actual)]);
    let known = |input: &PairInput| (entry_slippage(input), notional_per_leg(input));

    // (reference - actual) x quantity does not fit
    let product = pair(vec![one(Side::Short, "10", &max, "1")]);
    assert_eq!(known(&product).0, None, "product");
    // reference - actual does not fit
    let difference = pair(vec![one(Side::Short, "1", &max, "-1")]);
    assert_eq!(known(&difference).0, None, "difference");
    // each leg's slippage fits (+6e28), their sum (1.2e29) does not
    let sixty = "60000000000000000000000000001"; // 6e28 + 1
    let sum = pair(vec![one(Side::Long, "1", "1", sixty), one(Side::Short, "1", sixty, "1")]);
    assert_eq!(known(&sum).0, None, "sum of the legs");
    // the opening value (price x quantity) does not fit; the slippage of the same fill is 0
    let value = pair(vec![one(Side::Long, "2", &max, &max)]);
    assert_eq!(known(&value), (Some(Decimal::ZERO), None), "value of a fill");
    // each value fits (5e28), their sum does not
    let fifty = "50000000000000000000000000000";
    let total = pair(vec![one(Side::Long, "1", fifty, fifty), one(Side::Short, "1", fifty, fifty)]);
    assert_eq!(known(&total), (Some(Decimal::ZERO), None), "sum of the values");
}

#[test]
fn cost_attribution_a_notional_that_is_not_positive_is_unknown() {
    // a notional is the denominator of every percentage: never 0 or negative
    let input = pair(vec![opening(Exchange::Binance, Side::Long, vec![open_fill("l-open", "10", "100", "0")])]);
    assert_eq!(notional_per_leg(&input), None);
    assert_eq!(entry_slippage(&input), Some(d("-1000")), "the slippage itself is still computed");
}

use rust_decimal::Decimal;
use std::str::FromStr;
use tong_funding_core::quantity::{ClosingQuantity, LotSize, Quantity, QuantityError};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn lot(step: &str, min: &str) -> LotSize {
    LotSize { step_size: d(step), min_qty: d(min) }
}

#[test]
fn floors_to_step() {
    let q = Quantity::round_down(d("2.14589213"), &lot("0.1", "0.1")).unwrap();
    assert_eq!(q.value(), d("2.1"));
}

#[test]
fn exact_multiple_is_not_reduced() {
    let q = Quantity::round_down(d("2.3"), &lot("0.1", "0.1")).unwrap();
    assert_eq!(q.value(), d("2.3"));
}

#[test]
fn never_rounds_up() {
    let q = Quantity::round_down(d("2.19999"), &lot("0.1", "0.1")).unwrap();
    assert_eq!(q.value(), d("2.1"));
}

#[test]
fn below_minimum_is_error() {
    let e = Quantity::round_down(d("0.0004"), &lot("0.001", "0.001")).unwrap_err();
    assert!(matches!(e, QuantityError::BelowMinimum { .. }), "{e:?}");
}

#[test]
fn rounds_to_zero_even_with_zero_min_is_error() {
    let e = Quantity::round_down(d("0.0004"), &lot("0.001", "0")).unwrap_err();
    assert!(matches!(e, QuantityError::BelowMinimum { .. }), "{e:?}");
}

#[test]
fn nonpositive_step_is_error() {
    let e = Quantity::round_down(d("1"), &lot("0", "0")).unwrap_err();
    assert!(matches!(e, QuantityError::InvalidStepSize(_)), "{e:?}");
}

#[test]
fn order_string_decimals_follow_step() {
    let l = lot("0.1", "0.1");
    assert_eq!(Quantity::round_down(d("2.1"), &l).unwrap().to_order_string(&l), "2.1");
    assert_eq!(Quantity::round_down(d("2.10"), &l).unwrap().to_order_string(&l), "2.1");
    assert_eq!(Quantity::round_down(d("2"), &l).unwrap().to_order_string(&l), "2.0");
    let l1 = lot("1", "1");
    assert_eq!(Quantity::round_down(d("2"), &l1).unwrap().to_order_string(&l1), "2");
    assert_eq!(Quantity::round_down(d("2.7"), &l1).unwrap().to_order_string(&l1), "2");
}

#[test]
fn order_string_with_trailing_zero_step() {
    let l = lot("0.010", "0.010");
    assert_eq!(Quantity::round_down(d("1.5"), &l).unwrap().to_order_string(&l), "1.50");
}

#[test]
fn finer_step_from_notional() {
    let l = lot("0.001", "0.001");
    let q = Quantity::from_notional(d("1200"), d("60200"), &l).unwrap();
    assert_eq!(q.to_order_string(&l), "0.019");
}

#[test]
fn from_notional_rejects_nonpositive_price() {
    let e = Quantity::from_notional(d("1200"), d("0"), &lot("0.001", "0.001")).unwrap_err();
    assert!(matches!(e, QuantityError::InvalidPrice(_)), "{e:?}");
}

#[test]
fn okx_converts_base_to_contracts() {
    let q = Quantity::okx_contracts(d("0.021"), d("0.01"), &lot("1", "1")).unwrap();
    assert_eq!(q.value(), d("2"));
}

#[test]
fn okx_nonpositive_ct_val_is_error() {
    for ct in ["0", "-0.01"] {
        let e = Quantity::okx_contracts(d("0.021"), d(ct), &lot("1", "1")).unwrap_err();
        assert!(matches!(e, QuantityError::InvalidContractValue(_)), "{e:?}");
    }
}

#[test]
fn okx_below_min_contracts_is_error() {
    let e = Quantity::okx_contracts(d("0.005"), d("0.01"), &lot("1", "1")).unwrap_err();
    assert!(matches!(e, QuantityError::BelowMinimum { .. }), "{e:?}");
}

#[test]
fn closing_quantity_uses_absolute_position_without_rounding() {
    let q = ClosingQuantity::from_exchange_position(d("-0.019")).unwrap();
    assert_eq!(q.value(), d("0.019"));
    let q = ClosingQuantity::from_exchange_position(d("0.0191234")).unwrap();
    assert_eq!(q.value(), d("0.0191234"));
}

#[test]
fn closing_string_never_truncates_position_digits() {
    let l = lot("0.001", "0.001");
    let q = ClosingQuantity::from_exchange_position(d("-0.0191234")).unwrap();
    assert_eq!(q.to_order_string(&l), "0.0191234");
}

#[test]
fn no_position_is_error() {
    let e = ClosingQuantity::from_exchange_position(d("0")).unwrap_err();
    assert_eq!(e, QuantityError::NoPosition);
}

// ---- matched-leg-quantity: both legs trade the same coin quantity ----

use tong_funding_core::quantity::{MatchedLeg, matched_quantity, DEFAULT_QTY_PRECISION};

fn leg(price: &str, step: &str, min: &str) -> MatchedLeg {
    MatchedLeg { price: d(price), lot: lot(step, min), ct_val: None }
}

#[test]
fn matched_uses_the_coarser_common_step_and_the_higher_price() {
    let m = matched_quantity(d("1000"), &leg("60000", "0.001", "0.001"), &leg("60100", "0.0001", "0.0001")).unwrap();
    assert_eq!(m.common_step, d("0.001"));
    assert_eq!(m.base_qty, d("0.016"));
    assert_eq!((m.long.value(), m.short.value()), (d("0.016"), d("0.016")));
    assert!(m.base_qty * d("60000") <= d("1000") && m.base_qty * d("60100") <= d("1000"));
}

#[test]
fn matched_never_goes_finer_than_the_default_precision() {
    assert_eq!(DEFAULT_QTY_PRECISION, d("0.000001"));
    let m = matched_quantity(d("1000"), &leg("3000", "0.00000001", "0.00000001"), &leg("3000", "0.00000001", "0.00000001")).unwrap();
    assert_eq!((m.common_step, m.base_qty), (d("0.000001"), d("0.333333")));
}

#[test]
fn matched_common_step_is_the_least_common_multiple() {
    let m = matched_quantity(d("100"), &leg("9", "0.002", "0.002"), &leg("9", "0.005", "0.005")).unwrap();
    assert_eq!((m.common_step, m.base_qty), (d("0.01"), d("11.11")));
}

#[test]
fn matched_okx_leg_is_sent_in_whole_contracts() {
    let okx = MatchedLeg { price: d("60000"), lot: lot("1", "1"), ct_val: Some(d("0.01")) };
    let m = matched_quantity(d("1000"), &okx, &leg("60000", "0.001", "0.001")).unwrap();
    assert_eq!((m.common_step, m.base_qty), (d("0.01"), d("0.01")));
    assert_eq!((m.long.value(), m.short.value()), (d("1"), d("0.01")));
}

#[test]
fn matched_below_the_larger_minimum_is_an_error() {
    let e = matched_quantity(d("4"), &leg("1000", "0.001", "0.001"), &leg("1000", "0.001", "0.005")).unwrap_err();
    assert_eq!(e, QuantityError::BelowMinimum { adjusted: d("0.004"), min_qty: d("0.005") });
}

#[test]
fn matched_rejects_non_positive_inputs_and_overflow_without_panicking() {
    assert!(matches!(matched_quantity(d("1000"), &leg("0", "0.001", "0.001"), &leg("1", "0.001", "0.001")), Err(QuantityError::InvalidPrice(_))));
    assert!(matches!(matched_quantity(d("1000"), &leg("1", "0", "0"), &leg("1", "0.001", "0.001")), Err(QuantityError::InvalidStepSize(_))));
    let okx = MatchedLeg { price: d("1"), lot: lot("1", "1"), ct_val: Some(d("0")) };
    assert!(matches!(matched_quantity(d("1000"), &okx, &leg("1", "0.001", "0.001")), Err(QuantityError::InvalidContractValue(_))));
    // Co-prime 28-digit steps: the LCM does not fit; an error, never a panic.
    let big = matched_quantity(d("1000"), &leg("1", "0.9999999999999999999999999999", "0"), &leg("1", "0.9999999999999999999999999997", "0"));
    assert!(matches!(big, Err(QuantityError::NoCommonStep(..))), "{big:?}");
}

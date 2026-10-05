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

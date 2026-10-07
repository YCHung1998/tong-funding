//! Cost estimate of a pair at one shared quantity (spec: trade-cost-estimate).
//! Pure `Decimal` maths: the staged orders page (display) and the pre-trade Margin check both use
//! [`leg_cost`], so the displayed margin and the checked margin cannot drift apart.

use crate::types::{Decimal, Pct, Price};
use thiserror::Error;

/// Best bid / best ask of one symbol on one exchange, sizes in BASE-coin units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopOfBook {
    pub bid_price: Price,
    pub bid_qty: Decimal,
    pub ask_price: Price,
    pub ask_qty: Decimal,
    /// When this system received the quote (Unix ms).
    pub observed_at: i64,
}

/// Why a cost cannot be computed (nothing is ever computed with a default).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CostError {
    #[error("quantity must be positive, got {0}")]
    InvalidQuantity(Decimal),
    #[error("price must be positive, got {0}")]
    InvalidPrice(Decimal),
    #[error("leverage must be positive, got {0}")]
    InvalidLeverage(Decimal),
    #[error("taker fee must not be negative, got {0}")]
    InvalidFee(Decimal),
}

/// One leg's estimated cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegCost {
    /// `qty × price`.
    pub value: Decimal,
    /// `value × fee_pct / 100`.
    pub open_fee: Decimal,
    /// Same value and rate as the opening fee (estimate only; paid when closing).
    pub close_fee_est: Decimal,
    /// `value / leverage + open_fee`.
    pub margin: Decimal,
}

/// `qty × price`, its opening fee and the margin it needs. `fee_pct` is a percentage number.
pub fn leg_cost(qty: Decimal, price: Price, fee_pct: Pct, leverage: Decimal) -> Result<LegCost, CostError> {
    if qty <= Decimal::ZERO {
        return Err(CostError::InvalidQuantity(qty));
    }
    if price <= Decimal::ZERO {
        return Err(CostError::InvalidPrice(price));
    }
    if leverage <= Decimal::ZERO {
        return Err(CostError::InvalidLeverage(leverage));
    }
    if fee_pct < Decimal::ZERO {
        return Err(CostError::InvalidFee(fee_pct));
    }
    let value = qty * price;
    let open_fee = value * fee_pct / Decimal::ONE_HUNDRED;
    Ok(LegCost { value, open_fee, close_fee_est: open_fee, margin: value / leverage + open_fee })
}

/// Inputs of the pair estimate. `long_ask*` is the long leg's best ask (it buys), `short_bid*` the
/// short leg's best bid (it sells).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostInput {
    pub qty: Decimal,
    pub long_ask: Price,
    pub long_ask_qty: Decimal,
    pub short_bid: Price,
    pub short_bid_qty: Decimal,
    pub long_fee_pct: Pct,
    pub short_fee_pct: Pct,
    pub leverage: Decimal,
}

/// A leg's cost plus whether `qty` exceeds the best level's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegEstimate {
    pub cost: LegCost,
    /// `qty` is larger than the top-of-book size: the real fill walks into the second level and is
    /// worse than shown. A hint only; it never blocks anything.
    pub exceeds_top_level: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostEstimate {
    pub long: LegEstimate,
    pub short: LegEstimate,
    /// Sum of both legs' margin.
    pub total_margin: Decimal,
    /// Both legs' opening + estimated closing fees.
    pub total_fees: Decimal,
}

pub fn estimate_cost(i: &CostInput) -> Result<CostEstimate, CostError> {
    let long = LegEstimate {
        cost: leg_cost(i.qty, i.long_ask, i.long_fee_pct, i.leverage)?,
        exceeds_top_level: i.qty > i.long_ask_qty,
    };
    let short = LegEstimate {
        cost: leg_cost(i.qty, i.short_bid, i.short_fee_pct, i.leverage)?,
        exceeds_top_level: i.qty > i.short_bid_qty,
    };
    Ok(CostEstimate {
        long,
        short,
        total_margin: long.cost.margin + short.cost.margin,
        total_fees: [long.cost, short.cost].iter().map(|c| c.open_fee + c.close_fee_est).sum(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn spec_input() -> CostInput {
        CostInput {
            qty: d("0.016"),
            long_ask: d("60010"),
            long_ask_qty: d("5"),
            short_bid: d("60090"),
            short_bid_qty: d("5"),
            long_fee_pct: d("0.05"),
            short_fee_pct: d("0.055"),
            leverage: d("5"),
        }
    }

    #[test]
    fn spec_numbers() {
        let e = estimate_cost(&spec_input()).unwrap();
        assert_eq!(e.long.cost.value, d("960.16"));
        assert_eq!(e.short.cost.value, d("961.44"));
        assert_eq!(e.long.cost.open_fee, d("0.48008"));
        assert_eq!(e.short.cost.open_fee, d("0.528792"));
        assert_eq!(e.long.cost.margin, d("192.51208"));
        assert_eq!(e.short.cost.margin, d("192.816792"));
        assert_eq!(e.total_margin, d("385.328872"));
        assert_eq!(e.long.cost.close_fee_est, d("0.48008"));
        assert_eq!(e.short.cost.close_fee_est, d("0.528792"));
        assert_eq!(e.total_fees, d("2.017744"));
        assert!(!e.long.exceeds_top_level && !e.short.exceeds_top_level);
    }

    #[test]
    fn quantity_above_top_level_is_flagged_but_priced_at_the_top() {
        let mut i = spec_input();
        i.long_ask_qty = d("0.010");
        let e = estimate_cost(&i).unwrap();
        assert!(e.long.exceeds_top_level);
        assert!(!e.short.exceeds_top_level);
        assert_eq!(e.long.cost.value, d("960.16"));
        i.long_ask_qty = d("0.016");
        assert!(!estimate_cost(&i).unwrap().long.exceeds_top_level, "equal is not exceeding");
    }

    #[test]
    fn spec_pretrade_margin_192_48() {
        // 0.016 x 60,000 / 5 + 0.48 fee
        let c = leg_cost(d("0.016"), d("60000"), d("0.05"), d("5")).unwrap();
        assert_eq!(c.margin, d("192.48"));
        assert_eq!(c.open_fee, d("0.48"));
    }

    #[test]
    fn invalid_inputs_are_errors_never_defaults() {
        assert!(matches!(leg_cost(d("0"), d("1"), d("0.05"), d("5")), Err(CostError::InvalidQuantity(_))));
        assert!(matches!(leg_cost(d("1"), d("0"), d("0.05"), d("5")), Err(CostError::InvalidPrice(_))));
        assert!(matches!(leg_cost(d("1"), d("1"), d("0.05"), d("0")), Err(CostError::InvalidLeverage(_))));
        assert!(matches!(leg_cost(d("1"), d("1"), d("-0.05"), d("5")), Err(CostError::InvalidFee(_))));
        let mut i = spec_input();
        i.short_bid = d("0");
        assert!(estimate_cost(&i).is_err());
    }
}

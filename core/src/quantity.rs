//! Order quantities: floor to the exchange step, never round up (spec: quantity-precision).

use crate::types::{Notional, Price};
use rust_decimal::Decimal;
use thiserror::Error;

/// Exchange lot-size filter: `step_size` increment and `min_qty` minimum order size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LotSize {
    pub step_size: Decimal,
    pub min_qty: Decimal,
}

/// Why a quantity could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QuantityError {
    #[error("quantity {adjusted} is below the minimum order size {min_qty}")]
    BelowMinimum { adjusted: Decimal, min_qty: Decimal },
    #[error("step_size must be positive, got {0}")]
    InvalidStepSize(Decimal),
    #[error("ct_val must be positive, got {0}")]
    InvalidContractValue(Decimal),
    #[error("price must be positive, got {0}")]
    InvalidPrice(Decimal),
    #[error("exchange reports no open position")]
    NoPosition,
    #[error("no common step for {0} and {1}")]
    NoCommonStep(Decimal, Decimal),
}

/// A tradable quantity. Only constructible via step rounding or from an exchange position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Quantity(Decimal);

impl Quantity {
    /// Floors `raw` to `lot.step_size`; errors if the result is below `lot.min_qty`.
    pub fn round_down(raw: Decimal, lot: &LotSize) -> Result<Quantity, QuantityError> {
        if lot.step_size <= Decimal::ZERO {
            return Err(QuantityError::InvalidStepSize(lot.step_size));
        }
        // Decimal remainder is exact, so exact multiples (2.3 / 0.1) are never reduced.
        let adjusted = raw - raw % lot.step_size;
        if adjusted <= Decimal::ZERO || adjusted < lot.min_qty {
            return Err(QuantityError::BelowMinimum { adjusted, min_qty: lot.min_qty });
        }
        Ok(Quantity(adjusted))
    }

    /// Converts `notional / price` to a base quantity, then floors it to the step.
    pub fn from_notional(
        notional: Notional,
        price: Price,
        lot: &LotSize,
    ) -> Result<Quantity, QuantityError> {
        if price <= Decimal::ZERO {
            return Err(QuantityError::InvalidPrice(price));
        }
        Quantity::round_down(notional / price, lot)
    }

    /// OKX: converts a base-asset quantity to contracts (`base_qty / ct_val`), then floors to `lot`.
    pub fn okx_contracts(
        base_qty: Decimal,
        ct_val: Decimal,
        lot: &LotSize,
    ) -> Result<Quantity, QuantityError> {
        if ct_val <= Decimal::ZERO {
            return Err(QuantityError::InvalidContractValue(ct_val));
        }
        Quantity::round_down(base_qty / ct_val, lot)
    }

    /// The underlying decimal value.
    pub fn value(self) -> Decimal {
        self.0
    }

    /// The order string: decimal places derived from `step_size`.
    pub fn to_order_string(self, lot: &LotSize) -> String {
        format_decimal(self.0, lot)
    }
}

/// Decimal places come from `step_size`; digits finer than the step are never truncated.
fn format_decimal(value: Decimal, lot: &LotSize) -> String {
    let step_decimals = lot.step_size.normalize().scale();
    let own = value.normalize();
    if own.scale() > step_decimals {
        own.to_string()
    } else {
        let mut v = own;
        v.rescale(step_decimals);
        v.to_string()
    }
}

/// The finest base-coin unit a paired quantity is ever sized to (1e-6).
pub const DEFAULT_QTY_PRECISION: Decimal = Decimal::from_parts(1, 0, 0, false, 6);

/// One leg of a paired entry: its price, its lot filter in the exchange's order unit and, on OKX,
/// the contract value (base coin per contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedLeg {
    pub price: Price,
    pub lot: LotSize,
    pub ct_val: Option<Decimal>,
}

/// The same base-coin quantity for both legs, and each leg's order quantity (contracts on OKX).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedQuantity {
    pub base_qty: Decimal,
    pub common_step: Decimal,
    pub long: Quantity,
    pub short: Quantity,
}

/// Sizes a pair to ONE base-coin quantity (spec: quantity-precision, "配對開倉的兩腿使用同一數量").
/// Common step = LCM of both legs' steps (in base coin; OKX `lotSz × ctVal`) and
/// [`DEFAULT_QTY_PRECISION`]; minimum = the larger base-coin minimum; quantity =
/// `notional / max(long price, short price)` floored to the common step, so each leg's value is
/// at most `notional` and as close to it as the step allows.
pub fn matched_quantity(notional: Notional, long: &MatchedLeg, short: &MatchedLeg) -> Result<MatchedQuantity, QuantityError> {
    let (long_step, long_min) = base_lot(long)?;
    let (short_step, short_min) = base_lot(short)?;
    let common_step = lcm(lcm(long_step, short_step)?, DEFAULT_QTY_PRECISION)?;
    let min_qty = long_min.max(short_min);
    let raw = notional / long.price.max(short.price);
    let base_qty = (raw - raw % common_step).normalize();
    if base_qty <= Decimal::ZERO || base_qty < min_qty {
        return Err(QuantityError::BelowMinimum { adjusted: base_qty, min_qty });
    }
    // The common step is a multiple of every leg's step, so these are exact.
    let order = |leg: &MatchedLeg| Quantity(leg.ct_val.map_or(base_qty, |ct| (base_qty / ct).normalize()));
    Ok(MatchedQuantity { base_qty, common_step, long: order(long), short: order(short) })
}

/// A leg's step and minimum in base-coin units, validating price, step and contract value.
fn base_lot(leg: &MatchedLeg) -> Result<(Decimal, Decimal), QuantityError> {
    if leg.price <= Decimal::ZERO {
        return Err(QuantityError::InvalidPrice(leg.price));
    }
    if leg.lot.step_size <= Decimal::ZERO {
        return Err(QuantityError::InvalidStepSize(leg.lot.step_size));
    }
    match leg.ct_val {
        Some(ct) if ct <= Decimal::ZERO => Err(QuantityError::InvalidContractValue(ct)),
        Some(ct) => Ok(((leg.lot.step_size * ct).normalize(), leg.lot.min_qty * ct)),
        None => Ok((leg.lot.step_size.normalize(), leg.lot.min_qty)),
    }
}

/// Exact least common multiple of two positive decimals (as integers at their common scale).
fn lcm(a: Decimal, b: Decimal) -> Result<Decimal, QuantityError> {
    let scale = a.scale().max(b.scale());
    let int = |x: Decimal| {
        let mut x = x;
        x.rescale(scale);
        x.mantissa()
    };
    let (ia, ib) = (int(a), int(b));
    let gcd = |mut x: i128, mut y: i128| {
        while y != 0 {
            (x, y) = (y, x % y);
        }
        x
    };
    (ia / gcd(ia, ib))
        .checked_mul(ib)
        .and_then(|m| Decimal::try_from_i128_with_scale(m, scale).ok())
        .map(|d| d.normalize())
        .ok_or(QuantityError::NoCommonStep(a, b))
}

/// A quantity for CLOSING a position, taken from the exchange-reported position (absolute value,
/// never rounded). It is a different type from [`Quantity`] on purpose: opening orders accept only
/// `Quantity` (which can only come from lot-size rounding), so a position-derived, unrounded
/// amount can never be used to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClosingQuantity(Decimal);

impl ClosingQuantity {
    /// Absolute value of the position; zero is an error (nothing to close).
    pub fn from_exchange_position(position: Decimal) -> Result<ClosingQuantity, QuantityError> {
        if position.is_zero() {
            return Err(QuantityError::NoPosition);
        }
        Ok(ClosingQuantity(position.abs()))
    }

    /// The underlying decimal value.
    pub fn value(self) -> Decimal {
        self.0
    }

    /// The order string (never truncates position digits).
    pub fn to_order_string(self, lot: &LotSize) -> String {
        format_decimal(self.0, lot)
    }
}

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

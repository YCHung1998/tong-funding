//! Shared primitives. Money-like values are plain `Decimal` aliases (no wrapper types, by
//! design: see openspec/changes/core-domain-and-fixtures/design.md "決定紀錄").
//!
//! Unit convention (spec: net-edge): funding rates are fractions (0.0001 = 0.01%), while every
//! field/parameter ending in `_pct` is a *percentage number* (0.01 = 0.01%).

use serde::{Deserialize, Serialize};

pub use rust_decimal::Decimal;

pub type Rate = Decimal;
pub type Price = Decimal;
pub type Notional = Decimal;
/// A percentage number: 0.01 means 0.01%.
pub type Pct = Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Exchange {
    Binance,
    Bybit,
    Okx,
}

impl Exchange {
    pub const ALL: [Exchange; 3] = [Exchange::Binance, Exchange::Bybit, Exchange::Okx];

    pub fn name(self) -> &'static str {
        match self {
            Exchange::Binance => "Binance",
            Exchange::Bybit => "Bybit",
            Exchange::Okx => "OKX",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Long,
    Short,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_names_and_order() {
        let names: Vec<_> = Exchange::ALL.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["Binance", "Bybit", "OKX"]);
    }
}

//! Spread tag (spec spread-position-tag): compares the long leg's exchange price with the short
//! leg's exchange price. Display / record only; never feeds a trading decision. Pure, `Decimal`.

use crate::types::{Decimal, Price};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadKind {
    Positive,
    Negative,
    Neutral,
    /// A price is missing / stale, or the long price is 0: nothing is guessed.
    Unknown,
}

impl SpreadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SpreadKind::Positive => "positive",
            SpreadKind::Negative => "negative",
            SpreadKind::Neutral => "neutral",
            SpreadKind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpreadTag {
    pub kind: SpreadKind,
    /// `(short − long) ÷ long × 100`, not rounded; `None` when `kind` is `Unknown`.
    pub pct: Option<Decimal>,
}

pub fn spread_tag(long_price: Option<Price>, short_price: Option<Price>) -> SpreadTag {
    let unknown = SpreadTag { kind: SpreadKind::Unknown, pct: None };
    let (Some(long), Some(short)) = (long_price, short_price) else { return unknown };
    if long.is_zero() {
        return unknown;
    }
    let pct = (short - long) / long * Decimal::ONE_HUNDRED;
    let kind = match long.cmp(&short) {
        std::cmp::Ordering::Less => SpreadKind::Positive,
        std::cmp::Ordering::Greater => SpreadKind::Negative,
        std::cmp::Ordering::Equal => SpreadKind::Neutral,
    };
    SpreadTag { kind, pct: Some(pct) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn long_below_short_is_positive_with_positive_pct() {
        let t = spread_tag(Some(d("100")), Some(d("100.2")));
        assert_eq!(t.kind, SpreadKind::Positive);
        assert_eq!(t.pct, Some(d("0.2")));
    }

    #[test]
    fn long_above_short_is_negative_with_negative_pct() {
        let t = spread_tag(Some(d("100.3")), Some(d("100")));
        assert_eq!(t.kind, SpreadKind::Negative);
        assert!(t.pct.unwrap() < Decimal::ZERO);
    }

    #[test]
    fn equal_prices_are_neutral_with_zero_pct() {
        let t = spread_tag(Some(d("100")), Some(d("100")));
        assert_eq!(t.kind, SpreadKind::Neutral);
        assert_eq!(t.pct, Some(Decimal::ZERO));
    }

    #[test]
    fn a_missing_price_is_unknown_never_zero() {
        assert_eq!(spread_tag(None, Some(d("100"))), SpreadTag { kind: SpreadKind::Unknown, pct: None });
        assert_eq!(spread_tag(Some(d("100")), None), SpreadTag { kind: SpreadKind::Unknown, pct: None });
        assert_eq!(spread_tag(None, None).kind, SpreadKind::Unknown);
    }

    #[test]
    fn zero_long_price_is_unknown_without_dividing() {
        assert_eq!(spread_tag(Some(Decimal::ZERO), Some(d("100"))), SpreadTag { kind: SpreadKind::Unknown, pct: None });
    }

    #[test]
    fn pct_is_exact_decimal_not_rounded() {
        // (3 − 7) ÷ 7 × 100 keeps every digit rust_decimal can hold; no rounding to 2 dp.
        let t = spread_tag(Some(d("7")), Some(d("3")));
        assert_eq!(t.pct, Some((d("3") - d("7")) / d("7") * Decimal::ONE_HUNDRED));
        assert!(t.pct.unwrap().scale() > 2);
    }

    #[test]
    fn kind_text_is_the_spec_word() {
        assert_eq!(SpreadKind::Positive.as_str(), "positive");
        assert_eq!(SpreadKind::Negative.as_str(), "negative");
        assert_eq!(SpreadKind::Neutral.as_str(), "neutral");
        assert_eq!(SpreadKind::Unknown.as_str(), "unknown");
    }
}

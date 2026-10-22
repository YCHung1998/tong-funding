//! Opportunity kind (edge-kind spec): where the expected funding income comes from.
//! Display and recording only; never used for qualification, direction or ordering.

use crate::funding::FundingObservation;
use crate::net_edge::expected_income_rate;
use crate::types::Decimal;

/// `IntervalMismatch` when the rate-gap income is below this share of the model income.
/// Display only: changing it must never affect trading.
pub const INTERVAL_MISMATCH_SHARE: Decimal = Decimal::from_parts(5, 0, 0, false, 1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    IntervalMismatch,
    RateGap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeClassification {
    /// `None` = no expected income.
    pub kind: Option<EdgeKind>,
    /// `(r_S / h_S − r_L / h_L) × 100`.
    pub hourly_spread_pct_per_hour: Decimal,
}

/// Classifies an already-assigned (long, short) pair. `None` when either settlement interval is
/// unknown (never guessed) or a computation overflows: this runs on the order path and must never
/// panic. The model income uses Net Edge's own definition ([`expected_income_rate`]); the rate-gap
/// income is the hourly spread held for the longer of the two settlement periods.
pub fn classify_edge(long: &FundingObservation, short: &FundingObservation) -> Option<EdgeClassification> {
    let secs = |o: &FundingObservation| o.funding_interval_secs.filter(|s| *s > 0);
    let (secs_l, secs_s) = (secs(long)?, secs(short)?);
    let hours = |s: i64| Decimal::from(s).checked_div(Decimal::from(3600));
    let (hours_l, hours_s) = (hours(secs_l)?, hours(secs_s)?);
    let hundred = Decimal::ONE_HUNDRED;
    let hourly_spread_pct_per_hour = short.funding_rate.checked_div(hours_s)?.checked_sub(long.funding_rate.checked_div(hours_l)?)?.checked_mul(hundred)?;
    // `expected_income_rate` adds the two rates unchecked and r_S − r_L is the largest sum it can
    // reach: refuse a pair whose rates do not fit, instead of letting it panic.
    short.funding_rate.checked_sub(long.funding_rate)?;
    let model_income_pct = expected_income_rate(long, short).checked_mul(hundred)?;
    let kind = if model_income_pct <= Decimal::ZERO {
        None
    } else {
        let rate_gap_income_pct = hourly_spread_pct_per_hour.checked_mul(hours_l.max(hours_s))?;
        let mismatch_below = INTERVAL_MISMATCH_SHARE.checked_mul(model_income_pct)?;
        Some(if secs_l != secs_s && rate_gap_income_pct < mismatch_below { EdgeKind::IntervalMismatch } else { EdgeKind::RateGap })
    };
    Some(EdgeClassification { kind, hourly_spread_pct_per_hour })
}

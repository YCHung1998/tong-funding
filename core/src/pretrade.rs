//! Pre-trade validation: ten named checks, all must pass (spec: pretrade-validation).
//! Pure function; the current time is injected (milliseconds).

use crate::types::{Exchange, Pct, Price};
use rust_decimal::Decimal;

/// The ten named pre-trade checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Check {
    DataFresh,
    CoinListed,
    ExchangeAllowed,
    NetEdgeQualified,
    PriceDrift,
    Liquidity,
    Margin,
    Leverage,
    ExistingExposure,
    RiskLimits,
}

/// Freshly fetched data for one leg of the pair.
#[derive(Debug, Clone, PartialEq)]
pub struct LegInput {
    pub exchange: Exchange,
    /// Price re-fetched ~15s before settlement; `None` falls back to `scan_price`.
    pub baseline_price: Option<Price>,
    /// Price captured at scan time.
    pub scan_price: Price,
    pub latest_price: Price,
    pub price_observed_at_ms: i64,
    pub funding_observed_at_ms: i64,
    pub available_margin: Decimal,
    pub listed: bool,
    pub exchange_allowed: bool,
    /// 24h quote (USDT) volume for this leg; `None` (missing data) fails the liquidity check.
    pub volume_24h_quote: Option<Decimal>,
    /// A position/open order on this symbol that does not belong to this pair.
    pub has_foreign_exposure: bool,
}

/// Everything the pre-trade check looks at.
#[derive(Debug, Clone, PartialEq)]
pub struct PretradeInput {
    pub now_ms: i64,
    pub net_edge_qualified: bool,
    pub long: LegInput,
    pub short: LegInput,
    pub margin_needed: Decimal,
    pub leverage: Decimal,
    pub open_pair_count: u32,
}

/// Effective risk limits.
#[derive(Debug, Clone, PartialEq)]
pub struct PretradeLimits {
    /// Percentage number: 0.05 means 0.05%.
    pub max_price_drift_pct: Pct,
    pub stale_data_threshold_ms: i64,
    pub max_leverage: Decimal,
    pub max_concurrent_pairs: u32,
    /// Both legs' 24h quote volume must be at least this (USDT).
    pub min_24h_volume_usdt: Decimal,
}

/// PASS, or BLOCK with every failed check (in canonical order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PretradeVerdict {
    Pass,
    Block { failed: Vec<Check> },
}

impl PretradeVerdict {
    /// All failed checks (empty for PASS).
    pub fn failed(&self) -> &[Check] {
        match self {
            PretradeVerdict::Pass => &[],
            PretradeVerdict::Block { failed } => failed,
        }
    }
}

/// Runs all ten checks and returns PASS or BLOCK with the full failed list.
pub fn evaluate_pretrade(input: &PretradeInput, limits: &PretradeLimits) -> PretradeVerdict {
    let legs = [&input.long, &input.short];
    let mut failed = Vec::new();
    let mut flag = |check: Check, bad: bool| {
        if bad {
            failed.push(check);
        }
    };

    let stale = |at: i64| input.now_ms - at > limits.stale_data_threshold_ms;
    flag(
        Check::DataFresh,
        legs.iter().any(|l| stale(l.price_observed_at_ms) || stale(l.funding_observed_at_ms)),
    );
    flag(Check::CoinListed, legs.iter().any(|l| !l.listed));
    flag(Check::ExchangeAllowed, legs.iter().any(|l| !l.exchange_allowed));
    flag(Check::NetEdgeQualified, !input.net_edge_qualified);
    flag(Check::PriceDrift, legs.iter().any(|l| drift_exceeds(l, limits.max_price_drift_pct)));
    flag(
        Check::Liquidity,
        legs.iter().any(|l| l.volume_24h_quote.is_none_or(|v| v < limits.min_24h_volume_usdt)),
    );
    flag(Check::Margin, legs.iter().any(|l| l.available_margin < input.margin_needed));
    flag(Check::Leverage, input.leverage > limits.max_leverage);
    flag(Check::ExistingExposure, legs.iter().any(|l| l.has_foreign_exposure));
    flag(
        Check::RiskLimits,
        u64::from(input.open_pair_count) + 1 > u64::from(limits.max_concurrent_pairs),
    );

    if failed.is_empty() {
        PretradeVerdict::Pass
    } else {
        PretradeVerdict::Block { failed }
    }
}

/// True if the leg's drift from its baseline is strictly above the limit (or cannot be computed).
fn drift_exceeds(leg: &LegInput, max_pct: Pct) -> bool {
    let baseline = leg.baseline_price.unwrap_or(leg.scan_price);
    if baseline <= Decimal::ZERO || leg.latest_price <= Decimal::ZERO {
        return true;
    }
    // Multiply before dividing so a drift of exactly the limit stays exact.
    let drift_pct = (leg.latest_price - baseline).abs() * Decimal::ONE_HUNDRED / baseline;
    drift_pct > max_pct
}

//! Funding observations: fields, dual timestamps, interval derivation, staleness, settlement.
//! All times are Unix milliseconds (`i64`) injected by the caller; this module never reads a clock.

use crate::types::{Decimal, Exchange, Price, Rate};
use serde::{Deserialize, Serialize};

/// Listing / data-quality state of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataStatus {
    Listed,
    NotListed,
    DataError,
    Stale,
}

/// One funding snapshot of a symbol on an exchange.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FundingObservation {
    pub exchange: Exchange,
    pub symbol: String,
    /// Fraction (0.0001 = 0.01%).
    pub funding_rate: Rate,
    /// `None` when the exchange data could not provide it (never guessed as 8h).
    pub funding_interval_secs: Option<i64>,
    pub next_funding_time: i64,
    pub mark_price: Price,
    /// `None` when missing (treated as 0 by net-edge).
    pub volume_24h_quote: Option<Decimal>,
    /// When the exchange produced the data.
    pub exchange_timestamp: i64,
    /// When this system received it.
    pub observed_at: i64,
    pub data_status: DataStatus,
}

/// Binance: `fundingIntervalHours` to seconds; missing, zero or negative gives `None`.
pub fn binance_interval_secs(funding_interval_hours: Option<i64>) -> Option<i64> {
    funding_interval_hours?.checked_mul(3600).filter(|s| *s > 0)
}

/// Bybit: `fundingInterval` (minutes) to seconds; missing, zero or negative gives `None`.
pub fn bybit_interval_secs(funding_interval_minutes: Option<i64>) -> Option<i64> {
    funding_interval_minutes?.checked_mul(60).filter(|s| *s > 0)
}

/// OKX: `nextFundingTime - fundingTime` (ms) to seconds; missing, zero or negative gives `None`.
pub fn okx_interval_secs(funding_time_ms: Option<i64>, next_funding_time_ms: Option<i64>) -> Option<i64> {
    let diff_ms = next_funding_time_ms?.checked_sub(funding_time_ms?)?;
    Some(diff_ms / 1000).filter(|s| *s > 0)
}

impl FundingObservation {
    /// Builds an observation; a `Listed` one without a valid interval becomes `DataError`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exchange: Exchange,
        symbol: impl Into<String>,
        funding_rate: Rate,
        funding_interval_secs: Option<i64>,
        next_funding_time: i64,
        mark_price: Price,
        volume_24h_quote: Option<Decimal>,
        exchange_timestamp: i64,
        observed_at: i64,
        data_status: DataStatus,
    ) -> Self {
        let funding_interval_secs = funding_interval_secs.filter(|s| *s > 0);
        let data_status = if data_status == DataStatus::Listed && funding_interval_secs.is_none() {
            DataStatus::DataError
        } else {
            data_status
        };
        Self {
            exchange,
            symbol: symbol.into(),
            funding_rate,
            funding_interval_secs,
            next_funding_time,
            mark_price,
            volume_24h_quote,
            exchange_timestamp,
            observed_at,
            data_status,
        }
    }
}

/// True when `now_ms - observed_at` is strictly greater than `stale_threshold_ms`.
pub fn is_stale(obs: &FundingObservation, now_ms: i64, stale_threshold_ms: i64) -> bool {
    now_ms.saturating_sub(obs.observed_at) > stale_threshold_ms
}

/// Status after applying staleness: a `Listed` observation that is stale becomes `Stale`.
pub fn effective_status(obs: &FundingObservation, now_ms: i64, stale_threshold_ms: i64) -> DataStatus {
    if obs.data_status == DataStatus::Listed && is_stale(obs, now_ms, stale_threshold_ms) {
        DataStatus::Stale
    } else {
        obs.data_status
    }
}

/// Display-only 8h-equivalent rate (`rate * 28800 / interval`); `None` if interval is missing or non-positive.
pub fn equivalent_8h_rate(rate: Rate, interval_secs: Option<i64>) -> Option<Rate> {
    let secs = interval_secs.filter(|s| *s > 0)?;
    Some(rate * Decimal::from(28_800) / Decimal::from(secs))
}

/// Settlement time of a (long, short) pair and which legs settle then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairSettlement {
    pub time: i64,
    pub long_settles: bool,
    pub short_settles: bool,
}

/// Pair settlement time is the earlier `next_funding_time`; only legs equal to it settle.
pub fn pair_settlement(long: &FundingObservation, short: &FundingObservation) -> PairSettlement {
    let time = long.next_funding_time.min(short.next_funding_time);
    PairSettlement {
        time,
        long_settles: long.next_funding_time == time,
        short_settles: short.next_funding_time == time,
    }
}

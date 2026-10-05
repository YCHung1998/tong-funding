//! Net Edge (single-settlement model), qualification and best-pair selection.
//! Funding rates are fractions; every `_pct` value is a percentage number (0.01 = 0.01%).
//! No defaults for fees/slippage/threshold: missing ones are errors, never 0.

use crate::funding::{pair_settlement, DataStatus, FundingObservation};
use crate::types::{Decimal, Exchange, Notional, Pct};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Settings the net-edge calculation needs (converted from risk settings by the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct NetEdgeParams {
    /// Per-exchange taker fee (percentage number); `None`/absent means not configured.
    pub taker_fee_pct: BTreeMap<Exchange, Pct>,
    pub est_slippage_pct: Option<Pct>,
    pub safety_margin_pct: Pct,
    pub net_edge_threshold_pct: Option<Pct>,
    pub min_24h_volume_usdt: Decimal,
    pub allowed_exchanges: BTreeSet<Exchange>,
    /// Empty means no restriction.
    pub allowed_coins: BTreeSet<String>,
}

/// Why a Net Edge could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NetEdgeError {
    #[error("missing taker fee setting for {0:?}")]
    MissingTakerFee(Exchange),
    #[error("missing est_slippage_pct setting")]
    MissingEstSlippage,
    #[error("missing net_edge_threshold_pct setting")]
    MissingThreshold,
    #[error("notional must be positive")]
    NonPositiveNotional,
}

/// Net Edge of one (long, short) pairing; no display rounding applied.
#[derive(Debug, Clone, PartialEq)]
pub struct NetEdge {
    pub long: Exchange,
    pub short: Exchange,
    pub symbol: String,
    /// |rate difference|, display only.
    pub gross_spread: Decimal,
    pub funding_income_usdt: Decimal,
    pub fee_usdt: Decimal,
    pub slippage_usdt: Decimal,
    pub safety_margin_usdt: Decimal,
    pub net_edge_usdt: Decimal,
    pub net_edge_pct: Pct,
}

/// A computed pairing plus whether it qualifies.
#[derive(Debug, Clone, PartialEq)]
pub struct Opportunity {
    pub edge: NetEdge,
    pub qualifies: bool,
}

/// Lower rate becomes long, higher becomes short; on a tie `a` is long.
pub fn assign_legs<'a>(
    a: &'a FundingObservation,
    b: &'a FundingObservation,
) -> (&'a FundingObservation, &'a FundingObservation) {
    if b.funding_rate < a.funding_rate {
        (b, a)
    } else {
        (a, b)
    }
}

/// Computes Net Edge for an already-assigned (long, short) pair with per-leg notional `notional`.
pub fn compute_net_edge(
    long: &FundingObservation,
    short: &FundingObservation,
    notional: Notional,
    params: &NetEdgeParams,
) -> Result<NetEdge, NetEdgeError> {
    if notional <= Decimal::ZERO {
        return Err(NetEdgeError::NonPositiveNotional);
    }
    let fee_of = |ex: Exchange| {
        params
            .taker_fee_pct
            .get(&ex)
            .copied()
            .ok_or(NetEdgeError::MissingTakerFee(ex))
    };
    let fee_l = fee_of(long.exchange)?;
    let fee_s = fee_of(short.exchange)?;
    let slippage_pct = params.est_slippage_pct.ok_or(NetEdgeError::MissingEstSlippage)?;

    let hundred = Decimal::ONE_HUNDRED;
    let settle = pair_settlement(long, short);
    let mut income_rate = Decimal::ZERO;
    if settle.long_settles {
        income_rate -= long.funding_rate;
    }
    if settle.short_settles {
        income_rate += short.funding_rate;
    }
    let funding_income_usdt = notional * income_rate;
    let fee_usdt = notional * Decimal::TWO * (fee_l + fee_s) / hundred;
    let slippage_usdt = notional * Decimal::from(4) * slippage_pct / hundred;
    let safety_margin_usdt = notional * params.safety_margin_pct / hundred;
    let net_edge_usdt = funding_income_usdt - fee_usdt - slippage_usdt - safety_margin_usdt;
    Ok(NetEdge {
        long: long.exchange,
        short: short.exchange,
        symbol: long.symbol.clone(),
        gross_spread: (short.funding_rate - long.funding_rate).abs(),
        funding_income_usdt,
        fee_usdt,
        slippage_usdt,
        safety_margin_usdt,
        net_edge_usdt,
        net_edge_pct: net_edge_usdt / notional * hundred,
    })
}

/// Assigns legs by rate, computes Net Edge and applies the qualification rules.
pub fn evaluate_pair(
    a: &FundingObservation,
    b: &FundingObservation,
    notional: Notional,
    params: &NetEdgeParams,
) -> Result<Opportunity, NetEdgeError> {
    let (long, short) = assign_legs(a, b);
    let edge = compute_net_edge(long, short, notional, params)?;
    let threshold = params
        .net_edge_threshold_pct
        .ok_or(NetEdgeError::MissingThreshold)?;
    let volume_ok = |o: &FundingObservation| {
        o.volume_24h_quote.unwrap_or(Decimal::ZERO) >= params.min_24h_volume_usdt
    };
    let qualifies = edge.net_edge_pct >= threshold
        && long.data_status == DataStatus::Listed
        && short.data_status == DataStatus::Listed
        && volume_ok(long)
        && volume_ok(short)
        && params.allowed_exchanges.contains(&long.exchange)
        && params.allowed_exchanges.contains(&short.exchange)
        && (params.allowed_coins.is_empty() || params.allowed_coins.contains(&long.symbol));
    Ok(Opportunity { edge, qualifies })
}

/// Evaluates every cross-exchange pair of the same symbol and returns the highest Net Edge.
pub fn best_opportunity(
    observations: &[FundingObservation],
    notional: Notional,
    params: &NetEdgeParams,
) -> Result<Option<Opportunity>, NetEdgeError> {
    let mut best: Option<Opportunity> = None;
    for (i, a) in observations.iter().enumerate() {
        for b in &observations[i + 1..] {
            if a.symbol != b.symbol || a.exchange == b.exchange {
                continue;
            }
            let cand = evaluate_pair(a, b, notional, params)?;
            if best
                .as_ref()
                .is_none_or(|cur| cand.edge.net_edge_pct > cur.edge.net_edge_pct)
            {
                best = Some(cand);
            }
        }
    }
    Ok(best)
}

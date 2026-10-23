//! loss-circuit-breaker: the pure evaluation. Input = the per-pair amounts (net PnL, or
//! price PnL minus fees when the net is not known yet, or `None` when nothing can be computed)
//! of the pairs closed since the interval start, in close order. No I/O, no clock.

use serde::{Deserialize, Serialize};

use crate::types::Decimal;

/// Why the breaker tripped. When several hold at once the order is `UnknownPnl` (fail closed),
/// `MaxLoss`, `ConsecutiveLosses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TripReason {
    /// A closed pair's PnL could not be computed.
    UnknownPnl,
    /// Cumulative amount <= -max_loss.
    MaxLoss,
    /// The trailing run of losing pairs reached the limit.
    ConsecutiveLosses,
}

impl TripReason {
    /// The stable code written into `LOSS_BREAKER_TRIPPED`.
    pub fn code(self) -> &'static str {
        match self {
            TripReason::UnknownPnl => "UNKNOWN_PNL",
            TripReason::MaxLoss => "MAX_LOSS",
            TripReason::ConsecutiveLosses => "CONSECUTIVE_LOSSES",
        }
    }

    /// Plain Chinese for notifications and the status bar.
    pub fn text(self) -> &'static str {
        match self {
            TripReason::UnknownPnl => "損益未知",
            TripReason::MaxLoss => "累計虧損達上限",
            TripReason::ConsecutiveLosses => "連虧達上限",
        }
    }
}

/// The breaker's numbers for one interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerState {
    /// Signed sum of every known amount (profits offset losses); unknown amounts add nothing.
    pub net_sum: Decimal,
    /// Length of the trailing run of losing pairs (amount < 0; an unknown amount counts as a loss).
    pub consecutive: u32,
    /// How many pairs have an unknown amount.
    pub unknown_count: u32,
    pub tripped: bool,
    pub reason: Option<TripReason>,
}

impl BreakerState {
    /// The loss used so far, as a non-negative number (`6.2` for a cumulative `-6.2`).
    pub fn loss_used(&self) -> Decimal {
        (-self.net_sum).max(Decimal::ZERO)
    }
}

/// Evaluates the interval. `amounts` are in close order. Trips when any amount is unknown, when
/// `Σ <= -max_loss` (the boundary trips), or when the trailing losing run `>= max_consecutive`
/// (an amount `>= 0` resets the run). A non-positive limit trips at once: fail closed.
pub fn evaluate(amounts: &[Option<Decimal>], max_loss: Decimal, max_consecutive: u32) -> BreakerState {
    let mut net_sum = Decimal::ZERO;
    let mut consecutive = 0_u32;
    let mut unknown_count = 0_u32;
    for a in amounts {
        match a {
            Some(v) => {
                net_sum += *v;
                if *v < Decimal::ZERO {
                    consecutive += 1;
                } else {
                    consecutive = 0;
                }
            }
            None => {
                unknown_count += 1;
                consecutive += 1;
            }
        }
    }
    let reason = if unknown_count > 0 {
        Some(TripReason::UnknownPnl)
    } else if net_sum <= -max_loss {
        Some(TripReason::MaxLoss)
    } else if consecutive >= max_consecutive {
        Some(TripReason::ConsecutiveLosses)
    } else {
        None
    };
    BreakerState { net_sum, consecutive, unknown_count, tripped: reason.is_some(), reason }
}

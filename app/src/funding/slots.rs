//! The staged funding tracing of one slot (change funding-slot-tracing, design D3/D4/D6/D10).
//! Pure: [`step`] turns the stored row, the clock and the outcome of one check into the next row;
//! nothing here queries an exchange or the store.
//!
//! Schedule (every value a named constant, provisional until the delay data is in):
//! - `TRACING_1`: checks at `t+5, +10, … +30 min`; `TRACING_2`: `t+40, +50, +60 min`; unfound at
//!   `t+60` → `MANUAL_CHECK` (reason `NOT_FOUND`, or `FETCH_FAILED` when that check itself failed).
//! - `MANUAL_CHECK` is re-checked hourly until `t+3 days` (and on a user request).
//! - The stage follows the elapsed time since `t`: a check that runs late (machine asleep) is
//!   judged by when it actually ran. A failed check never counts as "nothing there".

use tong_funding_core::types::Decimal;

use crate::store::funding_slots::{ManualReason, Resolution, SlotRow, SlotState};

const MIN: i64 = 60_000;
/// Stage 1: one check every 5 minutes up to `t + 30 min`.
pub const STAGE1_STEP_MS: i64 = 5 * MIN;
pub const STAGE1_END_MS: i64 = 30 * MIN;
/// Stage 2: one check every 10 minutes up to `t + 60 min`.
pub const STAGE2_STEP_MS: i64 = 10 * MIN;
pub const STAGE2_END_MS: i64 = 60 * MIN;
/// `MANUAL_CHECK` sweep: hourly for 3 days after `t`.
pub const SWEEP_STEP_MS: i64 = 60 * MIN;
pub const SWEEP_END_MS: i64 = 3 * 24 * 60 * MIN;
/// "No further check is scheduled".
pub const NEVER_MS: i64 = i64::MAX;

/// What the public funding-rate history says about the slot's settlement time.
#[derive(Debug, Clone, PartialEq)]
pub enum RateCheck {
    /// The rate at the settlement time is exactly 0 (`source` names the endpoint).
    Zero { source: String },
    NonZero,
    /// The public history was read successfully and has NO funding at that time: the exchange did
    /// not settle then. Only counts once the tracing period is over (design: the ledger may lag).
    NoRow { source: String },
    /// Not asked, failed, or not available: changes nothing.
    Unknown,
}

/// The result of one check of one slot.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    /// The ledger row for the slot was found.
    Found { amount: Decimal, dedupe_key: String, exchange_id: String, first_seen_ms: i64 },
    /// The query completed over a window covering `t` and holds no row for it.
    NotFound { rate: RateCheck },
    /// The query failed (network, rate limit, keys, incomplete page).
    Failed { error: String },
    /// The row exists but falls inside another pair's window: never linked by guess.
    Ambiguous { detail: String },
}

/// The next stored form of the slot.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Settle it (and append the linked event).
    Settled(SlotRow),
    /// Store the progress (stage, next check time, counters, error).
    Progress(SlotRow),
}

/// The first check time of a slot settling at `settle_ms`.
pub fn first_check_ms(settle_ms: i64) -> i64 {
    settle_ms.saturating_add(STAGE1_STEP_MS)
}

/// The stage a check made at `now_ms` belongs to.
pub fn stage_at(settle_ms: i64, now_ms: i64) -> SlotState {
    let elapsed = now_ms.saturating_sub(settle_ms);
    if elapsed < STAGE1_END_MS {
        SlotState::Tracing1
    } else if elapsed < STAGE2_END_MS {
        SlotState::Tracing2
    } else {
        SlotState::ManualCheck
    }
}

/// When the next check is due after one made at `now_ms` ([`NEVER_MS`] after the sweep period).
pub fn next_check_after(settle_ms: i64, now_ms: i64) -> i64 {
    let elapsed = now_ms.saturating_sub(settle_ms);
    let next_elapsed = if elapsed < STAGE1_END_MS {
        (elapsed / STAGE1_STEP_MS + 1) * STAGE1_STEP_MS
    } else if elapsed < STAGE2_END_MS {
        STAGE1_END_MS + ((elapsed - STAGE1_END_MS) / STAGE2_STEP_MS + 1) * STAGE2_STEP_MS
    } else if elapsed < SWEEP_END_MS {
        elapsed + SWEEP_STEP_MS
    } else {
        return NEVER_MS;
    };
    // The grid point right after the stage-1 end is `t + 40 min` (stage 2 starts one step later).
    settle_ms.saturating_add(next_elapsed)
}

/// Applies one check outcome to a not-yet-settled slot.
pub fn step(row: &SlotRow, now_ms: i64, outcome: CheckOutcome) -> Step {
    let mut next = row.clone();
    next.updated_ms = now_ms;
    let manual_reason = match &outcome {
        CheckOutcome::Failed { .. } => ManualReason::FetchFailed,
        CheckOutcome::Ambiguous { .. } => ManualReason::Ambiguous,
        _ => ManualReason::NotFound,
    };
    match outcome {
        CheckOutcome::Found { amount, dedupe_key, exchange_id, first_seen_ms } => {
            next.checks_ok += 1;
            next.resolution = Some(Resolution::Ledger);
            next.amount = Some(amount);
            next.ledger_dedupe_key = Some(dedupe_key);
            next.ledger_exchange_id = Some(exchange_id);
            next.first_seen_ms = Some(first_seen_ms);
            next.state = SlotState::Settled;
            next.manual_reason = None;
            next.last_error = None;
            return Step::Settled(next);
        }
        CheckOutcome::NotFound { rate: RateCheck::Zero { source } } => {
            next.checks_ok += 1;
            next.resolution = Some(Resolution::ZeroRate);
            next.amount = Some(Decimal::ZERO);
            next.zero_rate_source = Some(source);
            next.first_seen_ms = Some(now_ms);
            next.state = SlotState::Settled;
            next.manual_reason = None;
            next.last_error = None;
            return Step::Settled(next);
        }
        CheckOutcome::NotFound { rate } => {
            // The exchange published no funding for that time and the whole tracing period has passed
            // without a ledger row: it did not settle there. Settled as zero, with the reason kept.
            if let RateCheck::NoRow { source } = rate
                && stage_at(row.settle_ms, now_ms) == SlotState::ManualCheck
            {
                next.checks_ok += 1;
                next.resolution = Some(Resolution::NoSettlement);
                next.amount = Some(Decimal::ZERO);
                next.zero_rate_source = Some(source);
                next.first_seen_ms = Some(now_ms);
                next.state = SlotState::Settled;
                next.manual_reason = None;
                next.last_error = None;
                return Step::Settled(next);
            }
            next.checks_ok += 1;
            next.last_error = None;
        }
        CheckOutcome::Ambiguous { detail } => {
            next.checks_ok += 1;
            next.last_error = Some(format!("ambiguous: {detail}"));
        }
        CheckOutcome::Failed { error } => {
            next.checks_failed += 1;
            next.last_error = Some(error);
        }
    }
    next.state = stage_at(row.settle_ms, now_ms);
    next.manual_reason = (next.state == SlotState::ManualCheck).then_some(manual_reason);
    next.next_check_ms = next_check_after(row.settle_ms, now_ms);
    Step::Progress(next)
}

/// Nearest-rank percentiles of the observed publication delays (ms): (p50, p95, p99).
pub fn delay_percentiles(delays: &[i64]) -> Option<(i64, i64, i64)> {
    if delays.is_empty() {
        return None;
    }
    let mut v = delays.to_vec();
    v.sort_unstable();
    let rank = |p: usize| v[((p * v.len()).div_ceil(100)).clamp(1, v.len()) - 1];
    Some((rank(50), rank(95), rank(99)))
}

/// A user "重新查詢": the slot is due now (state and reason stay as they are).
pub fn request_recheck(row: &SlotRow, now_ms: i64) -> SlotRow {
    SlotRow { next_check_ms: now_ms, updated_ms: now_ms, ..row.clone() }
}

#[cfg(test)]
#[path = "slots_tests.rs"]
mod tests;

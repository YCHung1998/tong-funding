//! Cost attribution of a pair's entry (change cost-attribution, spec cost-attribution): the two
//! values the PnL `breakdown` does not carry. Pure functions on the same `PairInput` as
//! `pnl::compute_pnl`; nothing here reads a clock, the store or the network. Money is `Decimal`.
//!
//! Everything else the spec splits the price PnL into comes from `breakdown`: the exit slippage is
//! `total.slippage - entry_slippage`, the holding drift is `total.price_ref`.
//!
//! Missing data is never a 0: a value that cannot be computed is `None`. All arithmetic is checked,
//! an overflow is `None`, never a panic (this runs inside the PnL gate; the actor is not restarted
//! after a panic).

use crate::pnl::{FillAction, LegInput, PairInput};
use crate::types::{Decimal, Side};

/// One opening fill with everything the two values need.
struct Opening {
    expected: Decimal,
    actual: Decimal,
    quantity: Decimal,
}

/// The opening fills of a leg (same filter as `compute_pnl`: a zero quantity is no fill). An empty
/// list means the leg did not trade. `None` as soon as one fill lacks its reference price, its fill
/// price or a quantity in coins (OKX `contract_value_missing`): the entry is then unknown for the
/// whole pair, whichever value is asked for.
fn openings(leg: &LegInput) -> Option<Vec<Opening>> {
    leg.fills
        .iter()
        .filter(|f| f.action == FillAction::Open && !f.quantity.is_zero())
        .map(|f| (!f.contract_value_missing).then_some(Opening { expected: f.expected_price?, actual: f.actual_price?, quantity: f.quantity }))
        .collect()
}

/// Entry slippage in USDT: `Σ dir × (reference − actual) × q` over every opening fill, with the
/// direction of `compute_pnl` (opening a long -1, opening a short +1), so a positive value is
/// adverse, like `breakdown.total.slippage`. A leg without an opening fill contributes 0; `None`
/// when no leg opened, when an opening fill is unusable (see [`openings`]) or on overflow.
pub fn entry_slippage(input: &PairInput) -> Option<Decimal> {
    let mut sum = Decimal::ZERO;
    let mut opened = false;
    for leg in &input.legs {
        for o in openings(leg)? {
            opened = true;
            let part = o.expected.checked_sub(o.actual)?.checked_mul(o.quantity)?;
            sum = match leg.side {
                Side::Long => sum.checked_sub(part)?,
                Side::Short => sum.checked_add(part)?,
            };
        }
    }
    opened.then(|| sum.normalize())
}

/// Notional per leg in USDT: the average, over the legs that opened, of the leg's opening fill
/// value `Σ q × actual`. `None` when no leg opened, when an opening fill is unusable (see
/// [`openings`]), on overflow, or when the average is not positive (it is the denominator of every
/// percentage a reader derives).
pub fn notional_per_leg(input: &PairInput) -> Option<Decimal> {
    let mut total = Decimal::ZERO;
    let mut opened_legs = 0usize;
    for leg in &input.legs {
        let fills = openings(leg)?;
        if !fills.is_empty() {
            opened_legs += 1; // at most `input.legs.len()`
        }
        for o in fills {
            total = total.checked_add(o.actual.checked_mul(o.quantity)?)?;
        }
    }
    let average = total.checked_div(Decimal::from(opened_legs))?; // no leg opened: division by zero is `None`
    (average > Decimal::ZERO).then(|| average.normalize())
}

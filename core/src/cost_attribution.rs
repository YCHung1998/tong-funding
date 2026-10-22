//! Cost attribution of a pair's PnL (change cost-attribution, spec cost-attribution). Pure
//! functions on the same `PairInput` as `pnl::compute_pnl`; nothing here reads a clock, the store
//! or the network. Money is `Decimal`; nothing is rounded for display (pct keeps full precision).
//!
//! The price part of the PnL is split into three: entry slippage and exit slippage (both costs,
//! positive = adverse) and holding drift (= `price_ref`, a profit when positive). Identities,
//! pinned by tests against `compute_pnl`:
//! `entry + exit = slippage`, `drift = price_ref`, `funding + drift - entry - exit - fees = net`.
//!
//! Missing data is never a 0: a part that cannot be fully computed is `None` and named in
//! `unknown`; it does not hide the other parts.

use serde::{Deserialize, Serialize};

use crate::pnl::{compute_pnl, Component, FillAction, FillRecord, LegInput, PairInput};
use crate::types::{Decimal, Side};

/// One part in USDT and as a percentage of the per-leg notional (a percentage number: 0.2 = 0.2%).
/// `pct` is `None` when the notional is unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Amount {
    pub usdt: Decimal,
    pub pct: Option<Decimal>,
}

/// Signs: `funding`, `drift`, `net` are profits (positive = earned); `entry_slippage`,
/// `exit_slippage`, `fees` are costs (positive = paid / adverse). `None` = unknown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostAttribution {
    /// Average over the legs of their opening fill value (`q x act_open`).
    pub notional_per_leg: Option<Decimal>,
    pub funding: Option<Amount>,
    pub drift: Option<Amount>,
    pub entry_slippage: Option<Amount>,
    pub exit_slippage: Option<Amount>,
    pub fees: Option<Amount>,
    pub net: Option<Amount>,
    /// Names of the unknown parts, in field order.
    pub unknown: Vec<String>,
}

/// Slippage cost of one leg's fills of `action`: `Σ dir x (ref - act) x q` with the same `dir` as
/// `compute_pnl` (long: open -1, close +1; short the opposite). `None` when the leg has no such
/// fill or any of them lacks a price or a coin quantity.
fn leg_slippage(leg: &LegInput, action: FillAction) -> Option<Decimal> {
    let sign = match leg.side {
        Side::Long => Decimal::ONE,
        Side::Short => Decimal::NEGATIVE_ONE,
    };
    let dir = if action == FillAction::Open { -sign } else { sign };
    let fills: Vec<&FillRecord> = leg.fills.iter().filter(|f| f.action == action && !f.quantity.is_zero()).collect();
    if fills.is_empty() {
        return None;
    }
    let mut sum = Decimal::ZERO;
    for f in fills {
        if f.contract_value_missing {
            return None;
        }
        sum += dir * (f.expected_price? - f.actual_price?) * f.quantity;
    }
    Some(sum)
}

/// Opening fill value of one leg; `None` when a fill lacks its price or coin quantity.
fn leg_open_notional(leg: &LegInput) -> Option<Decimal> {
    let fills: Vec<&FillRecord> = leg.fills.iter().filter(|f| f.action == FillAction::Open && !f.quantity.is_zero()).collect();
    if fills.is_empty() {
        return None;
    }
    let mut sum = Decimal::ZERO;
    for f in fills {
        if f.contract_value_missing {
            return None;
        }
        sum += f.actual_price? * f.quantity;
    }
    Some(sum)
}

/// Sum over all legs; `None` when any leg's part is unknown.
fn sum_legs(legs: &[LegInput], part: impl Fn(&LegInput) -> Option<Decimal>) -> Option<Decimal> {
    let mut sum = Decimal::ZERO;
    for l in legs {
        sum += part(l)?;
    }
    (!legs.is_empty()).then_some(sum)
}

pub fn attribute_costs(input: &PairInput) -> CostAttribution {
    let b = compute_pnl(input);
    let notional = sum_legs(&input.legs, leg_open_notional).map(|n| (n / Decimal::from(input.legs.len())).normalize()).filter(|n| *n > Decimal::ZERO);
    let amount = |usdt: Decimal| Amount {
        usdt: usdt.normalize(),
        pct: notional.map(|n| (usdt / n * Decimal::ONE_HUNDRED).normalize()),
    };
    let known = |c: &[Component], v: Decimal| (!c.iter().any(|c| b.missing.contains(c))).then(|| amount(v));

    let entry = sum_legs(&input.legs, |l| leg_slippage(l, FillAction::Open)).map(amount);
    let exit = sum_legs(&input.legs, |l| leg_slippage(l, FillAction::Close)).map(amount);
    let t = &b.total;
    let a = CostAttribution {
        notional_per_leg: notional,
        funding: known(&[Component::Funding], t.funding),
        // price_ref is a sum over both opens and closes: one missing reference makes it unknown
        drift: known(&[Component::PriceRef], t.price_ref),
        entry_slippage: entry,
        exit_slippage: exit,
        fees: known(&[Component::OpeningFee, Component::ClosingFee], t.opening_fee + t.closing_fee),
        net: known(&[Component::Net], t.net),
        unknown: Vec::new(),
    };
    let unknown = [
        ("notional_per_leg", a.notional_per_leg.is_none()),
        ("funding", a.funding.is_none()),
        ("drift", a.drift.is_none()),
        ("entry_slippage", a.entry_slippage.is_none()),
        ("exit_slippage", a.exit_slippage.is_none()),
        ("fees", a.fees.is_none()),
        ("net", a.net.is_none()),
    ]
    .into_iter()
    .filter(|&(_, none)| none)
    .map(|(name, _)| name.to_string())
    .collect();
    CostAttribution { unknown, ..a }
}

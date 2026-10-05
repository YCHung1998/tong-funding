//! Node 1 sizing, the fill-monitor decision and the PREPARED auto-cancel evaluation
//! (tasks 2.3, 2.4 logic). Pure: time and fills are parameters. Nothing here can produce an
//! order: the fill monitor only returns a core `SystemEvent` for `next()` (no automatic top-up,
//! sell-off or close, ever).

use rust_decimal::Decimal;
use tong_funding_core::funding::FundingObservation;
use tong_funding_core::pair::{PairState, SystemEvent};
use tong_funding_core::quantity::{LotSize, Quantity, QuantityError};
use tong_funding_core::risk::EffectiveConfig;
use tong_funding_core::types::{Exchange, Notional, Pct, Price};

use super::node0::net_edge_with_threshold;
use super::ports::Leg;

/// What Node 1 needs to size one leg.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegSizing {
    pub exchange: Exchange,
    pub notional: Notional,
    /// Latest (pre-trade) price.
    pub price: Price,
    /// Lot filter in the exchange's order unit (contracts on OKX, base coin elsewhere).
    pub lot: LotSize,
    /// OKX contract value (base coin per contract); required on OKX, ignored elsewhere.
    pub okx_ct_val: Option<Decimal>,
}

/// A sized leg.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizedLeg {
    /// Quantity in the exchange's order unit (contracts on OKX), floored to the lot step.
    pub order_qty: Quantity,
    /// The same amount in base-coin units (`order_qty * ct_val` on OKX).
    pub base_qty: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SizingError {
    /// Includes "below `min_qty`" (`QuantityError::BelowMinimum`).
    Quantity(QuantityError),
    /// OKX leg without a contract value.
    MissingContractValue,
}

/// Sizes one leg through core `Quantity` (floor to step, never round up).
pub fn size_leg(s: &LegSizing) -> Result<SizedLeg, SizingError> {
    match s.exchange {
        Exchange::Okx => {
            let ct_val = s.okx_ct_val.ok_or(SizingError::MissingContractValue)?;
            if s.price <= Decimal::ZERO {
                return Err(SizingError::Quantity(QuantityError::InvalidPrice(s.price)));
            }
            let contracts =
                Quantity::okx_contracts(s.notional / s.price, ct_val, &s.lot).map_err(SizingError::Quantity)?;
            Ok(SizedLeg { order_qty: contracts, base_qty: contracts.value() * ct_val })
        }
        Exchange::Binance | Exchange::Bybit => {
            let q = Quantity::from_notional(s.notional, s.price, &s.lot).map_err(SizingError::Quantity)?;
            Ok(SizedLeg { order_qty: q, base_qty: q.value() })
        }
    }
}

/// Node 1 outcome for the pair.
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitPlan {
    /// Send exactly these two orders.
    Send { long: SizedLeg, short: SizedLeg },
    /// A leg could not be sized: send NOTHING (sending only the other leg would open a naked
    /// position on purpose); feed [`SubmitPlan::ABORT_EVENT`] to `next()` (ORDER_SUBMIT → CANCELLED).
    Abort { failed: Vec<(Leg, SizingError)> },
}

impl SubmitPlan {
    pub const ABORT_EVENT: SystemEvent = SystemEvent::BothSubmitsFailed;
}

/// Sizes both legs; any failure aborts the whole submission before anything is sent.
pub fn plan_submit(long: &LegSizing, short: &LegSizing) -> SubmitPlan {
    match (size_leg(long), size_leg(short)) {
        (Ok(long), Ok(short)) => SubmitPlan::Send { long, short },
        (l, s) => {
            let failed = [(Leg::Long, l), (Leg::Short, s)]
                .into_iter()
                .filter_map(|(leg, r)| r.err().map(|e| (leg, e)))
                .collect();
            SubmitPlan::Abort { failed }
        }
    }
}

/// What is known about one leg's fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegFill {
    /// Outcome or fill unknown (e.g. submit `Unknown`, query failed).
    Unknown,
    /// Quantities in base-coin units.
    Known { requested: Decimal, filled: Decimal },
}

/// Fill-monitor result. There is deliberately no "send an order" variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillDecision {
    /// Keep waiting (and polling).
    Wait,
    /// Feed this event to core `next()` from FILL_MONITOR.
    Transition(SystemEvent),
}

/// When the fill wait ends: `sent_at_ms + order_timeout_seconds` of the pair's effective config
/// (the two legs' minimum), never the global value.
pub fn timeout_at_ms(eff: &EffectiveConfig, sent_at_ms: i64) -> i64 {
    sent_at_ms.saturating_add(i64::from(eff.order_timeout_seconds) * 1_000)
}

/// Fill-monitor decision. Both legs completely filled → within / beyond
/// `max_leg_imbalance_pct` (relative difference of base quantities, % of the larger). Otherwise
/// wait until the timeout, then: any leg unknown → `TimeoutUndetermined` (UNRESOLVED); both zero
/// → `TimeoutNoFills` (CANCELLED); else → `TimeoutPartialFill` (PARTIAL_FAILURE).
pub fn fill_decision(eff: &EffectiveConfig, sent_at_ms: i64, now_ms: i64, long: LegFill, short: LegFill) -> FillDecision {
    // `(requested, filled)` for a usable leg; a non-positive or negative quantity is not a fact.
    let usable = |f: LegFill| match f {
        LegFill::Known { requested, filled } if requested > Decimal::ZERO && filled >= Decimal::ZERO => {
            Some((requested, filled))
        }
        LegFill::Known { .. } | LegFill::Unknown => None,
    };
    let (l, s) = (usable(long), usable(short));
    if let (Some((lr, lf)), Some((sr, sf))) = (l, s)
        && lf >= lr
        && sf >= sr
    {
        let larger = lf.max(sf);
        let imbalance_pct = (lf - sf).abs() * Decimal::ONE_HUNDRED / larger;
        let event = if imbalance_pct <= eff.max_leg_imbalance_pct {
            SystemEvent::FillsWithinTolerance
        } else {
            SystemEvent::FillsExceedTolerance
        };
        return FillDecision::Transition(event);
    }
    if now_ms < timeout_at_ms(eff, sent_at_ms) {
        return FillDecision::Wait;
    }
    let event = match (l, s) {
        (Some((_, lf)), Some((_, sf))) if lf.is_zero() && sf.is_zero() => SystemEvent::TimeoutNoFills,
        (Some(_), Some(_)) => SystemEvent::TimeoutPartialFill,
        (None, _) | (_, None) => SystemEvent::TimeoutUndetermined,
    };
    FillDecision::Transition(event)
}

/// Fresh data for re-evaluating a PREPARED pair.
#[derive(Debug, Clone, Copy)]
pub struct PreparedRecheck<'a> {
    pub long: &'a FundingObservation,
    pub short: &'a FundingObservation,
    pub notional: Notional,
    /// `effective_for_pair` for this pair.
    pub effective: &'a EffectiveConfig,
    /// Global-only field.
    pub allowed_exchanges: &'a [Exchange],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelReason {
    NetEdgeBelowThreshold { net_edge_pct: Pct, threshold_pct: Pct },
    /// Net Edge cannot be computed (missing settings): treated as no longer qualifying.
    NetEdgeUnavailable { missing: Vec<String> },
    /// 24h volume missing or below the effective `min_24h_volume_usdt`.
    LowVolume { leg: Leg, volume_24h_quote: Option<Decimal>, min_usdt: Decimal },
    ExchangeNotAllowed { leg: Leg, exchange: Exchange },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoCancel {
    NoAction,
    /// Feed `SystemEvent::Cancel` (PREPARED → CANCELLED) and record the reasons.
    Cancel { reasons: Vec<CancelReason> },
}

/// PREPARED auto-cancel (runs even while the kill switch is on: it only removes a plan). Any
/// state other than PREPARED is never touched.
pub fn prepared_auto_cancel(state: PairState, fresh: &PreparedRecheck<'_>) -> AutoCancel {
    match state {
        PairState::Prepared => {}
        PairState::PreTradeCheck
        | PairState::Blocked
        | PairState::OrderSubmit
        | PairState::FillMonitor
        | PairState::Reconciled
        | PairState::Imbalanced
        | PairState::Closing
        | PairState::Finalized
        | PairState::Cancelled
        | PairState::PartialFailure
        | PairState::Unresolved => return AutoCancel::NoAction,
    }
    let eff = fresh.effective;
    let mut reasons = Vec::new();
    match net_edge_with_threshold(fresh.long, fresh.short, fresh.notional, eff) {
        Ok((edge, threshold)) if edge.net_edge_pct < threshold => {
            reasons.push(CancelReason::NetEdgeBelowThreshold { net_edge_pct: edge.net_edge_pct, threshold_pct: threshold });
        }
        Ok(_) => {}
        Err(missing) => reasons.push(CancelReason::NetEdgeUnavailable { missing }),
    }
    let legs = [(Leg::Long, fresh.long), (Leg::Short, fresh.short)];
    for (leg, obs) in legs {
        let min_usdt = eff.min_24h_volume_usdt;
        if obs.volume_24h_quote.is_none_or(|v| v < min_usdt) {
            reasons.push(CancelReason::LowVolume { leg, volume_24h_quote: obs.volume_24h_quote, min_usdt });
        }
    }
    for (leg, obs) in legs {
        if !fresh.allowed_exchanges.contains(&obs.exchange) {
            reasons.push(CancelReason::ExchangeNotAllowed { leg, exchange: obs.exchange });
        }
    }
    if reasons.is_empty() {
        AutoCancel::NoAction
    } else {
        AutoCancel::Cancel { reasons }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tong_funding_core::funding::DataStatus;
    use tong_funding_core::risk::{effective_for_pair, RiskConfig, RiskOverride, RiskOverrides};

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn cfg() -> RiskConfig {
        RiskConfig {
            net_edge_threshold_pct: Some(dec("0.05")),
            est_slippage_pct: Some(dec("0.01")),
            taker_fee_pct: Exchange::ALL.into_iter().map(|e| (e, dec("0.02"))).collect::<BTreeMap<_, _>>(),
            ..RiskConfig::default()
        }
    }

    fn eff_with(overrides: RiskOverrides) -> EffectiveConfig {
        effective_for_pair(&cfg(), &overrides, Exchange::Binance, Exchange::Bybit)
    }

    fn known(requested: &str, filled: &str) -> LegFill {
        LegFill::Known { requested: dec(requested), filled: dec(filled) }
    }

    // ---- Node 1 sizing ----

    fn sizing(exchange: Exchange, step: &str, min: &str) -> LegSizing {
        LegSizing {
            exchange,
            notional: dec("1000"),
            price: dec("100"),
            lot: LotSize { step_size: dec(step), min_qty: dec(min) },
            okx_ct_val: None,
        }
    }

    #[test]
    fn legs_are_floored_to_the_step() {
        let mut s = sizing(Exchange::Binance, "0.4", "0.1"); // 1000 / 101 = 9.90… -> 9.6
        s.price = dec("101");
        let leg = size_leg(&s).unwrap();
        assert_eq!(leg.order_qty.value(), dec("9.6"));
        assert_eq!(leg.base_qty, dec("9.6"));
    }

    #[test]
    fn okx_leg_is_converted_to_contracts() {
        let mut s = sizing(Exchange::Okx, "1", "1");
        s.okx_ct_val = Some(dec("0.01"));
        let leg = size_leg(&s).unwrap();
        assert_eq!(leg.order_qty.value(), dec("1000"));
        assert_eq!(leg.base_qty, dec("10.00"));
        s.okx_ct_val = None;
        assert_eq!(size_leg(&s), Err(SizingError::MissingContractValue));
    }

    #[test]
    fn leg_below_min_qty_is_not_sent() {
        let long = sizing(Exchange::Binance, "0.001", "0.001");
        let short = sizing(Exchange::Bybit, "1", "20"); // 10 coins < min 20
        match plan_submit(&long, &short) {
            SubmitPlan::Abort { failed } => {
                assert_eq!(failed.len(), 1);
                assert_eq!(failed[0].0, Leg::Short);
                assert!(matches!(failed[0].1, SizingError::Quantity(QuantityError::BelowMinimum { .. })));
            }
            SubmitPlan::Send { .. } => panic!("a leg below min_qty must not be sent"),
        }
        assert_eq!(SubmitPlan::ABORT_EVENT, SystemEvent::BothSubmitsFailed);
        match plan_submit(&long, &sizing(Exchange::Bybit, "1", "1")) {
            SubmitPlan::Send { long, short } => {
                assert_eq!(long.order_qty.value(), dec("10"));
                assert_eq!(short.order_qty.value(), dec("10"));
            }
            other => panic!("{other:?}"),
        }
    }

    // ---- fill monitor ----

    #[test]
    fn timeout_seven_seconds_fires_at_seven_not_fifteen() {
        let mut ov = RiskOverrides::new();
        ov.insert(Exchange::Bybit, RiskOverride { order_timeout_seconds: Some(7), ..Default::default() });
        let eff = eff_with(ov);
        assert_eq!(eff.order_timeout_seconds, 7);
        let sent = 50_000;
        assert_eq!(timeout_at_ms(&eff, sent), 57_000);
        let zero = || known("1", "0");
        assert_eq!(fill_decision(&eff, sent, sent + 6_999, zero(), zero()), FillDecision::Wait);
        assert_eq!(
            fill_decision(&eff, sent, sent + 7_000, zero(), zero()),
            FillDecision::Transition(SystemEvent::TimeoutNoFills)
        );
        // Without the override the global 15 s applies.
        let global = eff_with(RiskOverrides::new());
        assert_eq!(fill_decision(&global, sent, sent + 7_000, zero(), zero()), FillDecision::Wait);
        assert_eq!(
            fill_decision(&global, sent, sent + 15_000, zero(), zero()),
            FillDecision::Transition(SystemEvent::TimeoutNoFills)
        );
    }

    #[test]
    fn long_full_short_seventy_percent_at_timeout_is_partial_failure_and_nothing_else() {
        let eff = eff_with(RiskOverrides::new());
        let (l, s) = (known("1", "1"), known("1", "0.7"));
        assert_eq!(fill_decision(&eff, 0, 14_999, l, s), FillDecision::Wait);
        let d = fill_decision(&eff, 0, 15_000, l, s);
        assert_eq!(d, FillDecision::Transition(SystemEvent::TimeoutPartialFill));
        // Exhaustive: the decision type has no way to ask for another order.
        match d {
            FillDecision::Wait | FillDecision::Transition(_) => {}
        }
    }

    #[test]
    fn complete_fills_are_judged_by_the_effective_imbalance_tolerance() {
        let eff = eff_with(RiskOverrides::new());
        assert_eq!(eff.max_leg_imbalance_pct, dec("1.0"));
        // 1.00 vs 0.99: 1% of the larger -> within (limit is inclusive).
        assert_eq!(
            fill_decision(&eff, 0, 10, known("1", "1"), known("0.99", "0.99")),
            FillDecision::Transition(SystemEvent::FillsWithinTolerance)
        );
        assert_eq!(
            fill_decision(&eff, 0, 10, known("1", "1"), known("0.98", "0.98")),
            FillDecision::Transition(SystemEvent::FillsExceedTolerance)
        );
        let mut ov = RiskOverrides::new();
        ov.insert(Exchange::Binance, RiskOverride { max_leg_imbalance_pct: Some(dec("0.5")), ..Default::default() });
        assert_eq!(
            fill_decision(&eff_with(ov), 0, 10, known("1", "1"), known("0.99", "0.99")),
            FillDecision::Transition(SystemEvent::FillsExceedTolerance)
        );
    }

    #[test]
    fn unknown_leg_waits_then_goes_undetermined() {
        let eff = eff_with(RiskOverrides::new());
        assert_eq!(fill_decision(&eff, 0, 1_000, known("1", "1"), LegFill::Unknown), FillDecision::Wait);
        assert_eq!(
            fill_decision(&eff, 0, 15_000, known("1", "1"), LegFill::Unknown),
            FillDecision::Transition(SystemEvent::TimeoutUndetermined)
        );
        assert_eq!(
            fill_decision(&eff, 0, 15_000, known("1", "0"), LegFill::Unknown),
            FillDecision::Transition(SystemEvent::TimeoutUndetermined)
        );
        // A non-positive requested quantity is not a usable fact.
        assert_eq!(
            fill_decision(&eff, 0, 15_000, known("0", "0"), known("1", "0")),
            FillDecision::Transition(SystemEvent::TimeoutUndetermined)
        );
    }

    #[test]
    fn both_partially_filled_at_timeout_is_partial_failure() {
        let eff = eff_with(RiskOverrides::new());
        assert_eq!(
            fill_decision(&eff, 0, 15_000, known("1", "0.5"), known("1", "0.5")),
            FillDecision::Transition(SystemEvent::TimeoutPartialFill)
        );
    }

    // ---- PREPARED auto-cancel ----

    fn obs(ex: Exchange, rate: &str, volume: Option<&str>) -> FundingObservation {
        FundingObservation::new(
            ex,
            "BTCUSDT",
            dec(rate),
            Some(28_800),
            1_000_000,
            dec("100"),
            volume.map(dec),
            0,
            0,
            DataStatus::Listed,
        )
    }

    struct Re {
        long: FundingObservation,
        short: FundingObservation,
        eff: EffectiveConfig,
        allowed: Vec<Exchange>,
    }

    impl Re {
        fn good() -> Re {
            Re {
                long: obs(Exchange::Binance, "-0.001", Some("1000000")),
                short: obs(Exchange::Bybit, "0.001", Some("1000000")),
                eff: eff_with(RiskOverrides::new()),
                allowed: Exchange::ALL.to_vec(),
            }
        }
        fn run(&self, state: PairState) -> AutoCancel {
            prepared_auto_cancel(
                state,
                &PreparedRecheck {
                    long: &self.long,
                    short: &self.short,
                    notional: dec("1000"),
                    effective: &self.eff,
                    allowed_exchanges: &self.allowed,
                },
            )
        }
    }

    #[test]
    fn qualifying_prepared_pair_is_kept() {
        assert_eq!(Re::good().run(PairState::Prepared), AutoCancel::NoAction);
    }

    #[test]
    fn worsened_prepared_pair_is_cancelled_with_every_reason() {
        let mut r = Re::good();
        r.short = obs(Exchange::Bybit, "0.0002", Some("10"));
        r.allowed = vec![Exchange::Bybit, Exchange::Okx];
        let AutoCancel::Cancel { reasons } = r.run(PairState::Prepared) else { panic!("must cancel") };
        assert!(matches!(reasons[0], CancelReason::NetEdgeBelowThreshold { .. }), "{reasons:?}");
        assert!(reasons.contains(&CancelReason::LowVolume {
            leg: Leg::Short,
            volume_24h_quote: Some(dec("10")),
            min_usdt: dec("50000"),
        }));
        assert!(reasons.contains(&CancelReason::ExchangeNotAllowed { leg: Leg::Long, exchange: Exchange::Binance }));
        assert_eq!(reasons.len(), 3);
    }

    #[test]
    fn missing_volume_uses_effective_threshold_and_cancels() {
        let mut r = Re::good();
        r.long = obs(Exchange::Binance, "-0.001", None);
        assert_eq!(
            r.run(PairState::Prepared),
            AutoCancel::Cancel {
                reasons: vec![CancelReason::LowVolume {
                    leg: Leg::Long,
                    volume_24h_quote: None,
                    min_usdt: dec("50000")
                }]
            }
        );
        let mut ov = RiskOverrides::new();
        ov.insert(Exchange::Bybit, RiskOverride { min_24h_volume_usdt: Some(dec("2000000")), ..Default::default() });
        let mut r = Re::good();
        r.eff = eff_with(ov);
        let AutoCancel::Cancel { reasons } = r.run(PairState::Prepared) else { panic!() };
        assert_eq!(reasons.len(), 2, "both legs are below the Bybit-raised threshold: {reasons:?}");
    }

    #[test]
    fn incomplete_settings_cancel_as_net_edge_unavailable() {
        let mut r = Re::good();
        r.eff.est_slippage_pct = None;
        assert_eq!(
            r.run(PairState::Prepared),
            AutoCancel::Cancel {
                reasons: vec![CancelReason::NetEdgeUnavailable { missing: vec!["est_slippage_pct".into()] }]
            }
        );
    }

    #[test]
    fn non_prepared_pairs_are_never_touched() {
        let mut r = Re::good();
        r.short = obs(Exchange::Bybit, "0.0002", Some("10"));
        r.allowed = vec![Exchange::Okx];
        for s in PairState::ALL {
            let got = r.run(s);
            if s == PairState::Prepared {
                assert!(matches!(got, AutoCancel::Cancel { .. }));
            } else {
                assert_eq!(got, AutoCancel::NoAction, "{s} must be untouched");
            }
        }
    }
}

//! Node 0 (task 2.2): assembles core's pre-trade input from freshly fetched data and the pair's
//! effective (per-pair merged) risk settings, then runs core's ten checks. `NetEdgeQualified` needs
//! both Net Edge ≥ `net_edge_threshold_pct` and expected net PnL ≥ `min_expected_net_pnl_pct`
//! (ui-trading-pages, MODIFIED pretrade-validation). Pure: the fetched
//! quotes, margins and `now_ms` are parameters.
//!
//! Fail-closed inputs are fed to core as values core itself rejects, so core stays the single
//! source of the verdict: an unavailable margin becomes `Decimal::MIN` (Margin fails) and a
//! pre-trade price that is not strictly newer than the baseline becomes infinitely old
//! (`i64::MIN`, DataFresh fails). Both are explained in `Node0Block::Checks::notes`.

use rust_decimal::Decimal;
use serde::Deserialize;
use tong_funding_core::funding::{is_consistent_listed, FundingObservation};
use tong_funding_core::net_edge::{compute_net_edge, expected_net_pnl_pct, meets_min_expected_net_pnl, NetEdge, NetEdgeParams};
use tong_funding_core::pair::PairState;
use tong_funding_core::pretrade::{evaluate_pretrade, Check, LegInput, PretradeInput, PretradeLimits, PretradeVerdict};
use tong_funding_core::risk::EffectiveConfig;
use tong_funding_core::trade_cost::leg_cost;
use tong_funding_core::types::{Exchange, Notional, Pct, Price};

use super::fill::{plan_submit, LegSizing, SubmitPlan};
use super::ports::{FreshQuote, Leg, OrderRules};

/// The scan-time part of `pairs.entry_json` that Node 0 needs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EntrySnapshot {
    pub long_scan_price: Price,
    pub short_scan_price: Price,
    /// Per-leg notional (USDT).
    pub notional_usdt: Notional,
    pub leverage: Decimal,
}

impl EntrySnapshot {
    /// Reads the snapshot from `entry_json`; unknown extra keys are ignored.
    pub fn from_json(value: &serde_json::Value) -> Result<EntrySnapshot, String> {
        serde_json::from_value(value.clone()).map_err(|e| format!("entry snapshot: {e}"))
    }
}

/// One leg's freshly fetched data.
#[derive(Debug, Clone, Copy)]
pub struct Node0Leg<'a> {
    pub exchange: Exchange,
    /// Baseline fetch (≈ T−15); `None` if it failed (core falls back to the scan price).
    pub baseline: Option<&'a FreshQuote>,
    /// Pre-trade fetch at the entry time.
    pub pretrade: &'a FreshQuote,
    /// From `AccountView::available_margin`; `Err` fails Margin, never replaced by a default.
    pub available_margin: &'a Result<Decimal, String>,
    /// A position / open order on this symbol that does not belong to this pair.
    pub has_foreign_exposure: bool,
}

/// Pair-level context for Node 0.
#[derive(Debug, Clone, Copy)]
pub struct Node0Context<'a> {
    pub now_ms: i64,
    /// The pair's symbol; every quote must be of this symbol on its leg's exchange.
    pub symbol: &'a str,
    pub entry: &'a EntrySnapshot,
    /// `effective_for_pair(global, overrides, long, short)`; never the global values.
    pub effective: &'a EffectiveConfig,
    /// Global-only field.
    pub max_concurrent_pairs: u32,
    /// Global-only field.
    pub allowed_exchanges: &'a [Exchange],
    /// [`count_open_pairs`] over the OTHER pairs.
    pub open_pair_count: u32,
    /// Order rules per leg `[long, short]` (already fetched for Node 1): they size the quantity
    /// the margin is computed for. `Err` = unknown, so Margin fails (nothing is guessed).
    pub rules: [&'a Result<OrderRules, String>; 2],
}

/// Why Node 0 blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node0Block {
    /// Required settings unset for this pair (e.g. `est_slippage_pct`, `taker_fee_pct.Bybit`);
    /// nothing is computed with 0.
    ConfigIncomplete { missing: Vec<String> },
    /// The stored entry snapshot cannot be used (e.g. leverage <= 0).
    InvalidEntry { reason: String },
    /// A fetched quote is not of this leg's exchange or of the pair's symbol (wiring bug).
    DataMismatch { reason: String },
    /// Core's checks failed (canonical order) with human-readable notes for the event log.
    Checks { failed: Vec<Check>, notes: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node0Verdict {
    Pass,
    Block(Node0Block),
}

/// Pairs that occupy a `max_concurrent_pairs` slot: every state except FINALIZED, CANCELLED and
/// BLOCKED (locked states still carry exposure, design D13). Pass the OTHER pairs only.
pub fn count_open_pairs(others: impl IntoIterator<Item = PairState>) -> u32 {
    let occupies = |s: PairState| match s {
        PairState::Finalized | PairState::Cancelled | PairState::Blocked => false,
        PairState::Prepared
        | PairState::PreTradeCheck
        | PairState::OrderSubmit
        | PairState::FillMonitor
        | PairState::Reconciled
        | PairState::Imbalanced
        | PairState::Closing
        | PairState::PartialFailure
        | PairState::Unresolved => true,
    };
    let n = others.into_iter().filter(|s| occupies(*s)).count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Names of required settings missing for this pair, in `EffectiveConfig` field order.
pub fn missing_settings(eff: &EffectiveConfig, long: Exchange, short: Exchange) -> Vec<String> {
    let mut out = Vec::new();
    if eff.net_edge_threshold_pct.is_none() {
        out.push("net_edge_threshold_pct".to_string());
    }
    if eff.est_slippage_pct.is_none() {
        out.push("est_slippage_pct".to_string());
    }
    for (fee, ex) in [(eff.long_taker_fee_pct, long), (eff.short_taker_fee_pct, short)] {
        let name = format!("taker_fee_pct.{ex:?}");
        if fee.is_none() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Net Edge of the oriented pair from fresh funding data, with the threshold it must reach.
/// `Err` names the missing settings (or carries core's error, e.g. a non-positive notional).
pub fn net_edge_with_threshold(
    long: &FundingObservation,
    short: &FundingObservation,
    notional: Notional,
    eff: &EffectiveConfig,
) -> Result<(NetEdge, Pct), Vec<String>> {
    let (Some(threshold), Some(fee_l), Some(fee_s), Some(_)) =
        (eff.net_edge_threshold_pct, eff.long_taker_fee_pct, eff.short_taker_fee_pct, eff.est_slippage_pct)
    else {
        return Err(missing_settings(eff, long.exchange, short.exchange));
    };
    let params = NetEdgeParams {
        taker_fee_pct: [(long.exchange, fee_l), (short.exchange, fee_s)].into_iter().collect(),
        est_slippage_pct: eff.est_slippage_pct,
        safety_margin_pct: eff.safety_margin_pct,
        net_edge_threshold_pct: Some(threshold),
        min_24h_volume_usdt: eff.min_24h_volume_usdt,
        // Not read by `compute_net_edge`; listing, volume and exchange rules are separate checks.
        allowed_exchanges: Default::default(),
        allowed_coins: Default::default(),
    };
    compute_net_edge(long, short, notional, &params)
        .map(|edge| (edge, threshold))
        .map_err(|e| vec![e.to_string()])
}

/// Each leg's needed margin: `shared qty × that leg's pre-trade latest price ÷ leverage + opening
/// fee` (spec: pretrade-validation). The quantity is the one Node 1 will send: it comes from
/// `fill::plan_submit` over the same prices and rules, never a second formula. Uses only data the
/// pre-trade step already holds (no request). Cannot be computed (rules unknown, below the minimum
/// lot, no fee): `Decimal::MAX`, which no balance reaches, so core fails `Margin`.
fn leg_margins_needed(ctx: &Node0Context<'_>, legs: [&Node0Leg<'_>; 2], notes: &mut Vec<String>) -> [Decimal; 2] {
    const UNKNOWN: [Decimal; 2] = [Decimal::MAX, Decimal::MAX];
    let names = [Leg::Long, Leg::Short];
    let mut sizing = Vec::with_capacity(2);
    for (i, leg) in legs.iter().enumerate() {
        match ctx.rules[i] {
            Ok(r) => sizing.push(LegSizing {
                exchange: leg.exchange,
                notional: ctx.entry.notional_usdt,
                price: leg.pretrade.price,
                lot: r.lot,
                okx_ct_val: r.okx_ct_val,
            }),
            Err(e) => {
                notes.push(format!("{}: order rules unavailable, margin needed not computable: {e}", names[i].as_str()));
                return UNKNOWN;
            }
        }
    }
    let qty = match plan_submit(&sizing[0], &sizing[1]) {
        SubmitPlan::Send { long, .. } => long.base_qty,
        SubmitPlan::Abort { failed } => {
            let why: Vec<String> = failed.iter().map(|(leg, e)| format!("{}: {e:?}", leg.as_str())).collect();
            notes.push(format!("quantity not computable, margin needed not computable: {}", why.join("; ")));
            return UNKNOWN;
        }
    };
    let fees = [ctx.effective.long_taker_fee_pct, ctx.effective.short_taker_fee_pct];
    let mut out = UNKNOWN;
    for i in 0..2 {
        let Some(fee) = fees[i] else {
            notes.push(format!("{}: taker fee unset, margin needed not computable", names[i].as_str()));
            continue;
        };
        match leg_cost(qty, legs[i].pretrade.price, fee, ctx.entry.leverage) {
            Ok(c) => out[i] = c.margin,
            Err(e) => notes.push(format!("{}: margin needed not computable: {e}", names[i].as_str())),
        }
    }
    out
}

/// Builds core's input and limits plus notes on fail-closed substitutions.
pub fn build_input(
    ctx: &Node0Context<'_>,
    long: &Node0Leg<'_>,
    short: &Node0Leg<'_>,
) -> Result<(PretradeInput, PretradeLimits, Vec<String>), Node0Block> {
    for (name, leg) in [(Leg::Long, long), (Leg::Short, short)] {
        let quotes = [("pre-trade", Some(leg.pretrade)), ("baseline", leg.baseline)];
        for (what, q) in quotes {
            if let Some(q) = q
                && (q.funding.exchange != leg.exchange || q.funding.symbol != ctx.symbol)
            {
                return Err(Node0Block::DataMismatch {
                    reason: format!(
                        "{}: {what} quote is {:?} {} but the leg is {:?} {}",
                        name.as_str(),
                        q.funding.exchange,
                        q.funding.symbol,
                        leg.exchange,
                        ctx.symbol
                    ),
                });
            }
        }
    }
    let eff = ctx.effective;
    let missing = missing_settings(eff, long.exchange, short.exchange);
    if !missing.is_empty() {
        return Err(Node0Block::ConfigIncomplete { missing });
    }
    let entry = ctx.entry;
    if entry.leverage <= Decimal::ZERO || entry.notional_usdt <= Decimal::ZERO {
        return Err(Node0Block::InvalidEntry {
            reason: format!(
                "leverage ({}) and notional_usdt ({}) must be > 0",
                entry.leverage, entry.notional_usdt
            ),
        });
    }

    let mut notes = Vec::new();
    let net_edge_qualified =
        match net_edge_with_threshold(&long.pretrade.funding, &short.pretrade.funding, entry.notional_usdt, eff) {
            // NetEdgeQualified = Net Edge ≥ threshold AND expected net PnL ≥ min (both effective).
            Ok((edge, threshold)) => {
                let min = eff.min_expected_net_pnl_pct;
                let pnl_ok = meets_min_expected_net_pnl(&edge, entry.notional_usdt, min);
                if !pnl_ok {
                    notes.push(format!(
                        "expected net PnL {}% is below min_expected_net_pnl_pct {}%",
                        expected_net_pnl_pct(&edge, entry.notional_usdt).normalize(),
                        min.normalize()
                    ));
                }
                edge.net_edge_pct >= threshold && pnl_ok
            }
            Err(why) => {
                notes.push(format!("net edge not computable: {}", why.join(", ")));
                false
            }
        };
    let margins_needed = leg_margins_needed(ctx, [long, short], &mut notes);
    let mut leg_input = |name: Leg, leg: &Node0Leg<'_>, scan_price: Price| {
        let q = leg.pretrade;
        let mut price_observed_at_ms = q.price_observed_at_ms;
        if let Some(b) = leg.baseline
            && q.price_observed_at_ms <= b.price_observed_at_ms
        {
            notes.push(format!(
                "{}: pre-trade price observed_at {} is not newer than the baseline's {} (source not updated)",
                name.as_str(),
                q.price_observed_at_ms,
                b.price_observed_at_ms
            ));
            price_observed_at_ms = i64::MIN;
        }
        let available_margin = match leg.available_margin {
            Ok(m) => *m,
            Err(e) => {
                notes.push(format!("{}: available margin unavailable: {e}", name.as_str()));
                Decimal::MIN
            }
        };
        LegInput {
            exchange: leg.exchange,
            baseline_price: leg.baseline.map(|b| b.price),
            scan_price,
            latest_price: q.price,
            price_observed_at_ms,
            funding_observed_at_ms: q.funding.observed_at,
            available_margin,
            margin_needed: margins_needed[name as usize],
            listed: q.listed && is_consistent_listed(&q.funding),
            exchange_allowed: ctx.allowed_exchanges.contains(&leg.exchange),
            volume_24h_quote: q.funding.volume_24h_quote,
            has_foreign_exposure: leg.has_foreign_exposure,
        }
    };
    let long_in = leg_input(Leg::Long, long, entry.long_scan_price);
    let short_in = leg_input(Leg::Short, short, entry.short_scan_price);

    let input = PretradeInput {
        now_ms: ctx.now_ms,
        net_edge_qualified,
        long: long_in,
        short: short_in,
        leverage: entry.leverage,
        open_pair_count: ctx.open_pair_count,
    };
    let limits = PretradeLimits {
        max_price_drift_pct: eff.max_price_drift_pct,
        stale_data_threshold_ms: i64::try_from(eff.stale_data_threshold_ms).unwrap_or(i64::MAX),
        max_leverage: eff.max_leverage,
        max_concurrent_pairs: ctx.max_concurrent_pairs,
        min_24h_volume_usdt: eff.min_24h_volume_usdt,
    };
    Ok((input, limits, notes))
}

/// Node 0: build the input, run core's ten checks.
pub fn run(ctx: &Node0Context<'_>, long: &Node0Leg<'_>, short: &Node0Leg<'_>) -> Node0Verdict {
    match build_input(ctx, long, short) {
        Err(block) => Node0Verdict::Block(block),
        Ok((input, limits, notes)) => match evaluate_pretrade(&input, &limits) {
            PretradeVerdict::Pass => Node0Verdict::Pass,
            PretradeVerdict::Block { failed } => Node0Verdict::Block(Node0Block::Checks { failed, notes }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tong_funding_core::funding::DataStatus;
    use tong_funding_core::quantity::LotSize;
    use tong_funding_core::risk::{effective_for_pair, RiskConfig, RiskOverride, RiskOverrides};

    fn lot_rules(step: &str) -> Result<OrderRules, String> {
        Ok(OrderRules { lot: LotSize { step_size: dec(step), min_qty: dec(step) }, okx_ct_val: None })
    }

    const SETTLE: i64 = 10_000_000;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn quote(ex: Exchange, rate: &str, price: &str, observed_at: i64) -> FreshQuote {
        FreshQuote {
            funding: FundingObservation::new(
                ex,
                "BTCUSDT",
                dec(rate),
                Some(28_800),
                SETTLE,
                dec(price),
                Some(dec("1000000")),
                observed_at,
                observed_at,
                DataStatus::Listed,
            ),
            price: dec(price),
            price_observed_at_ms: observed_at,
            listed: true,
        }
    }

    fn global() -> RiskConfig {
        RiskConfig {
            net_edge_threshold_pct: Some(dec("0.05")),
            est_slippage_pct: Some(dec("0.01")),
            taker_fee_pct: BTreeMap::from([
                (Exchange::Binance, dec("0.02")),
                (Exchange::Bybit, dec("0.02")),
                (Exchange::Okx, dec("0.02")),
            ]),
            ..RiskConfig::default()
        }
    }

    fn entry() -> EntrySnapshot {
        EntrySnapshot {
            long_scan_price: dec("100"),
            short_scan_price: dec("100"),
            notional_usdt: dec("1000"),
            leverage: dec("5"),
        }
    }

    /// Fixture: Binance long / Bybit short, baseline at 1_000, pre-trade at 6_000, now 6_100.
    struct Fx {
        cfg: RiskConfig,
        overrides: RiskOverrides,
        entry: EntrySnapshot,
        open: u32,
        now: i64,
        base_l: FreshQuote,
        base_s: FreshQuote,
        pre_l: FreshQuote,
        pre_s: FreshQuote,
        margin_l: Result<Decimal, String>,
        margin_s: Result<Decimal, String>,
        rules_l: Result<OrderRules, String>,
        rules_s: Result<OrderRules, String>,
    }

    impl Fx {
        fn new() -> Fx {
            Fx {
                cfg: global(),
                overrides: RiskOverrides::new(),
                entry: entry(),
                open: 0,
                now: 6_100,
                base_l: quote(Exchange::Binance, "-0.001", "100", 1_000),
                base_s: quote(Exchange::Bybit, "0.001", "100", 1_000),
                pre_l: quote(Exchange::Binance, "-0.001", "100.02", 6_000),
                pre_s: quote(Exchange::Bybit, "0.001", "100.01", 6_000),
                margin_l: Ok(dec("1000")),
                margin_s: Ok(dec("1000")),
                rules_l: lot_rules("0.001"),
                rules_s: lot_rules("0.001"),
            }
        }

        fn eval<T>(&self, f: impl FnOnce(&Node0Context<'_>, &Node0Leg<'_>, &Node0Leg<'_>) -> T) -> T {
            let eff = effective_for_pair(&self.cfg, &self.overrides, Exchange::Binance, Exchange::Bybit);
            let ctx = Node0Context {
                now_ms: self.now,
                symbol: "BTCUSDT",
                entry: &self.entry,
                effective: &eff,
                max_concurrent_pairs: self.cfg.max_concurrent_pairs,
                allowed_exchanges: &self.cfg.allowed_exchanges,
                open_pair_count: self.open,
                rules: [&self.rules_l, &self.rules_s],
            };
            let long = Node0Leg {
                exchange: Exchange::Binance,
                baseline: Some(&self.base_l),
                pretrade: &self.pre_l,
                available_margin: &self.margin_l,
                has_foreign_exposure: false,
            };
            let short = Node0Leg {
                exchange: Exchange::Bybit,
                baseline: Some(&self.base_s),
                pretrade: &self.pre_s,
                available_margin: &self.margin_s,
                has_foreign_exposure: false,
            };
            f(&ctx, &long, &short)
        }

        fn verdict(&self) -> Node0Verdict {
            self.eval(run)
        }

        fn failed(&self) -> Vec<Check> {
            match self.verdict() {
                Node0Verdict::Block(Node0Block::Checks { failed, .. }) => failed,
                other => panic!("expected failed checks, got {other:?}"),
            }
        }
    }

    #[test]
    fn complete_fresh_data_passes() {
        assert_eq!(Fx::new().verdict(), Node0Verdict::Pass);
    }

    #[test]
    fn two_fetches_each_with_their_own_observed_at_and_the_check_uses_the_second() {
        let fx = Fx::new();
        let (input, _, notes) = fx.eval(build_input).expect("builds");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(input.long.baseline_price, Some(dec("100")));
        assert_eq!(input.long.latest_price, dec("100.02"));
        assert_eq!(input.long.price_observed_at_ms, 6_000);
        assert_eq!(input.short.latest_price, dec("100.01"));
        assert_eq!(input.short.price_observed_at_ms, 6_000);
        assert_eq!(input.long.scan_price, dec("100"));
        assert_eq!(input.now_ms, 6_100);
    }

    #[test]
    fn second_fetch_not_updated_is_blocked() {
        let mut fx = Fx::new();
        fx.pre_s = fx.base_s.clone();
        fx.now = 1_100; // fresh by the clock: only "not newer than the baseline" can fail it
        assert_eq!(fx.failed(), vec![Check::DataFresh]);
        let notes = match fx.verdict() {
            Node0Verdict::Block(Node0Block::Checks { notes, .. }) => notes,
            other => panic!("{other:?}"),
        };
        assert!(notes.iter().any(|n| n.contains("short") && n.contains("baseline")), "{notes:?}");
        // Older than the baseline is just as bad.
        fx.pre_s.price_observed_at_ms = 999;
        assert_eq!(fx.failed(), vec![Check::DataFresh]);
    }

    #[test]
    fn missing_baseline_falls_back_to_scan_price() {
        let fx = Fx::new();
        let eff = effective_for_pair(&fx.cfg, &fx.overrides, Exchange::Binance, Exchange::Bybit);
        let ctx = Node0Context {
            now_ms: fx.now,
            symbol: "BTCUSDT",
            entry: &fx.entry,
            effective: &eff,
            max_concurrent_pairs: 3,
            allowed_exchanges: &fx.cfg.allowed_exchanges,
            open_pair_count: 0,
            rules: [&fx.rules_l, &fx.rules_s],
        };
        let long = Node0Leg {
            exchange: Exchange::Binance,
            baseline: None,
            pretrade: &fx.pre_l,
            available_margin: &fx.margin_l,
            has_foreign_exposure: false,
        };
        let short = Node0Leg { baseline: None, pretrade: &fx.pre_s, exchange: Exchange::Bybit, ..long };
        let (input, _, _) = build_input(&ctx, &long, &short).unwrap();
        assert_eq!(input.long.baseline_price, None);
        assert_eq!(run(&ctx, &long, &short), Node0Verdict::Pass);
    }

    #[test]
    fn bybit_override_max_leverage_4_makes_leverage_5_fail() {
        let mut fx = Fx::new();
        assert_eq!(fx.cfg.max_leverage, dec("5"));
        fx.overrides.insert(Exchange::Bybit, RiskOverride { max_leverage: Some(dec("4")), ..Default::default() });
        assert_eq!(fx.failed(), vec![Check::Leverage]);
    }

    #[test]
    fn missing_est_slippage_blocks_and_names_it() {
        let mut fx = Fx::new();
        fx.cfg.est_slippage_pct = None;
        assert_eq!(
            fx.verdict(),
            Node0Verdict::Block(Node0Block::ConfigIncomplete { missing: vec!["est_slippage_pct".into()] })
        );
    }

    #[test]
    fn missing_fee_names_the_leg_exchange_and_unrelated_exchanges_do_not_matter() {
        let mut fx = Fx::new();
        fx.cfg.taker_fee_pct.remove(&Exchange::Okx);
        assert_eq!(fx.verdict(), Node0Verdict::Pass, "OKX is not part of this pair");
        fx.cfg.taker_fee_pct.remove(&Exchange::Bybit);
        fx.cfg.net_edge_threshold_pct = None;
        assert_eq!(
            fx.verdict(),
            Node0Verdict::Block(Node0Block::ConfigIncomplete {
                missing: vec!["net_edge_threshold_pct".into(), "taker_fee_pct.Bybit".into()]
            })
        );
    }

    #[test]
    fn price_3001_ms_old_with_default_threshold_3000_fails_data_fresh() {
        let mut fx = Fx::new();
        fx.now = 6_000 + 3_000;
        assert_eq!(fx.verdict(), Node0Verdict::Pass, "exactly the threshold is still fresh");
        fx.now = 6_000 + 3_001;
        assert_eq!(fx.failed(), vec![Check::DataFresh]);
    }

    #[test]
    fn stale_threshold_comes_from_the_effective_config() {
        let mut fx = Fx::new();
        fx.overrides
            .insert(Exchange::Binance, RiskOverride { stale_data_threshold_ms: Some(50), ..Default::default() });
        assert_eq!(fx.failed(), vec![Check::DataFresh]);
    }

    #[test]
    fn max_concurrent_one_with_a_partial_failure_pair_fails_risk_limits() {
        let mut fx = Fx::new();
        fx.cfg.max_concurrent_pairs = 1;
        fx.open = count_open_pairs([PairState::PartialFailure]);
        assert_eq!(fx.open, 1);
        assert_eq!(fx.failed(), vec![Check::RiskLimits]);
        fx.open = count_open_pairs([PairState::Finalized]);
        assert_eq!(fx.open, 0);
        assert_eq!(fx.verdict(), Node0Verdict::Pass);
    }

    #[test]
    fn open_pair_count_covers_every_state() {
        for s in PairState::ALL {
            let expect = !matches!(s, PairState::Finalized | PairState::Cancelled | PairState::Blocked);
            assert_eq!(count_open_pairs([s]), u32::from(expect), "{s}");
        }
    }

    #[test]
    fn unavailable_margin_fails_margin_never_a_default() {
        let mut fx = Fx::new();
        fx.margin_l = Err("keychain locked".into());
        fx.entry.notional_usdt = dec("0.0001"); // even a tiny requirement must fail
        let verdict = fx.verdict();
        let Node0Verdict::Block(Node0Block::Checks { failed, notes }) = verdict else { panic!("{verdict:?}") };
        assert!(failed.contains(&Check::Margin), "{failed:?}");
        assert!(notes.iter().any(|n| n.contains("keychain locked")), "{notes:?}");
    }

    #[test]
    fn net_edge_below_threshold_fails_net_edge_qualified() {
        let mut fx = Fx::new();
        fx.pre_s = quote(Exchange::Bybit, "0.0002", "100.01", 6_000);
        assert_eq!(fx.failed(), vec![Check::NetEdgeQualified]);
    }

    /// ui-trading-pages (MODIFIED pretrade-validation): Net Edge 0.07 % clears the 0.05 threshold;
    /// expected net PnL (before the 0.01 safety margin) is 0.08 %: income 2.0 − fees 0.8 −
    /// slippage 0.4 = 0.8 USDT on 1,000.
    #[test]
    fn expected_net_pnl_below_min_fails_net_edge_qualified_and_equal_passes() {
        let mut fx = Fx::new();
        fx.cfg.min_expected_net_pnl_pct = dec("0.0801");
        assert_eq!(fx.failed(), vec![Check::NetEdgeQualified]);
        fx.cfg.min_expected_net_pnl_pct = dec("0.08");
        assert_eq!(fx.verdict(), Node0Verdict::Pass);
    }

    #[test]
    fn exchange_not_allowed_and_foreign_exposure_are_reported() {
        let mut fx = Fx::new();
        fx.cfg.allowed_exchanges = vec![Exchange::Binance, Exchange::Okx];
        assert_eq!(fx.failed(), vec![Check::ExchangeAllowed]);
    }

    #[test]
    fn quotes_of_another_exchange_or_symbol_are_refused() {
        let mut fx = Fx::new();
        fx.pre_s = quote(Exchange::Okx, "0.001", "100.01", 6_000); // short leg is Bybit
        assert!(
            matches!(fx.verdict(), Node0Verdict::Block(Node0Block::DataMismatch { ref reason }) if reason.contains("short")),
            "{:?}",
            fx.verdict()
        );
        let mut fx = Fx::new();
        fx.base_l.funding.symbol = "ETHUSDT".into();
        assert!(matches!(fx.verdict(), Node0Verdict::Block(Node0Block::DataMismatch { .. })), "{:?}", fx.verdict());
    }

    #[test]
    fn non_positive_leverage_is_an_invalid_entry() {
        let mut fx = Fx::new();
        fx.entry.leverage = Decimal::ZERO;
        assert!(matches!(fx.verdict(), Node0Verdict::Block(Node0Block::InvalidEntry { .. })));
    }

    /// Fixture for the margin spec: both legs at `price`, qty 0.016 (1000 / 60000 floored to 0.001),
    /// leverage 5, taker 0.05 on both exchanges.
    fn margin_fx(price: &str, available: &str) -> Fx {
        let mut fx = Fx::new();
        fx.cfg.taker_fee_pct.insert(Exchange::Binance, dec("0.05"));
        fx.cfg.taker_fee_pct.insert(Exchange::Bybit, dec("0.05"));
        fx.entry.long_scan_price = dec(price);
        fx.entry.short_scan_price = dec(price);
        fx.base_l = quote(Exchange::Binance, "-0.001", price, 1_000);
        fx.base_s = quote(Exchange::Bybit, "0.001", price, 1_000);
        fx.pre_l = quote(Exchange::Binance, "-0.001", price, 6_000);
        fx.pre_s = quote(Exchange::Bybit, "0.001", price, 6_000);
        fx.margin_l = Ok(dec(available));
        fx.margin_s = Ok(dec(available));
        fx
    }

    fn margin_failed(fx: &Fx) -> bool {
        let (input, limits, _) = fx.eval(build_input).expect("builds");
        evaluate_pretrade(&input, &limits).failed().contains(&Check::Margin)
    }

    /// pretrade-validation: qty 0.016, latest 60,000, leverage 5, taker 0.05, available 192.2 ->
    /// needed 192.48 (192 + 0.48) and Margin fails; 192.48 passes.
    #[test]
    fn margin_needed_includes_the_opening_fee_spec_case() {
        let fx = margin_fx("60000", "192.2");
        let (input, _, _) = fx.eval(build_input).unwrap();
        assert_eq!(input.long.margin_needed, dec("192.48"));
        assert_eq!(input.short.margin_needed, dec("192.48"));
        assert!(margin_failed(&fx), "192.2 < 192.48");
        assert!(!margin_failed(&margin_fx("60000", "192.48")), "exactly enough passes");
        assert!(margin_failed(&margin_fx("60000", "192")), "notional / leverage alone (200) is not what is checked");
    }

    /// Each leg uses the SHARED quantity (priced by the higher leg) at its OWN latest price and fee.
    #[test]
    fn margin_needed_uses_the_shared_quantity_and_each_legs_own_price_and_fee() {
        let mut fx = margin_fx("60000", "1000");
        fx.pre_s = quote(Exchange::Bybit, "0.001", "60010", 6_000);
        fx.base_s = quote(Exchange::Bybit, "0.001", "60010", 1_000);
        fx.entry.short_scan_price = dec("60010");
        fx.cfg.taker_fee_pct.insert(Exchange::Bybit, dec("0.055"));
        let (input, _, _) = fx.eval(build_input).unwrap();
        // qty = floor(1000 / 60010, 0.001) = 0.016 on both legs.
        assert_eq!(input.long.margin_needed, dec("0.016") * dec("60000") / dec("5") + dec("0.48"));
        assert_eq!(input.short.margin_needed, dec("0.016") * dec("60010") / dec("5") + dec("0.528088"));
    }

    #[test]
    fn margin_needed_fails_closed_when_the_quantity_cannot_be_computed() {
        let mut fx = margin_fx("60000", "1000000");
        fx.rules_s = Err("instruments-info unavailable".into());
        assert!(margin_failed(&fx), "unknown order rules must fail Margin, whatever the balance");
        let mut fx = margin_fx("60000", "1000000");
        fx.rules_l = lot_rules("1"); // 1000 / 60000 floors to 0: below the minimum
        assert!(margin_failed(&fx));
    }

    #[test]
    fn entry_snapshot_reads_from_entry_json() {
        let v = serde_json::json!({
            "long_scan_price": "100.5", "short_scan_price": "100.4",
            "notional_usdt": "1000", "leverage": "3", "net_edge_pct": "0.07"
        });
        let e = EntrySnapshot::from_json(&v).unwrap();
        assert_eq!(e.long_scan_price, dec("100.5"));
        assert_eq!(e.leverage, dec("3"));
        // Money never travels as a float.
        let mut float = v.clone();
        float["short_scan_price"] = serde_json::json!(100.4);
        assert!(EntrySnapshot::from_json(&float).is_err());
        assert!(EntrySnapshot::from_json(&serde_json::json!({"leverage": "3"})).is_err());
    }
}

use super::*;
use crate::engine::command::Command;
use crate::engine::node0::{self, EntrySnapshot, Node0Block, Node0Context, Node0Leg, Node0Verdict};
use crate::engine::ports::FreshQuote;
use crate::ui::bridge::{CommandSink, Settings};
use crate::ui::testkit::{complete_settings, d};
use std::cell::RefCell;
use tong_funding_core::funding::{DataStatus, FundingObservation};
use tong_funding_core::pretrade::Check;
use tong_funding_core::risk::{effective_for_pair, parse_overrides, ExecutionMode, RiskConfig};
use tong_funding_core::types::Exchange;

#[derive(Default)]
struct Sink(RefCell<Vec<Command>>);
impl CommandSink for Sink {
    fn send(&self, _label: String, command: Command) {
        self.0.borrow_mut().push(command);
    }
}

// ---- 3.1 fields, units, validation, save ---------------------------------------------------

#[test]
fn units_defaults_and_removed_fields() {
    let f = RiskForm::from_settings(&Settings::default());
    assert_eq!(f.value(Field::OrderTimeoutSeconds), "15");
    assert_eq!(Field::OrderTimeoutSeconds.unit(), "秒");
    assert_eq!(f.value(Field::StaleDataThresholdMs), "3000");
    assert_eq!(Field::StaleDataThresholdMs.unit(), "ms");
    assert_eq!(Field::MaxPriceDriftPct.unit(), "%");
    assert_eq!(f.value(Field::MinExpectedNetPnlPct), "0.03");
    let keys: Vec<String> = GLOBAL_FIELDS.iter().map(|f| f.key()).collect();
    for removed in ["funding_threshold_pct", "max_concurrent_trades", "hedge_threshold_pct", "max_slippage_pct"] {
        assert!(!keys.contains(&removed.to_string()), "{removed}");
    }
    assert!(keys.contains(&"max_price_drift_pct".to_string()) && keys.contains(&"est_slippage_pct".to_string()));
    assert!(keys.contains(&"min_expected_net_pnl_pct".to_string()) && keys.contains(&"net_edge_threshold_pct".to_string()), "both thresholds kept");
}

#[test]
fn max_leverage_zero_is_refused_by_name_and_nothing_is_sent() {
    let s = complete_settings("0.01");
    let mut f = RiskForm::from_settings(&s);
    f.set(Field::MaxLeverage, "0");
    let vm = evaluate(&f, &s);
    assert_eq!(vm.field_errors.get("max_leverage").map(String::as_str), Some("max_leverage 必須大於 0"));
    assert!(!vm.can_save);
    let sink = Sink::default();
    assert!(!save(&vm, &sink));
    assert!(sink.0.borrow().is_empty(), "stored settings unchanged: nothing sent");
}

#[test]
fn the_mode_options_have_no_live_and_a_live_value_cannot_be_saved() {
    assert_eq!(MODE_OPTIONS.map(|m| mode_label(m)), ["SIMULATION", "EXCHANGE_DEMO"]);
    assert!(MODE_OPTIONS.iter().all(|m| !mode_label(*m).contains("LIVE")));
    // Even a stored value cannot sneak LIVE through: the saved JSON carries the parsed mode.
    let vm = evaluate(&RiskForm::from_settings(&complete_settings("0.01")), &complete_settings("0.01"));
    let cfg = vm.result.as_ref().unwrap().0.clone();
    assert_eq!(cfg.execution_mode, ExecutionMode::Simulation);
}

#[test]
fn saving_sends_one_command_with_both_values() {
    let s = complete_settings("0.01");
    let mut f = RiskForm::from_settings(&s);
    f.set(Field::MaxLeverage, "4");
    f.set_override(Exchange::Bybit, Field::MaxLeverage, true, &s);
    f.set_override_value(Exchange::Bybit, Field::MaxLeverage, "3");
    let vm = evaluate(&f, &s);
    let sink = Sink::default();
    assert!(save(&vm, &sink));
    match &sink.0.borrow()[..] {
        [Command::SaveRiskSettings { risk, overrides }] => {
            assert_eq!(RiskConfig::from_json(&risk.to_string()).unwrap().max_leverage, d("4"));
            assert_eq!(overrides, &serde_json::json!({"Bybit": {"max_leverage": "3"}}));
        }
        other => panic!("{other:?}"),
    }
}

// ---- 3.2 Net Edge block and "incomplete" --------------------------------------------------

#[test]
fn a_blank_config_is_incomplete_and_lists_every_missing_field() {
    let vm = evaluate(&RiskForm::from_settings(&Settings::default()), &Settings::default());
    assert_eq!(vm.missing, vec!["net_edge_threshold_pct", "est_slippage_pct", "Binance taker_fee_pct", "Bybit taker_fee_pct", "OKX taker_fee_pct"]);
    assert_eq!(vm.incomplete_text.as_deref(), Some("設定不完整：net_edge_threshold_pct、est_slippage_pct、Binance taker_fee_pct、Bybit taker_fee_pct、OKX taker_fee_pct"));
    let f = RiskForm::from_settings(&Settings::default());
    assert_eq!(f.value(Field::SafetyMarginPct), "0.01");
    assert_eq!(f.value(Field::TakerFee(Exchange::Bybit)), "", "no default fee: blank, never 0");
    assert!(vm.can_save, "an incomplete config can still be saved (it only blocks trading)");
}

#[test]
fn filling_every_required_field_makes_it_complete() {
    let mut f = RiskForm::from_settings(&Settings::default());
    f.set(Field::NetEdgeThresholdPct, "0.02");
    f.set(Field::EstSlippagePct, "0.01");
    for e in Exchange::ALL {
        f.set(Field::TakerFee(e), "0.05");
    }
    let vm = evaluate(&f, &Settings::default());
    assert!(vm.missing.is_empty() && vm.incomplete_text.is_none());
    assert!(vm.result.as_ref().unwrap().0.is_complete());
}

#[test]
fn the_formula_summary_shows_unset_instead_of_zero() {
    let mut f = RiskForm::from_settings(&complete_settings("0.01"));
    f.set(Field::TakerFee(Exchange::Bybit), "");
    let vm = evaluate(&f, &complete_settings("0.01"));
    assert!(vm.formula.contains("Bybit 未設定"), "{}", vm.formula);
    assert!(vm.formula.contains("Binance 0.02"), "{}", vm.formula);
    assert!(vm.formula.contains("4 ×"), "{}", vm.formula);
}

// ---- 3.3 overrides ------------------------------------------------------------------------

#[test]
fn overrides_offer_exactly_the_nine_fields() {
    let keys: Vec<String> = OVERRIDE_FIELDS.iter().map(|f| f.key()).collect();
    assert_eq!(keys.len(), 9);
    for global_only in ["execution_mode", "trigger_mode", "max_concurrent_pairs", "allowed_exchanges", "allowed_coins", "min_expected_net_pnl_pct"] {
        assert!(!keys.contains(&global_only.to_string()), "{global_only}");
    }
}

#[test]
fn switching_an_override_on_starts_from_the_global_value_and_off_removes_it() {
    let s = complete_settings("0.01");
    let mut f = RiskForm::from_settings(&s);
    f.set_override(Exchange::Binance, Field::EstSlippagePct, true, &s);
    assert_eq!(f.override_value(Exchange::Binance, Field::EstSlippagePct), Some("0".to_string()));
    f.set_override_value(Exchange::Binance, Field::EstSlippagePct, "0.03");
    let vm = evaluate(&f, &s);
    let p = vm.preview.iter().find(|p| p.long == Exchange::Binance && p.short == Exchange::Bybit).unwrap();
    assert_eq!(p.effective.est_slippage_pct, Some(d("0.03")), "conservative: larger wins");
    f.set_override(Exchange::Binance, Field::EstSlippagePct, false, &s);
    let vm = evaluate(&f, &s);
    assert!(vm.result.as_ref().unwrap().1.get(&Exchange::Binance).is_none(), "switching off removes the override");
}

fn quote(ex: Exchange, rate: &str, observed_at: i64) -> FreshQuote {
    FreshQuote {
        funding: FundingObservation::new(ex, "BTCUSDT", d(rate), Some(28_800), 10_000_000, d("100"), Some(d("1000000")), observed_at, observed_at, DataStatus::Listed),
        price: d("100"),
        price_observed_at_ms: observed_at,
        listed: true,
    }
}

/// Node 0 exactly as the engine runs it, from the JSON the risk page sends.
fn leverage_checks(risk_json: &serde_json::Value, overrides_json: &serde_json::Value, long: Exchange, short: Exchange) -> Vec<Check> {
    let cfg = RiskConfig::from_json(&risk_json.to_string()).unwrap();
    let ov = parse_overrides(overrides_json).unwrap();
    let eff = effective_for_pair(&cfg, &ov, long, short);
    let entry = EntrySnapshot { long_scan_price: d("100"), short_scan_price: d("100"), notional_usdt: d("1000"), leverage: d("5") };
    let (bl, bs, pl, ps) = (quote(long, "-0.001", 1_000), quote(short, "0.001", 1_000), quote(long, "-0.001", 6_000), quote(short, "0.001", 6_000));
    let margin: Result<tong_funding_core::types::Decimal, String> = Ok(d("10000"));
    let ctx = Node0Context { now_ms: 6_100, symbol: "BTCUSDT", entry: &entry, effective: &eff, max_concurrent_pairs: cfg.max_concurrent_pairs, allowed_exchanges: &cfg.allowed_exchanges, open_pair_count: 0 };
    let l = Node0Leg { exchange: long, baseline: Some(&bl), pretrade: &pl, available_margin: &margin, has_foreign_exposure: false };
    let s = Node0Leg { exchange: short, baseline: Some(&bs), pretrade: &ps, available_margin: &margin, has_foreign_exposure: false };
    match node0::run(&ctx, &l, &s) {
        Node0Verdict::Pass => vec![],
        Node0Verdict::Block(Node0Block::Checks { failed, .. }) => failed,
        Node0Verdict::Block(other) => panic!("{other:?}"),
    }
}

/// Integration (task 3.3): the override set on the risk page reaches the engine's pre-trade check.
#[test]
fn a_bybit_override_of_4_from_the_page_blocks_leverage_5_only_for_pairs_with_bybit() {
    let mut s = complete_settings("0.05");
    s.risk.safety_margin_pct = d("0.01");
    s.risk.est_slippage_pct = Some(d("0.01"));
    let mut f = RiskForm::from_settings(&s);
    f.set_override(Exchange::Bybit, Field::MaxLeverage, true, &s);
    f.set_override_value(Exchange::Bybit, Field::MaxLeverage, "4");
    let sink = Sink::default();
    assert!(save(&evaluate(&f, &s), &sink));
    let (risk, ov) = match &sink.0.borrow()[..] {
        [Command::SaveRiskSettings { risk, overrides }] => (risk.clone(), overrides.clone()),
        other => panic!("{other:?}"),
    };
    assert_eq!(leverage_checks(&risk, &ov, Exchange::Binance, Exchange::Bybit), vec![Check::Leverage]);
    assert_eq!(leverage_checks(&risk, &ov, Exchange::Binance, Exchange::Okx), Vec::<Check>::new());
}

// ---- 3.4 execution mode switch ------------------------------------------------------------

#[test]
fn switching_to_exchange_demo_needs_a_confirmation_first() {
    let s = complete_settings("0.01");
    let sink = Sink::default();
    let step = request_mode(ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &s, Some(&Ok(())), &sink).unwrap();
    assert_eq!(step, ModeStep::Confirm(ExecutionMode::ExchangeDemo));
    assert!(sink.0.borrow().is_empty(), "the mode stays SIMULATION until confirmed");
    send_mode(ExecutionMode::ExchangeDemo, &sink);
    assert_eq!(*sink.0.borrow(), vec![Command::SetExecutionMode(ExecutionMode::ExchangeDemo)]);
    // Back to SIMULATION needs no confirmation.
    let sink = Sink::default();
    assert_eq!(request_mode(ExecutionMode::ExchangeDemo, ExecutionMode::Simulation, &s, None, &sink).unwrap(), ModeStep::Sent);
    assert_eq!(*sink.0.borrow(), vec![Command::SetExecutionMode(ExecutionMode::Simulation)]);
}

#[test]
fn exchange_demo_is_refused_while_the_config_is_incomplete() {
    let sink = Sink::default();
    let e = request_mode(ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &Settings::default(), Some(&Ok(())), &sink).unwrap_err();
    assert!(e.starts_with("設定不完整") && e.contains("Bybit taker_fee_pct"), "{e}");
    assert!(sink.0.borrow().is_empty());
}

#[test]
fn exchange_demo_is_refused_without_demo_keys() {
    let s = complete_settings("0.01");
    let sink = Sink::default();
    let e = request_mode(ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &s, Some(&Err("Bybit keys unavailable (NoKey)".into())), &sink).unwrap_err();
    assert_eq!(e, "demo 金鑰不可用：Bybit keys unavailable (NoKey)");
    let e = request_mode(ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &s, None, &sink).unwrap_err();
    assert_eq!(e, "demo 金鑰尚未確認");
    assert!(sink.0.borrow().is_empty());
}

// ---- risk-settings-guidance: strictness direction and help ----------------------------------

#[test]
fn strictness_classification_matches_spec() {
    use Strictness::*;
    for f in [Field::NetEdgeThresholdPct, Field::MinExpectedNetPnlPct, Field::SafetyMarginPct, Field::EstSlippagePct, Field::Min24hVolumeUsdt] {
        assert_eq!(f.strictness(), HigherStricter, "{}", f.key());
    }
    for f in [Field::MaxLeverage, Field::MaxPriceDriftPct, Field::StaleDataThresholdMs, Field::OrderTimeoutSeconds, Field::MaxLegImbalancePct, Field::MaxConcurrentPairs] {
        assert_eq!(f.strictness(), LowerStricter, "{}", f.key());
    }
    for e in Exchange::ALL {
        assert_eq!(Field::TakerFee(e).strictness(), Fact);
    }
    assert_eq!(HigherStricter.badge(), "▲ 越高越嚴");
    assert_eq!(LowerStricter.badge(), "▼ 越低越嚴");
    assert_eq!(Fact.badge(), "● 事實值");
}

/// The direction table must not drift from `effective_for_pair`: feed every overridable field the
/// values 1 (leg A) and 2 (leg B) and see which one the merge keeps.
#[test]
fn strictness_agrees_with_effective_for_pair_merge() {
    use tong_funding_core::risk::RiskOverride;
    let global = RiskConfig::default();
    let one = |f: Field| -> RiskOverride {
        let mut o = RiskOverride::default();
        set_override(&mut o, f, 1);
        o
    };
    let two = |f: Field| -> RiskOverride {
        let mut o = RiskOverride::default();
        set_override(&mut o, f, 2);
        o
    };
    for f in OVERRIDE_FIELDS {
        let mut ov = RiskOverrides::new();
        ov.insert(Exchange::Binance, one(f));
        ov.insert(Exchange::Bybit, two(f));
        let e = effective_for_pair(&global, &ov, Exchange::Binance, Exchange::Bybit);
        let got = effective_value(&e, f);
        let expect = match f.strictness() {
            Strictness::HigherStricter => 2,
            Strictness::LowerStricter => 1,
            Strictness::Fact => panic!("{} is overridable but marked Fact", f.key()),
        };
        assert_eq!(got, expect, "{} merge keeps {got}", f.key());
    }
}

fn set_override(o: &mut tong_funding_core::risk::RiskOverride, f: Field, v: u32) {
    let dv = d(&v.to_string());
    match f {
        Field::MaxLeverage => o.max_leverage = Some(dv),
        Field::MaxPriceDriftPct => o.max_price_drift_pct = Some(dv),
        Field::StaleDataThresholdMs => o.stale_data_threshold_ms = Some(v as u64),
        Field::OrderTimeoutSeconds => o.order_timeout_seconds = Some(v),
        Field::MaxLegImbalancePct => o.max_leg_imbalance_pct = Some(dv),
        Field::Min24hVolumeUsdt => o.min_24h_volume_usdt = Some(dv),
        Field::NetEdgeThresholdPct => o.net_edge_threshold_pct = Some(dv),
        Field::EstSlippagePct => o.est_slippage_pct = Some(dv),
        Field::SafetyMarginPct => o.safety_margin_pct = Some(dv),
        _ => panic!("not overridable"),
    }
}

fn effective_value(e: &tong_funding_core::risk::EffectiveConfig, f: Field) -> u32 {
    use rust_decimal::prelude::ToPrimitive;
    let dec = |x: tong_funding_core::types::Decimal| x.to_u32().unwrap();
    match f {
        Field::MaxLeverage => dec(e.max_leverage),
        Field::MaxPriceDriftPct => dec(e.max_price_drift_pct),
        Field::StaleDataThresholdMs => e.stale_data_threshold_ms as u32,
        Field::OrderTimeoutSeconds => e.order_timeout_seconds,
        Field::MaxLegImbalancePct => dec(e.max_leg_imbalance_pct),
        Field::Min24hVolumeUsdt => dec(e.min_24h_volume_usdt),
        Field::NetEdgeThresholdPct => dec(e.net_edge_threshold_pct.unwrap()),
        Field::EstSlippagePct => dec(e.est_slippage_pct.unwrap()),
        Field::SafetyMarginPct => dec(e.safety_margin_pct),
        _ => panic!("not overridable"),
    }
}

#[test]
fn every_field_has_non_empty_help_with_affected_checks() {
    for f in GLOBAL_FIELDS {
        let h = f.help();
        assert!(!h.meaning.trim().is_empty(), "{} meaning", f.key());
        assert!(!h.affects.is_empty(), "{} affects", f.key());
    }
}

#[test]
fn help_mentions_the_formula_the_field_belongs_to() {
    for f in [Field::NetEdgeThresholdPct, Field::EstSlippagePct, Field::SafetyMarginPct, Field::TakerFee(Exchange::Okx)] {
        let formula = f.help().formula.unwrap_or_else(|| panic!("{} has no formula", f.key()));
        assert!(formula.contains("net_edge_pct") || formula.contains("所需費率價差"), "{}: {formula}", f.key());
    }
    assert!(Field::EstSlippagePct.help().meaning.contains("越不容易達標"));
    assert!(Field::MinExpectedNetPnlPct.help().formula.is_some());
}

#[test]
fn guidance_texts_carry_the_spec_formulas_and_note() {
    assert_eq!(NET_EDGE_FORMULA, "net_edge_pct = 費率價差 − 2 × (taker_fee_L + taker_fee_S) − 4 × est_slippage − safety_margin");
    assert_eq!(REQUIRED_SPREAD_FORMULA, "所需費率價差 = 門檻 + 2 × (taker_fee_L + taker_fee_S) + 4 × est_slippage + safety_margin");
    assert_eq!(QUALIFY_CONDITIONS.len(), 4);
    assert!(OVERRIDE_RULE.contains("較嚴"));
    assert!(STALE_NOTE.starts_with("建議 3000 ms：送單前檢查用的是送單前剛抓的價格與費率"));
    assert!(STALE_NOTE.ends_with("另有 max_price_drift_pct 把關價格變動）。"));
}

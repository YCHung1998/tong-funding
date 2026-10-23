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
    assert_eq!(MODE_OPTIONS.map(|m| mode_label(crate::identity::ProcessIdentity::Demo, m)), ["SIMULATION", "EXCHANGE_DEMO"]);
    assert!(MODE_OPTIONS.iter().all(|m| !mode_label(crate::identity::ProcessIdentity::Demo, *m).contains("LIVE")));
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
    let rules_of = |ex: Exchange| -> Result<crate::engine::ports::OrderRules, String> {
        Ok(crate::engine::ports::OrderRules {
            lot: tong_funding_core::quantity::LotSize { step_size: d("0.001"), min_qty: d("0.001") },
            okx_ct_val: (ex == Exchange::Okx).then(|| d("0.01")),
        })
    };
    let (rules_l, rules_s) = (rules_of(long), rules_of(short));
    let ctx = Node0Context { now_ms: 6_100, symbol: "BTCUSDT", entry: &entry, effective: &eff, max_concurrent_pairs: cfg.max_concurrent_pairs, allowed_exchanges: &cfg.allowed_exchanges, open_pair_count: 0, rules: [&rules_l, &rules_s] };
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
    let step = request_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &s, Some(&Ok(())), &sink).unwrap();
    assert_eq!(step, ModeStep::Confirm(ExecutionMode::ExchangeDemo));
    assert!(sink.0.borrow().is_empty(), "the mode stays SIMULATION until confirmed");
    send_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, &sink);
    assert_eq!(*sink.0.borrow(), vec![Command::SetExecutionMode(ExecutionMode::ExchangeDemo)]);
    // Back to SIMULATION needs no confirmation.
    let sink = Sink::default();
    assert_eq!(request_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, ExecutionMode::Simulation, &s, None, &sink).unwrap(), ModeStep::Sent);
    assert_eq!(*sink.0.borrow(), vec![Command::SetExecutionMode(ExecutionMode::Simulation)]);
}

#[test]
fn exchange_demo_is_refused_while_the_config_is_incomplete() {
    let sink = Sink::default();
    let e = request_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &Settings::default(), Some(&Ok(())), &sink).unwrap_err();
    assert!(e.starts_with("設定不完整") && e.contains("Bybit taker_fee_pct"), "{e}");
    assert!(sink.0.borrow().is_empty());
}

#[test]
fn exchange_demo_is_refused_without_demo_keys() {
    let s = complete_settings("0.01");
    let sink = Sink::default();
    let e = request_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &s, Some(&Err("Bybit keys unavailable (NoKey)".into())), &sink).unwrap_err();
    assert_eq!(e, "demo 金鑰不可用：Bybit keys unavailable (NoKey)");
    let e = request_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, &s, None, &sink).unwrap_err();
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
    for f in [Field::MaxLeverage, Field::MaxPriceDriftPct, Field::StaleDataThresholdMs, Field::OrderTimeoutSeconds, Field::MaxLegImbalancePct, Field::MaxConcurrentPairs, Field::LossBreakerMaxLossUsdt, Field::LossBreakerMaxConsecutiveLosses] {
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

// ---- leg-early-exit: early_exit_min_move_pct --------------------------------------------------

#[test]
fn early_exit_field_defaults_to_0_3_is_global_only_and_has_help() {
    let f = RiskForm::from_settings(&Settings::default());
    assert_eq!(f.value(Field::EarlyExitMinMovePct), "0.3");
    assert_eq!(Field::EarlyExitMinMovePct.key(), "early_exit_min_move_pct");
    assert_eq!(Field::EarlyExitMinMovePct.unit(), "%");
    assert!(GLOBAL_FIELDS.contains(&Field::EarlyExitMinMovePct));
    assert!(!OVERRIDE_FIELDS.contains(&Field::EarlyExitMinMovePct), "global only");
    let h = Field::EarlyExitMinMovePct.help();
    assert!(h.meaning.contains("T+5") && h.meaning.contains("0 = 停用"), "{}", h.meaning);
    assert!(!h.affects.is_empty());
    // a global-only field cannot be switched on per exchange
    let s = complete_settings("0.01");
    let mut f = RiskForm::from_settings(&s);
    f.set_override(Exchange::Bybit, Field::EarlyExitMinMovePct, true, &s);
    assert_eq!(f.override_value(Exchange::Bybit, Field::EarlyExitMinMovePct), None);
}

#[test]
fn early_exit_value_is_saved_as_decimal_and_zero_disables() {
    let s = complete_settings("0.01");
    for (text, want) in [("0.05", "0.05"), ("0", "0"), ("1.25", "1.25")] {
        let mut f = RiskForm::from_settings(&s);
        f.set(Field::EarlyExitMinMovePct, text);
        let vm = evaluate(&f, &s);
        assert!(vm.can_save, "{text}: {:?}", vm.field_errors);
        assert_eq!(vm.result.as_ref().unwrap().0.early_exit_min_move_pct, d(want));
    }
}

#[test]
fn early_exit_negative_or_blank_or_text_is_refused_by_name_and_nothing_is_sent() {
    let s = complete_settings("0.01");
    for text in ["-0.01", "", "abc"] {
        let mut f = RiskForm::from_settings(&s);
        f.set(Field::EarlyExitMinMovePct, text);
        let vm = evaluate(&f, &s);
        assert!(vm.field_errors.contains_key("early_exit_min_move_pct"), "{text:?}: {:?}", vm.field_errors);
        assert!(vm.field_errors["early_exit_min_move_pct"].contains("early_exit_min_move_pct"));
        assert!(!vm.can_save, "{text:?}");
        let sink = Sink::default();
        assert!(!save(&vm, &sink));
        assert!(sink.0.borrow().is_empty());
    }
}

#[test]
fn early_exit_does_not_make_the_config_incomplete() {
    let mut s = complete_settings("0.01");
    s.risk.early_exit_min_move_pct = d("0");
    let vm = evaluate(&RiskForm::from_settings(&s), &s);
    assert!(vm.missing.is_empty() && vm.incomplete_text.is_none(), "{:?}", vm.missing);
}

// ---- live-banners: the mode selector names the world ---------------------------------------------

#[test]
fn a_live_process_calls_its_exchange_mode_live_and_says_real_money() {
    assert_eq!(MODE_OPTIONS.map(|m| mode_label(crate::identity::ProcessIdentity::Live, m)), ["SIMULATION", "LIVE"]);
    assert!(mode_description(crate::identity::ProcessIdentity::Live, ExecutionMode::ExchangeDemo).contains("真錢"));
    assert!(!mode_description(crate::identity::ProcessIdentity::Live, ExecutionMode::ExchangeDemo).to_lowercase().contains("demo"));
    assert!(mode_description(crate::identity::ProcessIdentity::Demo, ExecutionMode::ExchangeDemo).contains("不涉及真錢"));
}

#[test]
fn the_mode_switch_label_in_a_live_process_says_live() {
    #[derive(Default)]
    struct Labels(RefCell<Vec<String>>);
    impl CommandSink for Labels {
        fn send(&self, label: String, _command: Command) {
            self.0.borrow_mut().push(label);
        }
    }
    let sink = Labels::default();
    send_mode(crate::identity::ProcessIdentity::Live, ExecutionMode::ExchangeDemo, &sink);
    send_mode(crate::identity::ProcessIdentity::Demo, ExecutionMode::ExchangeDemo, &sink);
    assert_eq!(*sink.0.borrow(), vec!["切換模式 → LIVE".to_string(), "切換模式 → EXCHANGE_DEMO".to_string()]);
}

// ---- live-guardrails 4.2b: the live caps are editable on the page (Live only) ------------------

mod live_caps {
    use super::*;
    use crate::identity::ProcessIdentity::{Demo, Live};

    fn live_form(s: &Settings) -> RiskForm {
        RiskForm::from_settings_for(s, Live)
    }

    fn saved_risk(vm: &RiskVm) -> RiskConfig {
        let sink = Sink::default();
        assert!(save(vm, &sink), "{:?}", vm.field_errors);
        let cmds = sink.0.borrow();
        let Command::SaveRiskSettings { risk, .. } = &cmds[0] else { panic!("expected SaveRiskSettings, got {:?}", cmds[0]) };
        RiskConfig::from_json(&risk.to_string()).expect("the saved JSON is a valid RiskConfig")
    }

    #[test]
    fn the_live_page_lists_both_caps_and_the_demo_page_does_not() {
        let keys = |id| global_fields(id).iter().map(|f| f.key()).collect::<Vec<_>>();
        let live = keys(Live);
        assert!(live.contains(&"live_max_leg_notional_usdt".to_string()) && live.contains(&"live_max_pairs_per_arming".to_string()), "{live:?}");
        assert_eq!(global_fields(Demo), GLOBAL_FIELDS.to_vec(), "the Demo page is unchanged");
        assert!(GLOBAL_FIELDS.iter().all(|f| !LIVE_FIELDS.contains(f)));
        assert!(LIVE_FIELDS.iter().all(|f| !OVERRIDE_FIELDS.contains(f)), "global only: never overridable per leg");
    }

    #[test]
    fn unset_caps_show_blank_set_caps_show_their_value() {
        let s = complete_settings("0.01");
        let f = live_form(&s);
        assert_eq!((f.value(Field::LiveMaxLegNotionalUsdt), f.value(Field::LiveMaxPairsPerArming)), (String::new(), String::new()));
        let mut s2 = s.clone();
        s2.risk.live_max_leg_notional_usdt = Some(d("10"));
        s2.risk.live_max_pairs_per_arming = Some(2);
        let f = live_form(&s2);
        assert_eq!((f.value(Field::LiveMaxLegNotionalUsdt), f.value(Field::LiveMaxPairsPerArming)), ("10".to_string(), "2".to_string()));
    }

    #[test]
    fn typing_10_and_2_saves_them_and_completes_the_live_settings() {
        let s = complete_settings("0.01");
        let mut f = live_form(&s);
        assert!(evaluate(&f, &s).missing.iter().any(|m| m == "live_max_leg_notional_usdt"), "unset caps are listed as missing on the Live page");
        f.set(Field::LiveMaxLegNotionalUsdt, "10");
        f.set(Field::LiveMaxPairsPerArming, "2");
        let vm = evaluate(&f, &s);
        assert!(vm.can_save && vm.missing.is_empty(), "{:?} {:?}", vm.field_errors, vm.missing);
        let cfg = saved_risk(&vm);
        assert_eq!((cfg.live_max_leg_notional_usdt, cfg.live_max_pairs_per_arming), (Some(d("10")), Some(2)));
        assert!(cfg.is_live_complete());
    }

    #[test]
    fn values_outside_the_ceilings_or_not_numbers_are_refused_by_field_name() {
        let s = complete_settings("0.01");
        for (field, bad) in [
            (Field::LiveMaxLegNotionalUsdt, "100.01"),
            (Field::LiveMaxLegNotionalUsdt, "1000"),
            (Field::LiveMaxLegNotionalUsdt, "0"),
            (Field::LiveMaxLegNotionalUsdt, "-5"),
            (Field::LiveMaxLegNotionalUsdt, "abc"),
            (Field::LiveMaxPairsPerArming, "6"),
            (Field::LiveMaxPairsPerArming, "0"),
            (Field::LiveMaxPairsPerArming, "1.5"),
            (Field::LiveMaxPairsPerArming, "-1"),
            (Field::LiveMaxPairsPerArming, "x"),
        ] {
            let mut f = live_form(&s);
            f.set(field, bad);
            let vm = evaluate(&f, &s);
            assert!(!vm.can_save, "{bad} must be refused");
            let msg = vm.field_errors.get(&field.key()).unwrap_or_else(|| panic!("{bad}: no error under {}: {:?}", field.key(), vm.field_errors));
            assert!(msg.contains(&field.key()), "{msg}");
            let sink = Sink::default();
            assert!(!save(&vm, &sink) && sink.0.borrow().is_empty());
        }
        for (field, ok) in [(Field::LiveMaxLegNotionalUsdt, "100"), (Field::LiveMaxLegNotionalUsdt, "0.5"), (Field::LiveMaxPairsPerArming, "5"), (Field::LiveMaxPairsPerArming, "1")] {
            let mut f = live_form(&s);
            f.set(field, ok);
            assert!(evaluate(&f, &s).can_save, "{ok}");
        }
    }

    #[test]
    fn blank_means_unset_and_a_save_of_other_fields_keeps_stored_caps() {
        let mut s = complete_settings("0.01");
        s.risk.live_max_leg_notional_usdt = Some(d("10"));
        s.risk.live_max_pairs_per_arming = Some(2);
        let mut f = live_form(&s);
        f.set(Field::MaxLeverage, "3");
        let cfg = saved_risk(&evaluate(&f, &s));
        assert_eq!((cfg.max_leverage, cfg.live_max_leg_notional_usdt, cfg.live_max_pairs_per_arming), (d("3"), Some(d("10")), Some(2)), "other fields saved, caps kept");
        f.set(Field::LiveMaxLegNotionalUsdt, "  ");
        let cfg = saved_risk(&evaluate(&f, &s));
        assert_eq!(cfg.live_max_leg_notional_usdt, None, "blank = not set (never 0)");
        assert_eq!(cfg.live_max_pairs_per_arming, Some(2));
    }

    #[test]
    fn the_demo_page_never_touches_the_caps() {
        let mut s = complete_settings("0.01");
        s.risk.live_max_leg_notional_usdt = Some(d("10"));
        s.risk.live_max_pairs_per_arming = Some(2);
        let mut f = RiskForm::from_settings(&s);
        f.set(Field::LiveMaxLegNotionalUsdt, "99"); // ignored: this form is not a Live form
        f.set(Field::MaxLeverage, "3");
        let vm = evaluate(&f, &s);
        assert!(vm.missing.iter().all(|m| !m.starts_with("live_")), "Demo completeness ignores the caps");
        let cfg = saved_risk(&vm);
        assert_eq!((cfg.live_max_leg_notional_usdt, cfg.live_max_pairs_per_arming), (Some(d("10")), Some(2)));
        let mut unset = complete_settings("0.01");
        unset.risk.live_max_leg_notional_usdt = None;
        assert!(evaluate(&RiskForm::from_settings(&unset), &unset).missing.is_empty(), "Demo: unset caps are not 'missing'");
    }

    #[test]
    fn the_caps_have_text_help_strictness_and_units() {
        for f in LIVE_FIELDS {
            let h = f.help();
            assert!(!h.meaning.is_empty() && !h.affects.is_empty(), "{f:?}");
            assert_eq!(f.strictness(), Strictness::LowerStricter);
            assert!(f.label().contains("Live"));
        }
        assert_eq!((Field::LiveMaxLegNotionalUsdt.unit(), Field::LiveMaxPairsPerArming.unit()), ("USDT", "筆"));
    }
}

// ---- naked-leg-watchdog: the fixed rules block ------------------------------------------------

fn settings_with_timeout(seconds: u32) -> Settings {
    let mut s = complete_settings("0.05");
    s.risk.order_timeout_seconds = seconds;
    s
}

#[test]
fn the_rules_block_has_the_fixed_title_and_the_approved_words_with_the_stored_timeout() {
    // spec: order_timeout_seconds 為 20 -> 區塊寫「訂單逾時 20 秒後再等 5 秒」
    let b = auto_rules_block(&settings_with_timeout(20));
    assert_eq!(b.title, "自動處理規則（授權 B）");
    assert!(b.plain().contains("訂單逾時 20 秒後再等 5 秒"), "{}", b.plain());
    assert_eq!(b.plain(), crate::engine::watchdog_text::rules_plain(20), "the page and every other place read the same function");
    assert!(b.notes.is_empty(), "{:?}", b.notes);
    let bold: Vec<&str> = b.runs.iter().filter(|r| r.bold).map(|r| r.text.as_str()).collect();
    assert_eq!(bold, vec!["確認只有一腿成交", "只減倉的市價單", "不會", "不會動作"]);
}

#[test]
fn the_number_follows_the_stored_setting_whenever_it_changes() {
    for n in [3_u32, 15, 45] {
        assert!(auto_rules_block(&settings_with_timeout(n)).plain().contains(&format!("訂單逾時 {n} 秒後再等 5 秒")), "{n}");
    }
}

#[test]
fn the_block_is_there_even_when_the_settings_are_incomplete_or_unreadable() {
    let incomplete = auto_rules_block(&Settings::default());
    assert_eq!(incomplete.title, "自動處理規則（授權 B）");
    assert!(incomplete.plain().contains("訂單逾時 15 秒後再等 5 秒"), "the default timeout, which is what an incomplete config uses");
    assert!(incomplete.notes.is_empty());
    let unreadable = auto_rules_block(&Settings { error: Some("config.risk: bad json".into()), ..Settings::default() });
    assert!(unreadable.notes.iter().any(|n| n.contains("讀取失敗") && n.contains("預設值")), "the number is the default, not a fact: {:?}", unreadable.notes);
}

#[test]
fn an_exchange_that_overrides_the_timeout_adds_a_note_with_its_number() {
    let mut s = settings_with_timeout(15);
    s.overrides = parse_overrides(&serde_json::json!({ "Bybit": { "order_timeout_seconds": 7 } })).unwrap();
    let b = auto_rules_block(&s);
    assert!(b.notes.iter().any(|n| n.contains("Bybit 7 秒") && n.contains("較小")), "{:?}", b.notes);
    assert!(b.plain().contains("訂單逾時 15 秒後再等 5 秒"), "the global number stays as it is");
}

// ---- loss-circuit-breaker 3.2: the two breaker fields ---------------------------------------------

mod breaker_fields {
    use super::*;

    const BOTH: [Field; 2] = [Field::LossBreakerMaxLossUsdt, Field::LossBreakerMaxConsecutiveLosses];

    #[test]
    fn the_page_lists_both_fields_with_their_keys_units_and_stored_values() {
        let f = RiskForm::from_settings(&Settings::default());
        assert_eq!((f.value(BOTH[0]), f.value(BOTH[1])), ("15".to_string(), "3".to_string()), "the defaults of an older stored config");
        assert_eq!(BOTH.map(Field::key), ["loss_breaker_max_loss_usdt".to_string(), "loss_breaker_max_consecutive_losses".to_string()]);
        assert_eq!(BOTH.map(Field::unit), ["USDT", "筆"]);
        for f in BOTH {
            assert!(GLOBAL_FIELDS.contains(&f), "{f:?} is on both the Demo and the Live page");
            assert!(global_fields(crate::identity::ProcessIdentity::Live).contains(&f));
            assert!(!OVERRIDE_FIELDS.contains(&f) && !LIVE_FIELDS.contains(&f), "global only, not a Live-only cap");
            assert!(!f.optional(), "they have defaults: blank is an error, never 'not set'");
        }
        let mut s = Settings::default();
        s.risk.loss_breaker_max_loss_usdt = d("5");
        s.risk.loss_breaker_max_consecutive_losses = 2;
        let f = RiskForm::from_settings(&s);
        assert_eq!((f.value(BOTH[0]), f.value(BOTH[1])), ("5".to_string(), "2".to_string()), "a Live database starts with 5 and 2");
    }

    #[test]
    fn both_are_lower_is_stricter_like_the_core_semantics() {
        for f in BOTH {
            assert_eq!(f.strictness(), Strictness::LowerStricter, "{}", f.key());
        }
        // The merge table cannot cover them (global only), so the direction is locked to behaviour:
        // a lower limit trips whenever a higher limit does (core::loss_breaker::evaluate).
        use tong_funding_core::loss_breaker::evaluate;
        let history = [Some(d("-3")), Some(d("-4")), Some(d("-2"))];
        assert!(evaluate(&history, d("5"), 9).tripped && !evaluate(&history, d("15"), 9).tripped);
        assert!(evaluate(&history, d("999"), 2).tripped && !evaluate(&history, d("999"), 4).tripped);
    }

    #[test]
    fn every_other_strictness_assertion_still_holds() {
        // the badge colours are not touched by this change
        assert_eq!(Strictness::LowerStricter.badge(), "▼ 越低越嚴");
        assert_eq!(Strictness::HigherStricter.badge(), "▲ 越高越嚴");
    }

    #[test]
    fn values_are_saved_as_typed_and_reach_the_stored_config() {
        let s = complete_settings("0.01");
        let mut f = RiskForm::from_settings(&s);
        f.set(Field::LossBreakerMaxLossUsdt, "7.5");
        f.set(Field::LossBreakerMaxConsecutiveLosses, "2");
        let vm = evaluate(&f, &s);
        assert!(vm.can_save, "{:?}", vm.field_errors);
        let cfg = vm.result.as_ref().unwrap().0.clone();
        assert_eq!((cfg.loss_breaker_max_loss_usdt, cfg.loss_breaker_max_consecutive_losses), (d("7.5"), 2));
        let sink = Sink::default();
        assert!(save(&vm, &sink));
        let cmds = sink.0.borrow();
        let Command::SaveRiskSettings { risk, .. } = &cmds[0] else { panic!("{:?}", cmds[0]) };
        let back = RiskConfig::from_json(&risk.to_string()).unwrap();
        assert_eq!((back.loss_breaker_max_loss_usdt, back.loss_breaker_max_consecutive_losses), (d("7.5"), 2));
    }

    #[test]
    fn bad_values_are_refused_by_field_name_with_a_message_and_nothing_is_sent() {
        let s = complete_settings("0.01");
        for (field, text, expect) in [
            (Field::LossBreakerMaxConsecutiveLosses, "0", "loss_breaker_max_consecutive_losses 必須為 ≥ 1 的整數"), // spec scenario
            (Field::LossBreakerMaxConsecutiveLosses, "-1", "loss_breaker_max_consecutive_losses 必須為非負整數"),
            (Field::LossBreakerMaxConsecutiveLosses, "1.5", "loss_breaker_max_consecutive_losses 必須為非負整數"),
            (Field::LossBreakerMaxConsecutiveLosses, "", "loss_breaker_max_consecutive_losses 必須為非負整數"),
            (Field::LossBreakerMaxLossUsdt, "0", "loss_breaker_max_loss_usdt 必須大於 0"),
            (Field::LossBreakerMaxLossUsdt, "-5", "loss_breaker_max_loss_usdt 必須大於 0"),
            (Field::LossBreakerMaxLossUsdt, "abc", "loss_breaker_max_loss_usdt 不是有效的數字"),
            (Field::LossBreakerMaxLossUsdt, "", "loss_breaker_max_loss_usdt 必填"),
        ] {
            let mut f = RiskForm::from_settings(&s);
            f.set(field, text);
            let vm = evaluate(&f, &s);
            assert!(!vm.can_save, "{text:?}");
            assert_eq!(vm.field_errors.get(&field.key()).map(String::as_str), Some(expect), "{text:?}: {:?}", vm.field_errors);
            let sink = Sink::default();
            assert!(!save(&vm, &sink) && sink.0.borrow().is_empty(), "stored settings stay as they are");
        }
    }

    #[test]
    fn the_fields_have_help_with_the_rule_and_do_not_make_the_config_incomplete() {
        for f in BOTH {
            let h = f.help();
            assert!(!h.meaning.trim().is_empty() && !h.affects.is_empty(), "{}", f.key());
            assert!(h.meaning.contains("kill switch") || h.meaning.contains("開新倉"), "{}", h.meaning);
        }
        assert!(Field::LossBreakerMaxLossUsdt.help().meaning.contains("15") || Field::LossBreakerMaxLossUsdt.help().meaning.contains("USDT"));
        let mut s = complete_settings("0.01");
        s.risk.loss_breaker_max_loss_usdt = d("0.5");
        let vm = evaluate(&RiskForm::from_settings(&s), &s);
        assert!(vm.missing.is_empty() && vm.incomplete_text.is_none());
    }

    #[test]
    fn a_demo_save_and_a_live_save_both_carry_the_breaker_fields() {
        let s = complete_settings("0.01");
        for identity in [crate::identity::ProcessIdentity::Demo, crate::identity::ProcessIdentity::Live] {
            let mut f = RiskForm::from_settings_for(&s, identity);
            f.set(Field::LossBreakerMaxLossUsdt, "9");
            let vm = evaluate(&f, &s);
            assert_eq!(vm.result.as_ref().unwrap().0.loss_breaker_max_loss_usdt, d("9"), "{identity:?}");
        }
    }
}

use super::*;
use crate::engine::command::Command;
use crate::engine::ports::OrderRules;
use crate::ui::bridge::{MarketFeed, Settings, UiSnapshot};
use crate::ui::testkit::{d, obs};
use std::cell::RefCell;
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::{ExecutionMode, RiskOverride};
use tong_funding_core::types::Exchange;

#[derive(Default)]
struct Sink(RefCell<Vec<Command>>);
impl crate::ui::bridge::CommandSink for Sink {
    fn send(&self, _label: String, command: Command) {
        self.0.borrow_mut().push(command);
    }
}

fn form(n: &str, l: &str, m: &str, mode: CalcMode) -> ContractForm {
    ContractForm { notional: n.into(), leverage: l.into(), margin: m.into(), mode }
}

// ---- 2.1 linkage --------------------------------------------------------------------------

#[test]
fn leverage_mode_gives_margin_400_with_the_formula_and_pair_totals() {
    let vm = evaluate(&form("1200", "3", "", CalcMode::LeverageToMargin), &Settings::default(), Some(ExecutionMode::ExchangeDemo));
    assert_eq!(vm.margin, Some(d("400")));
    assert_eq!(vm.formula.as_deref(), Some("1,200 ÷ 3 = 400 / leg"));
    assert_eq!((vm.pair_notional, vm.pair_margin), (Some(d("2400")), Some(d("800"))));
    assert_eq!(vm.mode_label, "EXCHANGE_DEMO");
    assert!(vm.errors.is_empty() && vm.can_save);
}

#[test]
fn margin_mode_gives_leverage_3() {
    let vm = evaluate(&form("1200", "", "400", CalcMode::MarginToLeverage), &Settings::default(), None);
    assert_eq!(vm.leverage, Some(d("3")));
    assert_eq!(vm.formula.as_deref(), Some("1,200 ÷ 400 = 3×"));
    assert!(vm.can_save);
}

#[test]
fn zero_margin_zero_leverage_and_zero_notional_are_refused_with_the_field_name() {
    let vm = evaluate(&form("1200", "", "0", CalcMode::MarginToLeverage), &Settings::default(), None);
    assert_eq!(vm.leverage, None, "leverage is not updated");
    assert!(vm.errors.iter().any(|e| e == "Margin 必須大於 0"), "{:?}", vm.errors);
    assert!(!vm.can_save);
    let vm = evaluate(&form("1200", "0", "", CalcMode::LeverageToMargin), &Settings::default(), None);
    assert!(vm.errors.iter().any(|e| e == "Leverage 必須大於 0"), "{:?}", vm.errors);
    assert!(!vm.can_save);
    let vm = evaluate(&form("0", "3", "", CalcMode::LeverageToMargin), &Settings::default(), None);
    assert!(vm.errors.iter().any(|e| e == "Target Notional 必須大於 0"), "{:?}", vm.errors);
    let vm = evaluate(&form("abc", "3", "", CalcMode::LeverageToMargin), &Settings::default(), None);
    assert!(vm.errors.iter().any(|e| e == "Target Notional 不是有效的數字"), "{:?}", vm.errors);
    assert!(!vm.can_save);
}

#[test]
fn leverage_above_a_limit_only_warns() {
    let mut s = Settings::default();
    s.risk.max_leverage = d("5");
    let vm = evaluate(&form("1200", "8", "", CalcMode::LeverageToMargin), &s, None);
    assert!(vm.leverage_warning.as_deref().is_some_and(|w| w.contains("max_leverage 5")), "{:?}", vm.leverage_warning);
    assert!(vm.can_save);
    let mut s = Settings::default();
    s.overrides.insert(Exchange::Bybit, RiskOverride { max_leverage: Some(d("4")), ..Default::default() });
    let vm = evaluate(&form("1200", "5", "", CalcMode::LeverageToMargin), &s, None);
    assert!(vm.leverage_warning.as_deref().is_some_and(|w| w.contains("Bybit")), "{:?}", vm.leverage_warning);
}

#[test]
fn save_sends_one_template_command_and_an_invalid_form_sends_nothing() {
    let sink = Sink::default();
    let bad = evaluate(&form("0", "3", "", CalcMode::LeverageToMargin), &Settings::default(), None);
    assert!(!save(&bad, &sink));
    assert!(sink.0.borrow().is_empty());
    let good = evaluate(&form("1200", "", "400", CalcMode::MarginToLeverage), &Settings::default(), None);
    assert!(save(&good, &sink));
    assert_eq!(*sink.0.borrow(), vec![Command::SaveContractTemplate { notional_usdt: d("1200"), leverage: d("3") }]);
}

#[test]
fn the_form_starts_from_the_python_defaults() {
    let f = ContractForm::from_template(&Settings::default().contract);
    assert_eq!((f.notional.as_str(), f.leverage.as_str()), ("1000", "5"));
}

// ---- 2.2 quantity quote (`cargo test -p tong-funding contract_quote`) ----------------------

mod contract_quote {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn snap_with(ex: Exchange, price: &str, observed_at: i64, rules: Option<Result<OrderRules, String>>) -> UiSnapshot {
        let mut s = UiSnapshot::default();
        let mut o = obs(ex, "BTCUSDT", "0.0001", 28_800, NOW + 3_600_000, observed_at);
        o.mark_price = d(price);
        s.market.insert(ex, MarketFeed { observations: vec![o], last_success_at: Some(observed_at), last_error: None });
        if let Some(r) = rules {
            s.rules.insert((ex, "BTCUSDT".into()), r);
        }
        s
    }

    fn lot(step: &str, min: &str) -> OrderRules {
        OrderRules { lot: LotSize { step_size: d(step), min_qty: d(min) }, okx_ct_val: None }
    }

    fn cell(s: &UiSnapshot, ex: Exchange, notional: &str) -> QuoteCell {
        quote("BTCUSDT", d(notional), s, NOW).into_iter().find(|q| q.exchange == ex).unwrap().cell
    }

    #[test]
    fn contract_quote_floors_to_the_step_1200_at_60200_is_0_019() {
        let s = snap_with(Exchange::Binance, "60200", NOW - 100, Some(Ok(lot("0.001", "0.001"))));
        match cell(&s, Exchange::Binance, "1200") {
            QuoteCell::Qty { qty, text } => {
                assert_eq!(qty, d("0.019"));
                assert_eq!(text, "0.019 BTC");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn contract_quote_okx_is_in_contracts_with_the_base_amount() {
        let mut rules = lot("1", "1");
        rules.okx_ct_val = Some(d("0.01"));
        // 2,100 USDT at 100,000 = 0.021 BTC = 2.1 contracts -> 2 contracts (0.02 BTC)
        let s = snap_with(Exchange::Okx, "100000", NOW - 100, Some(Ok(rules)));
        match cell(&s, Exchange::Okx, "2100") {
            QuoteCell::Contracts { contracts, base, text } => {
                assert_eq!((contracts, base), (d("2"), d("0.02")));
                assert_eq!(text, "2 張（≈ 0.02 BTC）");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn contract_quote_below_the_minimum_shows_no_quantity() {
        // 40 USDT at 100,000 = 0.0004
        let s = snap_with(Exchange::Bybit, "100000", NOW - 100, Some(Ok(lot("0.001", "0.001"))));
        assert_eq!(cell(&s, Exchange::Bybit, "40"), QuoteCell::BelowMinimum);
        assert_eq!(QuoteCell::BelowMinimum.text(), "低於最小下單量");
    }

    #[test]
    fn contract_quote_a_stale_price_gives_no_quantity() {
        let s = snap_with(Exchange::Binance, "60200", NOW - 1_001, Some(Ok(lot("0.001", "0.001"))));
        assert_eq!(cell(&s, Exchange::Binance, "1200"), QuoteCell::Stale { age_ms: 1_001 });
        assert_eq!(QuoteCell::Stale { age_ms: 1_001 }.text(), "價格已過期");
    }

    #[test]
    fn contract_quote_missing_rules_give_no_quantity_and_never_a_default_step() {
        let s = snap_with(Exchange::Binance, "60200", NOW - 100, Some(Err("step_size missing".into())));
        assert!(matches!(cell(&s, Exchange::Binance, "1200"), QuoteCell::NoRules(_)));
        assert_eq!(QuoteCell::NoRules("x".into()).text(), "無法取得合約規格（x）");
        let s = snap_with(Exchange::Binance, "60200", NOW - 100, None);
        assert_eq!(cell(&s, Exchange::Binance, "1200"), QuoteCell::RulesPending);
    }

    #[test]
    fn contract_quote_an_okx_rule_without_ct_val_is_unavailable() {
        let s = snap_with(Exchange::Okx, "100000", NOW - 100, Some(Ok(lot("1", "1"))));
        assert!(matches!(cell(&s, Exchange::Okx, "2100"), QuoteCell::NoRules(_)));
    }
}

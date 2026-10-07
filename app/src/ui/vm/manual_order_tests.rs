use super::*;
use crate::engine::command::{Blocker, Command, ManualOrder};
use crate::engine::ports::{OrderRules, OrderSide};
use crate::store::event_query::StoredEvent;
use crate::ui::bridge::{CommandSink, EngineState, MarketFeed, UiSnapshot};
use crate::ui::testkit::{complete_settings, d, obs};
use serde_json::json;
use std::cell::RefCell;
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

const NOW: i64 = 1_800_000_000_000;

#[derive(Default)]
struct Sink(RefCell<Vec<Command>>);
impl CommandSink for Sink {
    fn send(&self, _label: String, command: Command) {
        self.0.borrow_mut().push(command);
    }
}

fn snap(mode: ExecutionMode) -> UiSnapshot {
    let mut s = UiSnapshot { settings: complete_settings("0.01"), ..UiSnapshot::default() };
    for ex in [Exchange::Binance, Exchange::Bybit] {
        let mut o = obs(ex, "BTCUSDT", "0.0001", 28_800, NOW + 3_600_000, NOW - 100);
        o.mark_price = d("60000");
        s.market.insert(ex, MarketFeed { observations: vec![o], last_success_at: Some(NOW - 100), last_error: None });
        s.rules.insert((ex, "BTCUSDT".into()), Ok(OrderRules { lot: LotSize { step_size: d("0.001"), min_qty: d("0.001") }, okx_ct_val: None }));
    }
    // symbol-leverage-cap: fresh caps (read for a large notional) so EXCHANGE_DEMO can select.
    put_caps(&mut s, "BTCUSDT", "20", "20");
    s.engine = Some(EngineState { now_ms: NOW, trigger_mode: TriggerMode::Manual, execution_mode: mode, pairs: vec![], blockers: vec![], notices: vec![], alerts: vec![] });
    s
}

fn put_caps(s: &mut UiSnapshot, sym: &str, bin: &str, byb: &str) {
    for (ex, c) in [(Exchange::Binance, bin), (Exchange::Bybit, byb)] {
        s.leverage_caps.insert((ex, sym.into()), crate::ui::bridge::CapReading { notional: d("100000"), cap: Ok(d(c)), fetched_at: NOW - 1_000 });
    }
}

fn form(qty: &str) -> ManualForm {
    ManualForm { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), side: OrderSide::Buy, quantity: qty.into(), reduce_only: false, leverage: "5".into() }
}

#[test]
fn only_binance_and_bybit_panels_and_a_disallowed_exchange_is_disabled() {
    let mut s = snap(ExecutionMode::ExchangeDemo);
    s.settings.risk.allowed_exchanges = vec![Exchange::Binance];
    let vm = build(&form("0.001"), &CancelForm::default(), &s);
    assert_eq!(vm.panels.iter().map(|p| p.0).collect::<Vec<_>>(), vec![Exchange::Binance, Exchange::Bybit]);
    assert_eq!(vm.panels[1].1, Err("此交易所未在 allowed_exchanges 中".to_string()));
    let mut f = form("0.001");
    f.exchange = Exchange::Bybit;
    let vm = build(&f, &CancelForm::default(), &s);
    assert!(vm.submit_disabled.contains(&"此交易所未在 allowed_exchanges 中".to_string()));
}

#[test]
fn the_environment_text_is_truthful_in_both_modes() {
    let demo = build(&form("0.001"), &CancelForm::default(), &snap(ExecutionMode::ExchangeDemo));
    assert_eq!(demo.env_text, "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單");
    assert!(!demo.env_text.contains("無外部請求"));
    let sim = build(&form("0.001"), &CancelForm::default(), &snap(ExecutionMode::Simulation));
    assert_eq!(sim.env_text, "SIMULATION：由模擬器成交，不會送到交易所");
}

#[test]
fn quantity_is_floored_and_shown_in_the_confirmation() {
    let vm = build(&form("0.0014"), &CancelForm::default(), &snap(ExecutionMode::ExchangeDemo));
    assert_eq!(vm.rounded, Ok(d("0.001")));
    let c = open_confirm(&vm, &form("0.0014")).unwrap();
    assert_eq!(c.qty_text, "0.001 BTC");
    assert_eq!(c.est_notional, Some(d("60")));
    assert_eq!(c.warning, "單腿下單不會自動建立對腿");
}

#[test]
fn below_the_minimum_sends_nothing() {
    let sink = Sink::default();
    let vm = build(&form("0.0004"), &CancelForm::default(), &snap(ExecutionMode::ExchangeDemo));
    assert_eq!(vm.rounded, Err("低於最小下單量".to_string()));
    assert!(open_confirm(&vm, &form("0.0004")).is_none());
    assert!(sink.0.borrow().is_empty());
}

#[test]
fn nothing_is_sent_before_confirming_and_exactly_one_command_after_in_exchange_demo() {
    let sink = Sink::default();
    let vm = build(&form("0.0014"), &CancelForm::default(), &snap(ExecutionMode::ExchangeDemo));
    let c = open_confirm(&vm, &form("0.0014")).unwrap();
    assert!(sink.0.borrow().is_empty());
    confirm(&c, &sink);
    assert_eq!(
        *sink.0.borrow(),
        vec![Command::ManualOrder(ManualOrder { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), side: OrderSide::Buy, quantity: d("0.001"), reduce_only: false, leverage: Some(d("5")) })]
    );
}

/// Engine spec (merged engine-simulation): SIMULATION manual orders go to the simulator, so the
/// page sends the same single command (it is the engine that picks the executor).
#[test]
fn simulation_sends_the_same_single_command_to_the_engine() {
    let sink = Sink::default();
    let vm = build(&form("0.001"), &CancelForm::default(), &snap(ExecutionMode::Simulation));
    assert!(vm.submit_disabled.is_empty(), "{:?}", vm.submit_disabled);
    confirm(&open_confirm(&vm, &form("0.001")).unwrap(), &sink);
    assert_eq!(sink.0.borrow().len(), 1);
}

#[test]
fn the_kill_switch_disables_only_orders_that_are_not_reduce_only() {
    let mut s = snap(ExecutionMode::ExchangeDemo);
    s.engine.as_mut().unwrap().blockers = vec![Blocker::KillSwitch];
    let vm = build(&form("0.001"), &CancelForm::default(), &s);
    assert_eq!(vm.submit_disabled, vec!["緊急停止中".to_string()]);
    assert!(open_confirm(&vm, &form("0.001")).is_none());
    let mut f = form("0.001");
    f.reduce_only = true;
    let vm = build(&f, &CancelForm::default(), &s);
    assert!(vm.submit_disabled.is_empty());
}

#[test]
fn a_mode_change_updates_the_page_immediately() {
    let f = form("0.001");
    let a = build(&f, &CancelForm::default(), &snap(ExecutionMode::ExchangeDemo));
    let b = build(&f, &CancelForm::default(), &snap(ExecutionMode::Simulation));
    assert_ne!(a.env_text, b.env_text);
    assert_eq!(open_confirm(&b, &f).unwrap().mode, ExecutionMode::Simulation);
}

#[test]
fn cancel_needs_an_order_id_and_goes_through_the_engine() {
    let s = snap(ExecutionMode::ExchangeDemo);
    let sink = Sink::default();
    let empty = CancelForm { exchange: Exchange::Bybit, symbol: "BTCUSDT".into(), order_id: " ".into() };
    let vm = build(&form("0.001"), &empty, &s);
    assert_eq!(vm.cancel_disabled, vec!["Order ID 為空".to_string()]);
    assert!(!cancel(&vm, &empty, &sink));
    let c = CancelForm { order_id: "demoso00010000000000000".into(), ..empty };
    let vm = build(&form("0.001"), &c, &s);
    assert!(cancel(&vm, &c, &sink));
    assert_eq!(*sink.0.borrow(), vec![Command::ManualCancel { exchange: Exchange::Bybit, symbol: "BTCUSDT".into(), client_order_id: "demoso00010000000000000".into() }]);
}

#[test]
fn results_are_shown_as_reported_and_a_failed_cancel_is_never_success() {
    let mut s = snap(ExecutionMode::ExchangeDemo);
    let e = |id: i64, ty: &str, p: serde_json::Value| StoredEvent { id, ts_ms: NOW - id, event_type: ty.into(), pair_id: None, payload: p.to_string(), imported: false };
    s.trade_events = vec![
        e(1, "MANUAL_CANCEL_RESULT", json!({"result": "found", "state": "Filled", "client_order_id": "x1", "exchange": "Bybit", "simulated": false})),
        e(2, "MANUAL_ORDER_RESULT", json!({"outcome": "rejected", "reason": "-2019 Margin is insufficient", "client_order_id": "x2", "simulated": false})),
        e(3, "MANUAL_ORDER_RESULT", json!({"outcome": "accepted", "exchange_order_id": "8389765", "client_order_id": "x3", "simulated": false})),
        e(4, "MANUAL_ORDER_RESULT", json!({"outcome": "accepted", "exchange_order_id": "sim-1", "client_order_id": "simlo1", "simulated": true})),
    ];
    let vm = build(&form("0.001"), &CancelForm::default(), &s);
    let texts: Vec<&str> = vm.results.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts[0], "撤單：訂單已是 Filled，未撤銷（x1）");
    assert_eq!(texts[1], "下單失敗：-2019 Margin is insufficient（x2）");
    assert_eq!(texts[2], "下單成功：order id 8389765（x3）");
    assert_eq!(texts[3], "[模擬] 下單成功：order id sim-1（simlo1）");
    assert!(!vm.results[0].ok);
}

/// Design D2 / task 4.1: no UI file holds or names an exchange client; only the composition root
/// (`live.rs`, `wiring.rs`) builds them and hands them to the engine.
#[test]
fn ui_pages_hold_no_exchange_client() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
    let banned = ["SignedClient", "OrderClient", "Executor", "ReqwestTransport", "Adapter::", "execution::", "exchange::public", "exchange::signed::binance", "exchange::signed::bybit"];
    // `scanner_refresh.rs` drives the read-only public market adapters (batch snapshots, no order
    // capability) for the existing data source; it is called only from `live.rs`.
    let allowed_files = ["live.rs", "wiring.rs", "bench.rs", "scanner_refresh.rs"];
    let mut stack = vec![dir];
    let mut hits = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") || allowed_files.contains(&name.as_str()) {
                continue;
            }
            for (n, line) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
                if banned.iter().any(|b| line.contains(b)) {
                    hits.push(format!("{}:{}: {line}", p.display(), n + 1));
                }
            }
        }
    }
    assert!(hits.is_empty(), "{}", hits.join("\n"));
}

// ---- manual-order-position-picker -------------------------------------------------------------

mod picker {
    use super::*;
    use crate::engine::ports::{AccountOrder, AccountPosition, Listed};
    use crate::ui::bridge::LegAccount;

    fn acct(positions: Result<Listed<AccountPosition>, String>, orders: Result<Listed<AccountOrder>, String>) -> LegAccount {
        LegAccount { positions, open_orders: orders, available_margin: Ok(d("1")), fetched_at: NOW - 5 }
    }
    fn pos(ex: Exchange, sym: &str, q: &str) -> AccountPosition {
        AccountPosition { exchange: ex, symbol: sym.into(), quantity: d(q) }
    }
    fn ord(ex: Exchange, sym: &str, id: Option<&str>, q: &str) -> AccountOrder {
        AccountOrder { exchange: ex, symbol: sym.into(), client_order_id: id.map(String::from), remaining_quantity: d(q) }
    }
    fn ok<T>(items: Vec<T>) -> Result<Listed<T>, String> {
        Ok(Listed { items, complete: true })
    }
    fn rows(l: &PickList<PositionRow>) -> &Vec<PositionRow> {
        match &l.state {
            PickState::Rows { rows, .. } => rows,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn positions_use_the_account_of_the_execution_mode_and_drop_zero_rows() {
        let mut s = snap(ExecutionMode::ExchangeDemo);
        s.leg_accounts.insert((true, Exchange::Binance), acct(ok(vec![pos(Exchange::Binance, "SIMUSDT", "1")]), ok(vec![])));
        s.leg_accounts.insert((false, Exchange::Binance), acct(ok(vec![pos(Exchange::Binance, "BTCUSDT", "0.5"), pos(Exchange::Binance, "ZEROUSDT", "0")]), ok(vec![])));
        let l = open_positions(&s);
        assert_eq!(l.iter().map(|x| x.exchange).collect::<Vec<_>>(), vec![Exchange::Binance, Exchange::Bybit]);
        let r = rows(&l[0]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].symbol, "BTCUSDT");
        assert!(r[0].is_long);
        // Bybit has no demo entry yet: reported as not queried, not as "no positions".
        assert_eq!(l[1].state, PickState::NotQueried);
        s.engine.as_mut().unwrap().execution_mode = ExecutionMode::Simulation;
        assert_eq!(rows(&open_positions(&s)[0])[0].symbol, "SIMUSDT");
    }

    #[test]
    fn a_failed_read_shows_the_error_and_no_rows_and_incomplete_is_flagged() {
        let mut s = snap(ExecutionMode::Simulation);
        s.leg_accounts.insert((true, Exchange::Binance), acct(Err("HTTP 503".into()), ok(vec![])));
        s.leg_accounts.insert((true, Exchange::Bybit), acct(Ok(Listed { items: vec![pos(Exchange::Bybit, "ETHUSDT", "-0.5")], complete: false }), ok(vec![])));
        let l = open_positions(&s);
        assert_eq!(l[0].state, PickState::Failed("HTTP 503".into()));
        match &l[1].state {
            PickState::Rows { rows, incomplete } => {
                assert!(*incomplete);
                assert!(!rows[0].is_long);
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn an_empty_complete_account_yields_empty_rows() {
        let mut s = snap(ExecutionMode::Simulation);
        s.leg_accounts.insert((true, Exchange::Binance), acct(ok(vec![]), ok(vec![])));
        assert!(rows(&open_positions(&s)[0]).is_empty());
    }

    #[test]
    fn close_prefill_takes_the_opposite_side_the_absolute_quantity_and_reduce_only() {
        let long = close_prefill(&pos(Exchange::Binance, "BTCUSDT", "0.50"));
        assert_eq!(long, ManualPrefill { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), side: OrderSide::Sell, quantity: "0.5".into(), reduce_only: true });
        let short = close_prefill(&pos(Exchange::Bybit, "ETHUSDT", "-0.5"));
        assert_eq!(short, ManualPrefill { exchange: Exchange::Bybit, symbol: "ETHUSDT".into(), side: OrderSide::Buy, quantity: "0.5".into(), reduce_only: true });
    }

    #[test]
    fn a_prefilled_close_form_sends_nothing_and_goes_through_the_confirmation() {
        let sink = Sink::default();
        let s = snap(ExecutionMode::Simulation);
        let p = close_prefill(&pos(Exchange::Bybit, "BTCUSDT", "-0.0014"));
        let f = ManualForm { exchange: p.exchange, symbol: p.symbol, side: p.side, quantity: p.quantity, reduce_only: p.reduce_only, leverage: "5".into() };
        let vm = build(&f, &CancelForm::default(), &s);
        assert!(sink.0.borrow().is_empty());
        let c = open_confirm(&vm, &f).unwrap();
        assert_eq!(c.qty_text, "0.001 BTC");
        confirm(&c, &sink);
        assert_eq!(*sink.0.borrow(), vec![Command::ManualOrder(ManualOrder { exchange: Exchange::Bybit, symbol: "BTCUSDT".into(), side: OrderSide::Buy, quantity: d("0.001"), reduce_only: true, leverage: None })]);
    }

    #[test]
    fn orders_without_a_client_order_id_are_not_pickable() {
        let mut s = snap(ExecutionMode::Simulation);
        s.leg_accounts.insert(
            (true, Exchange::Binance),
            acct(ok(vec![]), ok(vec![ord(Exchange::Binance, "BTCUSDT", Some("tf-123"), "1"), ord(Exchange::Binance, "ETHUSDT", None, "2"), ord(Exchange::Binance, "SOLUSDT", Some("  "), "2")])),
        );
        let l = open_orders(&s);
        let rows = match &l[0].state {
            PickState::Rows { rows, .. } => rows.clone(),
            o => panic!("{o:?}"),
        };
        assert_eq!(rows.len(), 3);
        assert_eq!(cancel_prefill(&rows[0]), Some(CancelPrefill { exchange: Exchange::Binance, symbol: "BTCUSDT".into(), order_id: "tf-123".into() }));
        assert_eq!(cancel_prefill(&rows[1]), None);
        assert_eq!(cancel_prefill(&rows[2]), None);
        let mut s2 = snap(ExecutionMode::Simulation);
        s2.leg_accounts.insert((true, Exchange::Bybit), acct(ok(vec![]), Err("boom".into())));
        assert_eq!(open_orders(&s2)[1].state, PickState::Failed("boom".into()));
    }
}

// ---- manual-order-leverage ----------------------------------------------------------------------

fn demo() -> UiSnapshot {
    snap(ExecutionMode::ExchangeDemo)
}

#[test]
fn an_opening_order_needs_a_whole_number_leverage_from_1_to_125() {
    for bad in ["", "abc", "2.5", "0", "126", "-3"] {
        let mut f = form("0.001");
        f.leverage = bad.into();
        let vm = build(&f, &CancelForm::default(), &demo());
        assert!(vm.submit_disabled.contains(&"Leverage 必須是 1–125 的整數".to_string()), "{bad:?}: {:?}", vm.submit_disabled);
        assert!(open_confirm(&vm, &f).is_none());
    }
    for good in ["1", "5", " 125 "] {
        let mut f = form("0.001");
        f.leverage = good.into();
        let mut s = snap(ExecutionMode::Simulation);
        put_caps(&mut s, "BTCUSDT", "125", "125"); // the format is valid; the cap rule is tested separately
        let vm = build(&f, &CancelForm::default(), &s);
        assert!(vm.submit_disabled.is_empty(), "{good:?}: {:?}", vm.submit_disabled);
    }
}

#[test]
fn reduce_only_ignores_the_leverage_field_and_sends_none() {
    let sink = Sink::default();
    let mut f = form("0.001");
    f.reduce_only = true;
    f.leverage = "garbage".into();
    let vm = build(&f, &CancelForm::default(), &demo());
    assert!(vm.submit_disabled.is_empty(), "{:?}", vm.submit_disabled);
    assert_eq!((vm.leverage, vm.cap_text.clone()), (None, None));
    let c = open_confirm(&vm, &f).unwrap();
    confirm(&c, &sink);
    assert!(matches!(&sink.0.borrow()[0], Command::ManualOrder(ManualOrder { reduce_only: true, leverage: None, .. })));
}

#[test]
fn a_known_cap_below_the_leverage_disables_submit_with_the_reason() {
    let mut s = demo();
    put_caps(&mut s, "BTCUSDT", "20", "3");
    let mut f = form("0.001");
    f.exchange = Exchange::Bybit;
    let vm = build(&f, &CancelForm::default(), &s);
    assert!(vm.submit_disabled.contains(&"槓桿 5× 超過 Bybit 上限 3×".to_string()), "{:?}", vm.submit_disabled);
    assert!(vm.cap_text.unwrap().contains('✗'));
    // The same leverage on Binance (cap 20) is fine.
    assert!(build(&form("0.001"), &CancelForm::default(), &s).submit_disabled.is_empty());
}

#[test]
fn an_unknown_cap_blocks_in_exchange_demo_only() {
    let mut s = demo();
    s.leverage_caps.clear();
    let vm = build(&form("0.001"), &CancelForm::default(), &s);
    assert!(vm.submit_disabled.iter().any(|w| w.contains("槓桿上限未知")), "{:?}", vm.submit_disabled);
    let mut sim = snap(ExecutionMode::Simulation);
    sim.leverage_caps.clear();
    assert!(build(&form("0.001"), &CancelForm::default(), &sim).submit_disabled.is_empty());
}

#[test]
fn the_confirmation_shows_the_leverage_and_the_cap_and_the_command_carries_it() {
    let sink = Sink::default();
    let f = form("0.0014");
    let vm = build(&f, &CancelForm::default(), &demo());
    let c = open_confirm(&vm, &f).unwrap();
    assert_eq!(c.leverage, Some(d("5")));
    assert!(c.cap_text.as_deref().unwrap().contains("Binance 上限 20×"), "{:?}", c.cap_text);
    confirm(&c, &sink);
    assert!(matches!(&sink.0.borrow()[0], Command::ManualOrder(ManualOrder { leverage: Some(l), .. }) if *l == d("5")));
}

#[test]
fn the_cap_is_requested_for_the_estimate_rounded_up_to_the_next_thousand() {
    assert_eq!(cap_request_notional(d("60")), d("1000"));
    assert_eq!(cap_request_notional(d("1000")), d("1000"));
    assert_eq!(cap_request_notional(d("1000.01")), d("2000"));
    assert_eq!(cap_request_notional(d("0")), d("1000"));
}

use super::*;
use crate::engine::command::{Blocker, Command, PairView};
use crate::engine::ports::{AccountOrder, AccountPosition, Listed, OrderRules, OrderSide};
use crate::store::event_query::StoredEvent;
use crate::ui::bridge::{CommandSink, EngineState, LegAccount, MarketFeed, UiSnapshot};
use crate::ui::testkit::{complete_settings, d, obs};
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeSet;
use tong_funding_core::pair::PairState;
use tong_funding_core::quantity::LotSize;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

const NOW: i64 = 1_800_000_000_000;
const T: i64 = NOW + 3_600_000;

#[derive(Default)]
struct Sink(RefCell<Vec<Command>>);
impl CommandSink for Sink {
    fn send(&self, _label: String, command: Command) {
        self.0.borrow_mut().push(command);
    }
}

fn pv(uuid: &str, symbol: &str, state: PairState, simulated: bool) -> PairView {
    PairView {
        internal_uuid: uuid.into(),
        pair_id: format!("pid-{uuid}"),
        symbol: symbol.into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        state,
        settlement_ms: T,
        simulated,
    }
}

fn lot(step: &str, min: &str) -> Result<OrderRules, String> {
    Ok(OrderRules { lot: LotSize { step_size: d(step), min_qty: d(min) }, okx_ct_val: None })
}

/// Three PREPARED pairs: BTC and ETH at 1,200 USDT / 3×, DOGE whose legs round below the minimum.
fn snap() -> UiSnapshot {
    let mut s = UiSnapshot { settings: complete_settings("0.01"), ..UiSnapshot::default() };
    let mut feeds = std::collections::BTreeMap::new();
    for (sym, price) in [("BTCUSDT", "60200"), ("ETHUSDT", "3000"), ("DOGEUSDT", "100000")] {
        for ex in [Exchange::Binance, Exchange::Bybit] {
            let mut o = obs(ex, sym, "0.0001", 28_800, T, NOW - 500);
            o.mark_price = d(price);
            feeds.entry(ex).or_insert_with(Vec::new).push(o);
            s.rules.insert((ex, sym.into()), lot("0.001", "0.001"));
        }
        s.pair_entries.insert(
            format!("u-{sym}"),
            json!({"long_scan_price": price, "short_scan_price": price, "notional_usdt": if sym == "DOGEUSDT" { "40" } else { "1200" }, "leverage": "3", "net_edge_pct": "0.07", "gross_spread": "0.0012"}),
        );
    }
    for (ex, v) in feeds {
        s.market.insert(ex, MarketFeed { observations: v, last_success_at: Some(NOW - 500), last_error: None });
    }
    s.engine = Some(EngineState {
        now_ms: NOW,
        trigger_mode: TriggerMode::Manual,
        execution_mode: ExecutionMode::Simulation,
        pairs: ["BTCUSDT", "ETHUSDT", "DOGEUSDT"].iter().map(|sym| pv(&format!("u-{sym}"), sym, PairState::Prepared, true)).collect(),
        blockers: vec![],
        notices: vec![],
        alerts: vec![],
    });
    s
}

fn eng(s: &mut UiSnapshot) -> &mut EngineState {
    s.engine.as_mut().unwrap()
}

fn sel(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

// ---- 1.1 list, selection, summary --------------------------------------------------------

#[test]
fn staged_orders_rows_show_floored_quantities_and_the_net_edge() {
    let vm = build(&snap(), &BTreeSet::new(), NOW);
    assert_eq!(vm.staged_count, 3);
    let btc = vm.rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap();
    assert_eq!(btc.long_qty.text(), "0.019 BTC", "never 0.019934");
    assert_eq!(btc.net_edge_pct, Some(d("0.07")));
    assert_eq!((btc.notional, btc.leverage, btc.margin), (Some(d("1200")), Some(d("3")), Some(d("400"))));
    assert_eq!(btc.entry_in_ms, Some(T - 10_000 - NOW), "entry at T−10 s");
    assert_eq!(vm.mode_label(), "SIMULATION");
}

#[test]
fn staged_orders_select_all_skips_the_pair_below_the_minimum() {
    let s = snap();
    let vm = build(&s, &BTreeSet::new(), NOW);
    let doge = vm.rows.iter().find(|r| r.symbol == "DOGEUSDT").unwrap();
    assert_eq!(doge.selectable, Err("低於最小下單量".to_string()));
    let all = select_all(&vm);
    assert_eq!(all, sel(&["u-BTCUSDT", "u-ETHUSDT"]));
    let vm = build(&s, &all, NOW);
    assert_eq!(vm.selected_count, 2);
    assert!(select_none().is_empty());
    // A row that cannot be selected stays unselected even if asked for.
    let vm = build(&s, &sel(&["u-DOGEUSDT"]), NOW);
    assert_eq!(vm.selected_count, 0);
}

#[test]
fn staged_orders_summary_is_hand_calculated() {
    let vm = build(&snap(), &sel(&["u-BTCUSDT", "u-ETHUSDT"]), NOW);
    assert_eq!(vm.summary, Summary { pairs: 2, legs: 4, notional: d("4800"), margin: d("1600") });
}

#[test]
fn staged_orders_mode_label_follows_the_engine() {
    let mut s = snap();
    eng(&mut s).execution_mode = ExecutionMode::ExchangeDemo;
    assert_eq!(build(&s, &BTreeSet::new(), NOW).mode_label(), "EXCHANGE_DEMO");
}

#[test]
fn staged_orders_available_margin_unknown_is_never_zero() {
    let mut s = snap();
    s.leg_accounts.insert(
        (false, Exchange::Bybit),
        LegAccount { positions: Err("x".into()), open_orders: Err("x".into()), available_margin: Err("timeout".into()), fetched_at: NOW },
    );
    let vm = build(&s, &BTreeSet::new(), NOW);
    let bybit = vm.margins.iter().find(|m| m.0 == Exchange::Bybit).unwrap();
    assert_eq!(bybit.1, "未知（timeout）");
    let binance = vm.margins.iter().find(|m| m.0 == Exchange::Binance).unwrap();
    assert_eq!(binance.1, "未知（尚未查詢）");
}

#[test]
fn staged_orders_trigger_mode_toggle_sends_one_command() {
    let sink = Sink::default();
    let vm = build(&snap(), &BTreeSet::new(), NOW);
    assert!(toggle_trigger_mode(&vm, &sink));
    assert_eq!(*sink.0.borrow(), vec![Command::SetTriggerMode(TriggerMode::Auto)]);
}

// ---- 1.2 disabled reasons and the confirmation -------------------------------------------

#[test]
fn each_disabled_reason_is_reported() {
    let s = snap();
    assert_eq!(build(&s, &BTreeSet::new(), NOW).disabled, vec![DisabledReason::NothingSelected]);
    assert_eq!(DisabledReason::NothingSelected.text(), "尚未選取配對");

    let mut s2 = snap();
    s2.settings.risk.taker_fee_pct.remove(&Exchange::Bybit);
    let r = build(&s2, &sel(&["u-BTCUSDT"]), NOW).disabled;
    assert_eq!(r, vec![DisabledReason::ConfigIncomplete(vec!["Bybit taker_fee_pct".into()])]);
    assert_eq!(r[0].text(), "設定不完整：Bybit taker_fee_pct");

    let mut s3 = snap();
    eng(&mut s3).blockers = vec![Blocker::KillSwitch];
    let r = build(&s3, &sel(&["u-BTCUSDT"]), NOW).disabled;
    assert_eq!(r, vec![DisabledReason::KillSwitchOn]);
    assert_eq!(r[0].text(), "緊急停止中");

    let mut s4 = snap();
    eng(&mut s4).blockers = vec![Blocker::StoreHalted("disk".into())];
    assert!(matches!(build(&s4, &sel(&["u-BTCUSDT"]), NOW).disabled.as_slice(), [DisabledReason::Halted(_)]));

    let mut s5 = snap();
    eng(&mut s5).pairs[0].state = PairState::PreTradeCheck;
    // A selection of a pair that left PREPARED (e.g. taken by the scheduler).
    let r = build(&s5, &sel(&["u-BTCUSDT"]), NOW).disabled;
    assert_eq!(r, vec![DisabledReason::NotPrepared(vec!["BTCUSDT".into()])]);

    let mut s6 = snap();
    s6.engine = None;
    assert_eq!(build(&s6, &sel(&["u-BTCUSDT"]), NOW).disabled, vec![DisabledReason::EngineUnavailable]);
}

#[test]
fn the_confirmation_lists_two_legs_per_pair_and_nothing_is_sent_before_confirming() {
    let s = snap();
    let sink = Sink::default();
    let vm = build(&s, &sel(&["u-BTCUSDT", "u-ETHUSDT"]), NOW);
    let pending = open_confirm(&vm).expect("enabled");
    assert_eq!(pending.legs.len(), 2 * 2);
    let first = &pending.legs[0];
    assert_eq!((first.exchange, first.side, first.qty_text.as_str()), (Exchange::Binance, OrderSide::Buy, "0.019 BTC"));
    assert_eq!((first.notional, first.leverage, first.margin), (d("1200"), d("3"), d("400")));
    assert_eq!(pending.legs[1].side, OrderSide::Sell);
    assert_eq!(pending.env_text, "SIMULATION：由模擬器成交，不會送出真實訂單");
    assert!(sink.0.borrow().is_empty(), "opening the confirmation sends nothing");
    // Cancel = drop the pending confirmation: still nothing sent.
    drop(pending);
    assert!(sink.0.borrow().is_empty());
    let pending = open_confirm(&vm).unwrap();
    let out = confirm(&pending, &s, &sink);
    assert_eq!(out.excluded, Vec::<String>::new());
    assert_eq!(*sink.0.borrow(), vec![Command::EnterSelected { pairs: vec!["u-BTCUSDT".into(), "u-ETHUSDT".into()] }]);
}

#[test]
fn exchange_demo_confirmation_warns_about_real_demo_orders() {
    let mut s = snap();
    eng(&mut s).execution_mode = ExecutionMode::ExchangeDemo;
    let vm = build(&s, &sel(&["u-BTCUSDT"]), NOW);
    assert_eq!(open_confirm(&vm).unwrap().env_text, "EXCHANGE_DEMO：將對 demo / testnet 帳戶真實下單");
}

#[test]
fn a_pair_taken_by_the_scheduler_during_confirmation_is_excluded() {
    let s = snap();
    let sink = Sink::default();
    let pending = open_confirm(&build(&s, &sel(&["u-BTCUSDT", "u-ETHUSDT"]), NOW)).unwrap();
    let mut later = s.clone();
    eng(&mut later).pairs[0].state = PairState::PreTradeCheck;
    let out = confirm(&pending, &later, &sink);
    assert_eq!(out.excluded, vec!["BTCUSDT".to_string()]);
    assert_eq!(*sink.0.borrow(), vec![Command::EnterSelected { pairs: vec!["u-ETHUSDT".into()] }]);
    // Everything gone: no command at all.
    let sink = Sink::default();
    eng(&mut later).pairs[1].state = PairState::Cancelled;
    let out = confirm(&pending, &later, &sink);
    assert_eq!(out.excluded.len(), 2);
    assert!(sink.0.borrow().is_empty());
}

#[test]
fn a_disabled_page_opens_no_confirmation() {
    assert!(open_confirm(&build(&snap(), &BTreeSet::new(), NOW)).is_none());
}

// ---- 1.3 last result and manual handling -------------------------------------------------

fn ev(id: i64, ts: i64, ty: &str, pair: &str, payload: serde_json::Value) -> StoredEvent {
    StoredEvent { id, ts_ms: ts, event_type: ty.into(), pair_id: Some(pair.into()), payload: payload.to_string(), imported: false }
}

fn attempt(pair: &str, simulated: bool, short: serde_json::Value, to: &str) -> Vec<StoredEvent> {
    let mut v = vec![
        ev(1, NOW - 9_000, "PAIR_TRANSITION", pair, json!({"from": "PREPARED", "to": "PRE_TRADE_CHECK", "detail": {"simulated": simulated}})),
        ev(2, NOW - 8_900, "ORDER_SUBMITTED", pair, json!({"leg": "long", "action": "open", "exchange": "Binance", "symbol": "BTCUSDT", "simulated": simulated, "outcome": "accepted", "client_order_id": "simlo0001", "exchange_order_id": if simulated { json!("sim-1") } else { json!("8389765") }})),
        ev(3, NOW - 8_800, "ORDER_SUBMITTED", pair, short),
        ev(4, NOW - 8_700, "PAIR_TRANSITION", pair, json!({"from": "FILL_MONITOR", "to": to, "detail": {"simulated": simulated}})),
    ];
    v.reverse(); // newest first, as stored queries return them
    v
}

#[test]
fn a_simulation_result_is_labelled_and_shows_no_real_order_id() {
    let mut s = snap();
    s.trade_events = attempt("u-BTCUSDT", true, json!({"leg": "short", "action": "open", "exchange": "Bybit", "symbol": "BTCUSDT", "simulated": true, "outcome": "accepted", "client_order_id": "simso0001", "exchange_order_id": "sim-2"}), "RECONCILED");
    let vm = build(&s, &BTreeSet::new(), NOW);
    let a = &vm.last_results[0];
    assert_eq!(a.mode_label, "SIMULATION");
    assert!(a.legs.iter().all(|l| l.order_id_text.starts_with("模擬")), "{:?}", a.legs);
    assert!(!a.needs_manual);
}

#[test]
fn an_exchange_demo_result_shows_the_exchange_order_id() {
    let mut s = snap();
    s.trade_events = attempt("u-BTCUSDT", false, json!({"leg": "short", "action": "open", "exchange": "Bybit", "symbol": "BTCUSDT", "simulated": false, "outcome": "accepted", "client_order_id": "demoso0001", "exchange_order_id": "b-77"}), "RECONCILED");
    let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
    assert_eq!(a.mode_label, "EXCHANGE_DEMO");
    assert_eq!(a.legs[0].order_id_text, "8389765");
    assert_eq!(a.legs[1].order_id_text, "b-77");
}

#[test]
fn a_failed_leg_says_manual_handling_and_never_rolled_back() {
    let mut s = snap();
    eng(&mut s).pairs[0].state = PairState::PartialFailure;
    s.trade_events = attempt("u-BTCUSDT", false, json!({"leg": "short", "action": "open", "exchange": "Bybit", "symbol": "BTCUSDT", "simulated": false, "outcome": "rejected", "reason": "insufficient margin", "client_order_id": "demoso0001"}), "PARTIAL_FAILURE");
    let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
    assert!(a.needs_manual);
    assert!(a.headline.contains("需人工處理") && a.headline.contains("PARTIAL_FAILURE"), "{}", a.headline);
    assert!(!a.headline.contains("回滾"));
    assert_eq!(a.legs[1].status, LegStatus::Rejected("insufficient margin".into()));
}

#[test]
fn a_blocked_attempt_lists_every_failed_check() {
    let mut s = snap();
    s.trade_events = vec![
        ev(2, NOW - 8_000, "PAIR_TRANSITION", "u-BTCUSDT", json!({"from": "PRE_TRADE_CHECK", "to": "BLOCKED", "detail": {"simulated": true, "failed_checks": ["PriceDrift", "Margin"]}})),
        ev(1, NOW - 9_000, "PAIR_TRANSITION", "u-BTCUSDT", json!({"from": "PREPARED", "to": "PRE_TRADE_CHECK", "detail": {"simulated": true}})),
    ];
    let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
    assert_eq!(a.failed_checks, vec!["PriceDrift".to_string(), "Margin".to_string()]);
    assert!(a.legs.iter().all(|l| l.status == LegStatus::NotSent));
}

fn account(positions: Vec<(&str, &str)>, open: Vec<&str>) -> LegAccount {
    LegAccount {
        positions: Ok(Listed { items: positions.into_iter().map(|(s, q)| AccountPosition { exchange: Exchange::Binance, symbol: s.into(), quantity: d(q) }).collect(), complete: true }),
        open_orders: Ok(Listed { items: open.into_iter().map(|s| AccountOrder { exchange: Exchange::Binance, symbol: s.into(), client_order_id: None, remaining_quantity: d("1") }).collect(), complete: true }),
        available_margin: Ok(d("1000")),
        fetched_at: NOW,
    }
}

fn failed_pair(s: &mut UiSnapshot) {
    eng(s).pairs[0].state = PairState::PartialFailure;
    eng(s).pairs[0].simulated = false;
}

#[test]
fn manual_handling_buttons_follow_the_latest_position_query() {
    let mut s = snap();
    failed_pair(&mut s);
    s.leg_accounts.insert((false, Exchange::Binance), account(vec![("BTCUSDT", "0.019")], vec![]));
    s.leg_accounts.insert((false, Exchange::Bybit), account(vec![("BTCUSDT", "0.019")], vec![]));
    let h = build(&s, &BTreeSet::new(), NOW).manual.into_iter().find(|h| h.uuid == "u-BTCUSDT").unwrap();
    assert_eq!(h.confirm_closed, Err("Binance 仍有持倉；Bybit 仍有持倉".to_string()));
    let close = h.close.clone().expect("close possible");
    assert_eq!(close.len(), 2);
    assert_eq!(close[0].quantity, d("0.019"));

    // Bybit flat but the Binance query failed: still disabled, with the reason.
    s.leg_accounts.insert((false, Exchange::Bybit), account(vec![], vec![]));
    s.leg_accounts.insert(
        (false, Exchange::Binance),
        LegAccount { positions: Err("HTTP 503".into()), open_orders: Err("HTTP 503".into()), available_margin: Err("HTTP 503".into()), fetched_at: NOW },
    );
    let h = build(&s, &BTreeSet::new(), NOW).manual.into_iter().next().unwrap();
    assert_eq!(h.confirm_closed, Err("Binance 持倉查詢失敗：HTTP 503".to_string()));
    assert!(h.close.is_err());

    // An open order left on the symbol also blocks.
    s.leg_accounts.insert((false, Exchange::Binance), account(vec![], vec!["BTCUSDT"]));
    let h = build(&s, &BTreeSet::new(), NOW).manual.into_iter().next().unwrap();
    assert_eq!(h.confirm_closed, Err("Binance 有未成交委託".to_string()));

    // Both flat: enabled; the command is ConfirmClosed (the engine re-queries anyway).
    s.leg_accounts.insert((false, Exchange::Binance), account(vec![("ETHUSDT", "2")], vec!["ETHUSDT"]));
    let h = build(&s, &BTreeSet::new(), NOW).manual.into_iter().next().unwrap();
    assert_eq!(h.confirm_closed, Ok(()));
    let sink = Sink::default();
    assert!(send_confirm_closed(&h, &sink));
    assert_eq!(*sink.0.borrow(), vec![Command::ConfirmClosed { pair: "u-BTCUSDT".into(), verified_flat: true }]);
}

#[test]
fn manual_close_needs_its_own_confirmation_and_only_two_actions_exist() {
    let mut s = snap();
    failed_pair(&mut s);
    s.leg_accounts.insert((false, Exchange::Binance), account(vec![("BTCUSDT", "0.019")], vec![]));
    s.leg_accounts.insert((false, Exchange::Bybit), account(vec![], vec![]));
    let h = build(&s, &BTreeSet::new(), NOW).manual.into_iter().next().unwrap();
    assert_eq!(h.actions(), ["人工要求平倉", "人工確認已平倉"]);
    let sink = Sink::default();
    let c = request_close(&h).expect("positions known");
    assert_eq!(c.legs.len(), 1, "only the non-flat leg");
    assert!(sink.0.borrow().is_empty());
    send_close(&c, &sink);
    assert_eq!(*sink.0.borrow(), vec![Command::ManualClose { pair: "u-BTCUSDT".into() }]);
}

#[test]
fn reconciled_pairs_offer_close_now_only_in_manual_mode() {
    let mut s = snap();
    eng(&mut s).pairs[0].state = PairState::Reconciled;
    let vm = build(&s, &BTreeSet::new(), NOW);
    let r = vm.running.iter().find(|r| r.uuid == "u-BTCUSDT").unwrap();
    assert_eq!(r.exit_in_ms, T + 15_000 - NOW);
    assert!(r.close_now);
    eng(&mut s).trigger_mode = TriggerMode::Auto;
    let vm = build(&s, &BTreeSet::new(), NOW);
    let r = vm.running.iter().find(|r| r.uuid == "u-BTCUSDT").unwrap();
    assert!(!r.close_now, "AUTO shows 自動");
    assert_eq!(r.action_text(), "自動");
}

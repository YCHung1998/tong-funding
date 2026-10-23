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
        flat_confirmed: false,
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
fn staged_orders_both_legs_show_one_shared_quantity_at_most_the_notional() {
    // matched-leg-quantity: different prices and steps still give one quantity for both legs.
    let mut s = snap();
    for (ex, px, step) in [(Exchange::Binance, "60000", "0.001"), (Exchange::Bybit, "60100", "0.0001")] {
        s.market.get_mut(&ex).unwrap().observations.iter_mut().find(|o| o.symbol == "BTCUSDT").unwrap().mark_price = d(px);
        s.rules.insert((ex, "BTCUSDT".into()), lot(step, step));
    }
    let vm = build(&s, &BTreeSet::new(), NOW);
    let btc = vm.rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap();
    assert_eq!((btc.long_qty.order_qty(), btc.short_qty.order_qty()), (Some(d("0.019")), Some(d("0.019"))));
    assert_eq!(btc.long_qty.text(), "0.019 BTC（≈ 1,140.00 USDT）");
    assert_eq!(btc.short_qty.text(), "0.019 BTC（≈ 1,141.90 USDT）");
}


#[test]
fn staged_orders_rows_show_floored_quantities_and_the_net_edge() {
    let vm = build(&snap(), &BTreeSet::new(), NOW);
    assert_eq!(vm.staged_count, 3);
    let btc = vm.rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap();
    assert_eq!(btc.long_qty.text(), "0.019 BTC（≈ 1,143.80 USDT）", "never 0.019934");
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
    assert_eq!((first.exchange, first.side, first.qty_text.as_str()), (Exchange::Binance, OrderSide::Buy, "0.019 BTC（≈ 1,143.80 USDT）"));
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
    put_caps(&mut s, "BTCUSDT", "20", "10"); // EXCHANGE_DEMO needs known caps to select
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
    assert!(a.headline.contains("需人工處理（目前狀態：單腿失敗）"), "{}", a.headline);
    assert!(a.codes.contains("PARTIAL_FAILURE"), "the code stays as the secondary line: {}", a.codes);
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

// ---- spread-position-tag: the running list shows the live tag --------------------------------

mod spread_tag_running {
    use super::*;
    use tong_funding_core::spread_tag::SpreadKind;

    fn set_marks(s: &mut UiSnapshot, long_px: &str, short_px: &str) {
        for (ex, px) in [(Exchange::Binance, long_px), (Exchange::Bybit, short_px)] {
            s.market.get_mut(&ex).unwrap().observations.iter_mut().find(|o| o.symbol == "BTCUSDT").unwrap().mark_price = d(px);
        }
    }

    fn running(s: &UiSnapshot, now: i64) -> RunningRow {
        build(s, &BTreeSet::new(), now).running.into_iter().find(|r| r.uuid == "u-BTCUSDT").unwrap()
    }

    #[test]
    fn a_running_row_shows_the_tag_and_it_changes_with_the_quotes() {
        let mut s = snap();
        eng(&mut s).pairs[0].state = PairState::Reconciled;
        set_marks(&mut s, "100", "100.2");
        let v = running(&s, NOW).spread.expect("tag shown");
        assert_eq!((v.tag.kind, v.text.as_str()), (SpreadKind::Positive, "價差 positive +0.20%（標記價）"));
        set_marks(&mut s, "100.5", "100.2");
        assert_eq!(running(&s, NOW).spread.unwrap().tag.kind, SpreadKind::Negative);
    }

    #[test]
    fn a_failing_or_stale_leg_shows_no_tag() {
        let mut s = snap();
        eng(&mut s).pairs[0].state = PairState::Reconciled;
        assert_eq!(running(&s, NOW + 60_000).spread, None, "stale");
        s.market.get_mut(&Exchange::Bybit).unwrap().last_error = Some(("HTTP 500".into(), NOW));
        assert_eq!(running(&s, NOW).spread, None, "failing");
    }
}

// ---- trade-cost-estimate: per-row cost estimate at the shared quantity --------------------

mod cost {
    use super::*;
    use crate::ui::bridge::BookFeed;
    use tong_funding_core::trade_cost::TopOfBook;

    const QUOTE_AT: i64 = NOW - 4_000;

    fn book(bid: &str, bid_qty: &str, ask: &str, ask_qty: &str) -> TopOfBook {
        TopOfBook { bid_price: d(bid), bid_qty: d(bid_qty), ask_price: d(ask), ask_qty: d(ask_qty), observed_at: QUOTE_AT }
    }

    fn put_book(s: &mut UiSnapshot, ex: Exchange, symbol: &str, b: TopOfBook) {
        let feed: &mut BookFeed = s.books.entry(ex).or_default();
        feed.books.insert(symbol.into(), b);
        feed.last_success_at = Some(b.observed_at);
    }

    /// The spec scenario: BTC at 60,010 / 60,090 (marks 60,010), qty 0.016 (1,000 cap, step 0.001),
    /// 5x, taker 0.05 (Binance, long) and 0.055 (Bybit, short).
    fn spec_snap() -> UiSnapshot {
        let mut s = snap();
        s.pair_entries.insert(
            "u-BTCUSDT".into(),
            json!({"long_scan_price": "60010", "short_scan_price": "60010", "notional_usdt": "1000", "leverage": "5", "net_edge_pct": "0.07", "gross_spread": "0.0012"}),
        );
        for ex in [Exchange::Binance, Exchange::Bybit] {
            s.market.get_mut(&ex).unwrap().observations.iter_mut().find(|o| o.symbol == "BTCUSDT").unwrap().mark_price = d("60010");
        }
        s.settings.risk.taker_fee_pct.insert(Exchange::Binance, d("0.05"));
        s.settings.risk.taker_fee_pct.insert(Exchange::Bybit, d("0.055"));
        put_book(&mut s, Exchange::Binance, "BTCUSDT", book("60000", "5", "60010", "5"));
        put_book(&mut s, Exchange::Bybit, "BTCUSDT", book("60090", "5", "60100", "5"));
        s
    }

    fn btc(s: &UiSnapshot) -> StagedRow {
        build(s, &BTreeSet::new(), NOW).rows.into_iter().find(|r| r.symbol == "BTCUSDT").unwrap()
    }

    fn display(r: &StagedRow) -> &CostDisplay {
        match &r.cost {
            CostView::Estimate(c) => c,
            other => panic!("expected an estimate, got {other:?}"),
        }
    }

    #[test]
    fn long_pays_the_ask_short_receives_the_bid_at_the_shared_quantity_spec_numbers() {
        let r = btc(&spec_snap());
        assert_eq!(r.long_qty.order_qty(), Some(d("0.016")), "the shared quantity");
        let c = display(&r);
        assert_eq!((c.qty, c.long_ask, c.short_bid), (d("0.016"), d("60010"), d("60090")));
        let e = &c.estimate;
        assert_eq!((e.long.cost.value, e.short.cost.value), (d("960.16"), d("961.44")));
        assert_eq!((e.long.cost.open_fee, e.short.cost.open_fee), (d("0.48008"), d("0.528792")));
        assert_eq!((e.long.cost.margin, e.short.cost.margin), (d("192.51208"), d("192.816792")));
        assert_eq!((c.quote_at, c.quote_age_ms), (QUOTE_AT, 4_000));
        assert!(c.warnings.is_empty());
        let lines = r.cost.lines();
        assert_eq!(lines[0], "L Binance 賣一 60010 → 價值 960.16 · 開倉費 0.4801 · 保證金 192.51");
        assert_eq!(lines[1], "S Bybit 買一 60090 → 價值 961.44 · 開倉費 0.5288 · 保證金 192.82");
        assert!(lines[2].starts_with("合計保證金 385.33 USDT"), "{}", lines[2]);
        assert!(lines[3].contains("4 秒前"), "{}", lines[3]);
    }

    #[test]
    fn the_older_quote_sets_the_data_time() {
        let mut s = spec_snap();
        let mut old = book("60090", "5", "60100", "5");
        old.observed_at = NOW - 9_000;
        put_book(&mut s, Exchange::Bybit, "BTCUSDT", old);
        let r = btc(&s);
        assert_eq!((display(&r).quote_at, display(&r).quote_age_ms), (NOW - 9_000, 9_000));
    }

    #[test]
    fn quantity_above_the_top_level_warns_about_the_second_level_but_does_not_block() {
        let mut s = spec_snap();
        put_book(&mut s, Exchange::Binance, "BTCUSDT", book("60000", "5", "60010", "0.010"));
        let r = btc(&s);
        let c = display(&r);
        assert_eq!(c.estimate.long.cost.value, d("960.16"), "still priced at the best ask");
        assert!(c.warnings.iter().any(|w| w.contains("Binance") && w.contains("第二檔") && w.contains("0.01")), "{:?}", c.warnings);
        assert!(r.selectable.is_ok(), "a hint only; submit is not blocked");
    }

    #[test]
    fn a_new_snapshot_quote_updates_the_row() {
        let mut s = spec_snap();
        assert_eq!(display(&btc(&s)).estimate.long.cost.value, d("960.16"));
        put_book(&mut s, Exchange::Binance, "BTCUSDT", book("60000", "5", "60020", "5"));
        let c = display(&btc(&s)).clone();
        assert_eq!(c.estimate.long.cost.value, d("960.32"));
        assert_eq!(c.estimate.long.cost.margin, d("960.32") / d("5") + d("0.48016"));
    }

    #[test]
    fn missing_quote_shows_the_reason_and_no_numbers() {
        let mut s = spec_snap();
        s.books.get_mut(&Exchange::Bybit).unwrap().books.clear();
        let r = btc(&s);
        match &r.cost {
            CostView::Unavailable(why) => assert!(why.contains("無報價") && why.contains("Bybit") && !why.contains("Binance"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(r.cost.lines(), vec![format!("預估成本：{}", match &r.cost { CostView::Unavailable(w) => w.clone(), _ => unreachable!() })]);
        // No book feed at all (never fetched): same, never a mark price in its place.
        s.books.clear();
        assert!(matches!(&btc(&s).cost, CostView::Unavailable(w) if w.contains("無報價") && w.contains("Binance") && w.contains("Bybit")));
    }

    #[test]
    fn unset_taker_fee_shows_the_reason_and_no_numbers() {
        let mut s = spec_snap();
        s.settings.risk.taker_fee_pct.remove(&Exchange::Bybit);
        match &btc(&s).cost {
            CostView::Unavailable(why) => assert!(why.contains("手續費率") && why.contains("Bybit"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_quantity_or_leverage_shows_the_reason() {
        let mut s = spec_snap();
        s.rules.remove(&(Exchange::Bybit, "BTCUSDT".into()));
        assert!(matches!(&btc(&s).cost, CostView::Unavailable(w) if w.contains("共同數量")));
        let mut s = spec_snap();
        s.pair_entries.insert("u-BTCUSDT".into(), json!({"notional_usdt": "1000", "net_edge_pct": "0.07"}));
        assert!(matches!(&btc(&s).cost, CostView::Unavailable(w) if w.contains("槓桿")));
    }

    #[test]
    fn a_failed_quote_refresh_keeps_the_old_estimate_and_says_so() {
        let mut s = spec_snap();
        s.books.get_mut(&Exchange::Binance).unwrap().last_error = Some(("HTTP 429".into(), NOW - 100));
        let r = btc(&s);
        let c = display(&r);
        assert!(c.warnings.iter().any(|w| w.contains("Binance") && w.contains("更新失敗")), "{:?}", c.warnings);
        assert_eq!(c.quote_age_ms, 4_000, "the shown age is the quote's own age");
    }
}

// ---- symbol-leverage-cap: staged rows --------------------------------------------------------

fn put_caps(s: &mut UiSnapshot, sym: &str, bin: &str, byb: &str) {
    for (ex, c) in [(Exchange::Binance, bin), (Exchange::Bybit, byb)] {
        s.leverage_caps.insert((ex, sym.into()), crate::ui::bridge::CapReading { notional: d("1200"), cap: Ok(d(c)), fetched_at: NOW - 1_000 });
    }
}

#[test]
fn staged_row_shows_both_caps_and_a_leverage_above_a_cap_cannot_be_selected() {
    let mut s = snap(); // every pair: leverage 3x
    put_caps(&mut s, "BTCUSDT", "20", "10");
    put_caps(&mut s, "ETHUSDT", "20", "2");
    let vm = build(&s, &BTreeSet::new(), NOW);
    let btc = vm.rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap();
    let eth = vm.rows.iter().find(|r| r.symbol == "ETHUSDT").unwrap();
    assert_eq!(btc.selectable, Ok(()));
    assert!(btc.cap.as_ref().unwrap().text().contains("Binance 20×") && btc.cap.as_ref().unwrap().text().ends_with('✓'));
    assert_eq!(eth.selectable, Err("槓桿 3× 超過 Bybit 上限 2×".to_string()));
    assert!(eth.cap.as_ref().unwrap().text().contains("Bybit 2×"));
    assert_eq!(select_all(&vm), sel(&["u-BTCUSDT"]), "select-all skips the pair above its cap");
}

#[test]
fn staged_row_with_an_unknown_cap_is_selectable_in_simulation_but_not_in_exchange_demo() {
    let mut s = snap();
    assert_eq!(build(&s, &BTreeSet::new(), NOW).rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap().selectable, Ok(()), "SIMULATION: informational");
    eng(&mut s).execution_mode = ExecutionMode::ExchangeDemo;
    let vm = build(&s, &BTreeSet::new(), NOW);
    let btc = vm.rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap();
    assert!(btc.selectable.clone().unwrap_err().contains("槓桿上限未知"), "{:?}", btc.selectable);
    put_caps(&mut s, "BTCUSDT", "20", "10");
    let vm = build(&s, &BTreeSet::new(), NOW);
    assert_eq!(vm.rows.iter().find(|r| r.symbol == "BTCUSDT").unwrap().selectable, Ok(()));
}

// ---- okx-trading-enablement: an OKX leg on the staged-orders page -----------------------------

mod okx {
    use super::*;
    use crate::ui::bridge::BookFeed;
    use tong_funding_core::trade_cost::TopOfBook;

    /// BTC pair, long OKX (ctVal 0.001, lot 1 contract: 0.016 BTC = 16 contracts), short Bybit.
    /// The OKX top of book is already in COINS (the adapter multiplies the contract sizes by ctVal).
    fn okx_snap(ask_coins: &str) -> UiSnapshot {
        let mut s = snap();
        s.pair_entries.insert(
            "u-BTCUSDT".into(),
            json!({"long_scan_price": "60010", "short_scan_price": "60010", "notional_usdt": "1000", "leverage": "5", "net_edge_pct": "0.07", "gross_spread": "0.0012"}),
        );
        for ex in [Exchange::Binance, Exchange::Bybit] {
            s.market.get_mut(&ex).unwrap().observations.iter_mut().find(|o| o.symbol == "BTCUSDT").unwrap().mark_price = d("60010");
        }
        let mut o = obs(Exchange::Okx, "BTCUSDT", "0.0001", 28_800, T, NOW - 500);
        o.mark_price = d("60010");
        s.market.insert(Exchange::Okx, MarketFeed { observations: vec![o], last_success_at: Some(NOW - 500), last_error: None });
        s.rules.insert((Exchange::Okx, "BTCUSDT".into()), Ok(OrderRules { lot: LotSize { step_size: d("1"), min_qty: d("1") }, okx_ct_val: Some(d("0.001")) }));
        eng(&mut s).pairs.iter_mut().find(|p| p.symbol == "BTCUSDT").unwrap().long_exchange = Exchange::Okx;
        let at = NOW - 4_000;
        let book = |bid: &str, ask: &str, ask_qty: &str| TopOfBook { bid_price: d(bid), bid_qty: d("5"), ask_price: d(ask), ask_qty: d(ask_qty), observed_at: at };
        let mut okx: BookFeed = BookFeed::default();
        okx.books.insert("BTCUSDT".into(), book("60000", "60010", ask_coins));
        okx.last_success_at = Some(at);
        s.books.insert(Exchange::Okx, okx);
        let mut bybit: BookFeed = BookFeed::default();
        bybit.books.insert("BTCUSDT".into(), book("60090", "60100", "5"));
        bybit.last_success_at = Some(at);
        s.books.insert(Exchange::Bybit, bybit);
        s
    }

    fn btc(s: &UiSnapshot) -> StagedRow {
        build(s, &BTreeSet::new(), NOW).rows.into_iter().find(|r| r.symbol == "BTCUSDT").unwrap()
    }

    #[test]
    fn the_okx_leg_is_sized_in_contracts_and_priced_in_coins() {
        let r = btc(&okx_snap("5"));
        assert_eq!(r.long_qty.order_qty(), Some(d("16")), "the OKX leg sends contracts");
        assert!(r.long_qty.text().starts_with("16 張（≈ 0.016 BTC"), "{}", r.long_qty.text());
        assert_eq!(r.short_qty.order_qty(), Some(d("0.016")), "the other leg the same coin amount");
        match &r.cost {
            CostView::Estimate(c) => {
                assert_eq!(c.qty, d("0.016"), "the shared quantity is in coins");
                assert!(c.warnings.is_empty(), "{:?}", c.warnings);
            }
            other => panic!("{other:?}"),
        }
        assert!(r.selectable.is_ok(), "{:?}", r.selectable);
    }

    #[test]
    fn the_top_level_is_compared_in_coins_one_contract_size_below_the_shared_quantity_warns() {
        // OKX ask size 10 contracts = 0.010 BTC < 0.016 BTC
        let r = btc(&okx_snap("0.010"));
        match &r.cost {
            CostView::Estimate(c) => assert!(c.warnings.iter().any(|w| w.contains("OKX") && w.contains("第二檔") && w.contains("0.01")), "{:?}", c.warnings),
            other => panic!("{other:?}"),
        }
        assert!(r.selectable.is_ok(), "a hint only");
    }

    #[test]
    fn without_ct_val_there_is_no_quantity_and_no_estimate_and_the_reason_names_it() {
        let mut s = okx_snap("5");
        s.rules.insert((Exchange::Okx, "BTCUSDT".into()), Ok(OrderRules { lot: LotSize { step_size: d("1"), min_qty: d("1") }, okx_ct_val: None }));
        let r = btc(&s);
        assert!(matches!(&r.cost, CostView::Unavailable(w) if w.contains("ctVal")), "{:?}", r.cost);
        assert!(r.selectable.is_err());
    }

    #[test]
    fn the_margin_row_covers_okx_and_unknown_is_never_zero() {
        use crate::ui::bridge::LegAccount;
        let mut s = okx_snap("5");
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!(vm.margins.iter().map(|m| m.0).collect::<Vec<_>>(), vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx]);
        assert_eq!(vm.margins.iter().find(|m| m.0 == Exchange::Okx).unwrap().1, "未知（尚未查詢）");
        s.leg_accounts.insert((false, Exchange::Okx), LegAccount { positions: Err("x".into()), open_orders: Err("x".into()), available_margin: Ok(d("4640.5")), fetched_at: NOW });
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!(vm.margins.iter().find(|m| m.0 == Exchange::Okx).unwrap().1, "4,640.50 USDT");
    }
}

// ---- live-banners 3.1 / 3.3 / 3.4 -------------------------------------------------------------------

fn live_snap(mode: ExecutionMode) -> UiSnapshot {
    let mut s = snap();
    s.identity = crate::identity::ProcessIdentity::Live;
    eng(&mut s).execution_mode = mode;
    if mode == ExecutionMode::ExchangeDemo {
        put_caps(&mut s, "BTCUSDT", "20", "10");
    }
    s
}

#[test]
fn a_live_confirmation_repeats_the_environment_and_the_amount() {
    let vm = build(&live_snap(ExecutionMode::ExchangeDemo), &sel(&["u-BTCUSDT"]), NOW);
    let p = open_confirm(&vm).unwrap();
    assert!(p.env_text.starts_with("LIVE 真錢"), "{}", p.env_text);
    assert!(p.env_line().contains("LIVE 真錢") && p.env_line().contains("2,400.00"), "two legs of 1,200: {}", p.env_line());
    assert_eq!(p.button(), "以真錢下單");
}

#[test]
fn a_demo_confirmation_has_no_real_money_wording() {
    let mut s = snap();
    eng(&mut s).execution_mode = ExecutionMode::ExchangeDemo;
    put_caps(&mut s, "BTCUSDT", "20", "10");
    let p = open_confirm(&build(&s, &sel(&["u-BTCUSDT"]), NOW)).unwrap();
    assert!(p.env_line().contains("DEMO") && p.env_line().contains("2,400.00"));
    assert!(!p.env_line().contains("真錢") && !p.env_line().contains("LIVE") && !p.env_text.contains("LIVE"));
    assert_eq!(p.button(), "確認送出");
}

#[test]
fn staged_vm_and_rows_name_the_live_world() {
    let vm = build(&live_snap(ExecutionMode::ExchangeDemo), &BTreeSet::new(), NOW);
    assert_eq!(vm.mode_label(), "LIVE");
    assert!(vm.rows.iter().all(|r| r.badge == Some("LIVE")));
    let demo = build(&snap(), &BTreeSet::new(), NOW);
    assert!(demo.rows.iter().all(|r| r.badge.is_none()));
    assert_eq!(build(&live_snap(ExecutionMode::Simulation), &BTreeSet::new(), NOW).mode_label(), "LIVE SIMULATION");
}

#[test]
fn a_live_result_and_its_headline_say_live() {
    let mut s = live_snap(ExecutionMode::ExchangeDemo);
    s.trade_events = attempt("u-BTCUSDT", false, json!({"leg": "short", "action": "open", "exchange": "Bybit", "symbol": "BTCUSDT", "simulated": false, "outcome": "accepted", "client_order_id": "livedso1", "exchange_order_id": "b-77"}), "RECONCILED");
    let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
    assert_eq!(a.mode_label, "LIVE");
    assert!(a.headline.ends_with(" · 真錢（LIVE）"), "a real-money result names its world on the headline: {}", a.headline);
    // a demo result keeps the plain headline of the spec (the world is on the secondary line)
    let mut d = snap();
    d.trade_events = attempt("u-BTCUSDT", true, json!({"leg": "short", "action": "open", "exchange": "Bybit", "symbol": "BTCUSDT", "simulated": true, "outcome": "accepted", "client_order_id": "x"}), "RECONCILED");
    eng(&mut d).pairs[0].state = PairState::Reconciled;
    assert!(!build(&d, &BTreeSet::new(), NOW).last_results[0].headline.contains('（'));
}

#[test]
fn close_now_and_manual_close_confirmations_say_real_money_in_a_live_process() {
    let mut s = live_snap(ExecutionMode::ExchangeDemo);
    eng(&mut s).pairs[0] = pv("u-BTCUSDT", "BTCUSDT", PairState::Reconciled, false);
    eng(&mut s).pairs[1] = pv("u-ETHUSDT", "ETHUSDT", PairState::PartialFailure, false);
    let vm = build(&s, &BTreeSet::new(), NOW);
    let run = vm.running.iter().find(|r| r.symbol == "BTCUSDT").unwrap();
    assert!(run.env_line().contains("LIVE 真錢") && run.env_line().contains("2,400.00"), "{}", run.env_line());
    assert_eq!(run.button(), "以真錢平倉");
    let h = vm.manual.iter().find(|h| h.symbol == "ETHUSDT").unwrap();
    assert_eq!(h.env.env_name(), "LIVE");
    let c = CloseConfirm { uuid: h.uuid.clone(), symbol: h.symbol.clone(), legs: vec![], env: h.env, notional: h.notional };
    assert!(c.env_line().contains("LIVE 真錢"));
    assert_eq!(c.button(), "以真錢平倉");
    // The same pair in a Demo process keeps the old words.
    s.identity = crate::identity::ProcessIdentity::Demo;
    let demo = build(&s, &BTreeSet::new(), NOW);
    assert_eq!(demo.running.iter().find(|r| r.symbol == "BTCUSDT").unwrap().button(), "確認平倉");
    assert_eq!(demo.manual[0].env.env_name(), "EXCHANGE_DEMO");
}

// ---- ui-plain-language 2.2: the available-margin row and an exchange the risk settings left unticked ---

mod unticked_exchange {
    use super::*;

    const OKX_SPOT: &str = "exchange error acct_mode_unsupported: 帳戶模式不支援 (acctLv 1, posMode net_mode)";

    fn margin_failed(why: &str) -> LegAccount {
        LegAccount { positions: Err(why.into()), open_orders: Err(why.into()), available_margin: Err(why.into()), fetched_at: NOW }
    }

    fn okx_failing() -> UiSnapshot {
        let mut s = snap();
        s.leg_accounts.insert((false, Exchange::Okx), margin_failed(&format!("OKX: {OKX_SPOT}")));
        s
    }

    #[test]
    fn an_unticked_exchange_has_no_margin_line_only_the_muted_line_with_the_error_behind_it() {
        let mut s = okx_failing();
        s.settings.risk.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit];
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!(vm.margins.iter().map(|m| m.0).collect::<Vec<_>>(), [Exchange::Binance, Exchange::Bybit], "no margin line for OKX");
        assert!(vm.margins.iter().all(|(_, t)| !t.contains("acct_mode_unsupported")), "{:?}", vm.margins);
        assert_eq!(vm.disabled_exchanges.len(), 1);
        let n = &vm.disabled_exchanges[0];
        assert_eq!((n.exchange, n.text.as_str()), (Exchange::Okx, "OKX 已停用（風控設定未勾選）"));
        let detail = n.detail.as_deref().expect("detail behind the line");
        assert!(detail.contains("可用保證金") && detail.contains("OKX 帳戶模式不支援") && detail.contains("在 OKX 改為單幣種或跨幣種保證金模式"), "{detail}");
    }

    #[test]
    fn a_ticked_exchange_with_the_known_error_reads_in_plain_words() {
        let vm = build(&okx_failing(), &BTreeSet::new(), NOW);
        assert!(vm.disabled_exchanges.is_empty());
        let okx = vm.margins.iter().find(|m| m.0 == Exchange::Okx).expect("OKX is ticked");
        assert_eq!(okx.1, "未知（OKX 帳戶模式不支援（在 OKX 改為單幣種或跨幣種保證金模式））");
    }

    #[test]
    fn an_unticked_exchange_with_a_known_margin_is_still_one_line_and_the_margin_is_behind_it() {
        let mut s = snap();
        s.leg_accounts.insert((false, Exchange::Okx), account(vec![], vec![]));
        s.settings.risk.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit];
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!(vm.disabled_exchanges.len(), 1);
        assert_eq!(vm.disabled_exchanges[0].detail.as_deref(), Some("可用保證金：1,000.00 USDT"));
    }

    #[test]
    fn nothing_is_hidden_when_the_settings_cannot_be_read() {
        let mut s = okx_failing();
        s.settings.risk.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit];
        s.settings.error = Some("config.risk is not valid JSON".into());
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert!(vm.disabled_exchanges.is_empty());
        assert_eq!(vm.margins.len(), 3);
    }
}

// ---- ui-plain-language 3.1: the last execution results ------------------------------------------

mod last_results_plain {
    use super::*;
    use crate::ui::bridge::PairRecord;
    use crate::ui::funding::{LegFunding, PairFunding, PnlSummary};
    use tong_funding_core::pnl::{Component, Components, PnlBreakdown, PnlStatus};

    /// 2026-10-22 12:00:00 UTC, 2026-10-21 15:59:50 UTC and 2026-10-22 00:59:50 UTC.
    const T_PTB: i64 = 1_792_670_400_000;
    const T_OLD: i64 = 1_792_598_390_000;
    const T_NEW: i64 = 1_792_630_790_000;
    const PTB: &str = "ui-1792668499226-0-ptbusdt";

    fn record(symbol: &str, status: &str) -> PairRecord {
        PairRecord { pair_id: format!("{symbol}-1"), symbol: symbol.into(), long_exchange: Some(Exchange::Binance), short_exchange: Some(Exchange::Bybit), status: status.into() }
    }

    fn start(id: i64, ts: i64, pair: &str, simulated: bool) -> StoredEvent {
        ev(id, ts, "PAIR_TRANSITION", pair, json!({"from": "PREPARED", "to": "PRE_TRADE_CHECK", "detail": {"simulated": simulated}}))
    }

    fn blocked(id: i64, ts: i64, pair: &str, detail: serde_json::Value) -> StoredEvent {
        ev(id, ts, "PAIR_TRANSITION", pair, json!({"from": "PRE_TRADE_CHECK", "to": "BLOCKED", "event": "System(CheckFailed)", "detail": detail}))
    }

    /// The PTBUSDT block of 2026-10-22: the pair is no longer in the engine's snapshot (restart).
    fn ptb() -> UiSnapshot {
        let mut s = snap();
        s.pair_records.insert(PTB.into(), record("PTBUSDT", "BLOCKED"));
        s.trade_events = vec![
            blocked(2, T_PTB + 50, PTB, json!({"block": "pre-trade fetch failed: request timed out", "simulated": false})),
            start(1, T_PTB, PTB, false),
        ];
        s
    }

    #[test]
    fn a_blocked_attempt_reads_date_symbol_result_and_reason() {
        let a = build(&ptb(), &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == PTB).expect("the attempt");
        assert_eq!(a.headline, "10-22 12:00:00 UTC · PTBUSDT · 被擋下：送單前重抓價格逾時");
        assert_eq!((a.symbol.as_str(), a.state.as_str(), a.state_text.as_str()), ("PTBUSDT", "BLOCKED", "被擋下"));
        assert_eq!(a.block_reason.as_deref(), Some("pre-trade fetch failed: request timed out"));
        assert_eq!(a.when, "10-22 12:00:00 UTC");
        assert_eq!(a.mode_text, "Demo 帳戶（EXCHANGE_DEMO）");
        for code in ["Demo 帳戶（EXCHANGE_DEMO）", "BLOCKED", "pre-trade fetch failed: request timed out"] {
            assert!(a.codes.contains(code), "{code} stays as secondary text: {}", a.codes);
        }
        // the exchanges are the pair's, not blank, and nothing was sent
        assert_eq!(a.legs.iter().map(|l| l.exchange.as_str()).collect::<Vec<_>>(), ["Binance", "Bybit"]);
        assert!(a.legs.iter().all(|l| l.status == LegStatus::NotSent));
        assert!(!a.needs_manual);
    }

    #[test]
    fn the_reason_of_the_pretrade_timeout_and_of_a_restart_read_in_plain_words() {
        let mut s = ptb();
        s.trade_events[0] = blocked(2, T_PTB + 50, PTB, json!({"reason": "pre-trade check timed out", "detail": "nothing sent", "simulated": true}));
        let a = &build(&s, &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == PTB).unwrap();
        assert!(a.headline.ends_with("被擋下：送單前檢查逾時，沒有送出任何訂單"), "{}", a.headline);
        // a reason the table does not know is shown as the engine wrote it
        s.trade_events[0] = blocked(2, T_PTB + 50, PTB, json!({"block": "something new", "simulated": true}));
        let a = build(&s, &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == PTB).unwrap();
        assert!(a.headline.ends_with("被擋下：something new"), "{}", a.headline);
        // no reason recorded: just the result
        s.trade_events[0] = blocked(2, T_PTB + 50, PTB, json!({"simulated": true}));
        let a = build(&s, &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == PTB).unwrap();
        assert!(a.headline.ends_with("PTBUSDT · 被擋下"), "{}", a.headline);
    }

    #[test]
    fn failed_checks_are_listed_by_plain_name_with_the_code_kept() {
        let mut s = snap();
        s.trade_events = vec![
            blocked(2, T_PTB + 50, "u-BTCUSDT", json!({"block": "Checks { failed: [PriceDrift, Margin], notes: [] }", "failed_checks": ["PriceDrift", "Margin"], "simulated": true})),
            start(1, T_PTB, "u-BTCUSDT", true),
        ];
        let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
        assert_eq!(a.failed_checks, ["PriceDrift", "Margin"], "the raw names stay");
        assert!(a.headline.ends_with("BTCUSDT · 被擋下：價格漂移過大（PriceDrift）、可用保證金不足（Margin）"), "{}", a.headline);
    }

    #[test]
    fn symbol_and_exchanges_are_never_blank() {
        // nothing known about the pair at all
        let mut s = snap();
        s.trade_events = vec![blocked(2, T_PTB + 50, "u-ghost", json!({"simulated": true})), start(1, T_PTB, "u-ghost", true)];
        let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
        assert_eq!(a.symbol, "（幣種未知）");
        assert!(a.legs.iter().all(|l| l.exchange == "（交易所未知）"), "{:?}", a.legs);
        assert!(!a.headline.contains(" ·  · "), "{}", a.headline);
        // an order event names them
        s.trade_events.insert(0, ev(3, T_PTB + 40, "ORDER_SUBMITTED", "u-ghost", json!({"leg": "long", "action": "open", "exchange": "Bybit", "symbol": "ETHUSDT", "outcome": "accepted", "client_order_id": "c1"})));
        let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
        assert_eq!((a.symbol.as_str(), a.legs[0].exchange.as_str(), a.legs[1].exchange.as_str()), ("ETHUSDT", "Bybit", "（交易所未知）"));
        // the stored pair names them when the engine does not
        let mut s = snap();
        s.pair_records.insert("u-ghost".into(), record("SOLUSDT", "BLOCKED"));
        s.trade_events = vec![blocked(2, T_PTB + 50, "u-ghost", json!({"simulated": true})), start(1, T_PTB, "u-ghost", true)];
        let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
        assert_eq!((a.symbol.as_str(), a.legs[0].exchange.as_str(), a.legs[1].exchange.as_str()), ("SOLUSDT", "Binance", "Bybit"));
    }

    #[test]
    fn attempts_on_two_days_carry_their_date_and_run_newest_first() {
        let mut s = snap();
        s.pair_records.insert("u-old".into(), record("OLDUSDT", "BLOCKED"));
        s.pair_records.insert("u-new".into(), record("NEWUSDT", "BLOCKED"));
        // stored newest first, as the query returns them; the later time of day (15:59:50) is the OLDER day
        s.trade_events = vec![
            blocked(4, T_NEW + 50, "u-new", json!({"simulated": true})),
            start(3, T_NEW, "u-new", true),
            blocked(2, T_OLD + 50, "u-old", json!({"simulated": true})),
            start(1, T_OLD, "u-old", true),
        ];
        let r = build(&s, &BTreeSet::new(), NOW).last_results;
        assert_eq!(r.iter().map(|a| a.when.as_str()).collect::<Vec<_>>(), ["10-22 00:59:50 UTC", "10-21 15:59:50 UTC"]);
        assert!(r[0].headline.starts_with("10-22 00:59:50 UTC · NEWUSDT") && r[1].headline.starts_with("10-21 15:59:50 UTC · OLDUSDT"));
        // the order does not depend on the order the events arrive in
        s.trade_events.reverse();
        assert_eq!(build(&s, &BTreeSet::new(), NOW).last_results.iter().map(|a| a.when.clone()).collect::<Vec<_>>(), ["10-22 00:59:50 UTC", "10-21 15:59:50 UTC"]);
    }

    fn pnl(status: PnlStatus, net: &str, missing: &[Component]) -> PairFunding {
        let total = Components { net: d(net), ..Default::default() };
        let b = PnlBreakdown { legs: vec![], total, missing: missing.iter().copied().collect(), status: status.clone(), other_cost_included: false };
        PairFunding {
            simulated: false,
            legs: [LegFunding::Received(d("0")), LegFunding::Received(d("0"))],
            opening_fee: None,
            updated_at_ms: None,
            slots: vec![],
            pnl: Some(PnlSummary { event_id: 9, status: status.as_str().into(), reasons: vec![], breakdown: Some(b), comparison: None }),
            alerts: vec![],
            trace: vec![],
        }
    }

    fn finished(f: Option<PairFunding>) -> AttemptView {
        let mut s = snap();
        s.pair_records.insert("u-fin".into(), record("BTCUSDT", "FINALIZED"));
        if let Some(f) = f {
            s.funding.insert("BTCUSDT-1".into(), f);
        }
        s.trade_events = vec![
            ev(2, T_PTB + 900, "PAIR_TRANSITION", "u-fin", json!({"from": "CLOSING", "to": "FINALIZED", "detail": {"simulated": false}})),
            start(1, T_PTB, "u-fin", false),
        ];
        build(&s, &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == "u-fin").unwrap()
    }

    #[test]
    fn a_finished_pair_shows_its_net_pnl_from_the_latest_pnl_event() {
        let a = finished(Some(pnl(PnlStatus::Complete, "1.234", &[])));
        assert_eq!(a.headline, "10-22 12:00:00 UTC · BTCUSDT · 已完成 · 淨損益 +1.23 USDT");
        assert_eq!(a.net_pnl.as_deref(), Some("淨損益 +1.23 USDT"));
        let loss = finished(Some(pnl(PnlStatus::Complete, "-0.82", &[])));
        assert!(loss.headline.ends_with("已完成 · 淨損益 −0.82 USDT"), "{}", loss.headline);
    }

    #[test]
    fn an_incomplete_pnl_says_so_and_a_missing_net_is_a_dash_never_zero() {
        let part = finished(Some(pnl(PnlStatus::Incomplete(vec![]), "0.5", &[])));
        assert!(part.headline.ends_with("已完成 · 淨損益 +0.50 USDT（PnL 不完整）"), "{}", part.headline);
        let none = finished(Some(pnl(PnlStatus::Incomplete(vec![]), "0", &[Component::Net])));
        assert!(none.headline.ends_with("已完成 · 淨損益 —（PnL 不完整）"), "{}", none.headline);
    }

    #[test]
    fn without_a_pnl_or_when_not_finished_or_simulated_no_net_is_shown() {
        let a = finished(None);
        assert_eq!((a.headline.as_str(), a.net_pnl), ("10-22 12:00:00 UTC · BTCUSDT · 已完成", None));
        let mut sim = pnl(PnlStatus::Complete, "9", &[]);
        sim.simulated = true;
        assert_eq!(finished(Some(sim)).net_pnl, None);
        // a blocked pair has no PnL line even if some PnL data exists under its id
        let mut s = ptb();
        s.pair_records.insert(PTB.into(), record("PTBUSDT", "BLOCKED"));
        s.funding.insert("PTBUSDT-1".into(), pnl(PnlStatus::Complete, "9", &[]));
        assert_eq!(build(&s, &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == PTB).unwrap().net_pnl, None);
    }

    #[test]
    fn every_leg_keeps_exchange_side_status_and_the_order_id_as_secondary_text() {
        let mut s = snap();
        eng(&mut s).pairs[0].state = PairState::Reconciled;
        s.trade_events = attempt("u-BTCUSDT", false, json!({"leg": "short", "action": "open", "exchange": "Bybit", "symbol": "BTCUSDT", "simulated": false, "outcome": "accepted", "client_order_id": "demoso0001", "exchange_order_id": "b-77"}), "RECONCILED");
        let a = &build(&s, &BTreeSet::new(), NOW).last_results[0];
        assert_eq!(a.legs[0].summary(), "long · Binance · BUY · 成功");
        assert_eq!(a.legs[1].summary(), "short · Bybit · SELL · 成功");
        assert_eq!(a.legs[0].order_text().as_deref(), Some("訂單 8389765"));
        assert_eq!(a.legs[1].order_text().as_deref(), Some("訂單 b-77"));
        assert_eq!((a.state_text.as_str(), a.headline.ends_with("BTCUSDT · 持倉中")), ("持倉中", true));
        // a leg that was not sent has no order id to show
        let b = build(&ptb(), &BTreeSet::new(), NOW).last_results.into_iter().find(|a| a.uuid == PTB).unwrap();
        assert_eq!(b.legs[0].order_text(), None);
        assert_eq!(b.legs[0].summary(), "long · Binance · BUY · 未送出");
    }
}

// ---- ui-plain-language 3.2: states, modes and triggers on the staged orders page ------------------

mod plain_texts {
    use super::*;

    #[test]
    fn the_header_names_mode_and_trigger_in_plain_words_with_the_code_after_them() {
        let mut s = snap();
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!((vm.mode_text().as_str(), vm.trigger_text().as_str()), ("模擬（SIMULATION）", "手動（MANUAL）"));
        assert_eq!(vm.mode_label(), "SIMULATION", "the code itself is still available");
        eng(&mut s).execution_mode = ExecutionMode::ExchangeDemo;
        eng(&mut s).trigger_mode = TriggerMode::Auto;
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!((vm.mode_text().as_str(), vm.trigger_text().as_str()), ("Demo 帳戶（EXCHANGE_DEMO）", "自動（AUTO）"));
        s.engine = None;
        let vm = build(&s, &BTreeSet::new(), NOW);
        assert_eq!((vm.mode_text().as_str(), vm.trigger_text().as_str()), ("未知", "未知"));
    }

    #[test]
    fn the_not_prepared_reason_and_the_exclusion_note_use_the_plain_state_name() {
        assert_eq!(DisabledReason::NotPrepared(vec!["BTCUSDT".into(), "ETHUSDT".into()]).text(), "已不是「已暫存」狀態：BTCUSDT、ETHUSDT");
        assert_eq!(excluded_note(&["BTCUSDT".into()]), "已排除（已不是「已暫存」狀態）：BTCUSDT");
    }

    #[test]
    fn a_pair_needing_manual_handling_is_titled_with_its_plain_state_and_world() {
        let mut s = snap();
        failed_pair(&mut s);
        let h = build(&s, &BTreeSet::new(), NOW).manual.into_iter().next().unwrap();
        assert_eq!(h.heading(), "BTCUSDT · 單腿失敗（PARTIAL_FAILURE） · Demo 帳戶（EXCHANGE_DEMO）");
    }

    #[test]
    fn the_running_block_is_titled_in_plain_words() {
        assert_eq!(running_title(), "進行中（持倉中）");
    }
}

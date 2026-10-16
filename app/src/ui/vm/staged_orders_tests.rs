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
    assert!(a.headline.starts_with("LIVE"), "{}", a.headline);
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

//! funding-pnl task 4.1: "Funding 收到", settlement timeline, expected-vs-actual panel, alerts.
use serde_json::json;
use tong_funding_core::pair::PairState;
use tong_funding_core::pnl::{Comparison, PnlStatus};
use tong_funding_core::types::Exchange::{Binance, Bybit};

use super::*;
use crate::funding::pnl_record::tests::{T, both_fetched, d, ledger, scenario};
use crate::ui::alerts::{Category, alerts};
use crate::ui::bridge::UiSnapshot;
use crate::ui::nav::Page;
use crate::ui::positions::{Filter, TableView, build};
use crate::ui::testkit::{loaded, pair, position};

const H4: i64 = 4 * 3_600_000;

fn funding(legs: [LegFunding; 2], slots: Vec<TimelineSlot>) -> PairFunding {
    PairFunding { simulated: false, legs, opening_fee: Some(d("0.48")), updated_at_ms: Some(T + 70_000), slots, pnl: None, alerts: Vec::new() }
}

fn slot(t: i64, side: Side, exchange: Exchange, amount: Option<&str>, state: SlotState) -> TimelineSlot {
    TimelineSlot { time_ms: t, side, exchange, amount: amount.map(d), state }
}

#[test]
fn funding_vm_leg_cells_use_the_funding_colours_and_never_show_zero_for_unknown() {
    assert_eq!(leg_cell(Some(&LegFunding::Received(d("-0.12")))), ("−0.12 USDT".to_string(), Tone::Negative));
    assert_eq!(leg_cell(Some(&LegFunding::Received(d("0.36")))), ("+0.36 USDT".to_string(), Tone::Positive));
    assert_eq!(leg_cell(Some(&LegFunding::Received(d("0")))).1, Tone::Muted);
    let (text, tone) = leg_cell(Some(&LegFunding::Unavailable("尚未取得".into())));
    assert_eq!((text.as_str(), tone), ("—（尚未取得）", Tone::Muted));
    assert!(!text.contains("0.00"));
    assert_eq!(leg_cell(None).0, "—（尚未取得）");
    assert_eq!(leg_cell(Some(&LegFunding::Simulated)).0, "模擬");
}

#[test]
fn funding_vm_pair_total_and_running_total_say_close_costs_are_not_included() {
    let f = funding([LegFunding::Received(d("-0.12")), LegFunding::Received(d("0.36"))], vec![]);
    let (text, tone) = pair_funding_text(&f);
    assert!(text.starts_with("Funding 收到 +0.24 USDT（−0.12 USDT / +0.36 USDT）"), "{text}");
    assert!(text.contains("更新"), "shows when it was last updated: {text}");
    assert_eq!(tone, Tone::Positive);
    let r = running_total_text(&f, Some(d("1.00")));
    assert_eq!(r, "已付開倉手續費 0.48 USDT · 進行中合計 +0.76 USDT（尚未包含平倉成本）");
    let unknown = funding([LegFunding::Unavailable("尚未取得".into()), LegFunding::Received(d("0.36"))], vec![]);
    assert!(pair_funding_text(&unknown).0.starts_with("Funding 收到 —（尚未取得）"));
    assert!(running_total_text(&unknown, Some(d("1"))).contains("進行中合計 —（尚未包含平倉成本）"));
}

#[test]
fn funding_vm_timeline_is_in_time_order_with_a_running_total_and_marks_missing() {
    // Binance every 4 h, Bybit every 8 h: the legs settle at different times.
    let f = funding(
        [LegFunding::Received(d("0.24")), LegFunding::Received(d("-0.12"))],
        vec![
            slot(T, Side::Short, Bybit, Some("-0.12"), SlotState::Received),
            slot(T + H4, Side::Long, Binance, None, SlotState::Missing),
            slot(T, Side::Long, Binance, Some("0.36"), SlotState::Received),
        ],
    );
    let rows = timeline_rows(&f);
    assert_eq!(rows.iter().map(|r| r.leg.as_str()).collect::<Vec<_>>(), ["Binance LONG", "Bybit SHORT", "Binance LONG"]);
    assert_eq!(rows.iter().map(|r| r.cumulative.as_str()).collect::<Vec<_>>(), ["+0.36 USDT", "+0.24 USDT", "+0.24 USDT"]);
    assert_eq!(rows[2].state, "缺少（PnL 為 INCOMPLETE）");
    assert_eq!(rows[2].tone, Tone::Warning);
    assert_eq!(rows[2].amount, "—");
    assert!(rows[0].time.ends_with("UTC"));
}

#[test]
fn funding_vm_panel_complete_shows_every_component_and_other_cost_not_included() {
    let mut f = funding([LegFunding::Received(d("-0.12")), LegFunding::Received(d("0.36"))], vec![]);
    let b = tong_funding_core::pnl::PnlBreakdown {
        legs: vec![],
        total: tong_funding_core::pnl::Components {
            funding: d("0.24"),
            price_ref: d("0"),
            price_actual: d("-0.1"),
            opening_fee: d("0.48"),
            closing_fee: d("0.48"),
            slippage: d("0.1"),
            other_cost: d("0"),
            net: d("-0.82"),
        },
        missing: Default::default(),
        status: PnlStatus::Complete,
        other_cost_included: false,
    };
    f.pnl = Some(PnlSummary { event_id: 7, status: "COMPLETE".into(), reasons: vec![], breakdown: Some(b), comparison: Some(Comparison::NoSnapshot) });
    let p = pnl_panel(&f).unwrap();
    assert_eq!(p.status_line, None);
    let get = |k: &str| p.breakdown.iter().find(|(l, _)| l == k).unwrap().1.clone();
    assert_eq!(get("Net PnL"), "−0.82 USDT");
    assert_eq!(get("其他成本"), "0.00（未納入）");
    assert!(p.lines.iter().all(|l| l.expected == "無預期快照"), "{:?}", p.lines);
}

#[test]
fn funding_vm_panel_incomplete_lists_reasons_on_top_and_dashes_missing_parts() {
    let mut f = funding([LegFunding::Unavailable("尚未取得".into()), LegFunding::Received(d("0.36"))], vec![]);
    let mut missing = std::collections::BTreeSet::new();
    missing.insert(tong_funding_core::pnl::Component::Funding);
    missing.insert(tong_funding_core::pnl::Component::Net);
    let b = tong_funding_core::pnl::PnlBreakdown { legs: vec![], total: Default::default(), missing, status: PnlStatus::Incomplete(vec![]), other_cost_included: false };
    f.pnl = Some(PnlSummary { event_id: 9, status: "INCOMPLETE".into(), reasons: vec!["缺少結算流水".into()], breakdown: Some(b), comparison: None });
    let p = pnl_panel(&f).unwrap();
    assert_eq!(p.status_line.as_deref(), Some("INCOMPLETE · 缺少結算流水"));
    assert_eq!(p.breakdown[0], ("Funding PnL".to_string(), "—".to_string()));
    assert_eq!(p.breakdown.last().unwrap().1, "—");
}

#[test]
fn funding_vm_simulated_pairs_show_simulated_and_no_actual_column() {
    let f = PairFunding { simulated: true, legs: [LegFunding::Simulated, LegFunding::Simulated], opening_fee: None, updated_at_ms: None, slots: vec![], pnl: None, alerts: vec![] };
    assert_eq!(pair_funding_text(&f).0, "Funding 收到 模擬");
    let p = pnl_panel(&f).unwrap();
    assert_eq!((p.mode.as_str(), p.status_line.as_deref()), ("SIMULATION", Some("模擬，無實際 PnL")));
    assert!(p.lines.is_empty() && p.breakdown.is_empty());
}

fn positions_snapshot(f: PairFunding) -> UiSnapshot {
    let mut s = UiSnapshot::default();
    s.accounts.insert(Binance, loaded(vec![], vec![position(Binance, "BTCUSDT", "0.02", "60000", "60200", "3", "4", Some("400"))], T));
    s.accounts.insert(Bybit, loaded(vec![], vec![position(Bybit, "BTCUSDT", "-0.02", "60000", "60200", "3", "-4", Some("400"))], T));
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled)];
    s.funding.insert("p1".into(), f);
    s
}

#[test]
fn funding_vm_positions_rows_and_card_show_funding_received() {
    let snap = positions_snapshot(funding([LegFunding::Received(d("-0.12")), LegFunding::Received(d("0.36"))], vec![]));
    let vm = build(&snap, &Filter::default());
    let TableView::Rows(rows) = &vm.table else { panic!() };
    let cell = |e| rows.iter().find(|r| r.exchange == e).unwrap();
    assert_eq!((cell(Binance).cells()[8].as_str(), cell(Binance).funding_tone), ("−0.12 USDT", Tone::Negative));
    assert_eq!((cell(Bybit).cells()[8].as_str(), cell(Bybit).funding_tone), ("+0.36 USDT", Tone::Positive));
    let card = &vm.pair_cards[0];
    assert!(card.funding.text.starts_with("Funding 收到 +0.24 USDT"), "{}", card.funding.text);
    assert!(card.funding.running_total.ends_with("（尚未包含平倉成本）"));
    assert_eq!(crate::ui::positions::PNL_NOTE, "價差，未含 funding 與手續費");
}

#[test]
fn funding_vm_alerts_reach_the_banner_with_a_link_and_cannot_be_dismissed() {
    let mut f = funding([LegFunding::Received(d("-0.12")), LegFunding::Received(d("0.36"))], vec![]);
    f.alerts.push(FundingAlert { event_id: 42, event_type: PNL_RECONCILIATION.into(), text: "BTCUSDT 對帳 MISMATCH".into() });
    let snap = positions_snapshot(f);
    let a: Vec<_> = alerts(&snap, T).into_iter().filter(|a| a.category == Category::FundingData).collect();
    assert_eq!(a.len(), 1);
    assert!(a[0].message.contains("事件 #42"), "{}", a[0].message);
    assert_eq!(a[0].link, Some(Page::SystemLogs));
    assert!(!a[0].dismissible());
    // Without data problems there is no such alert.
    let clean = positions_snapshot(funding([LegFunding::Received(d("0")), LegFunding::Received(d("0"))], vec![]));
    assert!(alerts(&clean, T).iter().all(|a| a.category != Category::FundingData));
}

#[test]
fn funding_vm_loader_reads_the_store() {
    let fx = scenario(true);
    fx.db.write_funding_ledger(&[ledger(Binance, "BTCUSDT", "1", "-0.12", T), ledger(Bybit, "BTCUSDT", "2", "0.36", T)]).unwrap();
    both_fetched(&fx);
    let rows = fx.db.list_pairs().unwrap();
    let m = load_pair_funding(&fx.db, &rows, T + 80_000);
    let f = &m["pid-p1"];
    assert_eq!(f.legs, [LegFunding::Received(d("-0.12")), LegFunding::Received(d("0.36"))]);
    assert_eq!(f.opening_fee, Some(d("0.48")));
    assert_eq!(f.slots.len(), 2);
    assert!(f.slots.iter().all(|s| s.state == SlotState::Received));
    assert!(f.pnl.is_none() && f.alerts.is_empty());
    // A failed fetch is an alert until a later complete fetch of that exchange.
    fx.clock.set(T + 90_000);
    crate::store::events::EventStore::new(fx.db.clone())
        .append(crate::funding::FETCH_ERROR, None, json!({ "exchange": "Bybit", "symbol": null, "outcome": "incomplete" }))
        .unwrap();
    let m = load_pair_funding(&fx.db, &rows, T + 95_000);
    assert_eq!(m["pid-p1"].alerts.len(), 1);
    fx.clock.set(T + 100_000);
    crate::store::events::EventStore::new(fx.db.clone())
        .append(crate::funding::FUNDING_LEDGER_FETCHED, None, json!({ "exchange": "Bybit", "symbol": null, "start_ms": T - 10_000, "end_ms": T + 15_000, "outcome": "complete" }))
        .unwrap();
    assert!(load_pair_funding(&fx.db, &rows, T + 105_000)["pid-p1"].alerts.is_empty());
}

#[test]
fn funding_vm_loader_without_a_fetch_shows_not_fetched() {
    let fx = scenario(true);
    let rows = fx.db.list_pairs().unwrap();
    let f = &load_pair_funding(&fx.db, &rows, T + 80_000)["pid-p1"];
    assert_eq!(f.legs[0], LegFunding::Unavailable("尚未取得".into()));
    assert_eq!(leg_cell(Some(&f.legs[0])).0, "—（尚未取得）");
}

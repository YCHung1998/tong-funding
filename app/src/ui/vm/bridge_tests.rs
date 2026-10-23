//! Task 1.1 (scanner-page spec "表格重算有頻率上限"): coalescing and snapshot merging.
use tong_funding_core::types::Exchange;

use super::*;
use crate::identity::ProcessIdentity;
use crate::ui::testkit::obs;

fn ws_frame(symbol: &str, rate: &str, at: i64) -> SourceUpdate {
    SourceUpdate::MarketPartial { exchange: Exchange::Binance, observations: vec![obs(Exchange::Binance, symbol, rate, 28_800, 9_000_000, at)], at }
}

/// Drives the bridge like the UI loop does: polls every 100 ms and counts recomputes.
fn run(bridge: &mut Bridge, updates: &[(i64, SourceUpdate)], until_ms: i64) -> Vec<(i64, UiSnapshot)> {
    let mut out = Vec::new();
    let mut pending = updates.iter().peekable();
    let mut t = 0;
    while t <= until_ms {
        while let Some((at, u)) = pending.peek() {
            if *at > t {
                break;
            }
            bridge.push(u.clone());
            pending.next();
        }
        if let Some(s) = bridge.take_if_due(t) {
            out.push((t, s));
        }
        t += 100;
    }
    out
}

#[test]
fn five_updates_in_one_second_recompute_at_most_twice_and_keep_all_data() {
    // WebSocket pushes 5 frames within one second, each for a different symbol.
    let updates: Vec<_> = (0..5).map(|i| (i * 200 + 50, ws_frame(&format!("S{i}USDT"), "0.0001", i * 200 + 50))).collect();
    let mut bridge = Bridge::default();
    let runs = run(&mut bridge, &updates, 999);
    assert!(!runs.is_empty(), "the first update must be shown");
    assert!(runs.len() <= 2, "at most 2 recomputes in the second, got {}", runs.len());
    // After the second ends, the pending change is flushed and contains all five updates.
    let later = bridge.take_if_due(1_500).or_else(|| runs.last().map(|r| r.1.clone())).unwrap();
    let feed = &later.market[&Exchange::Binance];
    assert_eq!(feed.observations.len(), 5, "every update is merged, none dropped");
    assert_eq!(later.market_updates, 5);
}

#[test]
fn recomputes_are_at_least_half_a_second_apart() {
    let updates: Vec<_> = (0..30).map(|i| (i * 100, ws_frame("BTCUSDT", "0.0001", i * 100))).collect();
    let mut bridge = Bridge::default();
    let runs = run(&mut bridge, &updates, 2_999);
    for w in runs.windows(2) {
        assert!(w[1].0 - w[0].0 >= 500, "recomputes at {} and {}", w[0].0, w[1].0);
    }
    assert!(runs.len() <= 6, "3 s at 2 Hz: {}", runs.len());
}

#[test]
fn only_time_passing_never_recomputes() {
    let mut bridge = Bridge::default();
    bridge.push(ws_frame("BTCUSDT", "0.0001", 0));
    assert!(bridge.take_if_due(0).is_some());
    let before = bridge.recomputes();
    for t in (1_000..=10_000).step_by(1_000) {
        assert!(bridge.take_if_due(t).is_none(), "no data change at {t}");
    }
    assert_eq!(bridge.recomputes(), before);
    assert_eq!(bridge.next_due(), None);
}

#[test]
fn partial_updates_replace_only_their_symbols_and_full_updates_replace_all() {
    let mut s = UiSnapshot::default();
    apply_update(&mut s, SourceUpdate::Market { exchange: Exchange::Bybit, observations: vec![obs(Exchange::Bybit, "A", "0.1", 28_800, 1, 1), obs(Exchange::Bybit, "B", "0.1", 28_800, 1, 1)], at: 1 });
    apply_update(&mut s, SourceUpdate::MarketPartial { exchange: Exchange::Bybit, observations: vec![obs(Exchange::Bybit, "B", "0.2", 28_800, 1, 2)], at: 2 });
    let f = &s.market[&Exchange::Bybit];
    assert_eq!(f.observations.len(), 2);
    assert_eq!(f.observations[1].funding_rate.to_string(), "0.2");
    assert_eq!(f.last_success_at, Some(2));
    apply_update(&mut s, SourceUpdate::Market { exchange: Exchange::Bybit, observations: vec![obs(Exchange::Bybit, "C", "0.1", 28_800, 1, 3)], at: 3 });
    assert_eq!(s.market[&Exchange::Bybit].observations.len(), 1);
}

#[test]
fn a_failed_fetch_keeps_the_old_observations_and_marks_the_feed_failing() {
    let mut s = UiSnapshot::default();
    apply_update(&mut s, SourceUpdate::Market { exchange: Exchange::Okx, observations: vec![obs(Exchange::Okx, "A", "0.1", 28_800, 1, 1)], at: 1_000 });
    assert!(!s.market[&Exchange::Okx].failing());
    apply_update(&mut s, SourceUpdate::MarketError { exchange: Exchange::Okx, error: "request timed out".into(), at: 2_000 });
    let f = &s.market[&Exchange::Okx];
    assert_eq!(f.observations.len(), 1, "old data kept");
    assert!(f.failing());
    assert_eq!(f.last_success_at, Some(1_000));
}

#[test]
fn every_exchange_is_tradable_and_has_an_account_and_unknown_accounts_are_loading() {
    // okx-trading-enablement: OKX is a tradable, account-backed exchange like the other two
    assert_eq!(TRADABLE_EXCHANGES.to_vec(), vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx]);
    assert_eq!(ACCOUNT_EXCHANGES.to_vec(), vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx]);
    assert!(is_tradable(Exchange::Okx));
    let s = UiSnapshot::default();
    assert_eq!(s.account(Exchange::Okx), AccountState::Loading, "not read yet is loading, never public-data-only");
    assert_eq!(s.account(Exchange::Binance), AccountState::Loading);
    assert_eq!(s.clock(Exchange::Bybit), ClockState::Unsynced);
}

fn book(bid: &str, ask: &str, at: i64) -> tong_funding_core::trade_cost::TopOfBook {
    let d = |s: &str| s.parse().unwrap();
    tong_funding_core::trade_cost::TopOfBook { bid_price: d(bid), bid_qty: d("1"), ask_price: d(ask), ask_qty: d("1"), observed_at: at }
}

#[test]
fn books_update_replaces_on_success_and_keeps_old_quotes_on_failure() {
    let mut s = UiSnapshot::default();
    let ok = |at: i64, ask: &str| SourceUpdate::Books { exchange: Exchange::Bybit, books: Ok([("BTCUSDT".to_string(), book("99", ask, at))].into()), at };
    apply_update(&mut s, ok(1_000, "100"));
    assert_eq!(s.books[&Exchange::Bybit].books["BTCUSDT"].ask_price, "100".parse().unwrap());
    apply_update(&mut s, SourceUpdate::Books { exchange: Exchange::Bybit, books: Err("429".into()), at: 2_000 });
    let f = &s.books[&Exchange::Bybit];
    assert_eq!(f.books["BTCUSDT"].ask_price, "100".parse().unwrap(), "old quote kept, with its own observed_at");
    assert_eq!(f.books["BTCUSDT"].observed_at, 1_000);
    assert_eq!(f.last_error, Some(("429".to_string(), 2_000)));
    apply_update(&mut s, ok(3_000, "101"));
    let f = &s.books[&Exchange::Bybit];
    assert_eq!(f.books["BTCUSDT"].ask_price, "101".parse().unwrap());
    assert!(f.last_error.is_none());
    assert!(SourceUpdate::Books { exchange: Exchange::Okx, books: Err("x".into()), at: 0 }.is_market(), "quotes trigger the recompute too");
}

#[test]
fn the_okx_disabled_reason_is_kept_until_it_changes() {
    let mut s = UiSnapshot::default();
    assert_eq!(s.okx_disabled, None);
    apply_update(&mut s, SourceUpdate::OkxDisabled("OKX 50101: ...".into()));
    assert_eq!(s.okx_disabled.as_deref(), Some("OKX 50101: ..."));
}

#[test]
fn the_bybit_websocket_switch_is_kept() {
    let mut s = UiSnapshot::default();
    assert!(!s.bybit_ws_disabled);
    apply_update(&mut s, SourceUpdate::BybitWsDisabled);
    assert!(s.bybit_ws_disabled);
}

// ---- live-banners: the process identity reaches the snapshot from the first one on ------------

#[test]
fn a_bridge_started_with_an_identity_hands_it_to_every_snapshot() {
    for identity in [ProcessIdentity::Demo, ProcessIdentity::Live] {
        let mut bridge = Bridge::with_identity(identity);
        let runs = run(&mut bridge, &[(0, ws_frame("BTCUSDT", "0.0001", 0))], 1_000);
        assert!(!runs.is_empty());
        assert!(runs.iter().all(|(_, s)| s.identity == identity), "{identity:?}");
    }
}

#[test]
fn the_default_snapshot_is_demo() {
    assert_eq!(UiSnapshot::default().identity, ProcessIdentity::Demo);
}

// ---- live-guardrails G: the live status and the three live commands reach the pages ----------

#[test]
fn the_default_snapshot_has_an_empty_live_status() {
    let live = UiSnapshot::default().live;
    assert_eq!(live, LiveStatusView::default());
    assert!(!live.is_live && !live.armed && live.preflight.is_none() && live.frozen.is_none());
}

#[test]
fn a_live_status_update_replaces_the_view_and_the_view_mirrors_the_engine_status() {
    use crate::engine::live_guard::LiveStatus;
    let mut snap = UiSnapshot::default();
    let status = LiveStatus { is_live: true, armed: true, pairs_entered: 2, preflight_running: true, last_lock_reason: Some("why".into()), ..LiveStatus::default() };
    apply_update(&mut snap, SourceUpdate::LiveStatus(LiveStatusView::from(&status)));
    assert!(snap.live.is_live && snap.live.armed && snap.live.preflight_running);
    assert_eq!((snap.live.pairs_entered, snap.live.last_lock_reason.as_deref()), (2, Some("why")));
    apply_update(&mut snap, SourceUpdate::LiveStatus(LiveStatusView::default()));
    assert_eq!(snap.live, LiveStatusView::default());
}

#[test]
fn the_default_command_sink_methods_send_exactly_the_live_commands() {
    use crate::engine::ports::Leg;
    use std::cell::RefCell;
    struct Rec(RefCell<Vec<(String, Command)>>);
    impl CommandSink for Rec {
        fn send(&self, label: String, command: Command) {
            self.0.borrow_mut().push((label, command));
        }
    }
    let s = Rec(RefCell::new(Vec::new()));
    s.run_preflight("preflight".into());
    s.arm("arm".into(), "pf-1".into(), "hash".into(), "confirm".into(), true);
    s.flatten_leg("flatten".into(), "pair-1".into(), Leg::Short);
    let sent = s.0.borrow();
    assert_eq!(sent[0], ("preflight".to_string(), Command::RunPreflight));
    assert_eq!(sent[1], ("arm".to_string(), Command::Arm { preflight_id: "pf-1".into(), caps_hash: "hash".into(), confirmation: "confirm".into(), checklist_ack: true }));
    assert_eq!(sent[2], ("flatten".to_string(), Command::FlattenLeg { pair: "pair-1".into(), leg: Leg::Short }));
}

// ---- ui-plain-language: which exchanges the risk settings leave enabled ---------------------------

#[test]
fn an_exchange_is_enabled_when_allowed_exchanges_lists_it_and_unreadable_settings_enable_all() {
    let mut s = UiSnapshot::default();
    assert!(Exchange::ALL.iter().all(|e| s.exchange_enabled(*e)), "the default allows every exchange");
    s.settings.risk.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit];
    assert!(s.exchange_enabled(Exchange::Binance) && s.exchange_enabled(Exchange::Bybit));
    assert!(!s.exchange_enabled(Exchange::Okx), "OKX unticked");
    // settings that could not be read are not read as "everything unticked": nothing is hidden on a guess
    s.settings.error = Some("config.risk is not valid JSON".into());
    assert!(Exchange::ALL.iter().all(|e| s.exchange_enabled(*e)));
}

#[test]
fn stored_pair_records_replace_the_previous_ones() {
    let rec = |sym: &str| PairRecord { pair_id: "p".into(), symbol: sym.into(), long_exchange: Some(Exchange::Binance), short_exchange: None, status: "BLOCKED".into() };
    let mut s = UiSnapshot::default();
    apply_update(&mut s, SourceUpdate::PairRecords([("u1".to_string(), rec("A"))].into()));
    apply_update(&mut s, SourceUpdate::PairRecords([("u2".to_string(), rec("B"))].into()));
    assert_eq!(s.pair_records.keys().collect::<Vec<_>>(), ["u2"]);
}

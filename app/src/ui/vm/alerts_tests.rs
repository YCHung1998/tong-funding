//! Task 3.1: alert generation (spec alert-banner).
use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange::{self, Binance, Bybit, Okx};

use super::*;
use crate::exchange::health::feed::HealthSnapshot;
use crate::ui::bridge::{ClockState, KillSwitchState, SourceHealth, SourceId, SystemFlags, UiSnapshot};
use crate::ui::testkit::{loaded, pair};

const NOW: i64 = 1_791_201_600_000;

fn hs(source: SourceId, connected: bool, last_success_ago: Option<i64>, period: i64) -> SourceHealth {
    let threshold = 1_000_i64.max(3 * period);
    let stale = !connected || last_success_ago.is_none_or(|a| a > threshold);
    SourceHealth {
        source,
        health: Some(HealthSnapshot {
            source: format!("{source:?}"),
            connected,
            last_success_at: last_success_ago.map(|a| NOW - a),
            consecutive_failures: 0,
            expected_period_ms: period,
            stale_threshold_ms: threshold,
            stale,
        }),
        next_poll_at: None,
        rate_limited_until: None,
    }
}

/// Everything healthy: every alert in a test comes from what the test changes.
fn healthy() -> UiSnapshot {
    let mut s = UiSnapshot::default();
    s.health = vec![
        hs(SourceId::BinanceWs, true, Some(500), 1_000),
        hs(SourceId::BybitWs, true, Some(500), 1_000),
        hs(SourceId::MarketPoll(Binance), true, Some(1_000), 10_000),
        hs(SourceId::MarketPoll(Bybit), true, Some(1_000), 10_000),
        hs(SourceId::MarketPoll(Okx), true, Some(1_000), 10_000),
        hs(SourceId::Account(Binance), true, Some(1_000), 30_000),
        hs(SourceId::Account(Bybit), true, Some(1_000), 30_000),
        hs(SourceId::Account(Okx), true, Some(1_000), 30_000),
    ];
    for e in [Binance, Bybit, Okx] {
        s.accounts.insert(e, loaded(vec![], vec![], NOW));
        s.clocks.insert(e, ClockState::Synced { offset_ms: 10 });
    }
    s
}

fn set_health(s: &mut UiSnapshot, h: SourceHealth) {
    s.health.retain(|x| x.source != h.source);
    s.health.push(h);
}

#[test]
fn a_healthy_system_has_no_alerts() {
    assert_eq!(alerts(&healthy(), NOW), vec![]);
}

#[test]
fn manual_attention_sorts_before_stale_data() {
    let mut s = healthy();
    set_health(&mut s, hs(SourceId::MarketPoll(Bybit), true, Some(31_000), 10_000));
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::PartialFailure)];
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].category, Category::ManualAttention);
    assert_eq!(a[1].category, Category::Stale);
}

#[test]
fn each_locked_state_gives_one_manual_alert_with_pair_details_and_a_positions_link() {
    for st in [PairState::PartialFailure, PairState::Imbalanced, PairState::Unresolved] {
        let mut s = healthy();
        s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, st)];
        let a = alerts(&s, NOW);
        assert_eq!(a.len(), 1, "{st}");
        assert_eq!(a[0].message, format!("BTCUSDT · Binance / Bybit · {} · {MANUAL_TEXT}", st.as_str()));
        assert_eq!(a[0].link, Some(Page::Positions));
        assert!(!a[0].dismissible(), "manual-attention alerts cannot be closed");
    }
}

#[test]
fn the_alert_disappears_when_the_pair_leaves_the_locked_state() {
    let mut s = healthy();
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::PartialFailure)];
    assert_eq!(alerts(&s, NOW).len(), 1);
    s.pairs[0].state = Ok(PairState::Finalized);
    assert_eq!(alerts(&s, NOW), vec![]);
}

#[test]
fn unreadable_pair_state_is_an_anomaly() {
    let mut s = healthy();
    let mut p = pair("p1", "BTCUSDT", Binance, Bybit, PairState::Reconciled);
    p.state = Err("unknown pair state: \"WEIRD\"".into());
    s.pairs = vec![p];
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].category, Category::ManualAttention);
}

#[test]
fn the_same_source_is_reported_once() {
    let mut s = healthy();
    set_health(&mut s, hs(SourceId::MarketPoll(Bybit), true, Some(31_000), 10_000));
    s.health.push(hs(SourceId::MarketPoll(Bybit), true, Some(40_000), 10_000));
    let stale: Vec<_> = alerts(&s, NOW).into_iter().filter(|a| a.category == Category::Stale).collect();
    assert_eq!(stale.len(), 1);
}

#[test]
fn same_input_same_output_and_order() {
    let mut s = healthy();
    set_health(&mut s, hs(SourceId::MarketPoll(Okx), false, Some(5_000), 10_000));
    set_health(&mut s, hs(SourceId::MarketPoll(Bybit), true, Some(60_000), 10_000));
    s.pairs = vec![pair("p2", "ETHUSDT", Bybit, Binance, PairState::Unresolved), pair("p1", "BTCUSDT", Binance, Bybit, PairState::Imbalanced)];
    s.system.kill_switch = KillSwitchState::On;
    assert_eq!(alerts(&s, NOW), alerts(&s, NOW));
    let cats: Vec<_> = alerts(&s, NOW).iter().map(|a| a.category).collect();
    let mut sorted = cats.clone();
    sorted.sort();
    assert_eq!(cats, sorted);
}

#[test]
fn polling_source_within_its_threshold_is_not_stale() {
    let mut s = healthy();
    set_health(&mut s, hs(SourceId::MarketPoll(Bybit), true, Some(12_000), 10_000));
    assert_eq!(alerts(&s, NOW), vec![]);
}

#[test]
fn polling_source_beyond_threshold_shows_its_age() {
    let mut s = healthy();
    set_health(&mut s, hs(SourceId::MarketPoll(Bybit), true, Some(31_000), 10_000));
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].message, "Bybit 行情已 31 秒未更新");
    assert_eq!(a[0].age_ms, Some(31_000));
}

#[test]
fn websocket_disconnect_is_immediate() {
    let mut s = healthy();
    set_health(&mut s, hs(SourceId::BinanceWs, false, Some(200), 1_000));
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].category, Category::Disconnected);
    assert_eq!(a[0].message, "Binance WebSocket 已斷線");
}

#[test]
fn unknown_health_is_shown_not_hidden() {
    let mut s = healthy();
    s.health.retain(|h| h.source != SourceId::MarketPoll(Okx));
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(a[0].message, "OKX 行情 健康狀態未知");
    let mut s = healthy();
    s.health.iter_mut().find(|h| h.source == SourceId::MarketPoll(Okx)).unwrap().health = None;
    assert_eq!(alerts(&s, NOW).len(), 1);
}

#[test]
fn halts_show_their_reason_and_time() {
    let mut s = healthy();
    s.system = SystemFlags { store_halt: Some(("cannot open database: disk I/O error".into(), NOW - 60_000)), kill_switch: KillSwitchState::Off };
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].category, Category::Halted);
    assert!(a[0].message.contains("cannot open database: disk I/O error"), "{}", a[0].message);
    assert!(a[0].message.contains("11:59:00 UTC"), "{}", a[0].message);
    assert!(!a[0].dismissible());

    let mut s = healthy();
    s.system.kill_switch = KillSwitchState::On;
    assert!(alerts(&s, NOW)[0].message.contains("kill switch 已啟用"));
    s.system.kill_switch = KillSwitchState::Unknown;
    assert!(alerts(&s, NOW)[0].message.contains("kill switch 無法讀取"));
}

#[test]
fn unconnected_account_is_a_disconnected_alert_and_okx_is_not() {
    let mut s = healthy();
    s.accounts.insert(Bybit, AccountState::NotConnected { reason: "金鑰不存在".into() });
    let a = alerts(&s, NOW);
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(a[0].message, "Bybit 帳戶未連線：金鑰不存在");
    assert_eq!(a[0].category, Category::Disconnected);
}

#[test]
fn rate_limit_unsynced_clock_and_large_skew() {
    let mut s = healthy();
    let mut h = hs(SourceId::MarketPoll(Bybit), true, Some(1_000), 10_000);
    h.rate_limited_until = Some(NOW + 20_000);
    set_health(&mut s, h);
    s.clocks.insert(Binance, ClockState::Unsynced);
    s.clocks.insert(Bybit, ClockState::Synced { offset_ms: 3_000 });
    let msgs: Vec<_> = alerts(&s, NOW).into_iter().map(|a| (a.category, a.message)).collect();
    assert!(msgs.contains(&(Category::RateLimitOrClock, "Binance 時鐘未校時".into())), "{msgs:?}");
    assert!(msgs.contains(&(Category::RateLimitOrClock, "Bybit 校時偏移 3,000 ms 過大".into())), "{msgs:?}");
    assert!(msgs.contains(&(Category::RateLimitOrClock, "Bybit 限流退避中，剩 20 秒".into())), "{msgs:?}");
}

#[test]
fn freshness_verdict_matches_the_banner() {
    let mut s = healthy();
    let h = hs(SourceId::MarketPoll(Bybit), true, Some(31_000), 10_000);
    set_health(&mut s, h.clone());
    assert_eq!(source_status(&h, NOW), FreshStatus::Stale);
    assert!(alerts(&s, NOW).iter().any(|a| a.category == Category::Stale && a.source == SourceId::MarketPoll(Bybit).label()));
    assert_eq!(source_status(&hs(SourceId::BinanceWs, false, None, 1_000), NOW), FreshStatus::Offline);
    let unknown = SourceHealth { source: SourceId::BinanceWs, health: None, next_poll_at: None, rate_limited_until: None };
    assert_eq!(source_status(&unknown, NOW), FreshStatus::Unknown);
    let _: Exchange = Okx;
}

#[test]
fn a_tripped_okx_latch_is_a_severe_banner_that_cannot_be_dismissed() {
    // okx-trading-enablement 3.6: `OkxLatch::reason()` reaches the screen
    let mut s = healthy();
    assert!(alerts(&s, NOW).iter().all(|a| !a.message.contains("OKX 已停用")));
    s.okx_disabled = Some("OKX 50101: API key does not match the environment (demo flag sent); OKX disabled until restart".into());
    let a = alerts(&s, NOW);
    let okx = a.iter().find(|a| a.message.contains("OKX 已停用")).expect("an OKX banner");
    assert_eq!(okx.category, Category::Halted);
    assert!(!okx.dismissible());
    assert!(okx.message.contains("50101") && okx.message.contains("重新啟動"), "{}", okx.message);
}

#[test]
fn okx_is_alerted_like_the_other_exchanges_while_it_is_allowed_and_silent_when_it_is_not() {
    let mut s = healthy();
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    s.clocks.insert(Okx, ClockState::Unsynced);
    let msgs: Vec<_> = alerts(&s, NOW).into_iter().map(|a| a.message).collect();
    assert!(msgs.contains(&"OKX 帳戶未連線：Keychain 沒有 API key".to_string()), "{msgs:?}");
    assert!(msgs.contains(&"OKX 時鐘未校時".to_string()), "{msgs:?}");
    // OKX taken out of allowed_exchanges: it is not traded, so its keys and clock are not an alert
    s.settings.risk.allowed_exchanges = vec![Binance, Bybit];
    let msgs: Vec<_> = alerts(&s, NOW).into_iter().map(|a| a.message).collect();
    assert!(msgs.iter().all(|m| !m.contains("OKX")), "{msgs:?}");
}

#[test]
fn an_unticked_okx_with_an_unfinished_pair_is_alerted_until_the_pair_is_finished() {
    // the risk page lets OKX be unticked while a pair is open on it: its lost connection must still show
    let mut s = healthy();
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    s.settings.risk.allowed_exchanges = vec![Binance, Bybit];
    s.pairs = vec![crate::ui::testkit::pair("p1", "ETHUSDT", Binance, Okx, PairState::Reconciled)];
    let msgs: Vec<_> = alerts(&s, NOW).into_iter().map(|a| a.message).collect();
    assert!(msgs.contains(&"OKX 帳戶未連線：Keychain 沒有 API key".to_string()), "{msgs:?}");
    // a stored (not yet finished) record counts too, a finished one does not
    s.pairs.clear();
    s.pair_records = [("u1".to_string(), crate::ui::bridge::PairRecord { pair_id: "p1".into(), symbol: "ETHUSDT".into(), long_exchange: Some(Okx), short_exchange: Some(Bybit), status: "CLOSING".into() })].into();
    assert!(alerts(&s, NOW).iter().any(|a| a.message.contains("OKX 帳戶未連線")));
    s.pair_records.get_mut("u1").unwrap().status = "FINALIZED".into();
    assert!(alerts(&s, NOW).iter().all(|a| !a.message.contains("OKX")));
}

#[test]
fn an_account_the_system_cannot_use_by_its_setting_is_a_notice_not_a_disconnect() {
    // The OKX account is in spot mode (acctLv 1): not a lost connection, nothing the banner can fix.
    let mut s = healthy();
    s.accounts.insert(Okx, AccountState::Failed { error: "exchange error acct_mode_unsupported: 帳戶模式不支援 (acctLv 1, posMode net_mode)".into(), at: NOW - 5_000 });
    let a = alerts(&s, NOW);
    let okx = a.iter().find(|a| a.message.contains("acct_mode_unsupported")).expect("an OKX alert");
    assert_eq!(okx.category, Category::Setting);
    assert_eq!(okx.category.tier(), Tier::Notice);
    assert!(okx.dismissible());
    assert!(a.iter().all(|a| !(a.category == Category::Disconnected && a.message.contains("OKX 帳戶讀取失敗"))));
    // A genuine read failure stays a disconnect (critical).
    s.accounts.insert(Okx, AccountState::Failed { error: "timeout".into(), at: NOW });
    let a = alerts(&s, NOW);
    assert!(a.iter().any(|a| a.category == Category::Disconnected && a.message.contains("OKX 帳戶讀取失敗")));
}

#[test]
fn every_category_has_a_tier_and_a_code_that_reads_back() {
    use Category::*;
    for c in [ManualAttention, Halted, Disconnected, Stale, FundingData, Setting, RateLimitOrClock] {
        assert_eq!(Category::from_code(c.code()), Some(c), "{c:?}");
    }
    assert_eq!(Category::from_code("nope"), None);
    let tiers: Vec<_> = [ManualAttention, Halted, Disconnected, Stale, RateLimitOrClock, Setting, FundingData].map(Category::tier).to_vec();
    assert_eq!(tiers, [Tier::Critical, Tier::Critical, Tier::Critical, Tier::Notice, Tier::Notice, Tier::Notice, Tier::Review]);
}

// ---- bybit-ws-feed: the Bybit WebSocket is a notice, never a banner row --------------------------

const BYBIT_WS_DOWN: &str = "Bybit WebSocket 已斷線，行情退回 REST 輪詢（約每 10 秒更新）";

#[test]
fn a_down_bybit_websocket_is_one_notice_that_says_the_rest_poll_took_over() {
    for h in [hs(SourceId::BybitWs, false, Some(200), 1_000), hs(SourceId::BybitWs, true, Some(5_000), 1_000), hs(SourceId::BybitWs, true, None, 1_000)] {
        let mut s = healthy();
        set_health(&mut s, h);
        let a = alerts(&s, NOW);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!((a[0].category, a[0].message.as_str()), (Category::Stale, BYBIT_WS_DOWN));
        let attn = crate::ui::banner::attention(&a, &Default::default(), NOW);
        assert!(attn.critical.is_empty(), "not a banner row");
        assert_eq!(attn.notices.len(), 1);
    }
    let mut unknown = healthy();
    unknown.health.retain(|h| h.source != SourceId::BybitWs);
    let a = alerts(&unknown, NOW);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].message, BYBIT_WS_DOWN);
}

#[test]
fn a_healthy_disabled_or_not_allowed_bybit_websocket_has_no_alert() {
    assert_eq!(alerts(&healthy(), NOW), vec![]);
    let mut off = healthy();
    off.health.retain(|h| h.source != SourceId::BybitWs);
    off.bybit_ws_disabled = true;
    assert_eq!(alerts(&off, NOW), vec![], "switched to rest on purpose");
    let mut not_allowed = healthy();
    not_allowed.health.retain(|h| h.source != SourceId::BybitWs);
    not_allowed.settings.risk.allowed_exchanges = vec![Binance, Okx];
    assert_eq!(alerts(&not_allowed, NOW), vec![]);
}

#[test]
fn the_bybit_websocket_has_its_own_source_id_and_belongs_to_bybit() {
    assert_eq!(SourceId::BybitWs.label(), "Bybit WebSocket");
    assert_eq!(SourceId::BybitWs.exchange(), Bybit);
}

// ---- ui-clarity red-team E1 / E3 ----

fn lost_connection(positions: Vec<crate::exchange::signed::models::Position>, ex: Exchange) -> AccountState {
    // what the live source now keeps when a poll finds the account not connected
    let mut a = loaded(vec![], positions, NOW - 5_000);
    if let AccountState::Loaded { error, .. } = &mut a {
        *error = Some((format!("{}Keychain 沒有 API key", crate::ui::bridge::NOT_CONNECTED_ERROR_PREFIX), NOW));
    }
    let _ = ex;
    a
}

#[test]
fn an_unticked_okx_that_held_a_position_keeps_its_alerts_after_it_turns_not_connected() {
    let mut s = healthy();
    s.settings.risk.allowed_exchanges = vec![Binance, Bybit];
    s.accounts.insert(Okx, lost_connection(vec![crate::ui::testkit::position(Okx, "ETHUSDT", "-0.07", "1500", "1500", "3", "0", None)], Okx));
    s.clocks.insert(Okx, ClockState::Unsynced);
    let a = alerts(&s, NOW);
    let disconnected = a.iter().find(|a| a.category == Category::Disconnected && a.source == SourceId::Account(Okx).label()).expect("a lost connection to an exchange holding a position");
    assert_eq!(disconnected.message, "OKX 帳戶未連線：Keychain 沒有 API key");
    assert!(a.iter().any(|a| a.message == "OKX 時鐘未校時"), "{a:?}");
    // nothing held: unticked and quiet, so no alert (same as before)
    s.accounts.insert(Okx, lost_connection(vec![], Okx));
    assert!(alerts(&s, NOW).iter().all(|a| !a.message.contains("OKX")));
}

#[test]
fn a_ticked_exchange_that_turns_not_connected_with_old_data_is_still_a_disconnect() {
    let mut s = healthy();
    s.accounts.insert(Bybit, lost_connection(vec![], Bybit));
    let a = alerts(&s, NOW);
    let d = a.iter().find(|a| a.category == Category::Disconnected && a.message.contains("Bybit 帳戶未連線")).expect("the banner must not go quiet");
    assert_eq!(d.message, "Bybit 帳戶未連線：Keychain 沒有 API key");
}

#[test]
fn a_live_process_never_alerts_about_okx_even_for_a_record_that_names_no_exchange() {
    use crate::identity::ProcessIdentity::Live;
    let mut s = healthy();
    s.identity = Live;
    s.settings.risk.allowed_exchanges = vec![Binance, Bybit];
    s.accounts.insert(Okx, AccountState::NotConnected { reason: "Keychain 沒有 API key".into() });
    s.clocks.insert(Okx, ClockState::Unsynced);
    // a stored unfinished pair that names no exchange could be on any of them, but Live trades two
    s.pair_records = [("u1".to_string(), crate::ui::bridge::PairRecord { pair_id: "p1".into(), symbol: "ETHUSDT".into(), long_exchange: None, short_exchange: None, status: "RECONCILED".into() })].into();
    let msgs: Vec<_> = alerts(&s, NOW).into_iter().map(|a| a.message).collect();
    assert!(msgs.iter().all(|m| !m.contains("OKX")), "{msgs:?}");
    // the same records in a Demo process still alert (OKX is tradable there)
    s.identity = crate::identity::ProcessIdentity::Demo;
    assert!(alerts(&s, NOW).iter().any(|a| a.message.contains("OKX 帳戶未連線")));
}

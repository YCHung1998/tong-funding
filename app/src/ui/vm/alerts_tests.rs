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
        hs(SourceId::MarketPoll(Binance), true, Some(1_000), 10_000),
        hs(SourceId::MarketPoll(Bybit), true, Some(1_000), 10_000),
        hs(SourceId::MarketPoll(Okx), true, Some(1_000), 10_000),
        hs(SourceId::Account(Binance), true, Some(1_000), 30_000),
        hs(SourceId::Account(Bybit), true, Some(1_000), 30_000),
    ];
    for e in [Binance, Bybit] {
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

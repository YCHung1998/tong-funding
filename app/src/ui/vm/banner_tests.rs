//! Task 3.2: banner, freshness indicator and load states (spec alert-banner).
use std::sync::Arc;

use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange::{Binance, Bybit};

use super::*;
use crate::exchange::health::feed::HealthSnapshot;
use crate::ports::ManualClock;
use crate::ui::alerts::{Alert, Category, alerts};
use crate::ui::bridge::{SourceHealth, SourceId, UiSnapshot};
use crate::ui::testkit::pair;

const NOW: i64 = 1_791_201_600_000;

fn alert(category: Category, source: &str) -> Alert {
    Alert { category, source: source.into(), message: format!("{source} problem"), age_ms: Some(65_000), link: None, fingerprint: String::new(), event_ms: None }
}

/// A funding finding bound to its event (`fingerprint` = event id, `event_ms` = when it happened).
fn finding(source: &str, event_id: i64, event_ms: i64) -> Alert {
    Alert { category: Category::FundingData, source: source.into(), message: format!("{source} PnL INCOMPLETE"), age_ms: None, link: Some(Page::SystemLogs), fingerprint: event_id.to_string(), event_ms: Some(event_ms) }
}

#[test]
fn banner_is_directly_under_the_header_on_all_eight_pages() {
    for page in Page::ALL {
        // live-banners: the environment banner is the topmost region of every page.
        assert_eq!(layout(page), [Region::EnvBanner, Region::Header, Region::Banner, Region::Content, Region::StatusBar], "{page:?}");
    }
    assert_eq!(Page::ALL.len(), 8);
}

#[test]
fn no_alert_means_no_banner_space_and_no_badge() {
    assert!(!banner(&[], &Acks::new(), NOW).visible());
    assert_eq!(badge(&attention(&[], &Acks::new(), NOW)), None);
}

#[test]
fn the_banner_shows_only_the_critical_tier() {
    let list = vec![
        alert(Category::ManualAttention, "p1"),
        alert(Category::Halted, "store"),
        alert(Category::Disconnected, "Binance 帳戶"),
        alert(Category::Stale, "Bybit 行情"),
        alert(Category::RateLimitOrClock, "Bybit 限流"),
        alert(Category::Setting, "OKX 帳戶"),
        finding("p9 PNL", 7, NOW),
    ];
    let vm = banner(&list, &Acks::new(), NOW);
    let shown: Vec<_> = vm.items.iter().map(|i| i.category).collect();
    assert_eq!(shown, ["需人工處理", "停機", "斷線"], "notices and data findings stay out of the banner");
    let attn = attention(&list, &Acks::new(), NOW);
    assert_eq!((attn.notices.len(), attn.reviews.len()), (3, 1));
}

#[test]
fn manual_and_halt_alerts_cannot_be_acknowledged_or_hidden() {
    let list = vec![alert(Category::ManualAttention, "p1"), alert(Category::Halted, "store"), alert(Category::Disconnected, "Binance 帳戶")];
    let mut acks = Acks::new();
    assert_eq!(acknowledge_all(&mut acks, &list), 1, "only the disconnect can be acknowledged");
    let vm = banner(&list, &acks, NOW);
    let shown: Vec<_> = vm.items.iter().map(|i| (i.category, i.closable)).collect();
    assert_eq!(shown, [("需人工處理", false), ("停機", false)]);
    assert_eq!(banner(&list, &Acks::new(), NOW).items.iter().filter(|i| i.closable).count(), 1);
    assert_eq!(banner(&list, &Acks::new(), NOW).items[0].age_text.as_deref(), Some("持續 1 分 5 秒"));
}

#[test]
fn an_acknowledgement_holds_only_for_the_same_finding() {
    let first = finding("p1 PNL", 738, NOW);
    let mut acks = Acks::new();
    acknowledge(&mut acks, &first);
    let attn = attention(std::slice::from_ref(&first), &acks, NOW);
    assert_eq!((attn.reviews.len(), attn.acknowledged), (0, 1), "acknowledged: out of the badge");
    assert_eq!(badge(&attn), None);
    // The same pair has a NEW finding (another event): it shows again.
    let newer = finding("p1 PNL", 900, NOW);
    let attn = attention(std::slice::from_ref(&newer), &acks, NOW);
    assert_eq!((attn.reviews.len(), attn.acknowledged), (1, 0));
}

#[test]
fn findings_about_old_events_leave_the_badge_but_recent_ones_stay() {
    let old = finding("ark PNL", 738, NOW - REVIEW_MAX_AGE_MS - 1);
    let edge = finding("ogn PNL", 1745, NOW - REVIEW_MAX_AGE_MS);
    let fresh = finding("nmr PNL", 3523, NOW - 60_000);
    let attn = attention(&[old, edge, fresh], &Acks::new(), NOW);
    assert_eq!((attn.reviews.len(), attn.aged_out), (2, 1));
    assert!(attn.reviews.iter().all(|a| a.source != "ark PNL"));
}

#[test]
fn the_badge_counts_findings_and_notices_and_the_drawer_lists_them() {
    let list = vec![finding("a PNL", 1, NOW), finding("b PNL", 2, NOW), alert(Category::Stale, "Bybit 行情")];
    let attn = attention(&list, &Acks::new(), NOW);
    let b = badge(&attn).unwrap();
    assert_eq!((b.text.as_str(), b.reviews, b.notices), ("⚠ 資料問題 2 · 注意 1", 2, 1));
    let d = drawer(&attn);
    assert_eq!((d.reviews.len(), d.notices.len()), (2, 1));
    assert!(d.reviews.iter().all(|i| i.closable && i.link == Some(("前往", Page::SystemLogs))));
    let only_notice = attention(&[alert(Category::Stale, "x")], &Acks::new(), NOW);
    assert_eq!(badge(&only_notice).unwrap().text, "⚠ 注意 1");
}

#[test]
fn a_changed_finding_shows_again_and_an_ended_condition_is_forgotten_but_an_unlisted_finding_is_kept() {
    let a = finding("p1 PNL", 738, NOW);
    let mut acks = Acks::new();
    acknowledge(&mut acks, &a);
    assert!(!prune_acks(&mut acks, std::slice::from_ref(&a)), "still the same finding: kept");
    assert!(!prune_acks(&mut acks, &[]), "a finding that is not listed right now (data not loaded yet) stays acknowledged");
    assert_eq!(acks.len(), 1);
    assert!(prune_acks(&mut acks, &[finding("p1 PNL", 900, NOW)]), "another event: the old acknowledgement is dropped");
    assert!(acks.is_empty());
    // A plain condition: acknowledged while active, forgotten when it ends.
    let stale = alert(Category::Stale, "Bybit 行情");
    acknowledge(&mut acks, &stale);
    assert!(!prune_acks(&mut acks, std::slice::from_ref(&stale)));
    assert!(prune_acks(&mut acks, &[]), "condition gone: forgotten, so it shows when it comes back");
    assert!(acks.is_empty());
}

#[test]
fn only_findings_are_stored_and_garbage_is_ignored() {
    let mut acks = Acks::new();
    acknowledge(&mut acks, &finding("ui-1 PNL", 738, NOW));
    acknowledge(&mut acks, &alert(Category::Stale, "Bybit 行情"));
    let stored = encode_acks(&acks);
    assert_eq!(stored.as_object().unwrap().len(), 1, "a plain condition is not stored");
    assert_eq!(decode_acks(&stored), Acks::from([((Category::FundingData, "ui-1 PNL".to_string()), "738".to_string())]));
    let junk = serde_json::json!({ "funding|x": 5, "nope|y": "1", "no-separator": "1", "stale|ok": "fp", "funding|ok": "9" });
    assert_eq!(decode_acks(&junk), Acks::from([((Category::FundingData, "ok".to_string()), "9".to_string())]));
    assert!(decode_acks(&serde_json::json!([1, 2])).is_empty());
}

#[test]
fn manual_attention_alert_links_to_positions() {
    let mut s = UiSnapshot::default();
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::PartialFailure)];
    let a: Vec<_> = alerts(&s, NOW).into_iter().filter(|a| a.category == Category::ManualAttention).collect();
    let vm = banner(&a, &Acks::new(), NOW);
    assert_eq!(vm.items[0].link, Some(("前往持倉", Page::Positions)));
    assert!(vm.items[0].severe);
}

#[test]
fn banner_still_shows_when_the_database_cannot_be_opened_and_nothing_is_modified() {
    let dir = crate::store::db::test_support::tempdir();
    let p = dir.path().join("funding.db");
    std::fs::write(&p, b"garbage garbage garbage".repeat(200)).unwrap();
    let before = std::fs::read(&p).unwrap();
    let db = Db::open(&p, Arc::new(ManualClock::new(NOW)));
    let mut s = UiSnapshot::default(); // no page data at all
    s.system = read_system_flags(&db, NOW);
    let a = alerts(&s, NOW);
    let halted: Vec<_> = a.iter().filter(|a| a.category == Category::Halted).collect();
    assert!(halted.iter().any(|a| a.source == "store" && a.message.contains("database")), "{halted:?}");
    assert!(banner(&a, &Acks::new(), NOW).visible());
    assert_eq!(std::fs::read(&p).unwrap(), before, "no data file was modified");
}

#[test]
fn a_healthy_store_reports_its_kill_switch() {
    let (_d, db, _) = crate::store::db::test_support::open_tmp();
    let f = read_system_flags(&db, NOW);
    assert_eq!((f.store_halt.clone(), f.kill_switch), (None, KillSwitchState::Off));
    db.set_kill_switch(true).unwrap();
    assert_eq!(read_system_flags(&db, NOW).kill_switch, KillSwitchState::On);
}

#[test]
fn indicator_and_banner_agree_on_a_stale_source() {
    let mut s = UiSnapshot::default();
    s.health.push(SourceHealth {
        source: SourceId::MarketPoll(Bybit),
        health: Some(HealthSnapshot { source: "bybit".into(), connected: true, last_success_at: Some(NOW - 31_000), consecutive_failures: 0, expected_period_ms: 10_000, stale_threshold_ms: 30_000, stale: false }),
        next_poll_at: Some(NOW + 7_000),
        rate_limited_until: None,
    });
    let f = freshness(&s, SourceId::MarketPoll(Bybit), NOW);
    assert_eq!(f.status, FreshStatus::Stale);
    assert_eq!(f.text(), "STALE · 31 秒前 · 下次 7s");
    assert!(alerts(&s, NOW).iter().any(|a| a.category == Category::Stale && a.source == "Bybit 行情"));
    assert_eq!(freshness(&s, SourceId::MarketPoll(Binance), NOW).status, FreshStatus::Unknown);
}

#[test]
fn loading_failed_and_empty_are_distinct() {
    assert_eq!(LoadView::Loading.text().as_deref(), Some("載入中"));
    assert_eq!(LoadView::Empty.text().as_deref(), Some("沒有資料"));
    let failed = LoadView::Failed { error: "request timed out".into(), at_ms: NOW - 5_000, now_ms: NOW };
    assert_eq!(failed.text().as_deref(), Some("載入失敗：request timed out（11:59:55 UTC 嘗試，5 秒前）"));
    assert_eq!(LoadView::Ready.text(), None);
}

// ---- alert acknowledgements (header badge) are stored in `config.alert_acks` ------------------

#[test]
fn alert_acks_round_trip_through_the_config_table_with_realistic_values() {
    let mk = |category, source: &str, message: &str, fingerprint: &str| Alert { category, source: source.into(), message: message.into(), age_ms: None, link: None, fingerprint: fingerprint.into(), event_ms: None };
    let mut acks = Acks::new();
    acknowledge(&mut acks, &mk(Category::FundingData, "ui-1791344628622-1-nmrusdt PNL", "NMRUSDT PnL INCOMPLETE：缺少結算流水", "3523"));
    acknowledge(&mut acks, &mk(Category::FundingData, "ui-3-klay PNL", "KAIAUSDT PnL INCOMPLETE：資金費需人工檢查（Binance long：NOT_FOUND）", "3527"));
    acknowledge(&mut acks, &mk(Category::FundingData, "ui-2-ognusdt PNL_RECONCILIATION", "OGNUSDT 對帳 MISMATCH（Binance keys unavailable (SecretStoreError); demo executor not built）", "1745"));
    let (_d, db, _) = crate::store::db::test_support::open_tmp();
    let v = db.config_set("alert_acks", &encode_acks(&acks), None).expect("the config table accepts the acknowledgements");
    let stored = db.config_get("alert_acks").unwrap().unwrap();
    assert_eq!(stored.version, v);
    assert_eq!(decode_acks(&stored.value), acks);
    // Overwriting with the optimistic version works like the other stored preferences.
    acks.clear();
    db.config_set("alert_acks", &encode_acks(&acks), Some(v)).unwrap();
    assert!(decode_acks(&db.config_get("alert_acks").unwrap().unwrap().value).is_empty());
}

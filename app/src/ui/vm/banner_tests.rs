//! Task 3.2: banner, freshness indicator and load states (spec alert-banner).
use std::collections::BTreeSet;
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
    Alert { category, source: source.into(), message: format!("{source} problem"), age_ms: Some(65_000), link: None }
}

#[test]
fn banner_is_directly_under_the_header_on_all_eight_pages() {
    for page in Page::ALL {
        assert_eq!(layout(page), [Region::Header, Region::Banner, Region::Content, Region::StatusBar], "{page:?}");
    }
    assert_eq!(Page::ALL.len(), 8);
}

#[test]
fn no_alert_means_no_banner_space() {
    let vm = banner(&[], &BTreeSet::new());
    assert!(!vm.visible());
}

#[test]
fn manual_and_halt_alerts_cannot_be_closed_or_hidden() {
    let list = vec![alert(Category::ManualAttention, "p1"), alert(Category::Halted, "store"), alert(Category::Stale, "Bybit 行情")];
    let all_keys: BTreeSet<_> = list.iter().map(|a| (a.category, a.source.clone())).collect();
    let vm = banner(&list, &all_keys);
    let shown: Vec<_> = vm.items.iter().map(|i| (i.category, i.closable)).collect();
    assert_eq!(shown, [("需人工處理", false), ("停機", false)], "only the dismissible stale alert was hidden");
    let vm = banner(&list, &BTreeSet::new());
    assert_eq!(vm.items.iter().filter(|i| i.closable).count(), 1);
    assert_eq!(vm.items[0].age_text.as_deref(), Some("持續 1 分 5 秒"));
}

#[test]
fn a_dismissed_alert_returns_when_its_condition_comes_back() {
    let stale = alert(Category::Stale, "Bybit 行情");
    let mut dismissed: BTreeSet<_> = [(Category::Stale, "Bybit 行情".to_string())].into();
    prune_dismissed(&mut dismissed, std::slice::from_ref(&stale));
    assert_eq!(dismissed.len(), 1, "still active: stays dismissed");
    prune_dismissed(&mut dismissed, &[]);
    assert!(dismissed.is_empty(), "condition gone: forgotten");
    assert!(banner(&[stale], &dismissed).visible(), "it shows again when it comes back");
}

#[test]
fn manual_attention_alert_links_to_positions() {
    let mut s = UiSnapshot::default();
    s.pairs = vec![pair("p1", "BTCUSDT", Binance, Bybit, PairState::PartialFailure)];
    let a: Vec<_> = alerts(&s, NOW).into_iter().filter(|a| a.category == Category::ManualAttention).collect();
    let vm = banner(&a, &BTreeSet::new());
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
    assert!(banner(&a, &BTreeSet::new()).visible());
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

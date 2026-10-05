//! Banner, freshness indicators and load states (task 3.2, spec alert-banner). The banner sits
//! between the header and the content on every page and depends only on the alert list, never on
//! the current page's data (it still shows when the database cannot be opened).

use std::collections::BTreeSet;

use super::alerts::{Alert, Category, FreshStatus, source_age_ms, source_status};
use super::bridge::{KillSwitchState, SourceId, SystemFlags, UiSnapshot};
use super::format;
use crate::store::db::Db;
use crate::ui::nav::Page;

/// Vertical regions of the main window, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Header,
    Banner,
    Content,
    StatusBar,
}

/// Window layout for a page: the banner is always directly under the header.
pub fn layout(_page: Page) -> [Region; 4] {
    // Deliberately independent of the page: the banner never depends on page data.
    [Region::Header, Region::Banner, Region::Content, Region::StatusBar]
}

fn age_text(category: Category, ms: i64) -> String {
    let s = format::secs(ms);
    let span = if s >= 60 { format!("{} 分 {} 秒", s / 60, s % 60) } else { format!("{s} 秒") };
    match category {
        Category::Stale => format!("{span}前"),
        _ => format!("持續 {span}"),
    }
}

/// Key of a dismissed alert: it stays hidden only while the same condition is active.
pub type AlertKey = (Category, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerItem {
    pub category: &'static str,
    pub message: String,
    /// `持續 1 分 5 秒` / `31 秒前`.
    pub age_text: Option<String>,
    pub closable: bool,
    /// Navigation link (label, page).
    pub link: Option<(&'static str, Page)>,
    pub key: AlertKey,
    pub severe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BannerVm {
    pub items: Vec<BannerItem>,
}

impl BannerVm {
    /// No alert: the banner takes no space at all.
    pub fn visible(&self) -> bool {
        !self.items.is_empty()
    }
}

/// Banner items; dismissed keys hide only dismissible alerts.
pub fn banner(alerts: &[Alert], dismissed: &BTreeSet<AlertKey>) -> BannerVm {
    let items = alerts
        .iter()
        .filter(|a| !(a.dismissible() && dismissed.contains(&(a.category, a.source.clone()))))
        .map(|a| BannerItem {
            category: a.category.label(),
            message: a.message.clone(),
            age_text: a.age_ms.map(|ms| age_text(a.category, ms)),
            closable: a.dismissible(),
            link: a.link.map(|p| (if p == Page::Positions { "前往持倉" } else { "前往" }, p)),
            key: (a.category, a.source.clone()),
            severe: !a.dismissible(),
        })
        .collect();
    BannerVm { items }
}

/// Forget dismissals whose condition has gone, so the alert shows again if it comes back.
pub fn prune_dismissed(dismissed: &mut BTreeSet<AlertKey>, alerts: &[Alert]) {
    dismissed.retain(|k| alerts.iter().any(|a| a.category == k.0 && a.source == k.1));
}

/// A block's freshness indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub status: FreshStatus,
    pub age_s: Option<i64>,
    pub next_poll_s: Option<i64>,
}

impl Freshness {
    /// `ONLINE · 3 秒前 · 下次 7s`.
    pub fn text(&self) -> String {
        let mut parts = vec![self.status.label().to_string()];
        if let Some(a) = self.age_s {
            parts.push(format!("{a} 秒前"));
        }
        if let Some(n) = self.next_poll_s {
            parts.push(format!("下次 {n}s"));
        }
        parts.join(" · ")
    }
}

/// The indicator for `source`; uses the same verdict as the banner. A source with no health
/// entry is `UNKNOWN`.
pub fn freshness(snap: &UiSnapshot, source: SourceId, now_ms: i64) -> Freshness {
    match snap.health_of(source) {
        None => Freshness { status: FreshStatus::Unknown, age_s: None, next_poll_s: None },
        Some(h) => Freshness {
            status: source_status(h, now_ms),
            age_s: source_age_ms(h, now_ms).map(format::secs),
            next_poll_s: h.next_poll_at.map(|t| format::secs(t - now_ms)),
        },
    }
}

/// Load state of a page block: loading, failed and empty are always distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadView {
    Loading,
    Failed { error: String, at_ms: i64, now_ms: i64 },
    Empty,
    Ready,
}

impl LoadView {
    pub fn text(&self) -> Option<String> {
        match self {
            LoadView::Loading => Some("載入中".into()),
            LoadView::Failed { error, at_ms, now_ms } => {
                Some(format!("載入失敗：{error}（{} UTC 嘗試，{} 秒前）", format::utc_hms(*at_ms), format::secs(now_ms - at_ms)))
            }
            LoadView::Empty => Some("沒有資料".into()),
            LoadView::Ready => None,
        }
    }
}

/// System flags straight from the store (halt reason and kill switch), independent of any page.
/// A halted store means the kill switch cannot be read either: reported as unknown (fail closed).
pub fn read_system_flags(db: &Db, since_ms: i64) -> SystemFlags {
    if let Some(reason) = db.halt_reason() {
        return SystemFlags { store_halt: Some((reason.to_string(), since_ms)), kill_switch: KillSwitchState::Unknown };
    }
    // `kill_switch_halted` fails closed (an unreadable value halts the store and reads as on).
    let on = db.kill_switch_halted();
    match db.halt_reason() {
        Some(reason) => SystemFlags { store_halt: Some((reason.to_string(), since_ms)), kill_switch: KillSwitchState::Unknown },
        None => SystemFlags { store_halt: None, kill_switch: if on { KillSwitchState::On } else { KillSwitchState::Off } },
    }
}

#[cfg(test)]
#[path = "banner_tests.rs"]
mod tests;

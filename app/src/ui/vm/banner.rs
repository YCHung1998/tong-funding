//! Banner, freshness indicators and load states (task 3.2, spec alert-banner). The banner sits
//! between the header and the content on every page and depends only on the alert list, never on
//! the current page's data (it still shows when the database cannot be opened).

use std::collections::BTreeMap;

use super::alerts::{Alert, Category, FreshStatus, Tier, source_age_ms, source_status};
use super::bridge::{KillSwitchState, SourceId, SystemFlags, UiSnapshot};
use super::format;
use crate::store::db::Db;
use crate::ui::nav::Page;

/// Vertical regions of the main window, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// live-banners: the fixed environment banner, above everything and not dismissable.
    EnvBanner,
    Header,
    Banner,
    Content,
    StatusBar,
}

/// Window layout for a page: the banner is always directly under the header.
pub fn layout(_page: Page) -> [Region; 5] {
    // Deliberately independent of the page: the banners never depend on page data.
    [Region::EnvBanner, Region::Header, Region::Banner, Region::Content, Region::StatusBar]
}

fn age_text(category: Category, ms: i64) -> String {
    let s = format::secs(ms);
    let span = if s >= 60 { format!("{} 分 {} 秒", s / 60, s % 60) } else { format!("{s} 秒") };
    match category {
        Category::Stale => format!("{span}前"),
        _ => format!("持續 {span}"),
    }
}

/// Key of an acknowledged alert (category + source).
pub type AlertKey = (Category, String);

/// Acknowledged alerts: key → the fingerprint that was acknowledged. An acknowledgement holds only
/// while the alert has that same fingerprint (a new finding shows again).
pub type Acks = BTreeMap<AlertKey, String>;

/// Findings about pairs older than this leave the notification badge (they stay in the system log).
pub const REVIEW_MAX_AGE_MS: i64 = 3 * 24 * 3_600_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerItem {
    pub category: &'static str,
    pub message: String,
    /// `持續 1 分 5 秒` / `31 秒前`.
    pub age_text: Option<String>,
    /// Can be acknowledged ("已確認"); halts and locked pairs cannot.
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

fn item_of(a: &Alert) -> BannerItem {
    BannerItem {
        category: a.category.label(),
        message: a.message.clone(),
        age_text: a.age_ms.map(|ms| age_text(a.category, ms)),
        closable: a.dismissible(),
        link: a.link.map(|p| (if p == Page::Positions { "前往持倉" } else { "前往" }, p)),
        key: (a.category, a.source.clone()),
        severe: !a.dismissible(),
    }
}

fn acknowledged(a: &Alert, acks: &Acks) -> bool {
    a.dismissible() && acks.get(&(a.category, a.source.clone())).is_some_and(|f| *f == a.fingerprint)
}

fn aged_out(a: &Alert, now_ms: i64) -> bool {
    a.category.tier() == Tier::Review && a.event_ms.is_some_and(|t| now_ms.saturating_sub(t) > REVIEW_MAX_AGE_MS)
}

/// What needs attention now, by where it is shown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attention {
    /// The banner.
    pub critical: Vec<Alert>,
    /// The badge's drawer, "注意".
    pub notices: Vec<Alert>,
    /// The badge's drawer, "資料問題".
    pub reviews: Vec<Alert>,
    /// Hidden because they were acknowledged.
    pub acknowledged: usize,
    /// Hidden because the finding is older than [`REVIEW_MAX_AGE_MS`].
    pub aged_out: usize,
}

/// Splits the active alerts by tier; acknowledged ones (same fingerprint) and old findings are left out.
pub fn attention(alerts: &[Alert], acks: &Acks, now_ms: i64) -> Attention {
    let mut out = Attention::default();
    for a in alerts {
        if acknowledged(a, acks) {
            out.acknowledged += 1;
        } else if aged_out(a, now_ms) {
            out.aged_out += 1;
        } else {
            match a.category.tier() {
                Tier::Critical => out.critical.push(a.clone()),
                Tier::Notice => out.notices.push(a.clone()),
                Tier::Review => out.reviews.push(a.clone()),
            }
        }
    }
    out
}

/// The banner: only the critical tier.
pub fn banner(alerts: &[Alert], acks: &Acks, now_ms: i64) -> BannerVm {
    BannerVm { items: attention(alerts, acks, now_ms).critical.iter().map(item_of).collect() }
}

/// The drawer behind the header badge.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DrawerVm {
    pub notices: Vec<BannerItem>,
    pub reviews: Vec<BannerItem>,
}

pub fn drawer(attn: &Attention) -> DrawerVm {
    DrawerVm { notices: attn.notices.iter().map(item_of).collect(), reviews: attn.reviews.iter().map(item_of).collect() }
}

/// The small header indicator; `None` when there is nothing to review (it takes no space).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeVm {
    pub text: String,
    pub notices: usize,
    pub reviews: usize,
}

pub fn badge(attn: &Attention) -> Option<BadgeVm> {
    let (n, r) = (attn.notices.len(), attn.reviews.len());
    if n + r == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if r > 0 {
        parts.push(format!("資料問題 {r}"));
    }
    if n > 0 {
        parts.push(format!("注意 {n}"));
    }
    Some(BadgeVm { text: format!("⚠ {}", parts.join(" · ")), notices: n, reviews: r })
}

/// Records the acknowledgement of `alert` (only acknowledgeable alerts).
pub fn acknowledge(acks: &mut Acks, alert: &Alert) {
    if alert.dismissible() {
        acks.insert((alert.category, alert.source.clone()), alert.fingerprint.clone());
    }
}

/// Acknowledges every alert of the drawer (one tier or both). Returns how many were recorded.
pub fn acknowledge_all(acks: &mut Acks, alerts: &[Alert]) -> usize {
    let before = acks.len();
    for a in alerts {
        acknowledge(acks, a);
    }
    acks.len().saturating_sub(before)
}

/// Only an acknowledgement of a finding about an event is kept across restarts (it stays valid for
/// that event); one of a plain condition lasts while the condition is active.
fn persistent(category: Category) -> bool {
    category == Category::FundingData
}

/// Forget acknowledgements that no longer hold. `true` = the set changed (store it):
/// - a finding whose event changed (the same key now has another fingerprint) shows again;
/// - an acknowledged condition that has ended is forgotten (it shows if it comes back);
/// - an acknowledged finding that is not listed right now is KEPT (the data may not be loaded
///   yet, and the same event stays acknowledged when it is listed again).
pub fn prune_acks(acks: &mut Acks, alerts: &[Alert]) -> bool {
    let before = acks.len();
    acks.retain(|k, f| match alerts.iter().find(|a| a.category == k.0 && a.source == k.1) {
        Some(a) => a.fingerprint == *f,
        None => persistent(k.0),
    });
    acks.len() != before
}

/// The stored form: `{"<category code>|<source>": "<fingerprint>"}`, findings only.
pub fn encode_acks(acks: &Acks) -> serde_json::Value {
    serde_json::Value::Object(acks.iter().filter(|((c, _), _)| persistent(*c)).map(|((c, s), f)| (format!("{}|{s}", c.code()), serde_json::Value::String(f.clone()))).collect())
}

/// Reads the stored form; entries it cannot read are dropped (an unreadable ack just shows the
/// alert again), and so is anything that is not a finding.
pub fn decode_acks(v: &serde_json::Value) -> Acks {
    let Some(map) = v.as_object() else { return Acks::new() };
    map.iter()
        .filter_map(|(k, f)| {
            let (code, source) = k.split_once('|')?;
            let category = Category::from_code(code).filter(|c| persistent(*c))?;
            Some(((category, source.to_string()), f.as_str()?.to_string()))
        })
        .collect()
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

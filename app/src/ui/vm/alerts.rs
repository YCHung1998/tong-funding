//! Alert generation (task 3.1, spec alert-banner; design D6). One pure function turns source
//! health, system flags, clocks, accounts and pair states into an ordered, de-duplicated list.
//! Staleness is the source-level `feed-health` verdict (never the per-row 1 s rule); an unknown
//! state counts as an anomaly.

use tong_funding_core::types::Exchange;

use super::bridge::{AccountState, ClockState, KillSwitchState, SourceHealth, SourceId, UiSnapshot, NOT_CONNECTED_ERROR_PREFIX};
use super::format;
use crate::exchange::signed::signing::RECV_WINDOW_MS;
use crate::ui::nav::Page;

/// The accounts whose keys and clock are watched: Binance and Bybit, and OKX while it is in
/// `allowed_exchanges` (an exchange that is not traded needs no keys) or while it still holds
/// positions or is on a leg of an unfinished pair (the risk page lets it be unticked then, and a
/// lost connection to it must not go quiet).
fn alert_accounts(snap: &UiSnapshot) -> Vec<Exchange> {
    // only the exchanges this process trades: a stored record that names no exchange is "possibly
    // there" for every exchange, but a Live process has no path to OKX
    super::bridge::tradable_exchanges(snap.identity)
        .iter()
        .copied()
        .filter(|e| *e != Exchange::Okx || snap.settings.risk.allowed_exchanges.contains(e) || super::disabled::holds_exposure(snap, *e))
        .collect()
}

pub const MANUAL_TEXT: &str = "需人工處理，系統不會自動補單或平倉";

/// Categories in descending severity (the declaration order is the display order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    ManualAttention,
    Halted,
    Disconnected,
    Stale,
    /// funding-pnl: reconciliation MISMATCH / FAILED, ledger conflict, ledger fetch error,
    /// INCOMPLETE PnL. Not dismissible: it stays until the data changes (settlement-timeline).
    FundingData,
    /// An account setting the system cannot work with (e.g. OKX account mode): not a connection
    /// problem, so it is a notice, not a banner.
    Setting,
    RateLimitOrClock,
}

/// Where an alert is shown (design: the banner is only for what must not be missed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The banner: halts, locked pairs, lost connections.
    Critical,
    /// The header badge and its drawer: stale data, rate limits, clocks, settings.
    Notice,
    /// The header badge and its drawer: data-quality findings about pairs (funding / PnL).
    Review,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::ManualAttention => "需人工處理",
            Category::Halted => "停機",
            Category::Disconnected => "斷線",
            Category::Stale => "資料過期",
            Category::FundingData => "資料問題",
            Category::Setting => "設定問題",
            Category::RateLimitOrClock => "限流 / 時鐘",
        }
    }
    /// Manual-attention and halt alerts cannot be acknowledged: they stay until the condition ends.
    pub fn dismissible(self) -> bool {
        !matches!(self, Category::ManualAttention | Category::Halted)
    }
    pub fn tier(self) -> Tier {
        match self {
            Category::ManualAttention | Category::Halted | Category::Disconnected => Tier::Critical,
            Category::Stale | Category::Setting | Category::RateLimitOrClock => Tier::Notice,
            Category::FundingData => Tier::Review,
        }
    }
    /// Stable text used to store an acknowledgement.
    pub fn code(self) -> &'static str {
        match self {
            Category::ManualAttention => "manual",
            Category::Halted => "halted",
            Category::Disconnected => "disconnected",
            Category::Stale => "stale",
            Category::FundingData => "funding",
            Category::Setting => "setting",
            Category::RateLimitOrClock => "limit",
        }
    }
    pub fn from_code(code: &str) -> Option<Category> {
        [Category::ManualAttention, Category::Halted, Category::Disconnected, Category::Stale, Category::FundingData, Category::Setting, Category::RateLimitOrClock]
            .into_iter()
            .find(|c| c.code() == code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub category: Category,
    /// Dedupe key and display source (exchange source label or pair id).
    pub source: String,
    pub message: String,
    /// Age or duration in ms, when known.
    pub age_ms: Option<i64>,
    /// Navigation only (never an action that changes state).
    pub link: Option<Page>,
    /// What an acknowledgement is bound to: a finding about an event carries that event's id, so a
    /// new finding (another event) has another fingerprint and shows again; a plain condition has an
    /// empty one (its acknowledgement lasts while it is active).
    pub fingerprint: String,
    /// When the underlying event happened, if it is an event (used to age old findings out).
    pub event_ms: Option<i64>,
}

impl Alert {
    pub fn dismissible(&self) -> bool {
        self.category.dismissible()
    }
}

/// The freshness verdict of one source, shared by the banner and every freshness indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshStatus {
    Online,
    Stale,
    Offline,
    RateLimited,
    /// Health not available: treated as an anomaly.
    Unknown,
}

impl FreshStatus {
    pub fn label(self) -> &'static str {
        match self {
            FreshStatus::Online => "ONLINE",
            FreshStatus::Stale => "STALE",
            FreshStatus::Offline => "OFFLINE",
            FreshStatus::RateLimited => "RATE_LIMITED",
            FreshStatus::Unknown => "UNKNOWN",
        }
    }
}

/// Source-level verdict (feed-health): disconnected is immediate; stale is `max(threshold, 3 ×
/// period)` past the last success; rate-limit back-off is reported as such.
pub fn source_status(h: &SourceHealth, now_ms: i64) -> FreshStatus {
    let Some(s) = &h.health else { return FreshStatus::Unknown };
    if !s.connected {
        return FreshStatus::Offline;
    }
    let too_old = match s.last_success_at {
        None => true,
        Some(t) => now_ms < t || now_ms - t > s.stale_threshold_ms,
    };
    if s.stale || too_old {
        return FreshStatus::Stale;
    }
    if h.rate_limited_until.is_some_and(|u| u > now_ms) {
        return FreshStatus::RateLimited;
    }
    FreshStatus::Online
}

/// Age of the last success, if any.
pub fn source_age_ms(h: &SourceHealth, now_ms: i64) -> Option<i64> {
    h.health.as_ref()?.last_success_at.map(|t| (now_ms - t).max(0))
}

/// Builds every active alert, ordered by severity then source; one per (category, source).
pub fn alerts(snap: &UiSnapshot, now_ms: i64) -> Vec<Alert> {
    let mut out: Vec<Alert> = Vec::new();
    let mut funding_alerts: Vec<Alert> = Vec::new();
    let mut push = |category: Category, source: String, message: String, age_ms: Option<i64>, link: Option<Page>| {
        // A condition (stale, disconnected, ...) has no event: its acknowledgement lasts while it is active.
        out.push(Alert { category, source, message, age_ms, link, fingerprint: String::new(), event_ms: None });
    };

    // 1. Manual attention: locked pair states (and unreadable ones: unknown is an anomaly).
    for p in &snap.pairs {
        let state = match &p.state {
            Ok(st) if st.is_locked() => st.as_str().to_string(),
            Ok(_) => continue,
            Err(_) => "狀態無法讀取".to_string(),
        };
        let msg = format!("{} · {} / {} · {state} · {MANUAL_TEXT}", p.symbol, p.long_exchange.name(), p.short_exchange.name());
        push(Category::ManualAttention, p.pair_id.clone(), msg, None, Some(Page::Positions));
    }

    // 2. Halted: store fail-closed halt, kill switch on or unreadable.
    if let Some((reason, since)) = &snap.system.store_halt {
        let msg = format!("系統已停機：{reason}（自 {} UTC）· 既有資料未被修改，頁面僅供唯讀檢視", format::utc_hms(*since));
        push(Category::Halted, "store".into(), msg, Some((now_ms - since).max(0)), None);
    }
    match snap.system.kill_switch {
        KillSwitchState::On => push(Category::Halted, "kill switch".into(), "系統已停機：kill switch 已啟用".into(), None, None),
        KillSwitchState::Unknown => push(Category::Halted, "kill switch".into(), "系統已停機：kill switch 無法讀取（視為已啟用）".into(), None, None),
        KillSwitchState::Off => {}
    }

    // The OKX latch (an environment mismatch, 50101): OKX stays disabled until restart.
    if let Some(reason) = &snap.okx_disabled {
        push(Category::Halted, "OKX".into(), format!("OKX 已停用：{reason}（重新啟動前不會恢復）"), None, None);
    }

    // 3. Disconnected / not connected accounts.
    for e in alert_accounts(snap) {
        let source = SourceId::Account(e).label();
        match snap.account(e) {
            AccountState::NotConnected { reason } => push(Category::Disconnected, source, format!("{} 帳戶未連線：{reason}", e.name()), None, None),
            // The last known data is kept when a poll finds the account not connected: still a lost connection.
            AccountState::Loaded { fetched_at, error: Some((err, at)), .. } if at >= fetched_at && err.starts_with(NOT_CONNECTED_ERROR_PREFIX) => {
                let reason = &err[NOT_CONNECTED_ERROR_PREFIX.len()..];
                push(Category::Disconnected, source, format!("{} 帳戶未連線：{reason}", e.name()), None, None);
            }
            // An account the system cannot use by its own setting (not a lost connection).
            AccountState::Failed { error, at } if error.contains("unsupported") => push(Category::Setting, source, format!("{} 帳戶設定不支援：{error}", e.name()), Some((now_ms - at).max(0)), None),
            AccountState::Failed { error, at } => push(Category::Disconnected, source, format!("{} 帳戶讀取失敗：{error}", e.name()), Some((now_ms - at).max(0)), None),
            _ => {}
        }
    }

    // 3–5. Every expected source, judged at source level.
    let enabled: Vec<Exchange> = snap.settings.risk.allowed_exchanges.clone();
    let mut expected: Vec<SourceId> = Vec::new();
    if enabled.contains(&Exchange::Binance) {
        expected.push(SourceId::BinanceWs);
    }
    expected.extend(Exchange::ALL.into_iter().filter(|e| enabled.contains(e)).map(SourceId::MarketPoll));
    for e in alert_accounts(snap) {
        if matches!(snap.account(e), AccountState::Loaded { .. } | AccountState::Loading) {
            expected.push(SourceId::Account(e));
        }
    }
    // The Bybit WebSocket only overlays the REST poll: down means slower data, not no data, so it is a
    // notice (never a banner row) whatever the reason (offline, stale, unknown).
    if enabled.contains(&Exchange::Bybit) && !snap.bybit_ws_disabled {
        let up = snap.health_of(SourceId::BybitWs).is_some_and(|h| source_status(h, now_ms) == FreshStatus::Online);
        if !up {
            let age = snap.health_of(SourceId::BybitWs).and_then(|h| source_age_ms(h, now_ms));
            push(Category::Stale, SourceId::BybitWs.label(), "Bybit WebSocket 已斷線，行情退回 REST 輪詢（約每 10 秒更新）".into(), age, None);
        }
    }
    for id in expected {
        let label = id.label();
        let Some(h) = snap.health.iter().find(|h| h.source == id) else {
            push(Category::Disconnected, label.clone(), format!("{label} 健康狀態未知"), None, None);
            continue;
        };
        let age = source_age_ms(h, now_ms);
        match source_status(h, now_ms) {
            FreshStatus::Unknown => push(Category::Disconnected, label.clone(), format!("{label} 健康狀態未知"), None, None),
            FreshStatus::Offline => push(Category::Disconnected, label.clone(), format!("{label} 已斷線"), age, None),
            FreshStatus::Stale => {
                let msg = match age {
                    Some(a) => format!("{label}已 {} 秒未更新", format::secs(a)),
                    None => format!("{label}尚未取得資料"),
                };
                push(Category::Stale, label.clone(), msg, age, None);
            }
            FreshStatus::RateLimited | FreshStatus::Online => {}
        }
        if let Some(until) = h.rate_limited_until.filter(|u| *u > now_ms) {
            let ex = id.exchange().name();
            push(Category::RateLimitOrClock, format!("{ex} 限流"), format!("{ex} 限流退避中，剩 {} 秒", format::secs(until - now_ms)), None, None);
        }
    }

    // funding-pnl: funding / PnL data problems, each pointing at its event in the system log.
    for (pair_id, f) in &snap.funding {
        for al in &f.alerts {
            let msg = format!("{}（事件 #{}，見系統日誌）· 不會自動修正", al.text, al.event_id);
            // Bound to its event: an acknowledgement holds only for this finding (a new event shows again).
            funding_alerts.push(Alert {
                category: Category::FundingData,
                source: format!("{pair_id} {}", al.event_type),
                message: msg,
                age_ms: None,
                link: Some(Page::SystemLogs),
                fingerprint: al.event_id.to_string(),
                event_ms: Some(al.ts_ms),
            });
        }
    }

    // 5. Clocks of the exchanges that sign requests.
    for e in alert_accounts(snap) {
        let source = format!("{} 時鐘", e.name());
        match snap.clock(e) {
            ClockState::Unsynced => push(Category::RateLimitOrClock, source, format!("{} 時鐘未校時", e.name()), None, None),
            ClockState::Synced { offset_ms } if offset_ms.unsigned_abs() > RECV_WINDOW_MS / 2 => {
                let msg = format!("{} 校時偏移 {} ms 過大", e.name(), format::money(offset_ms.into(), 0));
                push(Category::RateLimitOrClock, source, msg, None, None);
            }
            ClockState::Synced { .. } => {}
        }
    }

    out.extend(funding_alerts);
    out.sort_by(|a, b| (a.category, &a.source, &a.message).cmp(&(b.category, &b.source, &b.message)));
    out.dedup_by(|b, a| a.category == b.category && a.source == b.source);
    out
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;

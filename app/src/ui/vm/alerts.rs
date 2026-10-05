//! Alert generation (task 3.1, spec alert-banner; design D6). One pure function turns source
//! health, system flags, clocks, accounts and pair states into an ordered, de-duplicated list.
//! Staleness is the source-level `feed-health` verdict (never the per-row 1 s rule); an unknown
//! state counts as an anomaly.

use tong_funding_core::types::Exchange;

use super::bridge::{AccountState, ClockState, KillSwitchState, SourceHealth, SourceId, UiSnapshot, ACCOUNT_EXCHANGES};
use super::format;
use crate::exchange::signed::signing::RECV_WINDOW_MS;
use crate::ui::nav::Page;

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
    RateLimitOrClock,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::ManualAttention => "需人工處理",
            Category::Halted => "停機",
            Category::Disconnected => "斷線",
            Category::Stale => "資料過期",
            Category::FundingData => "資料問題",
            Category::RateLimitOrClock => "限流 / 時鐘",
        }
    }
    /// Manual-attention and halt alerts have no close control.
    pub fn dismissible(self) -> bool {
        !matches!(self, Category::ManualAttention | Category::Halted | Category::FundingData)
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
    let mut push = |category: Category, source: String, message: String, age_ms: Option<i64>, link: Option<Page>| {
        out.push(Alert { category, source, message, age_ms, link });
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

    // 3. Disconnected / not connected accounts.
    for e in ACCOUNT_EXCHANGES {
        let source = SourceId::Account(e).label();
        match snap.account(e) {
            AccountState::NotConnected { reason } => push(Category::Disconnected, source, format!("{} 帳戶未連線：{reason}", e.name()), None, None),
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
    for e in ACCOUNT_EXCHANGES {
        if matches!(snap.account(e), AccountState::Loaded { .. } | AccountState::Loading) {
            expected.push(SourceId::Account(e));
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
            push(Category::FundingData, format!("{pair_id} {}", al.event_type), msg, None, Some(Page::SystemLogs));
        }
    }

    // 5. Clocks of the exchanges that sign requests.
    for e in ACCOUNT_EXCHANGES {
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

    out.sort_by(|a, b| (a.category, &a.source, &a.message).cmp(&(b.category, &b.source, &b.message)));
    out.dedup_by(|b, a| a.category == b.category && a.source == b.source);
    out
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;

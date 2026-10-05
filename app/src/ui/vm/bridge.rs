//! Data layer → UI bridge (task 1.1, design D2). The UI reads only [`UiSnapshot`]; it never calls
//! an adapter directly, except "refresh now", which is an explicit command sent through
//! [`ReadOnlyDataSource::request_refresh`].
//!
//! Update coalescing: market updates are merged into the snapshot as they arrive (nothing is
//! lost), but the expensive recompute of the page view-models runs at most [`MAX_RECOMPUTE_HZ`]
//! times per second. Countdowns tick on their own 1 Hz timer and never trigger a recompute.
//! Everything here is pure and takes the time as an argument.

use std::collections::BTreeMap;

use tong_funding_core::funding::FundingObservation;
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{RiskConfig, RiskOverrides};
use tong_funding_core::types::{Decimal, Exchange};

use crate::exchange::health::feed::HealthSnapshot;
use crate::exchange::signed::models::Position;
use crate::store::scan_buffer::ScanRecord;

/// Design D2: at most 2 recomputes per second (lower end of the shell's 2–10 Hz assumption; unverified).
pub const MAX_RECOMPUTE_HZ: i64 = 2;
/// Countdown cells update once per second, independently of data.
pub const COUNTDOWN_TICK_MS: i64 = 1_000;
/// Design D4: only these exchanges can take orders; OKX is price comparison only.
pub const TRADABLE_EXCHANGES: [Exchange; 2] = [Exchange::Binance, Exchange::Bybit];
/// Exchanges whose account (signed) data the pages show.
pub const ACCOUNT_EXCHANGES: [Exchange; 2] = [Exchange::Binance, Exchange::Bybit];

pub fn is_tradable(e: Exchange) -> bool {
    TRADABLE_EXCHANGES.contains(&e)
}

// ---- snapshot ---------------------------------------------------------------------------------

/// Latest public market data of one exchange. A failed fetch keeps the previous observations and
/// records the error; the observations are then shown as stale, never as new data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarketFeed {
    pub observations: Vec<FundingObservation>,
    /// Local time of the last successful fetch (`None` = never).
    pub last_success_at: Option<i64>,
    /// Message and local time of the latest failure, if it is newer than the last success.
    pub last_error: Option<(String, i64)>,
}

impl MarketFeed {
    /// True when the latest attempt failed (the observations are older than that attempt).
    pub fn failing(&self) -> bool {
        match (&self.last_error, self.last_success_at) {
            (Some((_, at)), Some(ok)) => *at >= ok,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }
}

/// A balance row before valuation: the asset and what the exchange said about its value.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetInput {
    pub asset: String,
    pub quantity: Decimal,
    /// The exchange's own USDT valuation, when it gives one.
    pub usdt_value: Option<Decimal>,
    /// `<ASSET>USDT` mark price on the same exchange, when known.
    pub mark_price: Option<Decimal>,
}

/// "Contract equity": used + available margin (design D9; field mapping unverified, task 4.1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContractEquity {
    pub used_margin: Decimal,
    pub available_margin: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountData {
    pub assets: Vec<AssetInput>,
    pub contract_equity: Option<ContractEquity>,
    pub positions: Vec<Position>,
    /// `Some(reason)` when the position list is a subset (pagination failed): never read as "no other positions".
    pub positions_incomplete: Option<String>,
    /// `DEMO` or `TESTNET`.
    pub environment: &'static str,
}

/// Account data of one exchange as the pages see it.
#[derive(Debug, Clone, PartialEq)]
pub enum AccountState {
    /// Public market data only (OKX).
    Unsupported,
    /// Missing keys, unsynced clock, ... (shown with the reason, never as zero).
    NotConnected { reason: String },
    Loading,
    /// First load failed and there is nothing to show.
    Failed { error: String, at: i64 },
    /// Data from the last successful poll; `error` is set when a later poll failed (data kept).
    Loaded { data: AccountData, fetched_at: i64, error: Option<(String, i64)> },
}

/// Which data source a health entry describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceId {
    BinanceWs,
    MarketPoll(Exchange),
    Account(Exchange),
}

impl SourceId {
    pub fn label(self) -> String {
        match self {
            SourceId::BinanceWs => "Binance WebSocket".into(),
            SourceId::MarketPoll(e) => format!("{} 行情", e.name()),
            SourceId::Account(e) => format!("{} 帳戶", e.name()),
        }
    }
    pub fn exchange(self) -> Exchange {
        match self {
            SourceId::BinanceWs => Exchange::Binance,
            SourceId::MarketPoll(e) | SourceId::Account(e) => e,
        }
    }
}

/// Health of one source as reported by `feed-health`; `health: None` = unknown (an anomaly).
#[derive(Debug, Clone, PartialEq)]
pub struct SourceHealth {
    pub source: SourceId,
    pub health: Option<HealthSnapshot>,
    /// Local time of the next scheduled poll (poll sources only).
    pub next_poll_at: Option<i64>,
    /// Rate-limit back-off end (local time), if backing off.
    pub rate_limited_until: Option<i64>,
}

/// Clock calibration of one exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockState {
    Unsynced,
    /// `offset_ms` = exchange time − local time.
    Synced { offset_ms: i64 },
}

/// Stored risk settings (as read from the `config` table).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub risk: RiskConfig,
    pub overrides: RiskOverrides,
    /// The stored settings could not be read or validated: everything that needs them shows "未設定".
    pub error: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { risk: RiskConfig::default(), overrides: RiskOverrides::new(), error: None }
    }
}

/// A pair as the UI knows it. `state: Err` = the stored state could not be read (treated as an anomaly).
#[derive(Debug, Clone, PartialEq)]
pub struct PairInfo {
    pub pair_id: String,
    pub symbol: String,
    pub long_exchange: Exchange,
    pub short_exchange: Exchange,
    pub state: Result<PairState, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSwitchState {
    Off,
    On,
    /// Could not be read: treated as halted (fail closed).
    Unknown,
}

/// System-wide flags: store halt and kill switch.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemFlags {
    /// `Some((reason, since_ms))` when the store is halted (fail closed).
    pub store_halt: Option<(String, i64)>,
    pub kill_switch: KillSwitchState,
}

impl Default for SystemFlags {
    fn default() -> Self {
        SystemFlags { store_halt: None, kill_switch: KillSwitchState::Off }
    }
}

/// Everything the pages render from. Contains no GPUI types and no live handles.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UiSnapshot {
    pub market: BTreeMap<Exchange, MarketFeed>,
    pub accounts: BTreeMap<Exchange, AccountState>,
    pub health: Vec<SourceHealth>,
    pub clocks: BTreeMap<Exchange, ClockState>,
    pub settings: Settings,
    pub pairs: Vec<PairInfo>,
    pub system: SystemFlags,
    /// This run's `SCAN_RUN` buffer (oldest first) and its capacity.
    pub scan_runs: Vec<ScanRecord>,
    pub scan_capacity: usize,
    /// Number of market updates merged so far (diagnostics; lets tests prove nothing was dropped).
    pub market_updates: u64,
    /// funding-pnl: funding / PnL data per `pair_id` (positions page).
    pub funding: BTreeMap<String, crate::ui::funding::PairFunding>,
}

impl UiSnapshot {
    pub fn account(&self, e: Exchange) -> AccountState {
        match self.accounts.get(&e) {
            Some(a) => a.clone(),
            None if !ACCOUNT_EXCHANGES.contains(&e) => AccountState::Unsupported,
            None => AccountState::Loading,
        }
    }
    pub fn health_of(&self, s: SourceId) -> Option<&SourceHealth> {
        self.health.iter().find(|h| h.source == s)
    }
    pub fn clock(&self, e: Exchange) -> ClockState {
        self.clocks.get(&e).copied().unwrap_or(ClockState::Unsynced)
    }
}

// ---- updates ----------------------------------------------------------------------------------

/// One change reported by the data source.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceUpdate {
    /// A full snapshot of one exchange's market (replaces that exchange's observations).
    Market { exchange: Exchange, observations: Vec<FundingObservation>, at: i64 },
    /// Partial update (e.g. a WebSocket frame): replaces only the given symbols.
    MarketPartial { exchange: Exchange, observations: Vec<FundingObservation>, at: i64 },
    MarketError { exchange: Exchange, error: String, at: i64 },
    Account { exchange: Exchange, state: AccountState },
    Health(Vec<SourceHealth>),
    Clock { exchange: Exchange, state: ClockState },
    Settings(Settings),
    Pairs(Vec<PairInfo>),
    System(SystemFlags),
    ScanRuns { records: Vec<ScanRecord>, capacity: usize },
    /// funding-pnl: replaces the per-pair funding data.
    Funding(BTreeMap<String, crate::ui::funding::PairFunding>),
}

impl SourceUpdate {
    /// Market data changes feed the rate-limited recompute; everything else is applied the same way.
    pub fn is_market(&self) -> bool {
        matches!(self, SourceUpdate::Market { .. } | SourceUpdate::MarketPartial { .. } | SourceUpdate::MarketError { .. })
    }
}

/// Applies one update to the snapshot. Pure.
pub fn apply_update(snap: &mut UiSnapshot, update: SourceUpdate) {
    match update {
        SourceUpdate::Market { exchange, observations, at } => {
            let feed = snap.market.entry(exchange).or_default();
            feed.observations = observations;
            feed.last_success_at = Some(at);
            feed.last_error = None;
            snap.market_updates += 1;
        }
        SourceUpdate::MarketPartial { exchange, observations, at } => {
            let feed = snap.market.entry(exchange).or_default();
            for o in observations {
                match feed.observations.iter_mut().find(|x| x.symbol == o.symbol) {
                    Some(slot) => *slot = o,
                    None => feed.observations.push(o),
                }
            }
            feed.last_success_at = Some(feed.last_success_at.map_or(at, |t| t.max(at)));
            snap.market_updates += 1;
        }
        SourceUpdate::MarketError { exchange, error, at } => {
            snap.market.entry(exchange).or_default().last_error = Some((error, at));
            snap.market_updates += 1;
        }
        SourceUpdate::Account { exchange, state } => {
            snap.accounts.insert(exchange, state);
        }
        SourceUpdate::Health(list) => {
            for h in list {
                match snap.health.iter_mut().find(|x| x.source == h.source) {
                    Some(slot) => *slot = h,
                    None => snap.health.push(h),
                }
            }
            snap.health.sort_by_key(|h| h.source);
        }
        SourceUpdate::Clock { exchange, state } => {
            snap.clocks.insert(exchange, state);
        }
        SourceUpdate::Settings(s) => snap.settings = s,
        SourceUpdate::Pairs(p) => snap.pairs = p,
        SourceUpdate::System(s) => snap.system = s,
        SourceUpdate::ScanRuns { records, capacity } => {
            snap.scan_runs = records;
            snap.scan_capacity = capacity;
        }
        SourceUpdate::Funding(f) => snap.funding = f,
    }
}

// ---- coalescing -------------------------------------------------------------------------------

/// Decides when a recompute may run: at most `max_hz` per second, never while nothing changed.
#[derive(Debug, Clone)]
pub struct Coalescer {
    min_interval_ms: i64,
    last_run_at: Option<i64>,
    dirty: bool,
}

impl Coalescer {
    pub fn new(max_hz: i64) -> Self {
        Coalescer { min_interval_ms: 1_000 / max_hz.max(1), last_run_at: None, dirty: false }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// True (and records the run) when there is a change and the minimum interval has passed.
    pub fn should_run(&mut self, now_ms: i64) -> bool {
        if !self.dirty {
            return false;
        }
        let due = self.last_run_at.is_none_or(|t| now_ms < t || now_ms - t >= self.min_interval_ms);
        if due {
            self.last_run_at = Some(now_ms);
            self.dirty = false;
        }
        due
    }

    /// Earliest time at which a pending change may be recomputed (`None` = nothing pending).
    pub fn next_due(&self) -> Option<i64> {
        self.dirty.then(|| self.last_run_at.map_or(i64::MIN, |t| t + self.min_interval_ms))
    }
}

/// Merges updates into a working snapshot immediately and hands out a copy for recomputation at
/// most `MAX_RECOMPUTE_HZ` times per second.
#[derive(Debug, Clone)]
pub struct Bridge {
    working: UiSnapshot,
    coalescer: Coalescer,
    recomputes: u64,
}

impl Default for Bridge {
    fn default() -> Self {
        Bridge::new(MAX_RECOMPUTE_HZ)
    }
}

impl Bridge {
    pub fn new(max_hz: i64) -> Self {
        Bridge { working: UiSnapshot::default(), coalescer: Coalescer::new(max_hz), recomputes: 0 }
    }

    pub fn push(&mut self, update: SourceUpdate) {
        apply_update(&mut self.working, update);
        self.coalescer.mark_dirty();
    }

    /// The snapshot to recompute from, if a recompute is due now; `None` otherwise.
    pub fn take_if_due(&mut self, now_ms: i64) -> Option<UiSnapshot> {
        if self.coalescer.should_run(now_ms) {
            self.recomputes += 1;
            Some(self.working.clone())
        } else {
            None
        }
    }

    pub fn recomputes(&self) -> u64 {
        self.recomputes
    }

    pub fn next_due(&self) -> Option<i64> {
        self.coalescer.next_due()
    }

    pub fn working(&self) -> &UiSnapshot {
        &self.working
    }
}

// ---- data source ------------------------------------------------------------------------------

/// Outcome of a "refresh now" request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshRequest {
    Started,
    /// A refresh is already running; the click is ignored (no extra requests).
    IgnoredInProgress,
}

/// Where the UI gets its data. The production implementation runs the adapters, feeds and
/// account polls; tests use scripted fakes.
pub trait ReadOnlyDataSource: Send + Sync {
    /// Pending updates since the last call (non-blocking).
    fn drain_updates(&self) -> Vec<SourceUpdate>;
    /// "Refresh now": re-fetch every enabled market source (bypassing the poll schedule).
    fn request_refresh(&self) -> RefreshRequest;
    /// True while a refresh is running.
    fn refresh_in_progress(&self) -> bool;
    /// One page of the event timeline (system log); `Err` when the store cannot be read.
    fn load_events(&self, query: &crate::store::event_query::EventQuery) -> Result<crate::store::event_query::EventPage, String>;
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;

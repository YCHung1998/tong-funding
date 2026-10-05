//! The production [`ReadOnlyDataSource`] (design D2): public market polls (every 10 s, the same
//! `refresh_sources` the "refresh now" button uses), the Binance mark-price WebSocket overlaid on
//! the Binance poll, clock sync, signed account polls (every 30 s) and the store (settings, pairs,
//! halt / kill switch, events, `SCAN_RUN` buffer). Everything runs on its own tokio runtime; the
//! UI only drains [`SourceUpdate`]s. Replaced by the engine snapshot in `engine-simulation`.
//!
//! Untested against the network here (no exchange access in CI); verified by task 4.1 on a Mac.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tong_funding_core::funding::FundingObservation;
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{RiskConfig, RiskOverrides, parse_overrides};
use tong_funding_core::types::{Decimal, Exchange};

use super::banner::read_system_flags;
use super::bridge::{
    AccountData, AccountState, AssetInput, ClockState, PairInfo, ReadOnlyDataSource, RefreshRequest, Settings, SourceHealth, SourceId, SourceUpdate,
};
use super::scanner_refresh::{RefreshGate, RefreshGuard, RefreshOutcome, refresh_sources};
use crate::exchange::error::AdapterError;
use crate::exchange::health::cache::MarkPriceCache;
use crate::exchange::health::clock_sync::{ClockStatus, ClockSync, RESYNC_INTERVAL_MS};
use crate::exchange::health::feed::{FeedHealth, MarkPriceFeed, TokioSleeper, TungsteniteSource};
use crate::exchange::health::gated::GatedTransport;
use crate::exchange::health::ratelimit::{BackoffState, RateLimiter, RequestClass, WeightGate};
use crate::exchange::public::binance::BinanceAdapter;
use crate::exchange::public::bybit::BybitAdapter;
use crate::exchange::public::endpoints::OKX_HOST;
use crate::exchange::public::feed_endpoints::BINANCE_MARK_PRICE_WS_URL;
use crate::exchange::public::okx::OkxAdapter;
use crate::exchange::reqwest_transport::ReqwestTransport;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost};
use crate::exchange::signed::models::{Balance, Completeness, Position};
use crate::exchange::signed::signing::{NotConnectedReason, Resync};
use crate::ports::{EventSink, SecretProvider, SystemClock, TimeSource};
use crate::store::db::Db;
use crate::store::event_query::{EventPage, EventQuery};
use crate::store::events::{EventStore, SCAN_RUN};
use crate::store::scan_buffer::DEFAULT_CAPACITY;
use crate::store::secrets::KeychainSecrets;
use crate::store::state::PairRow;

/// Market poll period (Python version: 10 s; unverified against the rate limits).
pub const MARKET_POLL_MS: i64 = 10_000;
/// Account poll period (Python version: 30 s).
pub const ACCOUNT_POLL_MS: i64 = 30_000;
/// How often the store (settings, pairs, flags) and health are re-read.
const STORE_POLL_MS: u64 = 2_000;
/// Environment variable choosing the Binance demo host among the compile-time constants
/// (`testnet` = default, `demo`); never a URL (exchange-readonly-adapters Open Question 2).
pub const BINANCE_HOST_ENV: &str = "TONG_FUNDING_BINANCE_HOST";

fn binance_host() -> BinanceHost {
    match std::env::var(BINANCE_HOST_ENV).ok().as_deref() {
        Some("demo") => BinanceHost::Demo,
        _ => BinanceHost::Testnet,
    }
}

fn env_label(h: BinanceHost) -> &'static str {
    match h {
        BinanceHost::Testnet => "TESTNET",
        BinanceHost::Demo => "DEMO",
    }
}

// ---- pure mappings (tested) -------------------------------------------------------------------

/// Signed balances and positions → the dashboard's account data. Non-USDT assets get the
/// `<ASSET>USDT` mark price of the same exchange when the exchange gave no USDT valuation.
/// Contract equity is left unset: which API fields map to it is unverified (design D9, task 4.1).
pub fn account_data(balances: &[Balance], positions: Vec<Position>, incomplete: Option<String>, marks: &HashMap<String, Decimal>, environment: &'static str) -> AccountData {
    let assets = balances
        .iter()
        .filter(|b| !b.amount.is_zero())
        .map(|b| AssetInput { asset: b.asset.clone(), quantity: b.amount, usdt_value: b.usdt_value, mark_price: marks.get(&format!("{}USDT", b.asset)).copied() })
        .collect();
    AccountData { assets, contract_equity: None, positions, positions_incomplete: incomplete, environment }
}

/// Stored pair rows → UI pairs. An unreadable state or entry becomes an anomaly, never dropped.
pub fn pair_infos(rows: &[PairRow]) -> Vec<PairInfo> {
    rows.iter()
        .filter_map(|r| {
            let state = r.status.parse::<PairState>().map_err(|e| e.to_string());
            if matches!(state, Ok(PairState::Finalized | PairState::Cancelled | PairState::Blocked)) {
                return None;
            }
            let leg = |k: &str| r.entry.get(k).cloned().and_then(|v| serde_json::from_value::<Exchange>(v).ok());
            match (leg("long_exchange"), leg("short_exchange")) {
                (Some(long_exchange), Some(short_exchange)) => Some(PairInfo { pair_id: r.pair_id.clone(), symbol: r.symbol.clone(), long_exchange, short_exchange, state }),
                _ => Some(PairInfo {
                    pair_id: r.pair_id.clone(),
                    symbol: r.symbol.clone(),
                    long_exchange: Exchange::Binance,
                    short_exchange: Exchange::Bybit,
                    state: Err(format!("pair {} entry has no leg exchanges", r.pair_id)),
                }),
            }
        })
        .collect()
}

pub fn not_connected_text(r: Option<NotConnectedReason>) -> String {
    match r {
        Some(NotConnectedReason::NoKey) => "Keychain 沒有 API key".into(),
        Some(NotConnectedReason::NoSecret) => "Keychain 沒有 API secret".into(),
        Some(NotConnectedReason::NoPassphrase) => "Keychain 沒有 passphrase".into(),
        Some(NotConnectedReason::ClockUnsynced) => "時鐘未校時".into(),
        Some(NotConnectedReason::SecretStoreError) => "無法讀取 Keychain".into(),
        None => "未連線".into(),
    }
}

/// Settings from the `config` table (same keys and validation as the engine).
pub fn load_settings(db: &Db) -> Settings {
    let risk = match db.config_get(crate::store::config_cli::KEY_RISK) {
        Ok(None) => Ok(RiskConfig::default()),
        Ok(Some(e)) => RiskConfig::from_json(&e.value.to_string()).map_err(|e| format!("risk: {e}")),
        Err(e) => Err(format!("risk: {e}")),
    };
    let overrides = match db.config_get(crate::store::config_cli::KEY_RISK_OVERRIDES) {
        Ok(None) => Ok(RiskOverrides::new()),
        Ok(Some(e)) => parse_overrides(&e.value).map_err(|e| format!("risk_overrides: {e}")),
        Err(e) => Err(format!("risk_overrides: {e}")),
    };
    match (risk, overrides) {
        (Ok(risk), Ok(overrides)) => Settings { risk, overrides, error: None },
        (r, o) => Settings { risk: r.clone().unwrap_or_default(), overrides: o.clone().unwrap_or_default(), error: r.err().or(o.err()) },
    }
}

// ---- the source -------------------------------------------------------------------------------

struct Shared {
    updates: Mutex<Vec<SourceUpdate>>,
}

impl Shared {
    fn push(&self, u: SourceUpdate) {
        self.updates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(u);
    }
}

pub struct LiveSource {
    shared: Arc<Shared>,
    gate: RefreshGate,
    refresh_tx: mpsc::UnboundedSender<RefreshGuard>,
    db: Option<Db>,
}

impl ReadOnlyDataSource for LiveSource {
    fn drain_updates(&self) -> Vec<SourceUpdate> {
        std::mem::take(&mut *self.shared.updates.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn request_refresh(&self) -> RefreshRequest {
        match self.gate.try_begin() {
            Some(guard) => {
                if self.refresh_tx.send(guard).is_ok() { RefreshRequest::Started } else { RefreshRequest::IgnoredInProgress }
            }
            None => RefreshRequest::IgnoredInProgress,
        }
    }

    fn refresh_in_progress(&self) -> bool {
        self.gate.in_progress()
    }

    fn load_events(&self, query: &EventQuery) -> Result<EventPage, String> {
        match &self.db {
            Some(db) => db.query_events(query).map_err(|e| e.to_string()),
            None => Err("資料庫不可用".into()),
        }
    }
}

type PublicT = GatedTransport<ReqwestTransport>;

struct ClockResync {
    sync: Arc<ClockSync>,
    transport: Arc<ReqwestTransport>,
    exchange: Exchange,
    base_url: &'static str,
}

impl Resync for ClockResync {
    fn resync(&self) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
        Box::pin(async move { self.sync.sync_once(self.transport.as_ref(), self.exchange, self.base_url).await.map(|_| ()) })
    }
}

impl LiveSource {
    /// Opens the store and starts every feed on a background runtime. Never blocks the UI.
    pub fn start() -> Arc<LiveSource> {
        let clock: Arc<SystemClock> = Arc::new(SystemClock);
        let db = Db::open_default(clock.clone()).ok();
        let shared = Arc::new(Shared { updates: Mutex::new(Vec::new()) });
        let (refresh_tx, refresh_rx) = mpsc::unbounded_channel();
        let gate = RefreshGate::default();
        let source = Arc::new(LiveSource { shared: shared.clone(), gate: gate.clone(), refresh_tx, db: db.clone() });

        std::thread::Builder::new()
            .name("tong-funding-data".into())
            .spawn(move || match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt.block_on(run(shared, db, gate, refresh_rx, clock)),
                Err(e) => eprintln!("data runtime failed to start: {e}"),
            })
            .ok();
        source
    }
}

/// Shared state of the running tasks.
struct Ctx {
    shared: Arc<Shared>,
    clock: Arc<SystemClock>,
    sink: Arc<dyn EventSink>,
    events: Option<EventStore>,
    limiter: Arc<RateLimiter>,
    market_health: BTreeMap<Exchange, Arc<FeedHealth>>,
    account_health: BTreeMap<Exchange, Arc<FeedHealth>>,
    clocks: BTreeMap<Exchange, Arc<ClockSync>>,
    next_market_poll: Mutex<i64>,
    next_account_poll: Mutex<i64>,
    /// Latest Binance REST observations (the WebSocket overlays rate / mark / next time on them).
    binance_rest: Mutex<HashMap<String, FundingObservation>>,
    marks: Mutex<BTreeMap<Exchange, HashMap<String, Decimal>>>,
    enabled: Mutex<BTreeSet<Exchange>>,
    last_accounts: Mutex<BTreeMap<Exchange, (AccountData, i64)>>,
}

struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: &str, _: Option<&str>, _: serde_json::Value) {}
}

async fn run(shared: Arc<Shared>, db: Option<Db>, gate: RefreshGate, mut refresh_rx: mpsc::UnboundedReceiver<RefreshGuard>, clock: Arc<SystemClock>) {
    let time: Arc<dyn TimeSource> = clock.clone();
    let events = db.clone().map(EventStore::new);
    let sink: Arc<dyn EventSink> = match &events {
        Some(es) => Arc::new(es.clone()),
        None => Arc::new(NullSink),
    };
    if db.is_none() {
        shared.push(SourceUpdate::System(super::bridge::SystemFlags {
            store_halt: Some(("無法定位或開啟資料庫（HOME 未設定？）".into(), clock_now(&clock))),
            kill_switch: super::bridge::KillSwitchState::Unknown,
        }));
    }
    let settings = db.as_ref().map(load_settings).unwrap_or_default();
    let stale_ms = settings.risk.stale_data_threshold_ms as i64;
    shared.push(SourceUpdate::Settings(settings.clone()));

    let mk_health = |name: &str, period: i64| Arc::new(FeedHealth::new(name, period, stale_ms, time.clone(), sink.clone()));
    let ctx = Arc::new(Ctx {
        shared: shared.clone(),
        clock: clock.clone(),
        sink: sink.clone(),
        events,
        limiter: Arc::new(RateLimiter::new(time.clone())),
        market_health: Exchange::ALL.into_iter().map(|e| (e, mk_health(&format!("{}_rest", e.name().to_lowercase()), MARKET_POLL_MS))).collect(),
        account_health: [Exchange::Binance, Exchange::Bybit].into_iter().map(|e| (e, mk_health(&format!("{}_account", e.name().to_lowercase()), ACCOUNT_POLL_MS))).collect(),
        clocks: Exchange::ALL.into_iter().map(|e| (e, Arc::new(ClockSync::new(time.clone())))).collect(),
        next_market_poll: Mutex::new(0),
        next_account_poll: Mutex::new(0),
        binance_rest: Mutex::new(HashMap::new()),
        marks: Mutex::new(BTreeMap::new()),
        enabled: Mutex::new(settings.risk.allowed_exchanges.iter().copied().collect()),
        last_accounts: Mutex::new(BTreeMap::new()),
    });

    let public = match ReqwestTransport::public_production() {
        Ok(t) => t,
        Err(e) => {
            for ex in Exchange::ALL {
                shared.push(SourceUpdate::MarketError { exchange: ex, error: e.to_string(), at: clock_now(&clock) });
            }
            return;
        }
    };
    let signed = ReqwestTransport::signed_demo().ok().map(Arc::new);
    let weight = Arc::new(WeightGate::new(time.clone()));
    let gated = |ex: Exchange| -> Result<Arc<PublicT>, AdapterError> {
        Ok(Arc::new(GatedTransport::new(ReqwestTransport::public_production()?, ex, ctx.limiter.clone(), weight.clone(), time.clone())))
    };
    let (Ok(tb), Ok(ty), Ok(to)) = (gated(Exchange::Binance), gated(Exchange::Bybit), gated(Exchange::Okx)) else { return };
    let dyn_clock: Arc<dyn crate::ports::Clock> = clock.clone();
    let adapters = Arc::new((BinanceAdapter::new(tb, dyn_clock.clone()), BybitAdapter::new(ty, dyn_clock.clone()), OkxAdapter::new(to, dyn_clock.clone())));

    // Binance mark-price WebSocket.
    let feed = Arc::new(MarkPriceFeed::new(time.clone(), sink.clone(), stale_ms));
    {
        let feed = feed.clone();
        tokio::spawn(async move {
            let mut src = TungsteniteSource::new(BINANCE_MARK_PRICE_WS_URL);
            feed.run(&mut src, &TokioSleeper).await;
        });
    }
    {
        let (ctx, cache) = (ctx.clone(), feed.cache());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                overlay_mark_prices(&ctx, &cache);
            }
        });
    }

    // Clock sync (signing hosts for Binance / Bybit, the public host for OKX countdowns).
    let bhost = binance_host();
    {
        let (ctx, signed, public) = (ctx.clone(), signed.clone(), Arc::new(public));
        tokio::spawn(async move {
            loop {
                for (ex, base) in [(Exchange::Binance, bhost.base_url()), (Exchange::Bybit, BybitHost::Demo.base_url())] {
                    if let Some(t) = &signed {
                        if ctx.clocks[&ex].resync_due(RESYNC_INTERVAL_MS) {
                            let _ = ctx.clocks[&ex].sync_once(t.as_ref(), ex, base).await;
                        }
                    }
                }
                if ctx.clocks[&Exchange::Okx].resync_due(RESYNC_INTERVAL_MS) {
                    let _ = ctx.clocks[&Exchange::Okx].sync_once(public.as_ref(), Exchange::Okx, OKX_HOST).await;
                }
                for ex in Exchange::ALL {
                    let state = match ctx.clocks[&ex].status() {
                        ClockStatus::Unsynced => ClockState::Unsynced,
                        ClockStatus::Synced { offset_ms, .. } => ClockState::Synced { offset_ms },
                    };
                    ctx.shared.push(SourceUpdate::Clock { exchange: ex, state });
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    // Account polls.
    if let Some(t) = signed.clone() {
        let secrets: Arc<dyn SecretProvider> = Arc::new(KeychainSecrets::system());
        let offset = |sync: Arc<ClockSync>| {
            move || match sync.status() {
                ClockStatus::Synced { offset_ms, .. } => Some(offset_ms),
                ClockStatus::Unsynced => None,
            }
        };
        let resync = |ex: Exchange, base: &'static str| Arc::new(ClockResync { sync: ctx.clocks[&ex].clone(), transport: t.clone(), exchange: ex, base_url: base });
        let binance = BinanceSignedClient::new(t.clone(), secrets.clone(), dyn_clock.clone(), Arc::new(offset(ctx.clocks[&Exchange::Binance].clone())), resync(Exchange::Binance, bhost.base_url()), bhost);
        let bybit = BybitSignedClient::new(t.clone(), secrets, dyn_clock.clone(), Arc::new(offset(ctx.clocks[&Exchange::Bybit].clone())), resync(Exchange::Bybit, BybitHost::Demo.base_url()), BybitHost::Demo);
        let ctx = ctx.clone();
        tokio::spawn(async move {
            // Give clock sync a moment so the first poll is not refused as "unsynced".
            tokio::time::sleep(Duration::from_secs(2)).await;
            loop {
                *ctx.next_account_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = clock_now(&ctx.clock) + ACCOUNT_POLL_MS;
                poll_binance_account(&ctx, &binance, env_label(bhost)).await;
                poll_bybit_account(&ctx, &bybit).await;
                tokio::time::sleep(Duration::from_millis(ACCOUNT_POLL_MS as u64)).await;
            }
        });
    } else {
        for ex in [Exchange::Binance, Exchange::Bybit] {
            shared.push(SourceUpdate::Account { exchange: ex, state: AccountState::NotConnected { reason: "簽名傳輸層無法建立".into() } });
        }
    }

    // Store + health refresher.
    {
        let (ctx, db, feed_health) = (ctx.clone(), db.clone(), feed.health());
        tokio::spawn(async move {
            loop {
                if let Some(db) = &db {
                    let settings = load_settings(db);
                    *ctx.enabled.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = settings.risk.allowed_exchanges.iter().copied().collect();
                    ctx.shared.push(SourceUpdate::Settings(settings));
                    ctx.shared.push(SourceUpdate::System(read_system_flags(db, clock_now(&ctx.clock))));
                    if let Ok(rows) = db.list_pairs() {
                        ctx.shared.push(SourceUpdate::Pairs(pair_infos(&rows)));
                        // funding-pnl: read-only funding / PnL data of the same pairs.
                        ctx.shared.push(SourceUpdate::Funding(crate::ui::funding::load_pair_funding(db, &rows, clock_now(&ctx.clock))));
                    }
                }
                push_health(&ctx, &feed_health);
                tokio::time::sleep(Duration::from_millis(STORE_POLL_MS)).await;
            }
        });
    }

    // Market polls and "refresh now" share one loop so they never run in parallel.
    let mut next = tokio::time::Instant::now();
    loop {
        let manual = tokio::select! {
            g = refresh_rx.recv() => match g { Some(g) => Some(g), None => return },
            _ = tokio::time::sleep_until(next) => None,
        };
        let guard = match manual {
            Some(g) => g,
            None => match gate.try_begin() {
                Some(g) => g,
                None => {
                    next = tokio::time::Instant::now() + Duration::from_millis(500);
                    continue;
                }
            },
        };
        let trigger = if next > tokio::time::Instant::now() { "manual" } else { "poll" };
        let enabled = ctx.enabled.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let out = refresh_sources(&adapters.0, &adapters.1, &adapters.2, &enabled, ctx.clock.as_ref()).await;
        apply_outcome(&ctx, &out, trigger);
        drop(guard);
        *ctx.next_market_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = clock_now(&ctx.clock) + MARKET_POLL_MS;
        next = tokio::time::Instant::now() + Duration::from_millis(MARKET_POLL_MS as u64);
    }
}

fn clock_now(c: &SystemClock) -> i64 {
    crate::ports::Clock::now_ms(c)
}

fn apply_outcome(ctx: &Ctx, out: &RefreshOutcome, trigger: &str) {
    let mut summary = serde_json::Map::new();
    for (ex, r) in &out.results {
        let h = &ctx.market_health[ex];
        match r {
            Ok(obs) => {
                h.on_success();
                let marks: HashMap<String, Decimal> = obs.iter().map(|o| (o.symbol.clone(), o.mark_price)).collect();
                ctx.marks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(*ex, marks);
                if *ex == Exchange::Binance {
                    *ctx.binance_rest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = obs.iter().map(|o| (o.symbol.clone(), o.clone())).collect();
                }
                summary.insert(ex.name().into(), json!({ "observations": obs.len() }));
            }
            Err(e) => {
                h.on_failure(e);
                summary.insert(ex.name().into(), json!({ "error": e.to_string() }));
            }
        }
    }
    for u in out.updates() {
        ctx.shared.push(u);
    }
    if let Some(es) = &ctx.events {
        es.emit(SCAN_RUN, None, json!({ "trigger": trigger, "duration_ms": out.finished_at - out.started_at, "sources": summary }));
        ctx.shared.push(SourceUpdate::ScanRuns { records: es.scan_runs(), capacity: DEFAULT_CAPACITY });
    }
}

/// Applies fresh WebSocket entries on top of the Binance poll (interval, volume and listing come
/// from the poll; rate, mark price and next settlement from the stream).
fn overlay_mark_prices(ctx: &Ctx, cache: &MarkPriceCache) {
    let rest = ctx.binance_rest.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let mut changed = Vec::new();
    let mut latest = 0;
    for c in cache.snapshot() {
        let Some(e) = c.data() else { continue };
        let Some(base) = rest.get(&e.symbol) else { continue };
        if e.exchange_timestamp <= base.exchange_timestamp {
            continue;
        }
        let mut o = base.clone();
        o.funding_rate = e.funding_rate;
        o.mark_price = e.mark_price;
        o.next_funding_time = e.next_funding_time;
        o.exchange_timestamp = e.exchange_timestamp;
        o.observed_at = e.observed_at;
        latest = latest.max(e.observed_at);
        changed.push(o);
    }
    if !changed.is_empty() {
        ctx.shared.push(SourceUpdate::MarketPartial { exchange: Exchange::Binance, observations: changed, at: latest });
    }
}

fn push_health(ctx: &Ctx, ws: &FeedHealth) {
    let now = clock_now(&ctx.clock);
    let backoff = |ex: Exchange, class: RequestClass| match ctx.limiter.state(ex, class) {
        BackoffState::Waiting { until_ms, .. } => Some(until_ms),
        BackoffState::Clear => None,
    };
    let next_market = *ctx.next_market_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let next_account = *ctx.next_account_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut list = vec![SourceHealth { source: SourceId::BinanceWs, health: Some(ws.snapshot()), next_poll_at: None, rate_limited_until: None }];
    for (ex, h) in &ctx.market_health {
        list.push(SourceHealth {
            source: SourceId::MarketPoll(*ex),
            health: Some(h.snapshot()),
            next_poll_at: (next_market > now).then_some(next_market),
            rate_limited_until: backoff(*ex, RequestClass::Batch),
        });
    }
    for (ex, h) in &ctx.account_health {
        list.push(SourceHealth {
            source: SourceId::Account(*ex),
            health: Some(h.snapshot()),
            next_poll_at: (next_account > now).then_some(next_account),
            rate_limited_until: backoff(*ex, RequestClass::Signed),
        });
    }
    ctx.shared.push(SourceUpdate::Health(list));
}

/// A failed account poll: keeps the last good data (never shows zero for a failure).
fn account_failed(ctx: &Ctx, ex: Exchange, error: &AdapterError, reason: Option<NotConnectedReason>) {
    let now = clock_now(&ctx.clock);
    let state = if matches!(error, AdapterError::NotConnected) {
        AccountState::NotConnected { reason: not_connected_text(reason) }
    } else {
        ctx.account_health[&ex].on_failure(error);
        match ctx.last_accounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&ex).cloned() {
            Some((data, fetched_at)) => AccountState::Loaded { data, fetched_at, error: Some((error.to_string(), now)) },
            None => AccountState::Failed { error: error.to_string(), at: now },
        }
    };
    ctx.shared.push(SourceUpdate::Account { exchange: ex, state });
}

fn account_loaded(ctx: &Ctx, ex: Exchange, data: AccountData) {
    let now = clock_now(&ctx.clock);
    ctx.account_health[&ex].on_success();
    ctx.last_accounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(ex, (data.clone(), now));
    ctx.shared.push(SourceUpdate::Account { exchange: ex, state: AccountState::Loaded { data, fetched_at: now, error: None } });
}

fn marks_of(ctx: &Ctx, ex: Exchange) -> HashMap<String, Decimal> {
    ctx.marks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&ex).cloned().unwrap_or_default()
}

async fn poll_binance_account(ctx: &Ctx, c: &BinanceSignedClient<ReqwestTransport>, env: &'static str) {
    let res = async { Ok::<_, AdapterError>((c.get_balances().await?, c.get_positions().await?)) }.await;
    match res {
        Ok((balances, positions)) => {
            let data = account_data(&balances, positions, None, &marks_of(ctx, Exchange::Binance), env);
            account_loaded(ctx, Exchange::Binance, data);
        }
        Err(e) => account_failed(ctx, Exchange::Binance, &e, c.last_not_connected_reason()),
    }
}

async fn poll_bybit_account(ctx: &Ctx, c: &BybitSignedClient<ReqwestTransport>) {
    let res = async { Ok::<_, AdapterError>((c.get_balances().await?, c.get_positions().await?)) }.await;
    match res {
        Ok((balances, listing)) => {
            let incomplete = match listing.completeness {
                Completeness::Complete => None,
                Completeness::Incomplete { reason } => Some(reason),
            };
            let data = account_data(&balances, listing.items, incomplete, &marks_of(ctx, Exchange::Bybit), "DEMO");
            account_loaded(ctx, Exchange::Bybit, data);
        }
        Err(e) => account_failed(ctx, Exchange::Bybit, &e, c.last_not_connected_reason()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testkit::{d, position};
    use serde_json::json;

    fn bal(asset: &str, amount: &str, usdt: Option<&str>) -> Balance {
        Balance { exchange: Exchange::Binance, asset: asset.into(), amount: d(amount), available: None, usdt_value: usdt.map(d), fetched_at: 0 }
    }

    #[test]
    fn balances_map_to_assets_with_same_exchange_marks_and_zero_rows_dropped() {
        let marks: HashMap<String, Decimal> = [("BTCUSDT".to_string(), d("60200"))].into();
        let data = account_data(&[bal("USDT", "16200", None), bal("BTC", "0.05", None), bal("BNB", "0", None)], vec![], None, &marks, "TESTNET");
        assert_eq!(data.assets.len(), 2);
        assert_eq!(data.assets[1].mark_price, Some(d("60200")));
        assert_eq!(data.assets[0].mark_price, None);
        assert_eq!(data.contract_equity, None, "mapping unverified (design D9)");
        assert_eq!(data.environment, "TESTNET");
        let _ = position(Exchange::Binance, "BTCUSDT", "1", "1", "1", "1", "0", None);
    }

    fn row(status: &str, entry: serde_json::Value) -> PairRow {
        PairRow { internal_uuid: "u".into(), pair_id: "p1".into(), symbol: "BTCUSDT".into(), status: status.into(), entry, created_ms: 0, updated_ms: 0 }
    }

    #[test]
    fn pair_rows_map_states_and_legs_and_unreadable_ones_are_anomalies() {
        let legs = json!({"long_exchange": "Binance", "short_exchange": "Bybit", "settlement_ms": 0, "simulated": true, "scan": {}});
        let p = pair_infos(&[row("PARTIAL_FAILURE", legs.clone())]);
        assert_eq!(p[0].state, Ok(PairState::PartialFailure));
        assert_eq!((p[0].long_exchange, p[0].short_exchange), (Exchange::Binance, Exchange::Bybit));
        assert!(pair_infos(&[row("FINALIZED", legs.clone())]).is_empty(), "closed pairs are not shown");
        assert!(pair_infos(&[row("WEIRD", legs)])[0].state.is_err());
        assert!(pair_infos(&[row("RECONCILED", json!({}))])[0].state.is_err());
    }

    #[test]
    fn settings_load_from_the_engine_keys_and_bad_values_are_reported() {
        let (_d, db, _) = crate::store::db::test_support::open_tmp();
        assert_eq!(load_settings(&db).error, None);
        db.config_set("risk", &json!({"net_edge_threshold_pct": "0.01"}), None).unwrap();
        assert_eq!(load_settings(&db).risk.net_edge_threshold_pct, Some(d("0.01")));
        db.config_set("risk_overrides", &json!({"Bybit": {"allowed_coins": []}}), None).unwrap();
        assert!(load_settings(&db).error.unwrap().contains("allowed_coins"));
    }
}

//! The production [`ReadOnlyDataSource`] (design D2): public market polls (every 10 s, the same
//! `refresh_sources` the "refresh now" button uses), the Binance mark-price WebSocket overlaid on
//! the Binance poll, clock sync, signed account polls (every 30 s) and the store (settings, pairs,
//! halt / kill switch, events, `SCAN_RUN` buffer). Everything runs on its own tokio runtime; the
//! UI only drains [`SourceUpdate`]s.
//!
//! Composition root (ui-trading-pages A): on the same runtime and with the same single `Db`, it
//! builds the real `EngineDeps` (simulator fed from these market polls, demo executor factory over
//! the Keychain, demo / simulated account views, `RecoveryReconciler`, public `MarketData`, clock
//! offsets) and starts the engine. Engine snapshots are forwarded as `SourceUpdate::Engine`; page
//! commands go to the engine through [`CommandSink`] (no page holds an exchange client).
//!
//! funding-pnl: the same root starts the funding ledger loop (`funding::runner`, real signed
//! ledger sources over the Keychain keys): due fetches, the reconciliation schedule, and an
//! immediate `SourceUpdate::Funding` after anything was written.
//!
//! Untested against the network here (no exchange access in CI); verified by task 4.1 on a Mac.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::{mpsc, oneshot, watch};
use tong_funding_core::funding::FundingObservation;
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{RiskConfig, RiskOverrides, parse_overrides};
use tong_funding_core::types::{Decimal, Exchange};

use super::banner::read_system_flags;
use super::bridge::{CapReading, 
    AccountData, AccountState, AssetInput, ClockState, CommandOutcome, CommandSink, ContractTemplate, LegAccount, PairInfo, ReadOnlyDataSource, RefreshRequest, Settings,
    SourceHealth, SourceId, SourceUpdate,
};
use super::wiring::{self, ClockOffsets, PublicMarketData};
use crate::engine::actor::{self as engine_actor, CommandMsg, EngineDeps, MarketPrices};
use crate::engine::alert::LogNotifier;
use crate::engine::command::{Command, CommandReply};
use crate::engine::ports::{AccountView, ExecutorFactory, ServerOffsets, StartupReconciler};
use crate::engine::preflight::{NoProbes, PreflightProbes};
use crate::identity::ProcessIdentity;
use crate::funding::slot_runner::RateSource;
use super::live_env;
use crate::engine::recovery::RecoveryReconciler;
use crate::engine::sim::{MarginFromAccount, SimPriceBook, SimulatedExecutor};
use crate::engine::timings::EngineTimings;
use crate::exchange::execution::http::{SignedDemoOrderTransport, SignedLiveOrderTransport};
use crate::exchange::execution::okx::{OkxLimits, OkxLimitsSource};
use crate::exchange::public::adapter::ExchangeAdapter;
use crate::exchange::signed::signing::load_credentials;
use crate::funding::fetch::{LedgerSource, TokioPause};
use crate::funding::runner::{self as funding_runner, FundingLoop};
use crate::funding::slot_runner;
use super::scanner_refresh::{RefreshGate, RefreshGuard, RefreshOutcome, refresh_sources};
use crate::exchange::error::AdapterError;
use crate::exchange::health::bybit_feed::{overlay_bybit, BybitTickerFeed, TickerCache};
use crate::exchange::health::cache::MarkPriceCache;
use crate::exchange::health::clock_sync::{ClockStatus, ClockSync, RESYNC_INTERVAL_MS};
use crate::exchange::health::feed::{FeedHealth, MarkPriceFeed, TokioSleeper, TungsteniteSource};
use crate::exchange::health::gated::GatedTransport;
use crate::exchange::health::ratelimit::{BackoffState, RateLimiter, RequestClass, WeightGate};
use crate::exchange::public::binance::BinanceAdapter;
use crate::exchange::public::bybit::BybitAdapter;
use crate::exchange::public::endpoints::OKX_HOST;
use crate::exchange::public::feed_endpoints::{BybitFeedMode, BINANCE_MARK_PRICE_WS_URL, BYBIT_FEED_ENV, BYBIT_TICKERS_WS_URL};
use crate::exchange::public::okx::OkxAdapter;
use crate::exchange::reqwest_transport::{PublicTransport, SignedDemoTransport, SignedLiveTransport};
use crate::exchange::transport::HttpTransport;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{BinanceEnv, BinanceHost, BybitEnv};
use crate::exchange::signed::okx::{OkxPosition, OkxSignedClient};
use crate::exchange::signed::models::{Balance, Completeness, Position, PositionMode};
use crate::exchange::signed::signing::NotConnectedReason;
use crate::ports::{EventSink, SecretProvider, SystemClock, TimeSource};
use crate::store::db::Db;
use crate::store::event_query::{EventPage, EventQuery};
use crate::store::events::{EventStore, SCAN_RUN};
use crate::store::scan_buffer::DEFAULT_CAPACITY;
use crate::store::secrets::BundleSecrets;
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

pub(super) fn binance_host() -> BinanceHost {
    match std::env::var(BINANCE_HOST_ENV).ok().as_deref() {
        Some("demo") => BinanceHost::Demo,
        _ => BinanceHost::Testnet,
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

/// An OKX position in coins: the exchange reports CONTRACTS, so the coin amount is `contracts x ctVal`
/// (signed: long positive, short negative). Without a usable `ctVal` there is no coin amount and the
/// error says so with the contracts as reported: a contract is never shown as a coin.
pub fn okx_to_position(p: &OkxPosition, ct_val: Option<Decimal>) -> Result<Position, String> {
    let Some(ct) = ct_val.filter(|c| *c > Decimal::ZERO) else {
        let side = if p.contracts.is_sign_negative() { "空" } else { "多" };
        return Err(format!("{} {side} {} 張（無法換算：ctVal 未知）", p.symbol, p.contracts.abs().normalize()));
    };
    let quantity = p.contracts * ct;
    Ok(Position {
        exchange: Exchange::Okx,
        symbol: p.symbol.clone(),
        side: if quantity.is_sign_negative() { tong_funding_core::types::Side::Short } else { tong_funding_core::types::Side::Long },
        quantity,
        entry_price: p.avg_px,
        mark_price: p.mark_px,
        leverage: p.lever,
        unrealized_pnl: p.upl,
        margin: p.imr,
        notional: p.notional_usd,
        mode: PositionMode::OneWay,
        fetched_at: p.fetched_at,
    })
}

/// OKX balances and positions → the dashboard's account data. Positions are converted to coins;
/// one that cannot be (no `ctVal`) is left out of `positions` AND named in `positions_incomplete`,
/// so the list is never read as complete. Contract equity stays unset, like the other exchanges: the
/// wallet rows already include the margin in use, so adding used + available margin would count it twice
/// (the field mapping is unverified, design D9).
pub fn okx_account_data(balances: &[Balance], positions: Vec<OkxPosition>, incomplete: Option<String>, ct_val: &dyn Fn(&str) -> Option<Decimal>, marks: &HashMap<String, Decimal>) -> AccountData {
    let mut data = account_data(balances, vec![], None, marks, "DEMO");
    let mut skipped = Vec::new();
    for p in &positions {
        match okx_to_position(p, ct_val(&p.symbol)) {
            Ok(pos) => data.positions.push(pos),
            Err(why) => skipped.push(why),
        }
    }
    let own = (!skipped.is_empty()).then(|| skipped.join("；"));
    data.positions_incomplete = match (incomplete, own) {
        (Some(reason), Some(own)) => Some(format!("{reason}；{own}")),
        (reason, own) => reason.or(own),
    };
    data
}

/// A mark price older than this is not evidence for the OKX notional cap (a missing mark refuses opens).
pub const MARK_MAX_AGE_MS: i64 = 60_000;
/// The live (manual-order) OKX leg cap is the same multiple of the contract template notional as a pair's
/// own cap is of its entry notional.
use crate::exchange::execution::okx::OKX_LEG_CAP_FACTOR;

/// The per-leg cap for OKX orders; `None` (unknown: opens are refused) when the template is unreadable.
pub fn okx_leg_cap(settings: &Settings) -> Option<Decimal> {
    settings.contract_error.is_none().then(|| settings.contract.notional_usdt * Decimal::from(OKX_LEG_CAP_FACTOR))
}

/// What the OKX size guard may know about `symbol`: `(ctVal, lotSz)` from the catalog, a fresh mark
/// price and the cap. Anything missing is `None` (an opening order is then not sent).
pub fn okx_limits_from(table: &HashMap<String, (Decimal, Decimal)>, marks: &HashMap<String, (Decimal, i64)>, cap: Option<Decimal>, symbol: &str, now_ms: i64) -> Option<OkxLimits> {
    let (ct_val, lot_sz) = *table.get(symbol)?;
    let (mark_px, at) = *marks.get(symbol)?;
    if now_ms.saturating_sub(at) > MARK_MAX_AGE_MS {
        return None;
    }
    Some(OkxLimits { ct_val, lot_sz, mark_px, max_leg_notional: cap? })
}

/// The live [`OkxLimitsSource`]: filled by the market poll (catalog and marks) and the settings
/// reader (cap); read by the executor, synchronously, before every OKX order.
#[derive(Default)]
pub struct LiveOkxLimits {
    state: Mutex<LimitsState>,
}

#[derive(Default)]
struct LimitsState {
    table: HashMap<String, (Decimal, Decimal)>,
    marks: HashMap<String, (Decimal, i64)>,
    cap: Option<Decimal>,
}

impl LiveOkxLimits {
    fn state(&self) -> std::sync::MutexGuard<'_, LimitsState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub fn table(&self) -> HashMap<String, (Decimal, Decimal)> {
        self.state().table.clone()
    }
    pub fn set_table(&self, table: HashMap<String, (Decimal, Decimal)>) {
        self.state().table = table;
    }
    pub fn set_marks(&self, marks: &HashMap<String, Decimal>, at: i64) {
        self.state().marks = marks.iter().map(|(k, v)| (k.clone(), (*v, at))).collect();
    }
    pub fn set_cap(&self, cap: Option<Decimal>) {
        self.state().cap = cap;
    }
}

impl OkxLimitsSource for LiveOkxLimits {
    fn limits(&self, symbol: &str) -> Option<OkxLimits> {
        let st = self.state();
        okx_limits_from(&st.table, &st.marks, st.cap, symbol, clock_now(&SystemClock))
    }
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

/// live-process 5.2: the settings as this process sees them: a live process never lists OKX in
/// `allowed_exchanges` (same rule as the engine's `load_risk`).
pub fn settings_for(identity: crate::identity::ProcessIdentity, mut s: Settings) -> Settings {
    if identity.is_live() {
        s.risk.allowed_exchanges.retain(|e| *e != Exchange::Okx);
    }
    s
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
    let (contract, contract_error) = match db.config_get(crate::engine::actor::CONFIG_CONTRACT_TEMPLATE) {
        Ok(None) => (ContractTemplate::default(), None),
        Ok(Some(e)) => match ContractTemplate::from_json(&e.value) {
            Ok(t) => (t, None),
            Err(why) => (ContractTemplate::default(), Some(why)),
        },
        Err(e) => (ContractTemplate::default(), Some(format!("contract_template: {e}"))),
    };
    match (risk, overrides) {
        (Ok(risk), Ok(overrides)) => Settings { risk, overrides, error: None, contract, contract_error },
        (r, o) => Settings { risk: r.clone().unwrap_or_default(), overrides: o.clone().unwrap_or_default(), error: r.err().or(o.err()), contract, contract_error },
    }
}

// ---- the source -------------------------------------------------------------------------------

struct Shared {
    updates: Mutex<Vec<SourceUpdate>>,
    /// Set once the engine runs: its command queue and the runtime to send from.
    engine: std::sync::OnceLock<(mpsc::Sender<CommandMsg>, tokio::runtime::Handle)>,
    /// "Fetch the lot rules of this symbol" requests from the pages.
    rules_tx: std::sync::OnceLock<mpsc::UnboundedSender<(Exchange, String)>>,
    /// "Read the leverage cap of this symbol at this notional" requests from the pages (symbol-leverage-cap).
    cap_tx: std::sync::OnceLock<mpsc::UnboundedSender<(Exchange, String, Decimal)>>,
    /// Last time a cap request was queued per key: the pages ask on every render, this keeps it to one per interval.
    cap_asked: Mutex<HashMap<(Exchange, String, Decimal), std::time::Instant>>,
}

/// A leverage cap is re-read at most this often per (exchange, symbol, notional).
const CAP_REFRESH: Duration = Duration::from_secs(20);

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
    identity: crate::identity::ProcessIdentity,
}

impl ReadOnlyDataSource for LiveSource {
    fn identity(&self) -> crate::identity::ProcessIdentity {
        self.identity
    }

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

    fn request_rules(&self, exchange: Exchange, symbol: &str) {
        if let Some(tx) = self.shared.rules_tx.get() {
            let _ = tx.send((exchange, symbol.trim().to_ascii_uppercase()));
        }
    }

    fn recheck_funding(&self, pair_id: &str) -> Result<usize, String> {
        let db = self.db.as_ref().ok_or("資料庫不可用")?;
        db.request_slot_recheck(pair_id, crate::ports::Clock::now_ms(&SystemClock)).map_err(|e| e.to_string())
    }

    fn request_leverage_cap(&self, exchange: Exchange, symbol: &str, notional: Decimal) {
        let Some(tx) = self.shared.cap_tx.get() else { return };
        let key = (exchange, symbol.trim().to_ascii_uppercase(), notional);
        let now = std::time::Instant::now();
        let mut asked = self.shared.cap_asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if asked.get(&key).is_some_and(|t| now.duration_since(*t) < CAP_REFRESH) {
            return;
        }
        asked.insert(key.clone(), now);
        let _ = tx.send(key);
    }

    fn load_ui_prefs(&self) -> Result<Option<serde_json::Value>, String> {
        self.load_config(super::zoom::KEY_UI_PREFS)
    }

    fn save_ui_prefs(&self, prefs: &serde_json::Value) -> Result<(), String> {
        self.save_config(super::zoom::KEY_UI_PREFS, prefs)
    }

    fn load_alert_acks(&self) -> Result<Option<serde_json::Value>, String> {
        self.load_config(KEY_ALERT_ACKS)
    }

    fn save_alert_acks(&self, acks: &serde_json::Value) -> Result<(), String> {
        self.save_config(KEY_ALERT_ACKS, acks)
    }
}

impl CommandSink for LiveSource {
    /// Sends `command` to the engine on the data runtime; the reply comes back as
    /// `SourceUpdate::CommandResult`. Without a running engine the command is refused, never queued.
    fn send(&self, label: String, command: Command) {
        let shared = self.shared.clone();
        match shared.engine.get().cloned() {
            None => shared.push(SourceUpdate::CommandResult(CommandOutcome { label, reply: CommandReply::Rejected("引擎未啟動".into()), at: clock_now(&SystemClock) })),
            Some((tx, rt)) => {
                rt.spawn(async move {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    let reply = if tx.send(CommandMsg { command, reply: Some(reply_tx) }).await.is_err() {
                        CommandReply::Rejected("engine stopped".into())
                    } else {
                        reply_rx.await.unwrap_or_else(|_| CommandReply::Rejected("engine stopped before replying".into()))
                    };
                    shared.push(SourceUpdate::CommandResult(CommandOutcome { label, reply, at: clock_now(&SystemClock) }));
                });
            }
        }
    }
}

pub(super) type PublicT = GatedTransport<PublicTransport>;

/// `config` key of the acknowledged notifications (header badge).
const KEY_ALERT_ACKS: &str = "alert_acks";

impl LiveSource {
    fn load_config(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let db = self.db.as_ref().ok_or("資料庫不可用")?;
        db.config_get(key).map(|e| e.map(|e| e.value)).map_err(|e| e.to_string())
    }

    /// Optimistic version: on a conflict re-read the latest version once and overwrite.
    fn save_config(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        use crate::store::db::StoreError;
        let db = self.db.as_ref().ok_or("資料庫不可用")?;
        let version = |db: &Db| db.config_get(key).map(|e| e.map(|e| e.version)).map_err(|e| e.to_string());
        match db.config_set(key, value, version(db)?) {
            Ok(_) => Ok(()),
            Err(StoreError::VersionConflict { .. }) => db.config_set(key, value, version(db)?).map(|_| ()).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Starts every feed and the engine on a background runtime over the ONE store instance the
    /// composition root opened (`None` = no store: the engine does not start). Never blocks the UI.
    pub fn start(db: Option<Db>, identity: crate::identity::ProcessIdentity) -> Arc<LiveSource> {
        let clock: Arc<SystemClock> = Arc::new(SystemClock);
        let shared = Arc::new(Shared { updates: Mutex::new(Vec::new()), engine: std::sync::OnceLock::new(), rules_tx: std::sync::OnceLock::new(), cap_tx: std::sync::OnceLock::new(), cap_asked: Mutex::new(HashMap::new()) });
        let (refresh_tx, refresh_rx) = mpsc::unbounded_channel();
        let gate = RefreshGate::default();
        let source = Arc::new(LiveSource { shared: shared.clone(), gate: gate.clone(), refresh_tx, db: db.clone(), identity });

        std::thread::Builder::new()
            .name("tong-funding-data".into())
            .spawn(move || match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt.block_on(run(shared, db, gate, refresh_rx, clock, identity)),
                Err(e) => eprintln!("data runtime failed to start: {e}"),
            })
            .ok();
        source
    }
}

/// Shared state of the running tasks.
pub(super) struct Ctx {
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
    /// Latest Bybit REST observations (the ticker WebSocket overlays rate / mark / next time on them).
    bybit_rest: Mutex<HashMap<String, FundingObservation>>,
    marks: Mutex<BTreeMap<Exchange, HashMap<String, Decimal>>>,
    enabled: Mutex<BTreeSet<Exchange>>,
    last_accounts: Mutex<BTreeMap<Exchange, (AccountData, i64)>>,
    /// Simulator price book and the engine's market watch, once the engine runs.
    feed: std::sync::OnceLock<(Arc<SimPriceBook>, watch::Sender<MarketPrices>)>,
    /// live-banners: the process identity, handed to the engine.
    identity: crate::identity::ProcessIdentity,
}

struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: &str, _: Option<&str>, _: serde_json::Value) {}
}

async fn run(shared: Arc<Shared>, db: Option<Db>, gate: RefreshGate, mut refresh_rx: mpsc::UnboundedReceiver<RefreshGuard>, clock: Arc<SystemClock>, identity: crate::identity::ProcessIdentity) {
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
    let settings = db.as_ref().map(load_settings).map(|s| settings_for(identity, s)).unwrap_or_default();
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
        account_health: [Exchange::Binance, Exchange::Bybit, Exchange::Okx].into_iter().map(|e| (e, mk_health(&format!("{}_account", e.name().to_lowercase()), ACCOUNT_POLL_MS))).collect(),
        clocks: Exchange::ALL.into_iter().map(|e| (e, Arc::new(ClockSync::new(time.clone())))).collect(),
        next_market_poll: Mutex::new(0),
        next_account_poll: Mutex::new(0),
        binance_rest: Mutex::new(HashMap::new()),
        bybit_rest: Mutex::new(HashMap::new()),
        marks: Mutex::new(BTreeMap::new()),
        enabled: Mutex::new(settings.risk.allowed_exchanges.iter().copied().collect()),
        last_accounts: Mutex::new(BTreeMap::new()),
        feed: std::sync::OnceLock::new(),
        identity,
    });

    let public = match PublicTransport::new() {
        Ok(t) => t,
        Err(e) => {
            for ex in Exchange::ALL {
                shared.push(SourceUpdate::MarketError { exchange: ex, error: e.to_string(), at: clock_now(&clock) });
            }
            return;
        }
    };
    if let Some(w) = proxy_env_warning(|k| std::env::var(k).ok()) {
        eprintln!("{w}");
    }
    let weight = Arc::new(WeightGate::new(time.clone()));
    let gated = |ex: Exchange| -> Result<Arc<PublicT>, AdapterError> {
        Ok(Arc::new(GatedTransport::new(PublicTransport::new()?, ex, ctx.limiter.clone(), weight.clone(), time.clone())))
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

    // Bybit ticker WebSocket: overlays the REST poll unless the roll-back switch says `rest`.
    let bybit_mode = BybitFeedMode::from_env_value(std::env::var(BYBIT_FEED_ENV).ok().as_deref());
    let bybit_feed = Arc::new(BybitTickerFeed::new(time.clone(), sink.clone(), stale_ms));
    if start_bybit_feed(bybit_mode) {
        {
            let (feed, ctx) = (bybit_feed.clone(), ctx.clone());
            tokio::spawn(async move {
                let mut src = TungsteniteSource::new(BYBIT_TICKERS_WS_URL);
                let symbols = move || bybit_ws_symbols(&ctx.bybit_rest.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                feed.run(&mut src, symbols, &TokioSleeper).await;
            });
        }
        {
            let (ctx, feed) = (ctx.clone(), bybit_feed.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if !feed.health_snapshot().stale {
                        overlay_bybit_prices(&ctx, &feed.cache());
                    }
                }
            });
        }
    } else {
        shared.push(SourceUpdate::BybitWsDisabled);
    }
    let bybit_health = start_bybit_feed(bybit_mode).then(|| bybit_feed.health());

    // The environment bundle (live-process 5.1): everything that depends on WHICH world this
    // process lives in is built by ONE builder per identity (`live_env`); the rest of `run` sees
    // trait objects only. Keychain item, hosts and transports all follow the identity; the Demo
    // builder never sees a live host and the Live one takes no host argument at all.
    let public = Arc::new(public);
    let secrets: Arc<dyn SecretProvider> = Arc::new(BundleSecrets::system_for(identity));
    let offsets: Arc<dyn ServerOffsets> = Arc::new(ClockOffsets { clocks: ctx.clocks.clone() });
    let okx_limits = Arc::new(LiveOkxLimits::default());
    okx_limits.set_cap(okx_leg_cap(&settings));
    let common = live_env::Common {
        secrets: secrets.clone(),
        clock: dyn_clock.clone(),
        time: time.clone(),
        clocks: ctx.clocks.clone(),
        offsets: offsets.clone(),
        limiter: ctx.limiter.clone(),
        db: db.clone(),
    };
    let bundle = match identity {
        ProcessIdentity::Demo => {
            let signed = SignedDemoTransport::new().ok().map(Arc::new);
            let orders = SignedDemoOrderTransport::new().map(Arc::new).map_err(|e| e.to_string());
            let parts = live_env::DemoParts { bhost: binance_host(), okx_public: public.clone(), adapters: adapters.clone(), okx_limits: okx_limits.clone() };
            live_env::build_demo_env(signed, orders, common, parts)
        }
        ProcessIdentity::Live => {
            let signed = SignedLiveTransport::new().ok().map(Arc::new);
            let orders = SignedLiveOrderTransport::new().map(Arc::new).map_err(|e| e.to_string());
            // live-guardrails: the real preflight probes (live keys, live position mode, live database, disk)
            let probes = live_env::LiveProbes::new(secrets.clone(), orders.clone().ok(), offsets.clone(), time.clone(), db.clone());
            live_env::build_live_env(signed, orders, common, Arc::new(probes))
        }
    };
    let okx_latch = bundle.okx_latch.clone();

    // Clock sync (the signing hosts of this environment for Binance / Bybit; the public host for OKX:
    // countdowns and, since okx-trading-enablement, the OKX signing offset).
    {
        let (ctx, sync, public) = (ctx.clone(), bundle.clock_sync.clone(), public.clone());
        tokio::spawn(async move {
            loop {
                if let Some(s) = &sync {
                    s.sync_due(&ctx.clocks).await;
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

    // Account polls (the same signed clients back the engine's account view).
    match bundle.poller.clone() {
        Some(poller) => {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                // Give clock sync a moment so the first poll is not refused as "unsynced".
                tokio::time::sleep(Duration::from_secs(2)).await;
                loop {
                    *ctx.next_account_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = clock_now(&ctx.clock) + ACCOUNT_POLL_MS;
                    poller.poll(&ctx).await;
                    tokio::time::sleep(Duration::from_millis(ACCOUNT_POLL_MS as u64)).await;
                }
            });
        }
        None => {
            for ex in super::bridge::tradable_exchanges(identity).iter().copied() {
                shared.push(SourceUpdate::Account { exchange: ex, state: AccountState::NotConnected { reason: "簽名傳輸層無法建立".into() } });
            }
        }
    }

    // The engine (composition root): same runtime, same store, real ports.
    let engine_running = match &db {
        Some(db) => start_engine(&ctx, db, adapters.clone(), bundle.account.clone(), bundle.factory.clone(), offsets.clone(), dyn_clock.clone(), bundle.probes.clone()),
        None => {
            shared.push(SourceUpdate::EngineUnavailable("資料庫不可用，引擎未啟動".into()));
            false
        }
    };

    // funding-pnl: ledger fetches and reconciliation (design D9 schedule, `funding::runner`).
    match &db {
        Some(db) if !bundle.ledgers.is_empty() => start_funding_loop(&ctx, db.clone(), bundle.ledgers.clone(), bundle.slot_ledgers.clone(), bundle.rates.clone(), secrets.clone()),
        Some(_) => eprintln!("funding ledger loop not started: signed transport unavailable"),
        None => {}
    }

    // Lot-rule requests from the pages (answered from the adapters' cached catalogs).
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<(Exchange, String)>();
        let _ = shared.rules_tx.set(tx);
        let (ctx, adapters) = (ctx.clone(), adapters.clone());
        tokio::spawn(async move {
            let mut asked: BTreeSet<(Exchange, String)> = BTreeSet::new();
            while let Some((ex, symbol)) = rx.recv().await {
                // One successful lookup per symbol and run is enough (rules change rarely).
                if symbol.is_empty() || asked.contains(&(ex, symbol.clone())) {
                    continue;
                }
                let market = PublicMarketData { adapters: adapters.clone() };
                let rules = crate::engine::ports::MarketData::order_rules(&market, ex, &symbol).await;
                if rules.is_ok() {
                    asked.insert((ex, symbol.clone()));
                }
                ctx.shared.push(SourceUpdate::Rules { exchange: ex, symbol, rules });
            }
        });
    }

    // Store + health refresher.
    {
        let (ctx, db, feed_health, bybit_health) = (ctx.clone(), db.clone(), feed.health(), bybit_health.clone());
        let secrets = secrets.clone();
        let (okx_limits, okx_latch) = (okx_limits.clone(), okx_latch.clone());
        tokio::spawn(async move {
            let mut latch_reported = false;
            let mut keys_cadence = DemoKeysCadence::default();
            loop {
                if let Some(db) = &db {
                    let settings = settings_for(ctx.identity, load_settings(db));
                    okx_limits.set_cap(okx_leg_cap(&settings));
                    *ctx.enabled.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = settings.risk.allowed_exchanges.iter().copied().collect();
                    ctx.shared.push(SourceUpdate::Settings(settings));
                    ctx.shared.push(SourceUpdate::System(read_system_flags(db, clock_now(&ctx.clock))));
                    // A failed read says so (the unfinished-pair checks must not trust the old records).
                    let listed = db.list_pairs().map_err(|e| ctx.shared.push(SourceUpdate::PairRecordsError(e.to_string())));
                    if let Ok(rows) = listed {
                        // With the engine running, its snapshot is the source of the pairs.
                        if !engine_running {
                            ctx.shared.push(SourceUpdate::Pairs(pair_infos(&rows)));
                        }
                        ctx.shared.push(SourceUpdate::PairEntries(pair_entries(&rows)));
                        ctx.shared.push(SourceUpdate::PairRecords(pair_records(&rows)));
                        // funding-pnl: read-only funding / PnL data of the same pairs.
                        ctx.shared.push(SourceUpdate::Funding(crate::ui::funding::load_pair_funding(db, &rows, clock_now(&ctx.clock))));
                        if let Some(tx) = ctx.shared.rules_tx.get() {
                            for r in rows.iter().filter(|r| r.status != "FINALIZED" && r.status != "CANCELLED") {
                                for ex in Exchange::ALL {
                                    let _ = tx.send((ex, r.symbol.clone()));
                                }
                            }
                        }
                    }
                    if let Ok(page) = db.query_events(&trade_events_query()) {
                        ctx.shared.push(SourceUpdate::TradeEvents(page.rows));
                    }
                }
                let allowed: Vec<Exchange> = ctx.enabled.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().copied().collect();
                if keys_cadence.due(&allowed) {
                    ctx.shared.push(SourceUpdate::DemoKeys(demo_keys_for(secrets.as_ref(), &allowed)));
                }
                if !latch_reported && let Some(reason) = okx_latch.reason() {
                    latch_reported = true;
                    ctx.shared.push(SourceUpdate::OkxDisabled(reason));
                }
                push_health(&ctx, &feed_health, bybit_health.as_deref());
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
        // the OKX size guard's data: catalog (cached) and the marks of this poll
        // (only when OKX answered this round: an old mark must keep its old time)
        if out.results.iter().any(|(ex, r)| *ex == Exchange::Okx && r.is_ok()) {
            okx_limits.set_marks(&marks_of(&ctx, Exchange::Okx), clock_now(&ctx.clock));
        }
        if let Ok(table) = adapters.2.limit_table().await {
            okx_limits.set_table(table);
        }
        drop(guard);
        *ctx.next_market_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = clock_now(&ctx.clock) + MARKET_POLL_MS;
        next = tokio::time::Instant::now() + Duration::from_millis(MARKET_POLL_MS as u64);
    }
}

/// Event types the trading pages read (newest first).
fn trade_events_query() -> EventQuery {
    let types = [
        crate::engine::transition::PAIR_TRANSITION,
        engine_actor::ORDER_SUBMITTED,
        engine_actor::MANUAL_ORDER_RESULT,
        engine_actor::MANUAL_CANCEL_RESULT,
        crate::engine::latency::ORDER_LATENCY,
        // naked-leg-watchdog: the manual-handling card is rebuilt from these
        crate::engine::watchdog::NAKED_LEG_WATCHDOG,
        crate::engine::live_guard::LEG_FLATTEN,
    ];
    EventQuery { types: Some(types.iter().map(|t| t.to_string()).collect()), before: None, limit: 300 }
}

/// Every stored pair, closed ones too, by internal uuid: the symbol and exchanges the history views
/// (last execution results) need after the engine's snapshot has dropped the pair.
pub fn pair_records(rows: &[PairRow]) -> BTreeMap<String, crate::ui::bridge::PairRecord> {
    rows.iter()
        .map(|r| {
            let leg = |k: &str| r.entry.get(k).cloned().and_then(|v| serde_json::from_value::<Exchange>(v).ok());
            let record = crate::ui::bridge::PairRecord {
                pair_id: r.pair_id.clone(),
                symbol: r.symbol.clone(),
                long_exchange: leg("long_exchange"),
                short_exchange: leg("short_exchange"),
                status: r.status.clone(),
            };
            (r.internal_uuid.clone(), record)
        })
        .collect()
}

/// `pairs.entry_json.scan` by internal uuid (the engine's envelope keeps the scan snapshot there).
pub fn pair_entries(rows: &[PairRow]) -> BTreeMap<String, serde_json::Value> {
    rows.iter().map(|r| (r.internal_uuid.clone(), r.entry.get("scan").cloned().unwrap_or(serde_json::Value::Null))).collect()
}

/// How often the Keychain is read for the demo-keys check (store ticks), unless the allowed exchanges change.
const DEMO_KEYS_EVERY_TICKS: u64 = 15;

/// When the demo-keys check runs: the first time, every [`DEMO_KEYS_EVERY_TICKS`] ticks, and at once when
/// the set of allowed exchanges changed (OKX added: its keys are suddenly required, and the other way).
#[derive(Default)]
struct DemoKeysCadence {
    ticks: u64,
    last_allowed: Option<Vec<Exchange>>,
}

impl DemoKeysCadence {
    fn due(&mut self, allowed: &[Exchange]) -> bool {
        let now: Vec<Exchange> = allowed.to_vec(); // already in the order of the set it comes from
        let changed = self.last_allowed.as_ref() != Some(&now);
        let slow = self.ticks % DEMO_KEYS_EVERY_TICKS == 0;
        self.ticks += 1;
        self.last_allowed = Some(now);
        changed || slow
    }
}

/// Binance AND Bybit demo keys readable, and OKX's (key, secret, passphrase) while OKX is in
/// `allowed_exchanges` (no value is kept or printed, only the reason). EXCHANGE_DEMO is refused otherwise.
fn demo_keys_for(secrets: &dyn SecretProvider, allowed: &[Exchange]) -> Result<(), String> {
    for ex in Exchange::ALL {
        if ex == Exchange::Okx && !allowed.contains(&ex) {
            continue;
        }
        if let Err(reason) = load_credentials(secrets, ex, ex == Exchange::Okx) {
            return Err(format!("{} keys unavailable ({reason:?})", ex.name()));
        }
    }
    Ok(())
}

/// Builds the real `EngineDeps` and starts the engine on this runtime. Returns whether it runs.
#[allow(clippy::too_many_arguments)]
fn start_engine<B, Y, O>(
    ctx: &Arc<Ctx>,
    db: &Db,
    adapters: Arc<(B, Y, O)>,
    account: Arc<dyn AccountView>,
    factory: Arc<dyn ExecutorFactory>,
    offsets: Arc<dyn ServerOffsets>,
    clock: Arc<dyn crate::ports::Clock>,
    probes: Arc<dyn PreflightProbes>,
) -> bool
where
    B: ExchangeAdapter + 'static,
    Y: ExchangeAdapter + 'static,
    O: ExchangeAdapter + 'static,
{
    let prices = Arc::new(SimPriceBook::default());
    let simulator = Arc::new(SimulatedExecutor::new(prices.clone()));
    // Leverage-cap reads for the pages (symbol-leverage-cap): fresh signed reads on the demo account.
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<(Exchange, String, Decimal)>();
        let _ = ctx.shared.cap_tx.set(tx);
        let (ctx, account) = (ctx.clone(), account.clone());
        tokio::spawn(async move {
            while let Some((exchange, symbol, notional)) = rx.recv().await {
                let cap = account.max_leverage(exchange, &symbol, notional).await;
                let reading = CapReading { notional, cap, fetched_at: clock_now(&ctx.clock) };
                ctx.shared.push(SourceUpdate::LeverageCap { exchange, symbol, reading });
            }
        });
    }
    // SIMULATION positions from the simulated ledger; margin from the demo account (decision 6).
    let sim_account: Arc<dyn AccountView> = Arc::new(simulator.account_view(Arc::new(MarginFromAccount(account.clone()))));
    let market: Arc<dyn crate::engine::ports::MarketData> = Arc::new(PublicMarketData { adapters });
    let reconciler: Arc<dyn StartupReconciler> = Arc::new(RecoveryReconciler::new(market.clone(), clock.clone()));
    let deps = EngineDeps {
        db: db.clone(),
        clock,
        timings: EngineTimings::default(),
        simulator,
        factory,
        market,
        offsets,
        account: account.clone(),
        sim_account: sim_account.clone(),
        reconciler: Some(reconciler),
        notifier: Arc::new(LogNotifier),
        identity: ctx.identity,
    };
    // Demo gets `NoProbes` (its engine never reads them); Live gets the real probes of its bundle.
    let handle = engine_actor::start_with_probes(deps, probes);
    let _ = ctx.feed.set((prices, handle.market.clone()));
    let _ = ctx.shared.engine.set((handle.commands.clone(), tokio::runtime::Handle::current()));
    // live-guardrails: every change of the engine's Live status reaches the pages as `SourceUpdate::LiveStatus`.
    {
        let (shared, mut rx) = (ctx.shared.clone(), handle.live.clone());
        tokio::spawn(async move {
            loop {
                shared.push(SourceUpdate::LiveStatus(super::bridge::LiveStatusView::from(&*rx.borrow_and_update())));
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
    }
    // Snapshot forwarder: every engine snapshot reaches the pages as `SourceUpdate::Engine`.
    {
        let (shared, mut rx) = (ctx.shared.clone(), handle.snapshots.clone());
        tokio::spawn(async move {
            loop {
                let (state, status) = {
                    let snapshot = rx.borrow_and_update();
                    (super::engine_view::engine_state(&snapshot), snapshot.status.clone())
                };
                shared.push(SourceUpdate::Engine(state));
                shared.push(SourceUpdate::EngineStatus(status));
                if rx.changed().await.is_err() {
                    shared.push(SourceUpdate::EngineUnavailable("引擎已停止".into()));
                    break;
                }
            }
        });
    }
    // Account reads through the engine's read-only views (manual handling, available margin).
    {
        let (ctx, account, sim_account) = (ctx.clone(), account, sim_account);
        tokio::spawn(async move {
            loop {
                for (simulated, view) in [(true, &sim_account), (false, &account)] {
                    for ex in super::bridge::tradable_exchanges(ctx.identity).iter().copied() {
                        let (positions, open_orders, available_margin) = tokio::join!(view.positions(ex), view.open_orders(ex), view.available_margin(ex));
                        let acc = LegAccount { positions, open_orders, available_margin, fetched_at: clock_now(&ctx.clock) };
                        ctx.shared.push(SourceUpdate::LegAccount { simulated, exchange: ex, account: acc });
                    }
                }
                tokio::time::sleep(Duration::from_millis(LEG_ACCOUNT_POLL_MS)).await;
            }
        });
    }
    // The handle's own command sender stays alive in `shared.engine`; dropping the rest is fine.
    drop(handle);
    true
}

/// Runs `funding::runner::tick` every `LOOP_TICK_MS` with the real signed ledger sources. Keys
/// are checked through `load_credentials` (only the reason is kept, never a value); every
/// decision inside the tick uses the `now_ms` passed in. After a tick that wrote anything the
/// positions page's funding data is re-read at once (the store poll also refreshes it).
fn start_funding_loop(
    ctx: &Arc<Ctx>,
    db: Db,
    ledgers: Vec<Arc<dyn LedgerSource>>,
    slot_ledgers: Vec<Arc<dyn LedgerSource>>,
    rates: Option<Arc<dyn RateSource>>,
    secrets: Arc<dyn SecretProvider>,
) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        // Let clock sync run first so the first signed request is not refused as "unsynced".
        tokio::time::sleep(Duration::from_secs(5)).await;
        let mut state = FundingLoop::default();
        // OKX also needs its passphrase (the other two do not have one)
        let ready = move |ex: Exchange| load_credentials(secrets.as_ref(), ex, ex == Exchange::Okx).map(|_| ()).map_err(|reason| format!("{} keys unavailable ({reason:?})", ex.name()));
        loop {
            let now = clock_now(&ctx.clock);
            let all: Vec<&dyn LedgerSource> = ledgers.iter().map(|s| s.as_ref()).collect();
            let report = funding_runner::tick(&db, &all, &ready, &TokioPause, &mut state, now).await;
            for e in &report.errors {
                eprintln!("funding ledger loop: {e}");
            }
            // funding-slot-tracing: create / check the funding slots (staged tracing, linking to pair_id).
            let slot_sources: Vec<&dyn LedgerSource> = slot_ledgers.iter().map(|s| s.as_ref()).collect();
            let slot_report = match &rates {
                Some(r) => slot_runner::slots_tick(&db, &slot_sources, r.as_ref(), &ready, &TokioPause, now).await,
                None => slot_runner::slots_tick(&db, &slot_sources, &slot_runner::NoRates, &ready, &TokioPause, now).await,
            };
            for e in &slot_report.errors {
                eprintln!("funding slot tracing: {e}");
            }
            if (report.wrote() || slot_report.wrote())
                && let Ok(rows) = db.list_pairs()
            {
                ctx.shared.push(SourceUpdate::Funding(crate::ui::funding::load_pair_funding(&db, &rows, clock_now(&ctx.clock))));
            }
            tokio::time::sleep(Duration::from_millis(funding_runner::LOOP_TICK_MS)).await;
        }
    });
}

/// Account reads for the trading pages (positions, open orders, margin).
const LEG_ACCOUNT_POLL_MS: u64 = 15_000;

/// The executor factory when no order transport could be built: EXCHANGE_DEMO stays refused.
pub(super) struct NoFactory(pub(super) String);
impl ExecutorFactory for NoFactory {
    fn create(&self, _: tong_funding_core::risk::ExecutionMode) -> Result<Arc<dyn crate::engine::ports::Executor>, String> {
        Err(self.0.clone())
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
                if let Some((book, market)) = ctx.feed.get() {
                    wiring::feed_prices(book, market, obs);
                }
                let marks: HashMap<String, Decimal> = obs.iter().map(|o| (o.symbol.clone(), o.mark_price)).collect();
                ctx.marks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(*ex, marks);
                if *ex == Exchange::Binance {
                    *ctx.binance_rest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = obs.iter().map(|o| (o.symbol.clone(), o.clone())).collect();
                }
                if *ex == Exchange::Bybit {
                    *ctx.bybit_rest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = obs.iter().map(|o| (o.symbol.clone(), o.clone())).collect();
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
        if let Some((book, market)) = ctx.feed.get() {
            wiring::feed_prices(book, market, &changed);
        }
        ctx.shared.push(SourceUpdate::MarketPartial { exchange: Exchange::Binance, observations: changed, at: latest });
    }
}

/// Whether the Bybit ticker WebSocket runs at all (`TONG_FUNDING_BYBIT_FEED=rest` turns it off).
fn start_bybit_feed(mode: BybitFeedMode) -> bool {
    mode == BybitFeedMode::Ws
}

/// The symbols to subscribe: the linear USDT perpetuals of the REST listing (dated futures are
/// not part of the scan and are not subscribed).
fn bybit_ws_symbols(rest: &HashMap<String, FundingObservation>) -> Vec<String> {
    let mut v: Vec<String> = rest.keys().filter(|s| s.ends_with("USDT") && !s.contains('-')).cloned().collect();
    v.sort();
    v
}

/// Applies the Bybit WebSocket entries on top of the Bybit poll (listing and rules from the poll).
fn overlay_bybit_prices(ctx: &Ctx, cache: &TickerCache) {
    let rest = ctx.bybit_rest.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let changed = overlay_bybit(&rest, &cache.entries());
    if changed.is_empty() {
        return;
    }
    let latest = changed.iter().map(|o| o.observed_at).max().unwrap_or(0);
    if let Some((book, market)) = ctx.feed.get() {
        wiring::feed_prices(book, market, &changed);
    }
    ctx.shared.push(SourceUpdate::MarketPartial { exchange: Exchange::Bybit, observations: changed, at: latest });
}

fn push_health(ctx: &Ctx, ws: &FeedHealth, bybit_ws: Option<&FeedHealth>) {
    let now = clock_now(&ctx.clock);
    let backoff = |ex: Exchange, class: RequestClass| match ctx.limiter.state(ex, class) {
        BackoffState::Waiting { until_ms, .. } => Some(until_ms),
        BackoffState::Clear => None,
    };
    let next_market = *ctx.next_market_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let next_account = *ctx.next_account_poll.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut list = vec![SourceHealth { source: SourceId::BinanceWs, health: Some(ws.snapshot()), next_poll_at: None, rate_limited_until: None }];
    if let Some(b) = bybit_ws {
        list.push(SourceHealth { source: SourceId::BybitWs, health: Some(b.snapshot()), next_poll_at: None, rate_limited_until: None });
    }
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

/// What a failed account poll shows. Every failure, a lost connection included, keeps the last good
/// data with the error (never zero for a failure, and a known position is never forgotten).
fn failed_account_state(error: &AdapterError, reason: Option<NotConnectedReason>, last: Option<(AccountData, i64)>, now: i64) -> AccountState {
    if matches!(error, AdapterError::NotConnected) {
        let text = not_connected_text(reason);
        // The last known data stays (it may show a position that is still open): only the error says
        // the account is not connected any more. With nothing known there is only the state.
        return match last {
            Some((data, fetched_at)) => AccountState::Loaded { data, fetched_at, error: Some((format!("{}{text}", crate::ui::bridge::NOT_CONNECTED_ERROR_PREFIX), now)) },
            None => AccountState::NotConnected { reason: text },
        };
    }
    match last {
        Some((data, fetched_at)) => AccountState::Loaded { data, fetched_at, error: Some((error.to_string(), now)) },
        None => AccountState::Failed { error: error.to_string(), at: now },
    }
}

/// A failed account poll: keeps the last good data (never shows zero for a failure).
fn account_failed(ctx: &Ctx, ex: Exchange, error: &AdapterError, reason: Option<NotConnectedReason>) {
    let now = clock_now(&ctx.clock);
    if !matches!(error, AdapterError::NotConnected) {
        ctx.account_health[&ex].on_failure(error);
    }
    let last = ctx.last_accounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&ex).cloned();
    ctx.shared.push(SourceUpdate::Account { exchange: ex, state: failed_account_state(error, reason, last, now) });
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

pub(super) async fn poll_binance_account<T: HttpTransport, E: BinanceEnv>(ctx: &Ctx, c: &BinanceSignedClient<T, E>, env: &'static str) {
    let res = async { Ok::<_, AdapterError>((c.get_balances().await?, c.get_positions().await?)) }.await;
    match res {
        Ok((balances, positions)) => {
            let data = account_data(&balances, positions, None, &marks_of(ctx, Exchange::Binance), env);
            account_loaded(ctx, Exchange::Binance, data);
        }
        Err(e) => account_failed(ctx, Exchange::Binance, &e, c.last_not_connected_reason()),
    }
}

pub(super) async fn poll_bybit_account<T: HttpTransport, E: BybitEnv>(ctx: &Ctx, c: &BybitSignedClient<T, E>, env: &'static str) {
    let res = async { Ok::<_, AdapterError>((c.get_balances().await?, c.get_positions().await?)) }.await;
    match res {
        Ok((balances, listing)) => {
            let incomplete = match listing.completeness {
                Completeness::Complete => None,
                Completeness::Incomplete { reason } => Some(reason),
            };
            let data = account_data(&balances, listing.items, incomplete, &marks_of(ctx, Exchange::Bybit), env);
            account_loaded(ctx, Exchange::Bybit, data);
        }
        Err(e) => account_failed(ctx, Exchange::Bybit, &e, c.last_not_connected_reason()),
    }
}

/// OKX account poll: balances and positions (converted to coins with the catalog's `ctVal`), shown as
/// the dashboard's OKX card. A position whose `ctVal` is unknown is named in the incomplete reason
/// instead of being dropped; the rules of the positions' symbols are requested for the pages.
pub(super) async fn poll_okx_account<T: HttpTransport>(ctx: &Ctx, c: &OkxSignedClient<T>, adapter: &OkxAdapter<PublicT>, limits: &LiveOkxLimits) {
    let res = async { Ok::<_, AdapterError>((c.get_balances().await?, c.get_positions().await?)) }.await;
    match res {
        Ok((balances, listing)) => {
            // the market loop keeps the catalog table current; ask the adapter (cached) only before its first round
            let table = match limits.table() {
                t if !t.is_empty() => t,
                _ => adapter.limit_table().await.unwrap_or_default(),
            };
            let incomplete = match &listing.completeness {
                Completeness::Complete => None,
                Completeness::Incomplete { reason } => Some(reason.clone()),
            };
            if let Some(tx) = ctx.shared.rules_tx.get() {
                for p in &listing.items {
                    let _ = tx.send((Exchange::Okx, p.symbol.clone()));
                }
            }
            let data = okx_account_data(&balances, listing.items, incomplete, &|s: &str| table.get(s).map(|(ct, _)| *ct), &marks_of(ctx, Exchange::Okx));
            account_loaded(ctx, Exchange::Okx, data);
        }
        Err(e) => account_failed(ctx, Exchange::Okx, &e, c.last_not_connected_reason()),
    }
}

/// One line for the operator when a proxy variable is set: the signed and order clients never use
/// an environment proxy (an intercepting proxy would see keys and signatures), unlike curl-style tools.
fn proxy_env_warning(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    let set: Vec<&str> = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"].into_iter().filter(|k| get(k).is_some_and(|v| !v.is_empty())).collect();
    (!set.is_empty()).then(|| format!("warning: {} set; signed and order traffic ignores proxy settings (direct connections only, by design)", set.join(", ")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_rest_switch_starts_no_bybit_websocket_and_only_usdt_perpetuals_are_subscribed() {
        assert!(start_bybit_feed(BybitFeedMode::from_env_value(None)));
        assert!(start_bybit_feed(BybitFeedMode::from_env_value(Some("ws"))));
        assert!(!start_bybit_feed(BybitFeedMode::from_env_value(Some("rest"))));
        let obs = |s: &str| FundingObservation {
            exchange: Exchange::Bybit,
            symbol: s.into(),
            funding_rate: Decimal::ZERO,
            funding_interval_secs: None,
            next_funding_time: 0,
            mark_price: Decimal::ONE,
            volume_24h_quote: None,
            exchange_timestamp: 0,
            observed_at: 0,
            data_status: tong_funding_core::funding::DataStatus::Listed,
        };
        let rest: HashMap<String, FundingObservation> = ["ETHUSDT", "BTCUSDT", "BTCUSDT-09OCT26", "BTCPERP"].into_iter().map(|s| (s.to_string(), obs(s))).collect();
        assert_eq!(bybit_ws_symbols(&rest), vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
    }

    #[test]
    fn a_live_process_never_sees_okx_as_allowed_or_tradable_and_demo_keeps_it() {
        use crate::identity::ProcessIdentity::{Demo, Live};
        let mut s = Settings::default();
        s.risk.allowed_exchanges = vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx];
        assert_eq!(settings_for(Live, s.clone()).risk.allowed_exchanges, vec![Exchange::Binance, Exchange::Bybit]);
        assert_eq!(settings_for(Demo, s).risk.allowed_exchanges.len(), 3);
        assert_eq!(super::super::bridge::tradable_exchanges(Live), &[Exchange::Binance, Exchange::Bybit]);
        assert_eq!(super::super::bridge::tradable_exchanges(Demo).len(), 3);
    }

    #[test]
    fn a_proxy_variable_gets_a_one_line_warning_and_nothing_else_does() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert_eq!(proxy_env_warning(env(&[])), None);
        assert_eq!(proxy_env_warning(env(&[("HTTPS_PROXY", "")])), None, "an empty value is not a proxy");
        let w = proxy_env_warning(env(&[("HTTPS_PROXY", "http://p:1"), ("http_proxy", "http://p:1")])).unwrap();
        assert!(w.contains("HTTPS_PROXY") && w.contains("http_proxy") && w.contains("ignores") && !w.contains("p:1") && !w.contains('\n'), "{w}");
    }

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

    // ---- okx-trading-enablement 3.4: OKX positions in coins ----

    fn okx_pos(symbol: &str, contracts: &str) -> OkxPosition {
        OkxPosition {
            symbol: symbol.into(),
            contracts: d(contracts),
            pos_side: "net".into(),
            mgn_mode: "cross".into(),
            avg_px: Some(d("60000")),
            mark_px: Some(d("60100")),
            upl: Some(d("-3")),
            lever: Some(d("5")),
            imr: Some(d("360")),
            notional_usd: Some(d("1803")),
            fetched_at: 7,
        }
    }

    #[test]
    fn an_okx_position_becomes_a_coin_position_with_contracts_times_ct_val() {
        let p = okx_to_position(&okx_pos("BTCUSDT", "-3"), Some(d("0.01"))).unwrap();
        assert_eq!((p.exchange, p.symbol.as_str(), p.side, p.quantity), (Exchange::Okx, "BTCUSDT", tong_funding_core::types::Side::Short, d("-0.03")));
        assert_eq!((p.entry_price, p.mark_price, p.leverage), (Some(d("60000")), Some(d("60100")), Some(d("5"))));
        assert_eq!((p.unrealized_pnl, p.margin, p.notional, p.fetched_at), (Some(d("-3")), Some(d("360")), Some(d("1803")), 7));
        assert_eq!(p.mode, crate::exchange::signed::models::PositionMode::OneWay);
        let long = okx_to_position(&okx_pos("ETHUSDT", "12.5"), Some(d("0.1"))).unwrap();
        assert_eq!((long.side, long.quantity), (tong_funding_core::types::Side::Long, d("1.25")));
    }

    #[test]
    fn an_okx_position_without_a_usable_ct_val_is_not_converted_and_says_so_with_its_contracts() {
        for ct in [None, Some(Decimal::ZERO), Some(d("-0.01"))] {
            let e = okx_to_position(&okx_pos("BTCUSDT", "-3"), ct).unwrap_err();
            assert!(e.contains("BTCUSDT") && e.contains("3 張") && e.contains("無法換算"), "{e}");
        }
    }

    #[test]
    fn okx_account_data_keeps_the_assets_converts_the_positions_and_never_drops_one_silently() {
        let bals = [Balance { exchange: Exchange::Okx, asset: "USDT".into(), amount: d("5000"), available: Some(d("4600")), usdt_value: Some(d("4999.8")), fetched_at: 1 }];
        let ct = |s: &str| (s == "BTCUSDT").then(|| d("0.01"));
        let data = okx_account_data(&bals, vec![okx_pos("BTCUSDT", "-3"), okx_pos("ETHUSDT", "7")], None, &ct, &HashMap::new());
        assert_eq!(data.assets.len(), 1);
        assert_eq!((data.assets[0].asset.as_str(), data.assets[0].quantity, data.assets[0].usdt_value), ("USDT", d("5000"), Some(d("4999.8"))));
        assert_eq!(data.positions.len(), 1, "only the convertible one is a position");
        assert_eq!(data.positions[0].quantity, d("-0.03"));
        let why = data.positions_incomplete.expect("the unconverted position makes the list incomplete");
        assert!(why.contains("ETHUSDT") && why.contains("7 張") && why.contains("無法換算"), "{why}");
        assert_eq!(data.environment, "DEMO");
        assert_eq!(data.contract_equity, None, "the wallet rows already include the margin: not counted twice (unverified mapping, like the other exchanges)");
        let all_ok = okx_account_data(&bals, vec![okx_pos("BTCUSDT", "-3")], None, &ct, &HashMap::new());
        assert_eq!(all_ok.positions_incomplete, None);
    }

    // ---- okx-trading-enablement 3.6: the size guard's limits ----

    fn tbl() -> HashMap<String, (Decimal, Decimal)> {
        [("BTCUSDT".to_string(), (d("0.01"), d("1")))].into()
    }

    #[test]
    fn limits_need_the_instrument_a_fresh_mark_and_a_known_cap() {
        let now = 1_000_000;
        let marks: HashMap<String, (Decimal, i64)> = [("BTCUSDT".to_string(), (d("60000"), now - 5_000))].into();
        let l = okx_limits_from(&tbl(), &marks, Some(d("2000")), "BTCUSDT", now).unwrap();
        assert_eq!((l.ct_val, l.lot_sz, l.mark_px, l.max_leg_notional), (d("0.01"), d("1"), d("60000"), d("2000")));
        assert_eq!(okx_limits_from(&tbl(), &marks, Some(d("2000")), "ETHUSDT", now), None, "unknown instrument");
        assert_eq!(okx_limits_from(&tbl(), &marks, None, "BTCUSDT", now), None, "unknown cap");
        assert_eq!(okx_limits_from(&tbl(), &HashMap::new(), Some(d("2000")), "BTCUSDT", now), None, "no mark");
        let old: HashMap<String, (Decimal, i64)> = [("BTCUSDT".to_string(), (d("60000"), now - MARK_MAX_AGE_MS - 1))].into();
        assert_eq!(okx_limits_from(&tbl(), &old, Some(d("2000")), "BTCUSDT", now), None, "a stale mark is not evidence");
    }

    #[test]
    fn the_leg_cap_is_twice_the_contract_template_notional_and_unknown_when_the_template_is_unreadable() {
        let s = Settings::default();
        assert_eq!(okx_leg_cap(&s), Some(d("2000")), "default template 1000 USDT");
        let s = Settings { contract: ContractTemplate { notional_usdt: d("300"), leverage: d("5") }, ..Settings::default() };
        assert_eq!(okx_leg_cap(&s), Some(d("600")));
        let bad = Settings { contract_error: Some("contract_template.notional_usdt must be > 0".into()), ..Settings::default() };
        assert_eq!(okx_leg_cap(&bad), None, "an unreadable template means an unknown cap: fail closed");
    }

    // ---- F5: the demo keys EXCHANGE_DEMO needs depend on the exchanges that may be traded ----

    fn keys(exchanges: &[(Exchange, bool)]) -> crate::ports::MemorySecrets {
        use crate::ports::SecretName::{ApiKey, ApiSecret, Passphrase};
        let mut m = crate::ports::MemorySecrets::default();
        for (e, with_pass) in exchanges {
            m = m.with(*e, ApiKey, "k").with(*e, ApiSecret, "s");
            if *with_pass {
                m = m.with(*e, Passphrase, "p");
            }
        }
        m
    }

    #[test]
    fn okx_keys_are_required_for_exchange_demo_only_while_okx_is_allowed() {
        let two = keys(&[(Exchange::Binance, false), (Exchange::Bybit, false)]);
        let all = [Exchange::Binance, Exchange::Bybit, Exchange::Okx];
        let e = demo_keys_for(&two, &all).unwrap_err();
        assert!(e.contains("OKX") && e.contains("NoKey"), "{e}");
        assert_eq!(demo_keys_for(&two, &[Exchange::Binance, Exchange::Bybit]), Ok(()), "OKX excluded: not needed");
        let no_pass = keys(&[(Exchange::Binance, false), (Exchange::Bybit, false), (Exchange::Okx, false)]);
        assert!(demo_keys_for(&no_pass, &all).unwrap_err().contains("NoPassphrase"));
        let full = keys(&[(Exchange::Binance, false), (Exchange::Bybit, false), (Exchange::Okx, true)]);
        assert_eq!(demo_keys_for(&full, &all), Ok(()));
        // Binance / Bybit stay required whatever is allowed
        assert!(demo_keys_for(&keys(&[(Exchange::Okx, true)]), &all).unwrap_err().contains("Binance"));
    }

    #[test]
    fn the_demo_keys_are_rechecked_at_once_when_the_allowed_exchanges_change_and_otherwise_on_a_slow_cadence() {
        let all = vec![Exchange::Binance, Exchange::Bybit, Exchange::Okx];
        let two = vec![Exchange::Binance, Exchange::Bybit];
        let mut g = DemoKeysCadence::default();
        assert!(g.due(&all), "the first check");
        for _ in 0..(DEMO_KEYS_EVERY_TICKS - 1) {
            assert!(!g.due(&all), "Keychain reads are not repeated every tick");
        }
        assert!(g.due(&all), "slow cadence");
        assert!(g.due(&two), "OKX taken out of allowed_exchanges: re-evaluated immediately");
        assert!(!g.due(&two));
        assert!(g.due(&all), "and put back");
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
    fn pair_records_keep_every_pair_including_closed_ones_by_internal_uuid() {
        let legs = json!({"long_exchange": "Bybit", "short_exchange": "Okx", "settlement_ms": 0, "simulated": false, "scan": {}});
        let r = pair_records(&[row("BLOCKED", legs.clone()), PairRow { internal_uuid: "u2".into(), pair_id: "p2".into(), symbol: "ETHUSDT".into(), ..row("FINALIZED", json!({})) }]);
        assert_eq!(r.len(), 2, "closed pairs are kept: the history views need them");
        let a = &r["u"];
        assert_eq!((a.pair_id.as_str(), a.symbol.as_str(), a.status.as_str()), ("p1", "BTCUSDT", "BLOCKED"));
        assert_eq!((a.long_exchange, a.short_exchange), (Some(Exchange::Bybit), Some(Exchange::Okx)));
        assert_eq!((r["u2"].long_exchange, r["u2"].short_exchange), (None, None), "an unreadable entry names no exchange");
    }

    // ---- ui-clarity red-team E1: a lost connection never makes a known position forgotten ----

    #[test]
    fn a_not_connected_poll_keeps_the_last_known_account_and_says_why() {
        let marks: HashMap<String, Decimal> = HashMap::new();
        let held = vec![position(Exchange::Okx, "ETHUSDT", "-0.07", "1500", "1500", "3", "0", None)];
        let data = account_data(&[bal("USDT", "100", None)], held, None, &marks, "DEMO");
        let kept = failed_account_state(&AdapterError::NotConnected, Some(NotConnectedReason::NoKey), Some((data.clone(), 5)), 9);
        assert_eq!(kept, AccountState::Loaded { data, fetched_at: 5, error: Some(("未連線：Keychain 沒有 API key".into(), 9)) });
        // nothing known yet: the plain connection state, and a real failure without data stays a failure
        assert_eq!(failed_account_state(&AdapterError::NotConnected, None, None, 9), AccountState::NotConnected { reason: "未連線".into() });
        assert_eq!(failed_account_state(&AdapterError::Timeout, None, None, 9), AccountState::Failed { error: "request timed out".into(), at: 9 });
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

    /// naked-leg-watchdog: the manual-handling card is rebuilt from the stored events, so the page must
    /// ask for the watchdog's records (and its flatten orders) next to the pair transitions.
    #[test]
    fn the_trading_pages_read_the_watchdog_events_too() {
        let types = trade_events_query().types.expect("a type filter");
        for t in ["PAIR_TRANSITION", "NAKED_LEG_WATCHDOG", "LEG_FLATTEN"] {
            assert!(types.contains(t), "{t} missing from {types:?}");
        }
    }
}

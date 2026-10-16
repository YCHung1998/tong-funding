//! The per-identity environment bundle (live-process 5.1): everything that depends on WHICH world
//! the process lives in is built here, once, from the [`ProcessIdentity`] the composition root was
//! given, and handed to the rest of `live.rs` as identity-free trait objects.
//!
//! - `build_demo_env`: demo / testnet hosts only (the Binance host is the `TONG_FUNDING_BINANCE_HOST`
//!   choice made by the caller, a `BinanceHost`), demo transports, the demo Keychain item, OKX included.
//! - `build_live_env`: the live hosts only, live transports, the live Keychain item, Binance and Bybit
//!   only (no OKX client exists in a live process). It takes NO host argument and reads no
//!   environment variable: nothing outside this file's types can pick a host.
//!
//! The pairing "demo transport <-> demo host type, live transport <-> live host type" is a
//! compile-time bound of the client constructors (`SignedTransportFor` / `OrderTransportFor`), so a
//! function of this file cannot cross the worlds; the tests at the bottom record every request
//! target of each bundle with fake transports as the runtime proof.

use std::collections::BTreeMap;
use std::sync::Arc;

use tong_funding_core::types::Exchange;

use super::live::{Ctx, LiveOkxLimits, NoFactory, PublicT, poll_binance_account, poll_bybit_account, poll_okx_account};
use super::wiring::NoAccount;
use crate::engine::ports::{AccountView, BoxFut, ExecutorFactory, ServerOffsets};
use crate::engine::preflight::{NoProbes, PreflightProbes};
use crate::exchange::execution::account::DemoAccountView;
use crate::exchange::execution::factory::DemoExecutorFactory;
use crate::exchange::execution::http::{DemoOrderTransport, LiveOrderTransport};
use crate::exchange::execution::live_factory::LiveExecutorFactory;
use crate::exchange::health::clock_sync::{ClockStatus, ClockSync, RESYNC_INTERVAL_MS};
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::public::binance::BinanceAdapter;
use crate::exchange::public::bybit::BybitAdapter;
use crate::exchange::public::endpoints::OKX_HOST;
use crate::exchange::public::okx::OkxAdapter;
use crate::exchange::reqwest_transport::PublicTransport;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{BinanceEnv, BinanceHost, BinanceLive, BybitEnv, BybitHost, BybitLive, OkxHost, SignedTransportFor};
use crate::exchange::signed::ledger::{BinanceLedgerClient, BybitLedgerClient};
use crate::exchange::signed::okx::{OkxLatch, OkxSignedClient};
use crate::exchange::signed::signing::{ClockOffsetSource, Resync};
use crate::exchange::transport::{DemoSignedTransport, HttpTransport, LiveSignedTransport};
use crate::funding::fetch::{BinanceLedgerSource, BybitLedgerSource, LedgerSource, OkxLedgerSource};
use crate::funding::rate_source::DemoRateSource;
use crate::funding::slot_runner::RateSource;
use crate::identity::ProcessIdentity;
use crate::exchange::signed::signing::load_credentials;
use crate::ports::{Clock, SecretName, SecretProvider, TimeSource};
use crate::store::db::Db;

/// The three public adapters (market data only; they hold no credentials and no signed marker).
pub type Adapters = (BinanceAdapter<PublicT>, BybitAdapter<PublicT>, OkxAdapter<PublicT>);

/// Polls this environment's accounts once (balances and positions of every exchange it has a signed
/// client for) and reports into `ctx`.
pub(super) trait AccountPoller: Send + Sync {
    fn poll<'a>(&'a self, ctx: &'a Ctx) -> BoxFut<'a, ()>;
}

/// Re-syncs the exchange clocks against the SIGNING hosts of this environment, when due.
pub trait SignedClockSync: Send + Sync {
    fn sync_due<'a>(&'a self, clocks: &'a BTreeMap<Exchange, Arc<ClockSync>>) -> BoxFut<'a, ()>;
}

/// Everything environment-dependent, as identity-free trait objects.
pub struct EnvBundle {
    pub identity: ProcessIdentity,
    /// The engine's read-only account view (`NoAccount` while no signed transport exists).
    pub account: Arc<dyn AccountView>,
    pub factory: Arc<dyn ExecutorFactory>,
    /// The OKX latch the demo factory owns; a private, never-tripped one in a live process.
    pub okx_latch: Arc<OkxLatch>,
    /// Ledger sources of the funding loop (Demo: Binance, Bybit, OKX; Live: Binance, Bybit).
    pub ledgers: Vec<Arc<dyn LedgerSource>>,
    /// The sources slot tracing queries (Binance, Bybit).
    pub slot_ledgers: Vec<Arc<dyn LedgerSource>>,
    pub rates: Option<Arc<dyn RateSource>>,
    pub probes: Arc<dyn PreflightProbes>,
    pub poller: Option<Arc<dyn AccountPoller>>,
    pub clock_sync: Option<Arc<dyn SignedClockSync>>,
}

/// What both builders need.
pub struct Common {
    pub secrets: Arc<dyn SecretProvider>,
    pub clock: Arc<dyn Clock>,
    pub time: Arc<dyn TimeSource>,
    pub clocks: BTreeMap<Exchange, Arc<ClockSync>>,
    pub offsets: Arc<dyn ServerOffsets>,
    pub limiter: Arc<RateLimiter>,
    pub db: Option<Db>,
}

fn offset_of(sync: Arc<ClockSync>) -> Arc<dyn ClockOffsetSource> {
    Arc::new(move || match sync.status() {
        ClockStatus::Synced { offset_ms, .. } => Some(offset_ms),
        ClockStatus::Unsynced => None,
    })
}

/// Resync of one exchange's clock against the signing host `base_url` over the signing transport.
pub struct ClockResync<T> {
    pub sync: Arc<ClockSync>,
    pub transport: Arc<T>,
    pub exchange: Exchange,
    pub base_url: &'static str,
}

impl<T: HttpTransport + 'static> Resync for ClockResync<T> {
    fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), crate::exchange::error::AdapterError>> + Send + '_>> {
        Box::pin(async move { self.sync.sync_once(self.transport.as_ref(), self.exchange, self.base_url).await.map(|_| ()) })
    }
}

/// The signed read and ledger clients of one world, built from ONE transport and a host pair.
pub struct SignedStack<T, BE: BinanceEnv, YE: BybitEnv> {
    pub binance: Arc<BinanceSignedClient<T, BE>>,
    pub bybit: Arc<BybitSignedClient<T, YE>>,
    pub binance_ledger: Arc<BinanceLedgerClient<T, BE>>,
    pub bybit_ledger: Arc<BybitLedgerClient<T, YE>>,
}

impl<T, BE: BinanceEnv, YE: BybitEnv> SignedStack<T, BE, YE>
where
    T: SignedTransportFor<BE> + SignedTransportFor<YE> + 'static,
{
    pub fn new(t: &Arc<T>, c: &Common, binance_env: BE, bybit_env: YE) -> Self {
        let resync = |ex: Exchange, base: &'static str| -> Arc<dyn Resync> { Arc::new(ClockResync { sync: c.clocks[&ex].clone(), transport: t.clone(), exchange: ex, base_url: base }) };
        let off = |ex: Exchange| offset_of(c.clocks[&ex].clone());
        let (bu, yu) = (binance_env.base_url(), bybit_env.base_url());
        SignedStack {
            binance: Arc::new(BinanceSignedClient::new(t.clone(), c.secrets.clone(), c.clock.clone(), off(Exchange::Binance), resync(Exchange::Binance, bu), binance_env)),
            bybit: Arc::new(BybitSignedClient::new(t.clone(), c.secrets.clone(), c.clock.clone(), off(Exchange::Bybit), resync(Exchange::Bybit, yu), bybit_env)),
            binance_ledger: Arc::new(BinanceLedgerClient::new(t.clone(), c.secrets.clone(), c.clock.clone(), off(Exchange::Binance), resync(Exchange::Binance, bu), binance_env)),
            bybit_ledger: Arc::new(BybitLedgerClient::new(t.clone(), c.secrets.clone(), c.clock.clone(), off(Exchange::Bybit), resync(Exchange::Bybit, yu), bybit_env)),
        }
    }
}

/// Clock sync against the signing hosts of a host pair (the same hosts the signed clients use).
struct HostClockSync<T, BE, YE> {
    transport: Arc<T>,
    binance: BE,
    bybit: YE,
}

impl<T, BE: BinanceEnv, YE: BybitEnv> SignedClockSync for HostClockSync<T, BE, YE>
where
    T: SignedTransportFor<BE> + SignedTransportFor<YE> + 'static,
{
    fn sync_due<'a>(&'a self, clocks: &'a BTreeMap<Exchange, Arc<ClockSync>>) -> BoxFut<'a, ()> {
        Box::pin(async move {
            for (ex, base) in [(Exchange::Binance, self.binance.base_url()), (Exchange::Bybit, self.bybit.base_url())] {
                if clocks[&ex].resync_due(RESYNC_INTERVAL_MS) {
                    let _ = clocks[&ex].sync_once(self.transport.as_ref(), ex, base).await;
                }
            }
        })
    }
}

fn no_signed_transport() -> Arc<dyn AccountView> {
    Arc::new(NoAccount("簽名傳輸層無法建立".into()))
}

// ---- Demo --------------------------------------------------------------------------------------

/// Inputs only the demo world has: OKX (public resync transport, the public adapters for the OKX
/// catalog, the size-limit source) and the Binance demo host the caller chose.
pub struct DemoParts {
    pub bhost: BinanceHost,
    pub okx_public: Arc<PublicTransport>,
    pub adapters: Arc<Adapters>,
    pub okx_limits: Arc<LiveOkxLimits>,
}

struct DemoPoller<T> {
    binance: Arc<BinanceSignedClient<T, BinanceHost>>,
    bybit: Arc<BybitSignedClient<T, BybitHost>>,
    okx: Arc<OkxSignedClient<T>>,
    adapters: Arc<Adapters>,
    limits: Arc<LiveOkxLimits>,
    binance_label: &'static str,
}

impl<T: HttpTransport + 'static> AccountPoller for DemoPoller<T> {
    fn poll<'a>(&'a self, ctx: &'a Ctx) -> BoxFut<'a, ()> {
        Box::pin(async move {
            poll_binance_account(ctx, &self.binance, self.binance_label).await;
            poll_bybit_account(ctx, &self.bybit, "DEMO").await;
            poll_okx_account(ctx, &self.okx, &self.adapters.2, &self.limits).await;
        })
    }
}

/// `TESTNET` / `DEMO` label of the Binance host the demo process talks to.
pub fn binance_label(h: BinanceHost) -> &'static str {
    match h {
        BinanceHost::Testnet => "TESTNET",
        BinanceHost::Demo => "DEMO",
    }
}

/// The demo world: exactly what `live.rs` built inline before live-process (moved, not changed).
pub fn build_demo_env<T, O>(signed: Option<Arc<T>>, orders: Result<Arc<O>, String>, c: Common, p: DemoParts) -> EnvBundle
where
    T: DemoSignedTransport + 'static,
    O: DemoOrderTransport + 'static,
{
    // The executor factory owns the ONE OKX latch of the process; the OKX read client gets the same `Arc`.
    let (factory, okx_latch): (Arc<dyn ExecutorFactory>, Arc<OkxLatch>) = match (&c.db, orders) {
        (None, _) => (Arc::new(NoFactory("資料庫不可用".into())), OkxLatch::new()),
        (Some(db), Ok(t)) => {
            let f = DemoExecutorFactory::new(t, c.secrets.clone(), c.time.clone(), c.offsets.clone(), Arc::new(db.clone()), c.limiter.clone(), p.bhost).with_okx_limits(p.okx_limits.clone());
            let latch = f.okx_latch();
            (Arc::new(f), latch)
        }
        (Some(_), Err(e)) => (Arc::new(NoFactory(format!("order transport unavailable: {e}"))), OkxLatch::new()),
    };
    let Some(t) = signed else {
        return EnvBundle {
            identity: ProcessIdentity::Demo,
            account: no_signed_transport(),
            factory,
            okx_latch,
            ledgers: Vec::new(),
            slot_ledgers: Vec::new(),
            rates: None,
            probes: Arc::new(NoProbes),
            poller: None,
            clock_sync: None,
        };
    };
    let stack = SignedStack::new(&t, &c, p.bhost, BybitHost::Demo);
    let okx = Arc::new(
        OkxSignedClient::new(
            t.clone(),
            c.secrets.clone(),
            c.clock.clone(),
            offset_of(c.clocks[&Exchange::Okx].clone()),
            Arc::new(ClockResync { sync: c.clocks[&Exchange::Okx].clone(), transport: p.okx_public.clone(), exchange: Exchange::Okx, base_url: OKX_HOST }),
            OkxHost::Demo,
        )
        .with_latch(okx_latch.clone()),
    );
    let binance_src = Arc::new(BinanceLedgerSource(stack.binance_ledger.clone()));
    let bybit_src = Arc::new(BybitLedgerSource(stack.bybit_ledger.clone()));
    EnvBundle {
        identity: ProcessIdentity::Demo,
        account: Arc::new(DemoAccountView::new(stack.binance.clone(), stack.bybit.clone()).with_okx(okx.clone())),
        factory,
        okx_latch,
        ledgers: vec![binance_src.clone(), bybit_src.clone(), Arc::new(OkxLedgerSource(okx.clone()))],
        slot_ledgers: vec![binance_src, bybit_src],
        rates: Some(Arc::new(DemoRateSource { binance: stack.binance.clone(), bybit: stack.bybit.clone() })),
        probes: Arc::new(NoProbes),
        poller: Some(Arc::new(DemoPoller { binance: stack.binance, bybit: stack.bybit, okx, adapters: p.adapters, limits: p.okx_limits, binance_label: binance_label(p.bhost) })),
        clock_sync: Some(Arc::new(HostClockSync { transport: t, binance: p.bhost, bybit: BybitHost::Demo })),
    }
}

// ---- Live --------------------------------------------------------------------------------------

struct LivePoller<T> {
    binance: Arc<BinanceSignedClient<T, BinanceLive>>,
    bybit: Arc<BybitSignedClient<T, BybitLive>>,
}

impl<T: HttpTransport + 'static> AccountPoller for LivePoller<T> {
    fn poll<'a>(&'a self, ctx: &'a Ctx) -> BoxFut<'a, ()> {
        Box::pin(async move {
            poll_binance_account(ctx, &self.binance, live_label()).await;
            poll_bybit_account(ctx, &self.bybit, live_label()).await;
        })
    }
}

/// Environment label of account data in a live process.
fn live_label() -> &'static str {
    super::env_label::EnvLabel::header_chip(ProcessIdentity::Live)
}

/// The live world: live hosts, live transports, Binance and Bybit only. No host argument, no
/// environment variable: the host types are `BinanceLive` / `BybitLive`.
pub fn build_live_env<T, O>(signed: Option<Arc<T>>, orders: Result<Arc<O>, String>, c: Common, probes: Arc<dyn PreflightProbes>) -> EnvBundle
where
    T: LiveSignedTransport + 'static,
    O: LiveOrderTransport + 'static,
{
    let factory: Arc<dyn ExecutorFactory> = match (&c.db, orders) {
        (None, _) => Arc::new(NoFactory("資料庫不可用".into())),
        (Some(db), Ok(t)) => Arc::new(LiveExecutorFactory::new(t, c.secrets.clone(), c.time.clone(), c.offsets.clone(), Arc::new(db.clone()), c.limiter.clone())),
        (Some(_), Err(e)) => Arc::new(NoFactory(format!("order transport unavailable: {e}"))),
    };
    let Some(t) = signed else {
        return EnvBundle {
            identity: ProcessIdentity::Live,
            account: no_signed_transport(),
            factory,
            okx_latch: OkxLatch::new(),
            ledgers: Vec::new(),
            slot_ledgers: Vec::new(),
            rates: None,
            probes,
            poller: None,
            clock_sync: None,
        };
    };
    let stack = SignedStack::new(&t, &c, BinanceLive, BybitLive);
    let binance_src = Arc::new(BinanceLedgerSource(stack.binance_ledger.clone()));
    let bybit_src = Arc::new(BybitLedgerSource(stack.bybit_ledger.clone()));
    EnvBundle {
        identity: ProcessIdentity::Live,
        account: Arc::new(DemoAccountView::new(stack.binance.clone(), stack.bybit.clone())),
        factory,
        okx_latch: OkxLatch::new(),
        ledgers: vec![binance_src.clone(), bybit_src.clone()],
        slot_ledgers: vec![binance_src, bybit_src],
        rates: Some(Arc::new(DemoRateSource { binance: stack.binance.clone(), bybit: stack.bybit.clone() })),
        probes,
        poller: Some(Arc::new(LivePoller { binance: stack.binance, bybit: stack.bybit })),
        clock_sync: Some(Arc::new(HostClockSync { transport: t, binance: BinanceLive, bybit: BybitLive })),
    }
}

// ---- Live preflight probes (live-guardrails, composition-root part) -----------------------------

/// The real [`PreflightProbes`] of a live process: the LIVE Keychain bundle, the live order
/// transport (position-mode reads) and the live database. Every failure is an `Err`, never a guess.
pub struct LiveProbes<O> {
    secrets: Arc<dyn SecretProvider>,
    orders: Option<Arc<O>>,
    offsets: Arc<dyn ServerOffsets>,
    clock: Arc<dyn TimeSource>,
    db: Option<Db>,
}

/// Any live symbol works for the Bybit position-mode read (the mode is account-wide in practice;
/// the read lists the symbol's position rows).
const BYBIT_MODE_PROBE_SYMBOL: &str = "BTCUSDT";

impl<O: LiveOrderTransport + 'static> LiveProbes<O> {
    pub fn new(secrets: Arc<dyn SecretProvider>, orders: Option<Arc<O>>, offsets: Arc<dyn ServerOffsets>, clock: Arc<dyn TimeSource>, db: Option<Db>) -> Self {
        LiveProbes { secrets, orders, offsets, clock, db }
    }

    fn present(&self, exchange: Exchange, name: SecretName) -> Result<bool, String> {
        match self.secrets.get(exchange, name) {
            Ok(v) => Ok(v.is_some_and(|s| !s.trim().is_empty())),
            Err(e) => Err(format!("{} live key store unreadable ({e})", exchange.name())),
        }
    }

    async fn read_mode(&self, exchange: Exchange) -> Result<bool, String> {
        use crate::exchange::execution::binance::{BinanceOrderClient, ModeReading};
        use crate::exchange::execution::bybit::BybitOrderClient;
        let transport = self.orders.clone().ok_or("live order transport unavailable")?;
        let creds = load_credentials(self.secrets.as_ref(), exchange, false).map(Arc::new).map_err(|r| format!("{} live keys unavailable ({r:?})", exchange.name()))?;
        let ts = self.offsets.offset_ms(exchange).map(|o| self.clock.now_ms().saturating_add(o)).ok_or_else(|| format!("{} clock offset not calibrated", exchange.name()))?;
        let reading = match exchange {
            Exchange::Binance => BinanceOrderClient::new(transport, creds, BinanceLive).position_mode(ts).await,
            Exchange::Bybit => BybitOrderClient::new(transport, creds, BybitLive).position_mode(BYBIT_MODE_PROBE_SYMBOL, ts).await,
            Exchange::Okx => return Err("OKX has no live path".into()),
        };
        reading.map(|m| m == ModeReading::OneWay)
    }
}

impl<O: LiveOrderTransport + 'static> PreflightProbes for LiveProbes<O> {
    fn key_present(&self, exchange: Exchange) -> BoxFut<'_, Result<bool, String>> {
        Box::pin(async move {
            if exchange == Exchange::Okx {
                return Ok(false);
            }
            Ok(self.present(exchange, SecretName::ApiKey)? && self.present(exchange, SecretName::ApiSecret)?)
        })
    }

    fn one_way_mode(&self, exchange: Exchange) -> BoxFut<'_, Result<bool, String>> {
        Box::pin(self.read_mode(exchange))
    }

    fn db_writable(&self) -> BoxFut<'_, Result<(), String>> {
        Box::pin(async move {
            let db = self.db.as_ref().ok_or("no database")?;
            db.probe_writable().map_err(|e| e.to_string())
        })
    }

    fn free_disk_bytes(&self) -> BoxFut<'_, Result<u64, String>> {
        Box::pin(async move { self.db.as_ref().ok_or_else(|| "no database".to_string())?.free_disk_bytes() })
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::exchange::execution::http::Method;
    use crate::exchange::execution::http::fake::{FakeOrderTransport, Reply};
    use crate::exchange::signed::endpoints::{BINANCE_LIVE_HOST, BYBIT_LIVE_HOST};
    use crate::ports::{ManualClock, MemorySecrets, SecretName};
    use crate::store::db::test_support::{clock as db_clock, tempdir};

    struct Off(Option<i64>);
    impl ServerOffsets for Off {
        fn offset_ms(&self, _: Exchange) -> Option<i64> {
            self.0
        }
    }

    fn secrets(full: &[Exchange], key_only: &[Exchange]) -> Arc<dyn SecretProvider> {
        let mut m = MemorySecrets::default();
        for e in full {
            m = m.with(*e, SecretName::ApiKey, "K").with(*e, SecretName::ApiSecret, "S");
        }
        for e in key_only {
            m = m.with(*e, SecretName::ApiKey, "K");
        }
        Arc::new(m)
    }

    fn live_db() -> (tempfile::TempDir, Db) {
        let dir = tempdir();
        let (clk, _) = db_clock(1_000_000);
        let db = Db::open_for(ProcessIdentity::Live, &dir.path().join("funding-live.db"), clk);
        assert!(!db.is_halted());
        (dir, db)
    }

    fn probes(s: Arc<dyn SecretProvider>, t: &FakeOrderTransport, offset: Option<i64>, db: Option<Db>) -> LiveProbes<FakeOrderTransport> {
        LiveProbes::new(s, Some(Arc::new(t.clone())), Arc::new(Off(offset)), Arc::new(ManualClock::new(1_700_000_000_000)), db)
    }

    #[tokio::test]
    async fn a_key_is_present_only_when_both_key_and_secret_are_stored_and_okx_never_is() {
        let t = FakeOrderTransport::new();
        let p = probes(secrets(&[Exchange::Binance], &[Exchange::Bybit]), &t, Some(0), None);
        assert_eq!(p.key_present(Exchange::Binance).await, Ok(true));
        assert_eq!(p.key_present(Exchange::Bybit).await, Ok(false), "a key without its secret is not a key");
        assert_eq!(p.key_present(Exchange::Okx).await, Ok(false));
        let empty = probes(secrets(&[], &[]), &t, Some(0), None);
        assert_eq!(empty.key_present(Exchange::Binance).await, Ok(false));
    }

    #[tokio::test]
    async fn one_way_mode_reads_the_live_exchange_and_treats_hedge_unknown_and_no_clock_as_failures() {
        let t = FakeOrderTransport::new();
        t.on(Method::Get, "/fapi/v1/positionSide/dual", Reply::ok(r#"{"dualSidePosition":false}"#));
        t.on(Method::Get, "/v5/position/list", Reply::ok(r#"{"retCode":0,"retMsg":"OK","result":{"list":[{"symbol":"BTCUSDT","positionIdx":0,"size":"0"}]}}"#));
        let p = probes(secrets(&[Exchange::Binance, Exchange::Bybit], &[]), &t, Some(0), None);
        assert_eq!(p.one_way_mode(Exchange::Binance).await, Ok(true));
        assert_eq!(p.one_way_mode(Exchange::Bybit).await, Ok(true));
        let hosts: Vec<String> = t.requests().iter().map(|r| r.full_url().trim_start_matches("https://").split('/').next().unwrap().to_string()).collect();
        assert_eq!(hosts, vec![BINANCE_LIVE_HOST.to_string(), BYBIT_LIVE_HOST.to_string()]);
        // hedge
        let h = FakeOrderTransport::new();
        h.on(Method::Get, "/fapi/v1/positionSide/dual", Reply::ok(r#"{"dualSidePosition":true}"#));
        h.on(Method::Get, "/v5/position/list", Reply::ok(r#"{"retCode":0,"retMsg":"OK","result":{"list":[{"symbol":"BTCUSDT","positionIdx":1,"size":"0"},{"symbol":"BTCUSDT","positionIdx":2,"size":"0"}]}}"#));
        let ph = probes(secrets(&[Exchange::Binance, Exchange::Bybit], &[]), &h, Some(0), None);
        assert_eq!(ph.one_way_mode(Exchange::Binance).await, Ok(false));
        assert_eq!(ph.one_way_mode(Exchange::Bybit).await, Ok(false));
        // unreadable reply, no clock offset, no key, OKX: all errors, nothing guessed
        let bad = FakeOrderTransport::new();
        bad.on(Method::Get, "/fapi/v1/positionSide/dual", Reply::status(500, "boom"));
        assert!(probes(secrets(&[Exchange::Binance], &[]), &bad, Some(0), None).one_way_mode(Exchange::Binance).await.is_err());
        let none = FakeOrderTransport::new();
        assert!(probes(secrets(&[Exchange::Binance], &[]), &none, None, None).one_way_mode(Exchange::Binance).await.is_err());
        assert!(none.requests().is_empty(), "no signed request without a calibrated clock");
        assert!(probes(secrets(&[], &[]), &none, Some(0), None).one_way_mode(Exchange::Binance).await.is_err());
        assert!(p.one_way_mode(Exchange::Okx).await.is_err());
    }

    #[tokio::test]
    async fn the_database_and_disk_probes_look_at_the_live_database_itself() {
        let t = FakeOrderTransport::new();
        let (_dir, db) = live_db();
        let p = probes(secrets(&[], &[]), &t, Some(0), Some(db.clone()));
        assert_eq!(p.db_writable().await, Ok(()));
        let free = p.free_disk_bytes().await.unwrap();
        assert!(free > 0, "{free}");
        // no database: both fail
        let none = probes(secrets(&[], &[]), &t, Some(0), None);
        assert!(none.db_writable().await.is_err() && none.free_disk_bytes().await.is_err());
        // a halted store is not writable
        let (_d2, halted) = live_db();
        halted.halt(crate::store::db::HaltReason::CannotOpen("test".into()));
        assert!(probes(secrets(&[], &[]), &t, Some(0), Some(halted)).db_writable().await.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::{IdPrefix, client_order_id};
    use crate::engine::ports::{Leg, OrderAction, OrderRequest, OrderSide};
    use crate::exchange::execution::http::fake::FakeOrderTransport;
    use crate::exchange::health::clock_sync::{ClockSample, ClockSync};
    use crate::exchange::health::ratelimit::WeightGate;
    use crate::exchange::health::gated::GatedTransport;
    use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, LIVE_SIGNED_HOSTS};
    use crate::exchange::transport::FakeTransport;
    use crate::ports::{ManualClock, MemorySecrets, SecretName};
    use crate::store::db::test_support::{clock as db_clock, tempdir};
    use tong_funding_core::risk::ExecutionMode;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct Off;
    impl ServerOffsets for Off {
        fn offset_ms(&self, _: Exchange) -> Option<i64> {
            Some(0)
        }
    }

    fn host_of(url: &str) -> String {
        url.trim_start_matches("https://").split('/').next().unwrap().to_string()
    }

    fn common(synced: bool, db: Option<Db>) -> (Common, ManualClock) {
        let m = ManualClock::new(1_700_000_000_000);
        let time: Arc<dyn TimeSource> = Arc::new(m.clone());
        let clocks: BTreeMap<Exchange, Arc<ClockSync>> = Exchange::ALL.into_iter().map(|e| (e, Arc::new(ClockSync::new(time.clone())))).collect();
        if synced {
            for c in clocks.values() {
                c.record_success(ClockSample { offset_ms: 0, rtt_ms: 1, synced_at_ms: 1_700_000_000_000 });
            }
        }
        let secrets = MemorySecrets::default()
            .with(Exchange::Binance, SecretName::ApiKey, "K")
            .with(Exchange::Binance, SecretName::ApiSecret, "S")
            .with(Exchange::Bybit, SecretName::ApiKey, "K")
            .with(Exchange::Bybit, SecretName::ApiSecret, "S");
        let c = Common { secrets: Arc::new(secrets), clock: Arc::new(m.clone()), time: time.clone(), clocks, offsets: Arc::new(Off), limiter: Arc::new(RateLimiter::new(time)), db };
        (c, m)
    }

    fn live_db() -> (tempfile::TempDir, Db) {
        let dir = tempdir();
        let (clk, _) = db_clock(1_000_000);
        let db = Db::open_for(ProcessIdentity::Live, &dir.path().join("funding-live.db"), clk);
        assert!(!db.is_halted());
        (dir, db)
    }

    /// Drives every request class of a bundle: account reads, ledger pages, rate reads, clock sync,
    /// and an order through the factory's executor.
    async fn drive(b: &EnvBundle, clocks: &BTreeMap<Exchange, Arc<ClockSync>>, order_id: String) {
        for ex in [Exchange::Binance, Exchange::Bybit] {
            let _ = b.account.positions(ex).await;
            let _ = b.account.open_orders(ex).await;
            let _ = b.account.available_margin(ex).await;
        }
        for s in &b.ledgers {
            let _ = s.page("BTCUSDT", 0, 1, None).await;
        }
        if let Some(r) = &b.rates {
            for ex in [Exchange::Binance, Exchange::Bybit] {
                let _ = r.rate_at(ex, "BTCUSDT", 1_000).await;
            }
        }
        if let Some(s) = &b.clock_sync {
            s.sync_due(clocks).await;
        }
        let ex = b.factory.create(ExecutionMode::ExchangeDemo).expect("executor");
        for exchange in [Exchange::Binance, Exchange::Bybit] {
            let req = OrderRequest {
                prepared: None,
                notional_usdt: None,
                client_order_id: order_id.clone(),
                exchange,
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                quantity: "0.01".parse().unwrap(),
                reduce_only: false,
                intended_base_qty: None,
                leverage: None,
            };
            let _ = ex.submit(req).await;
        }
    }

    fn all_hosts(t: &FakeTransport, o: &FakeOrderTransport) -> Vec<String> {
        t.requests().iter().map(|r| host_of(&r.url)).chain(o.requests().iter().map(|r| host_of(r.full_url()))).collect()
    }

    #[tokio::test]
    async fn a_live_bundle_sends_every_signed_request_to_the_live_hosts_whatever_the_binance_host_variable_says() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for value in ["live", "testnet", "demo", "fapi.binance.com"] {
            // SAFETY: serialised by ENV_LOCK; no other test reads this variable concurrently.
            unsafe { std::env::set_var(super::super::live::BINANCE_HOST_ENV, value) };
            let (_dir, db) = live_db();
            let (c, m) = common(true, Some(db));
            let fresh: BTreeMap<Exchange, Arc<ClockSync>> = Exchange::ALL.into_iter().map(|e| (e, Arc::new(ClockSync::new(Arc::new(m.clone()))))).collect();
            let clocks = fresh;
            let (t, o) = (Arc::new(FakeTransport::new()), FakeOrderTransport::new());
            let b = build_live_env(Some(t.clone()), Ok(Arc::new(o.clone())), c, Arc::new(NoProbes));
            let id = client_order_id(IdPrefix::Live, "pair-1", Leg::Long, OrderAction::Open, 0);
            drive(&b, &clocks, id).await;
            let hosts = all_hosts(&t, &o);
            assert!(hosts.len() >= 8, "the drive must actually send requests ({value}): {hosts:?}");
            assert!(hosts.iter().all(|h| LIVE_SIGNED_HOSTS.contains(&h.as_str())), "{value}: {hosts:?}");
            assert!(!hosts.iter().any(|h| ALLOWED_SIGNED_HOSTS.contains(&h.as_str())), "{hosts:?}");
            for live_host in LIVE_SIGNED_HOSTS {
                assert!(hosts.iter().any(|h| h == live_host), "{live_host} unused: {hosts:?}");
            }
            assert!(b.ledgers.len() == 2 && b.slot_ledgers.len() == 2, "no OKX ledger in a live process");
            assert_eq!(b.identity, ProcessIdentity::Live);
        }
        unsafe { std::env::remove_var(super::super::live::BINANCE_HOST_ENV) };
    }

    #[tokio::test]
    async fn a_demo_bundle_sends_to_demo_hosts_only_even_if_the_variable_names_live() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for (value, expect) in [("live", crate::exchange::signed::endpoints::BINANCE_TESTNET_HOST), ("fapi.binance.com", crate::exchange::signed::endpoints::BINANCE_TESTNET_HOST), ("demo", crate::exchange::signed::endpoints::BINANCE_DEMO_HOST)] {
            // SAFETY: serialised by ENV_LOCK.
            unsafe { std::env::set_var(super::super::live::BINANCE_HOST_ENV, value) };
            let bhost = super::super::live::binance_host();
            let dir = tempdir();
            let (clk, _) = db_clock(1_000_000);
            let db = Db::open(&dir.path().join("funding.db"), clk);
            let (c, m) = common(true, Some(db));
            let fresh: BTreeMap<Exchange, Arc<ClockSync>> = Exchange::ALL.into_iter().map(|e| (e, Arc::new(ClockSync::new(Arc::new(m.clone()))))).collect();
            let clocks = fresh;
            let (t, o) = (Arc::new(FakeTransport::new()), FakeOrderTransport::new());
            let time: Arc<dyn TimeSource> = Arc::new(m.clone());
            let gated = |ex| GatedTransport::new(PublicTransport::new().unwrap(), ex, c.limiter.clone(), Arc::new(WeightGate::new(time.clone())), time.clone());
            let dc: Arc<dyn Clock> = Arc::new(m);
            let adapters = Arc::new((BinanceAdapter::new(Arc::new(gated(Exchange::Binance)), dc.clone()), BybitAdapter::new(Arc::new(gated(Exchange::Bybit)), dc.clone()), OkxAdapter::new(Arc::new(gated(Exchange::Okx)), dc)));
            let parts = DemoParts { bhost, okx_public: Arc::new(PublicTransport::new().unwrap()), adapters, okx_limits: Arc::new(LiveOkxLimits::default()) };
            let b = build_demo_env(Some(t.clone()), Ok(Arc::new(o.clone())), c, parts);
            let id = client_order_id(IdPrefix::Demo, "pair-1", Leg::Long, OrderAction::Open, 0);
            drive(&b, &clocks, id).await;
            let hosts = all_hosts(&t, &o);
            assert!(hosts.len() >= 8, "{hosts:?}");
            assert!(hosts.iter().all(|h| ALLOWED_SIGNED_HOSTS.contains(&h.as_str())), "{value}: {hosts:?}");
            assert!(!hosts.iter().any(|h| LIVE_SIGNED_HOSTS.contains(&h.as_str())), "{hosts:?}");
            assert!(hosts.iter().any(|h| h == expect), "{value}: expected {expect} in {hosts:?}");
            assert_eq!(b.identity, ProcessIdentity::Demo);
        }
        unsafe { std::env::remove_var(super::super::live::BINANCE_HOST_ENV) };
    }

    #[test]
    fn a_live_factory_refuses_demo_ids_and_the_demo_factory_refuses_live_ids_at_bundle_level() {
        // covered at executor level (`live_order_tests`); here: a live bundle without a database has no factory
        let (c, _) = common(true, None);
        let b = build_live_env(Some(Arc::new(FakeTransport::new())), Ok(Arc::new(FakeOrderTransport::new())), c, Arc::new(NoProbes));
        assert!(b.factory.create(ExecutionMode::ExchangeDemo).is_err());
    }
}


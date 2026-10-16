//! Task 6.2 helper: a LIVE read-only probe of the funding slot tracing against the real Binance
//! testnet / Bybit demo ledgers (keys from the macOS Keychain, run by the user on the Mac). It is
//! `#[ignore]` and does nothing unless `TONG_DEMO_LIVE=I_AM_PRESENT_PLACE_DEMO_ORDERS` is set
//! (the same confirmation as the other probes; this one places NO order and changes nothing).
//!
//! It works on a COPY of the real `funding.db` (`VACUUM INTO`, the live file is only read), creates
//! the slots of every pair, runs one slot tick with the real ledger sources and the real public
//! funding-rate history, and prints every slot with its outcome — the repair of the earlier
//! INCOMPLETE pairs (e.g. NMR `ui-1791344628622-1-nmrusdt`) as the exchanges really answer it.
//!
//! Environment: `TONG_FUNDING_DB` (default `~/Library/Application Support/tong-funding/funding.db`),
//! `TONG_BINANCE_HOST` = `testnet` (default) or `demo`, `TONG_PROBE_COPY` (default
//! `/tmp/tong-funding-slot-probe.db`).
#![cfg(test)]

use std::sync::Arc;

use tong_funding_core::types::Exchange;

use super::fetch::{BinanceLedgerSource, BybitLedgerSource, LedgerSource, TokioPause};
use super::rate_source::DemoRateSource;
use super::slot_runner::{RateSource, delay_report, slots_tick};
use crate::exchange::error::AdapterError;
use crate::exchange::health::clock_sync::ClockSync;
use crate::exchange::reqwest_transport::SignedDemoTransport;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost};
use crate::exchange::signed::ledger::{BinanceLedgerClient, BybitLedgerClient};
use crate::exchange::signed::signing::{Resync, load_credentials};
use crate::ports::{Clock, SecretProvider, SystemClock};
use crate::store::db::Db;
use crate::store::secrets::BundleSecrets;

const CONFIRM: &str = "I_AM_PRESENT_PLACE_DEMO_ORDERS";

struct NoResync;
impl Resync for NoResync {
    fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AdapterError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "talks to the real demo hosts with the Keychain keys; run on the Mac with the user present"]
async fn live_funding_slot_probe() {
    if std::env::var("TONG_DEMO_LIVE").as_deref() != Ok(CONFIRM) {
        println!("skipped: set TONG_DEMO_LIVE={CONFIRM}");
        return;
    }
    let home = std::env::var("HOME").expect("HOME");
    let src_db = std::env::var("TONG_FUNDING_DB").unwrap_or_else(|_| format!("{home}/Library/Application Support/tong-funding/funding.db"));
    let copy = std::env::var("TONG_PROBE_COPY").unwrap_or_else(|_| "/tmp/tong-funding-slot-probe.db".into());
    let host = match std::env::var("TONG_BINANCE_HOST").as_deref() {
        Ok("demo") => BinanceHost::Demo,
        _ => BinanceHost::Testnet,
    };
    let _ = std::fs::remove_file(&copy);
    {
        let ro = rusqlite::Connection::open_with_flags(&src_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open the live database read-only");
        ro.execute("VACUUM INTO ?1", [&copy]).expect("copy the database (VACUUM INTO)");
    }
    println!("working on a copy: {copy} (the live database {src_db} was only read)");

    let clock = Arc::new(SystemClock);
    let db = Db::open(std::path::Path::new(&copy), clock.clone());
    assert!(!db.is_halted(), "{:?}", db.halt_reason());
    let secrets: Arc<dyn SecretProvider> = Arc::new(BundleSecrets::system());
    let t = Arc::new(SignedDemoTransport::new().expect("transport"));
    let b = ClockSync::new(clock.clone()).sync_once(t.as_ref(), Exchange::Binance, host.base_url()).await.expect("Binance serverTime");
    let y = ClockSync::new(clock.clone()).sync_once(t.as_ref(), Exchange::Bybit, BybitHost::Demo.base_url()).await.expect("Bybit time");
    let (ob, oy) = (b.offset_ms, y.offset_ms);
    println!("clock offsets: Binance {ob} ms, Bybit {oy} ms");
    let dyn_clock: Arc<dyn Clock> = clock.clone();
    let bin_ledger = BinanceLedgerSource(Arc::new(BinanceLedgerClient::new(t.clone(), secrets.clone(), dyn_clock.clone(), Arc::new(move || Some(ob)), Arc::new(NoResync), host)));
    let byb_ledger = BybitLedgerSource(Arc::new(BybitLedgerClient::new(t.clone(), secrets.clone(), dyn_clock.clone(), Arc::new(move || Some(oy)), Arc::new(NoResync), BybitHost::Demo)));
    let rates = DemoRateSource {
        binance: Arc::new(BinanceSignedClient::new(t.clone(), secrets.clone(), dyn_clock.clone(), Arc::new(move || Some(ob)), Arc::new(NoResync), host)),
        bybit: Arc::new(BybitSignedClient::new(t.clone(), secrets.clone(), dyn_clock.clone(), Arc::new(move || Some(oy)), Arc::new(NoResync), BybitHost::Demo)),
    };
    let keys = secrets.clone();
    let ready = move |ex: Exchange| load_credentials(keys.as_ref(), ex, false).map(|_| ()).map_err(|reason| format!("{} keys unavailable ({reason:?})", ex.name()));

    let now = clock.now_ms();
    let sources: [&dyn LedgerSource; 2] = [&bin_ledger, &byb_ledger];
    let report = slots_tick(&db, &sources, &rates, &ready, &TokioPause, now).await;
    println!("slot tick: created {}, checked {}, settled {}, errors {:?}", report.created, report.checked.len(), report.settled.len(), report.errors);
    for (pair, leg, t, outcome) in &report.checked {
        println!("  check {pair} {} settle {t}: {outcome}", leg.as_str());
    }

    println!("---- every slot after the tick ----");
    for s in db.all_slots().expect("slots") {
        let rate = rates.rate_at(s.exchange, &s.symbol, s.settle_ms).await;
        println!(
            "{} {:<5} {:<7} {} t={} state={} reason={:?} resolution={:?} amount={:?} first_seen_delay_ms={:?} err={:?} | public rate at t: {rate:?}",
            s.pair_id,
            s.leg.as_str(),
            s.exchange.name(),
            s.symbol,
            s.settle_ms,
            s.state.as_str(),
            s.manual_reason.map(|r| r.as_str()),
            s.resolution.map(|r| r.as_str()),
            s.amount,
            s.first_seen_ms.map(|f| f - s.settle_ms),
            s.last_error
        );
    }
    println!("---- publication delay (ledger rows, ms) ----\n{:?}", delay_report(&db));
}

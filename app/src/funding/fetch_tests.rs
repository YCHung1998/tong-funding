//! funding-pnl task 1.3: windows, paging to exhaustion, fail closed, 429 backoff; fetch timing.
use std::sync::Mutex;

use serde_json::json;
use tong_funding_core::pnl::FundingLedgerEntry;

use super::*;
use crate::ports::ManualClock;
use crate::store::db::test_support::open_tmp;

const DAY: i64 = 24 * 3_600_000;
const NOW: i64 = 1_791_216_000_000;

fn entry(id: &str, ts: i64) -> FundingLedgerEntry {
    FundingLedgerEntry::new(Exchange::Bybit, "BTCUSDT", "0.1".parse().unwrap(), "USDT", ts, id, "SETTLEMENT", json!({}))
}

fn page(ids: &[&str], next: Option<&str>) -> Result<LedgerPage, AdapterError> {
    Ok(LedgerPage { entries: ids.iter().map(|i| entry(i, 1)).collect(), rows: ids.len(), next_cursor: next.map(str::to_string) })
}

/// Replays scripted pages in order and records every call `(start, end, token)`.
struct FakeSource {
    exchange: Exchange,
    script: Mutex<Vec<Result<LedgerPage, AdapterError>>>,
    calls: Mutex<Vec<(i64, i64, Option<String>)>>,
}

impl FakeSource {
    fn new(exchange: Exchange, script: Vec<Result<LedgerPage, AdapterError>>) -> FakeSource {
        FakeSource { exchange, script: Mutex::new(script), calls: Mutex::default() }
    }
    fn calls(&self) -> Vec<(i64, i64, Option<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl LedgerSource for FakeSource {
    fn exchange(&self) -> Exchange {
        self.exchange
    }
    fn per_symbol(&self) -> bool {
        matches!(self.exchange, Exchange::Binance | Exchange::Okx)
    }
    fn page<'a>(&'a self, _symbol: &'a str, start_ms: i64, end_ms: i64, token: Option<&'a str>) -> BoxFut<'a, Result<LedgerPage, AdapterError>> {
        self.calls.lock().unwrap().push((start_ms, end_ms, token.map(str::to_string)));
        let mut s = self.script.lock().unwrap();
        let r = if s.is_empty() { Err(AdapterError::network("script exhausted")) } else { s.remove(0) };
        Box::pin(std::future::ready(r))
    }
}

#[derive(Default)]
struct FakePause(Mutex<Vec<u64>>);
impl Pause for FakePause {
    fn pause(&self, ms: u64) -> BoxFut<'_, ()> {
        self.0.lock().unwrap().push(ms);
        Box::pin(std::future::ready(()))
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(f)
}

#[test]
fn funding_fetch_ten_days_are_two_windows_of_at_most_seven_days() {
    let w = split_windows(0, 10 * DAY);
    assert_eq!(w.len(), 2);
    assert!(w.iter().all(|(s, e)| e - s < 7 * DAY), "{w:?}");
    assert_eq!((w[0].0, w[1].1, w[1].0), (0, 10 * DAY, w[0].1 + 1), "contiguous, no gap");
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], None), page(&["b"], None)]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", 0, 10 * DAY, 10 * DAY));
    assert_eq!(out.status, FetchStatus::Complete);
    assert_eq!(out.entries.len(), 2, "merged");
    assert_eq!(src.calls().len(), 2);
    assert_eq!(split_windows(5, 5), vec![(5, 5)]);
}

#[test]
fn funding_fetch_a_next_cursor_is_followed_until_empty() {
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], Some("c1")), page(&["b"], Some("c2")), page(&["c"], None)]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY));
    assert_eq!(out.status, FetchStatus::Complete);
    assert_eq!(out.entries.len(), 3);
    let tokens: Vec<Option<String>> = src.calls().into_iter().map(|c| c.2).collect();
    assert_eq!(tokens, [None, Some("c1".into()), Some("c2".into())]);
}

#[test]
fn funding_fetch_a_failed_second_page_is_incomplete_never_empty() {
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], Some("c1")), Err(AdapterError::Timeout)]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY));
    assert!(matches!(&out.status, FetchStatus::Incomplete(r) if r.contains("page 2")), "{:?}", out.status);
    assert_eq!(out.entries.len(), 1, "what was fetched is kept (writes are idempotent)");
}

#[test]
fn funding_fetch_repeated_cursor_and_page_cap_are_incomplete() {
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], Some("same")), page(&["b"], Some("same"))]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY));
    assert!(matches!(&out.status, FetchStatus::Incomplete(r) if r.contains("repeated")));
    let script = (0..MAX_PAGES).map(|i| page(&["x"], Some(&format!("c{i}")))).collect();
    let src = FakeSource::new(Exchange::Bybit, script);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY));
    assert!(matches!(&out.status, FetchStatus::Incomplete(r) if r.contains("page cap")), "{:?}", out.status);
}

#[test]
fn funding_fetch_429_waits_retry_after_then_retries() {
    let src = FakeSource::new(Exchange::Bybit, vec![Err(AdapterError::RateLimited { retry_after_ms: Some(3_000) }), page(&["a"], None)]);
    let pause = FakePause::default();
    let out = block_on(fetch_range(&src, &pause, "BTCUSDT", 0, DAY, DAY));
    assert_eq!(out.status, FetchStatus::Complete);
    assert_eq!(*pause.0.lock().unwrap(), vec![3_000]);
    assert_eq!(src.calls().len(), 2);
    // Without Retry-After the default backoff is used; after the retries it fails (closed).
    let script = (0..=MAX_RATE_LIMIT_RETRIES).map(|_| Err(AdapterError::RateLimited { retry_after_ms: None })).collect();
    let src = FakeSource::new(Exchange::Bybit, script);
    let pause = FakePause::default();
    let out = block_on(fetch_range(&src, &pause, "BTCUSDT", 0, DAY, DAY));
    assert!(matches!(out.status, FetchStatus::Incomplete(_)));
    assert_eq!(*pause.0.lock().unwrap(), vec![DEFAULT_BACKOFF_MS; MAX_RATE_LIMIT_RETRIES]);
}

#[test]
fn funding_fetch_binance_beyond_three_months_is_reported_not_empty() {
    let src = FakeSource::new(Exchange::Binance, vec![]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", NOW - 120 * DAY, NOW - 110 * DAY, NOW));
    assert_eq!(out.status, FetchStatus::BeyondRetention);
    assert!(src.calls().is_empty(), "nothing requested");
}

#[test]
fn funding_fetch_store_writes_entries_the_outcome_and_a_fetch_error() {
    let (_d, db, _) = open_tmp();
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], Some("c1")), Err(AdapterError::Timeout)]);
    let r = block_on(fetch_and_store(&db, &src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY)).unwrap();
    assert!(matches!(r.status, FetchStatus::Incomplete(_)));
    assert_eq!(r.write.inserted, 1);
    let types: Vec<String> = db.query_events(&Default::default()).unwrap().rows.into_iter().map(|e| e.event_type).collect();
    assert!(types.contains(&FETCH_ERROR.to_string()) && types.contains(&FUNDING_LEDGER_FETCHED.to_string()), "{types:?}");
    // Fetching the same window again writes nothing new.
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], None)]);
    let r = block_on(fetch_and_store(&db, &src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY)).unwrap();
    assert_eq!((r.status, r.write.inserted, r.write.skipped), (FetchStatus::Complete, 0, 1));
}

#[test]
fn funding_fetch_a_halted_store_fetches_nothing() {
    let (_d, db, _) = open_tmp();
    db.halt(crate::store::db::HaltReason::EventWriteFailed("x".into()));
    let src = FakeSource::new(Exchange::Bybit, vec![page(&["a"], None)]);
    assert!(block_on(fetch_and_store(&db, &src, &FakePause::default(), "BTCUSDT", 0, DAY, DAY)).is_err());
    assert!(src.calls().is_empty());
    assert!(plan_fetches(&db, NOW).unwrap().is_empty());
}

// ---- timing: which fetches are due ------------------------------------------------------------

fn seed_held_pair(db: &Db, clock: &ManualClock, uuid: &str, simulated: bool) {
    seed_held_pair_on(db, clock, uuid, simulated, Exchange::Binance, Exchange::Bybit);
}

fn seed_held_pair_on(db: &Db, clock: &ManualClock, uuid: &str, simulated: bool, long: Exchange, short: Exchange) {
    use crate::engine::actor::PairEnvelope;
    use crate::store::state::NewPair;
    use tong_funding_core::pair::PairState;
    let env = PairEnvelope { long_exchange: long, short_exchange: short, settlement_ms: NOW, simulated, scan: json!({}) };
    db.add_pair_if_not_pending(&NewPair { internal_uuid: uuid.into(), pair_id: uuid.into(), symbol: "BTCUSDT".into(), status: PairState::Prepared, entry: serde_json::to_value(env).unwrap() })
        .unwrap();
    db.set_pair_status(uuid, PairState::Reconciled).unwrap();
    let es = EventStore::new(db.clone());
    clock.set(NOW - 10_000);
    let leg = |ex: &str| json!({ "exchange": ex, "expected_price": "100", "next_funding_time": NOW, "funding_interval_secs": 28_800 });
    es.append("PAIR_TRANSITION", Some(uuid), json!({ "to": "ORDER_SUBMIT", "detail": { "entry_snapshot": { "long": leg(long.name()), "short": leg(short.name()) } } })).unwrap();
    for (leg, ex) in [("long", long.name()), ("short", short.name())] {
        es.append(
            "ORDER_SUBMITTED",
            Some(uuid),
            json!({ "client_order_id": format!("{uuid}-{leg}"), "leg": leg, "action": "open", "exchange": ex, "filled_quantity": "1", "avg_price": "100", "fee": "0", "fee_asset": "USDT" }),
        )
        .unwrap();
    }
}

#[test]
fn funding_fetch_plan_waits_for_the_delay_then_plans_both_legs() {
    let (_d, db, clock) = open_tmp();
    seed_held_pair(&db, &clock, "p1", false);
    assert!(plan_fetches(&db, NOW + FETCH_DELAY_MS - 1).unwrap().is_empty(), "not before settlement + delay");
    let plans = plan_fetches(&db, NOW + FETCH_DELAY_MS).unwrap();
    assert_eq!(plans.iter().map(|p| p.exchange).collect::<Vec<_>>(), [Exchange::Binance, Exchange::Bybit]);
    assert_eq!((plans[0].start_ms, plans[0].end_ms), (NOW - 10_000, NOW + FETCH_DELAY_MS));
}

#[test]
fn funding_fetch_plan_simulation_never_fetches() {
    let (_d, db, clock) = open_tmp();
    seed_held_pair(&db, &clock, "sim", true);
    assert!(plan_fetches(&db, NOW + FETCH_DELAY_MS).unwrap().is_empty());
}

#[test]
fn funding_fetch_plan_is_not_stopped_by_the_kill_switch() {
    let (_d, db, clock) = open_tmp();
    seed_held_pair(&db, &clock, "p1", false);
    db.set_kill_switch(true).unwrap();
    assert_eq!(plan_fetches(&db, NOW + FETCH_DELAY_MS).unwrap().len(), 2);
}

#[test]
fn funding_fetch_binance_source_pages_until_a_short_page() {
    use crate::exchange::signed::endpoints::BinanceHost;
    use crate::exchange::signed::ledger::{BINANCE_INCOME_PATH, BinanceLedgerClient};
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::{MemorySecrets, SecretName};
    struct NoResync;
    impl crate::exchange::signed::signing::Resync for NoResync {
        fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AdapterError>> + Send + '_>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }
    let row = |i: usize| json!({"symbol": "BTCUSDT", "incomeType": "FUNDING_FEE", "income": "0.1", "asset": "USDT", "time": 1, "tranId": i});
    let full: Vec<serde_json::Value> = (0..BINANCE_INCOME_LIMIT as usize).map(row).collect();
    let t = Arc::new(
        FakeTransport::new()
            .on("page=1&", Ok(HttpResponse::ok(serde_json::Value::Array(full).to_string())))
            .on("page=2&", Ok(HttpResponse::ok(json!([row(5000)]).to_string()))),
    );
    let secrets: Arc<dyn crate::ports::SecretProvider> =
        Arc::new(MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, "K_NOT_REAL").with(Exchange::Binance, SecretName::ApiSecret, "S_NOT_REAL"));
    let client = BinanceLedgerClient::new(t.clone(), secrets, Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), BinanceHost::Testnet);
    let src = BinanceLedgerSource(Arc::new(client));
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", NOW - DAY, NOW, NOW));
    assert_eq!(out.status, FetchStatus::Complete);
    assert_eq!(out.entries.len(), BINANCE_INCOME_LIMIT as usize + 1);
    assert_eq!(t.requests().len(), 2);
    assert!(t.requests().iter().all(|r| r.url.contains(BINANCE_INCOME_PATH)));
}

#[test]
fn funding_fetch_plan_includes_okx_legs_with_their_symbol() {
    let (_d, db, clock) = open_tmp();
    seed_held_pair_on(&db, &clock, "p1", false, Exchange::Bybit, Exchange::Okx);
    let plans = plan_fetches(&db, NOW + FETCH_DELAY_MS).unwrap();
    assert_eq!(plans.iter().map(|p| p.exchange).collect::<Vec<_>>(), [Exchange::Bybit, Exchange::Okx], "OKX is no longer skipped");
    let okx = plans.iter().find(|p| p.exchange == Exchange::Okx).unwrap();
    assert_eq!((okx.symbol.as_str(), okx.start_ms), ("BTCUSDT", NOW - 10_000));
}

// ---- the real OKX source: billId paging over the demo-flagged client ---------------------------------

fn okx_source(script: Vec<Result<crate::exchange::transport::HttpResponse, AdapterError>>) -> (OkxLedgerSource<crate::exchange::transport::FakeTransport>, Arc<crate::exchange::transport::FakeTransport>) {
    use crate::exchange::signed::endpoints::OkxHost;
    use crate::exchange::signed::ledger::OKX_BILLS_ARCHIVE_PATH;
    use crate::exchange::signed::okx::OkxSignedClient;
    use crate::exchange::transport::FakeTransport;
    use crate::ports::{MemorySecrets, SecretName};
    struct NoResync;
    impl crate::exchange::signed::signing::Resync for NoResync {
        fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AdapterError>> + Send + '_>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }
    let mut t = FakeTransport::new();
    for r in script {
        t = t.on(OKX_BILLS_ARCHIVE_PATH, r);
    }
    let t = Arc::new(t);
    let secrets: Arc<dyn crate::ports::SecretProvider> = Arc::new(
        MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, "K_NOT_REAL").with(Exchange::Okx, SecretName::ApiSecret, "S_NOT_REAL").with(Exchange::Okx, SecretName::Passphrase, "P_NOT_REAL"),
    );
    let signed = OkxSignedClient::new(t.clone(), secrets, Arc::new(ManualClock::new(NOW)), Arc::new(|| Some(0)), Arc::new(NoResync), OkxHost::Demo);
    (OkxLedgerSource(Arc::new(signed)), t)
}

fn okx_body(name: &str) -> Result<crate::exchange::transport::HttpResponse, AdapterError> {
    let path = format!("{}/tests/fixtures/funding/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    Ok(crate::exchange::transport::HttpResponse::ok(v["response"].to_string()))
}

#[test]
fn funding_fetch_okx_source_follows_the_last_bill_id_until_a_short_page() {
    let (src, t) = okx_source(vec![okx_body("okx_bills_funding_page1_full"), okx_body("okx_bills_funding_page2_short")]);
    assert!(src.per_symbol());
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", NOW - DAY, NOW, NOW));
    assert_eq!(out.status, FetchStatus::Complete);
    assert_eq!(out.entries.len(), 91 + 12);
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2);
    assert!(!reqs[0].url.contains("after="));
    assert!(reqs[1].url.contains("after=623950854533513120"), "the last billId of page 1: {}", reqs[1].url);
    assert!(reqs.iter().all(|r| crate::exchange::signed::endpoints::okx_headers_valid(&r.headers)), "every OKX request carries the demo flag exactly once");
}

#[test]
fn funding_fetch_okx_source_a_repeated_bill_id_cursor_is_incomplete() {
    let (src, _t) = okx_source(vec![okx_body("okx_bills_funding_page1_full"), okx_body("okx_bills_repeat_cursor")]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", NOW - DAY, NOW, NOW));
    assert!(matches!(&out.status, FetchStatus::Incomplete(r) if r.contains("cursor repeated")), "{:?}", out.status);
    assert_eq!(out.entries.len(), 91 + 100, "what was parsed before the failure is kept");
}

#[test]
fn funding_fetch_okx_source_a_failing_second_page_is_incomplete_not_empty() {
    let (src, _t) = okx_source(vec![okx_body("okx_bills_funding_page1_full"), Err(AdapterError::Timeout)]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", NOW - DAY, NOW, NOW));
    assert!(matches!(&out.status, FetchStatus::Incomplete(r) if r.contains("page 2")), "{:?}", out.status);
    assert_eq!(out.entries.len(), 91);
}

#[test]
fn funding_fetch_okx_source_a_contradicting_page_writes_nothing_from_that_page() {
    let (src, _t) = okx_source(vec![okx_body("okx_bills_sign_mismatch")]);
    let out = block_on(fetch_range(&src, &FakePause::default(), "BTCUSDT", NOW - DAY, NOW, NOW));
    assert!(matches!(out.status, FetchStatus::Incomplete(_)));
    assert!(out.entries.is_empty());
}

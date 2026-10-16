//! live-process 5.1: the signed read clients (account, ledger) work on the LIVE hosts through a
//! live host type paired with a live transport, and the pairing is enforced by the types: a demo
//! transport fits only demo hosts, a live transport only live hosts. No new endpoint: same paths,
//! other host.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::json;
use tong_funding_core::types::Exchange;

use super::binance::BinanceSignedClient;
use super::bybit::BybitSignedClient;
use super::endpoints::{ALLOWED_SIGNED_HOSTS, BINANCE_LIVE_HOST, BYBIT_LIVE_HOST, BinanceHost, BinanceLive, BybitHost, BybitLive, LIVE_SIGNED_HOSTS, SignedTransportFor};
use super::ledger::{BinanceLedgerClient, BybitLedgerClient};
use super::signing::Resync;
use crate::exchange::error::AdapterError;
use crate::exchange::reqwest_transport::{SignedDemoTransport, SignedLiveTransport};
use crate::exchange::transport::{FakeTransport, HttpResponse};
use crate::ports::{Clock, ManualClock, MemorySecrets, SecretName, SecretProvider};

const NOW: i64 = 1_700_000_000_000;

struct NoResync;
impl Resync for NoResync {
    fn resync(&self) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn secrets(e: Exchange) -> Arc<dyn SecretProvider> {
    Arc::new(MemorySecrets::default().with(e, SecretName::ApiKey, "TEST_KEY_NOT_REAL").with(e, SecretName::ApiSecret, "TEST_SECRET_NOT_REAL"))
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(ManualClock::new(NOW))
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://").split('/').next().unwrap().to_string()
}

fn ok(v: serde_json::Value) -> Result<HttpResponse, AdapterError> {
    Ok(HttpResponse::ok(v.to_string()))
}

#[test]
fn the_binance_read_client_on_the_live_host_sends_every_signed_request_to_the_live_host_only() {
    let t = Arc::new(FakeTransport::new().on("/fapi/v2/balance", ok(json!([]))).on("/fapi/v2/positionRisk", ok(json!([]))).on("/fapi/v1/openOrders", ok(json!([]))));
    let c = BinanceSignedClient::new(t.clone(), secrets(Exchange::Binance), clock(), Arc::new(|| Some(0)), Arc::new(NoResync), BinanceLive);
    block_on(c.get_balances()).unwrap();
    block_on(c.get_positions()).unwrap();
    block_on(c.get_open_orders()).unwrap();
    let reqs = t.requests();
    assert_eq!(reqs.len(), 3);
    for r in &reqs {
        assert_eq!(host_of(&r.url), BINANCE_LIVE_HOST, "{}", r.url);
        assert!(r.url.starts_with(&format!("https://{BINANCE_LIVE_HOST}/fapi/")), "{}", r.url);
    }
}

#[test]
fn the_bybit_read_client_on_the_live_host_sends_every_signed_request_to_the_live_host_only() {
    let t = Arc::new(
        FakeTransport::new()
            .on("/v5/account/wallet-balance", ok(json!({"retCode":0,"result":{"list":[]}})))
            .on("/v5/position/list", ok(json!({"retCode":0,"result":{"list":[],"nextPageCursor":""}})))
            .on("/v5/order/realtime", ok(json!({"retCode":0,"result":{"list":[],"nextPageCursor":""}}))),
    );
    let c = BybitSignedClient::new(t.clone(), secrets(Exchange::Bybit), clock(), Arc::new(|| Some(0)), Arc::new(NoResync), BybitLive);
    let _ = block_on(c.get_balances());
    let _ = block_on(c.get_positions());
    let _ = block_on(c.get_open_orders());
    let reqs = t.requests();
    assert!(reqs.len() >= 3);
    for r in &reqs {
        assert_eq!(host_of(&r.url), BYBIT_LIVE_HOST, "{}", r.url);
    }
}

#[test]
fn the_ledger_clients_on_the_live_hosts_send_to_the_live_hosts_only() {
    let tb = Arc::new(FakeTransport::new().on("/fapi/v1/income", ok(json!([]))));
    let b = BinanceLedgerClient::new(tb.clone(), secrets(Exchange::Binance), clock(), Arc::new(|| Some(0)), Arc::new(NoResync), BinanceLive);
    let _ = block_on(b.income_page("BTCUSDT", 1, 2, 1));
    let ty = Arc::new(FakeTransport::new().on("/v5/account/transaction-log", ok(json!({"retCode":0,"result":{"list":[],"nextPageCursor":""}}))));
    let y = BybitLedgerClient::new(ty.clone(), secrets(Exchange::Bybit), clock(), Arc::new(|| Some(0)), Arc::new(NoResync), BybitLive);
    let _ = block_on(y.transaction_log_page(1, 2, None));
    assert!(!tb.requests().is_empty() && !ty.requests().is_empty());
    assert!(tb.requests().iter().all(|r| host_of(&r.url) == BINANCE_LIVE_HOST));
    assert!(ty.requests().iter().all(|r| host_of(&r.url) == BYBIT_LIVE_HOST));
}

#[test]
fn live_hosts_are_not_in_the_demo_allow_list_and_the_demo_hosts_are_not_in_the_live_one() {
    for h in LIVE_SIGNED_HOSTS {
        assert!(!ALLOWED_SIGNED_HOSTS.contains(&h), "{h} must stay out of the demo allow-list");
    }
    for h in ALLOWED_SIGNED_HOSTS {
        assert!(!LIVE_SIGNED_HOSTS.contains(&h));
    }
}

/// "Does T pair with host E?" answered at compile time: the inherent method (only present when the
/// bound holds) wins over the trait fallback.
struct Probe<T>(PhantomData<T>);
trait Fallback {
    fn demo_binance(&self) -> bool {
        false
    }
    fn demo_bybit(&self) -> bool {
        false
    }
    fn live_binance(&self) -> bool {
        false
    }
    fn live_bybit(&self) -> bool {
        false
    }
}
impl<T> Fallback for Probe<T> {}
impl<T: SignedTransportFor<BinanceHost>> Probe<T> {
    fn demo_binance(&self) -> bool {
        true
    }
}
impl<T: SignedTransportFor<BybitHost>> Probe<T> {
    fn demo_bybit(&self) -> bool {
        true
    }
}
impl<T: SignedTransportFor<BinanceLive>> Probe<T> {
    fn live_binance(&self) -> bool {
        true
    }
}
impl<T: SignedTransportFor<BybitLive>> Probe<T> {
    fn live_bybit(&self) -> bool {
        true
    }
}

#[test]
fn a_demo_transport_pairs_only_with_demo_hosts_and_a_live_transport_only_with_live_hosts() {
    let demo = Probe::<SignedDemoTransport>(PhantomData);
    assert!(demo.demo_binance() && demo.demo_bybit() && !demo.live_binance() && !demo.live_bybit());
    let live = Probe::<SignedLiveTransport>(PhantomData);
    assert!(live.live_binance() && live.live_bybit() && !live.demo_binance() && !live.demo_bybit());
}

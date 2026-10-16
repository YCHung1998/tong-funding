//! live-process 5.1 + 3.x (ids): the order clients and the executor work on the LIVE hosts through a
//! live host type paired with a live order transport; a live executor takes `live`-prefixed ids
//! only and a demo executor `demo`-prefixed ids only; no OKX on the live side.
#![cfg(test)]

use std::marker::PhantomData;
use std::sync::Arc;

use tong_funding_core::types::Exchange;

use super::binance::BinanceOrderClient;
use super::bybit::BybitOrderClient;
use super::executor::DemoExecutor;
use super::executor_tests::*;
use super::http::fake::{FakeOrderTransport, Reply};
use super::http::{DemoOrderTransport, LiveOrderTransport, Method, OrderTransportFor, SignedDemoOrderTransport, SignedLiveOrderTransport};
use super::order::{ClientOrderId, OrderRef, ValidOrder};
use crate::engine::ids::{IdPrefix, client_order_id};
use crate::engine::ports::{Executor, Leg, OrderAction, OrderSide, QueryOutcome, SubmitOutcome};
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::endpoints::{BINANCE_LIVE_HOST, BYBIT_LIVE_HOST, BinanceHost, BinanceLive, BybitHost, BybitLive};
use crate::ports::ManualClock;

fn live_id(leg: Leg, action: OrderAction, seq: u16) -> String {
    client_order_id(IdPrefix::Live, "pair-ab12", leg, action, seq)
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://").split('/').next().unwrap().to_string()
}

fn live_executor(t: &FakeOrderTransport) -> (Arc<DemoExecutor<FakeOrderTransport, BinanceLive, BybitLive>>, Arc<Ledger>) {
    let clock = ManualClock::new(NOW);
    let ledger = Arc::new(Ledger::default());
    let ex = DemoExecutor::new(
        BinanceOrderClient::new(Arc::new(t.clone()), creds(), BinanceLive),
        BybitOrderClient::new(Arc::new(t.clone()), creds(), BybitLive),
        Arc::new(clock.clone()),
        Arc::new(Offsets(Some(0))),
        ledger.clone(),
        Arc::new(RateLimiter::new(Arc::new(clock))),
    );
    (Arc::new(ex), ledger)
}

#[test]
fn the_order_clients_on_the_live_hosts_build_every_request_for_the_live_hosts_only() {
    let t = FakeOrderTransport::new();
    let id = live_id(Leg::Long, OrderAction::Open, 0);
    let order = ValidOrder::from_request_for(&req(Exchange::Binance, &id, OrderSide::Buy, "0.019", false), IdPrefix::Live).unwrap();
    let b = BinanceOrderClient::new(Arc::new(t.clone()), creds(), BinanceLive).submit_request(&order, NOW).unwrap();
    assert_eq!(host_of(b.full_url()), BINANCE_LIVE_HOST, "{}", b.full_url());
    let y = BybitOrderClient::new(Arc::new(t.clone()), creds(), BybitLive).submit_request(&order, NOW).unwrap();
    assert_eq!(host_of(y.full_url()), BYBIT_LIVE_HOST, "{}", y.full_url());
}

#[tokio::test]
async fn a_live_executor_sends_live_ids_to_the_live_hosts_and_refuses_demo_ids_before_any_request() {
    let t = FakeOrderTransport::new();
    t.on(Method::Get, "/fapi/v1/positionSide/dual", Reply::ok(r#"{"dualSidePosition":false}"#));
    t.on(Method::Post, "/fapi/v1/order", Reply::ok(&binance_ack("x", "FILLED", "0.019")));
    let (ex, _) = live_executor(&t);
    let live = req(Exchange::Binance, &live_id(Leg::Long, OrderAction::Open, 0), OrderSide::Buy, "0.019", false);
    let out = ex.submit(live).await;
    assert!(matches!(out, SubmitOutcome::Accepted(_)), "{out:?}");
    let seen = t.requests();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|r| host_of(r.full_url()) == BINANCE_LIVE_HOST), "{:?}", seen.iter().map(|r| r.full_url().to_string()).collect::<Vec<_>>());

    let before = t.requests().len();
    let demo = req(Exchange::Binance, &demo_id(Leg::Long, OrderAction::Open, 1), OrderSide::Buy, "0.019", false);
    let out = ex.submit(demo).await;
    assert!(format!("{out:?}").contains("live"), "a demo id must be refused by the live executor: {out:?}");
    assert_eq!(t.requests().len(), before, "refused without a request");
}

#[tokio::test]
async fn a_demo_executor_refuses_live_ids_and_a_live_executor_has_no_okx() {
    let r = rig();
    let live = req(Exchange::Binance, &live_id(Leg::Long, OrderAction::Open, 0), OrderSide::Buy, "0.019", false);
    let out = r.ex.submit(live).await;
    assert!(matches!(out, SubmitOutcome::Rejected { .. }), "{out:?}");
    assert!(r.t.requests().is_empty(), "no request for a live id on the demo executor");

    let t = FakeOrderTransport::new();
    let (ex, _) = live_executor(&t);
    let okx = okx_req(&live_id(Leg::Long, OrderAction::Open, 0), OrderSide::Buy, "1", false);
    let out = ex.submit(okx).await;
    assert!(matches!(out, SubmitOutcome::Rejected { .. }), "{out:?}");
    assert!(t.requests().is_empty(), "the live side never talks to OKX");
}

#[tokio::test]
async fn cancel_and_query_on_a_live_executor_take_live_ids_only() {
    let t = FakeOrderTransport::new();
    let (ex, ledger) = live_executor(&t);
    let demo = demo_id(Leg::Long, OrderAction::Open, 0);
    ledger.0.lock().unwrap().push(demo.clone());
    assert!(matches!(ex.cancel(Exchange::Binance, "BTCUSDT", &demo).await, QueryOutcome::Failed { .. }));
    assert!(matches!(ex.query(Exchange::Binance, "BTCUSDT", &demo).await, QueryOutcome::Failed { .. }));
    assert!(t.requests().is_empty());
}

#[test]
fn client_order_id_is_checked_against_the_environment_prefix() {
    let (d, l, s) = (demo_id(Leg::Long, OrderAction::Open, 0), live_id(Leg::Long, OrderAction::Open, 0), client_order_id(IdPrefix::Sim, "p", Leg::Long, OrderAction::Open, 0));
    assert!(ClientOrderId::parse_for(&d, IdPrefix::Demo).is_ok() && ClientOrderId::parse_for(&l, IdPrefix::Live).is_ok());
    assert!(ClientOrderId::parse_for(&d, IdPrefix::Live).is_err() && ClientOrderId::parse_for(&l, IdPrefix::Demo).is_err());
    assert!(ClientOrderId::parse_for(&s, IdPrefix::Live).is_err() && ClientOrderId::parse_for(&s, IdPrefix::Demo).is_err());
    assert!(ClientOrderId::parse(&l).is_err(), "`parse` stays demo-only");
    assert_eq!(ClientOrderId::parse_for(&l, IdPrefix::Live).unwrap().as_str(), l);
    let _ = OrderRef::Client(ClientOrderId::parse_for(&l, IdPrefix::Live).unwrap());
    assert!(ClientOrderId::parse_for(&l, IdPrefix::Sim).is_err(), "a simulated prefix is never an exchange id");
}

/// "Does T pair with host E?" at compile time (inherent method present only when the bound holds).
struct Probe<T>(PhantomData<T>);
trait Fallback {
    fn demo_binance(&self) -> bool {
        false
    }
    fn live_binance(&self) -> bool {
        false
    }
    fn demo_bybit(&self) -> bool {
        false
    }
    fn live_bybit(&self) -> bool {
        false
    }
}
impl<T> Fallback for Probe<T> {}
impl<T: OrderTransportFor<BinanceHost>> Probe<T> {
    fn demo_binance(&self) -> bool {
        true
    }
}
impl<T: OrderTransportFor<BinanceLive>> Probe<T> {
    fn live_binance(&self) -> bool {
        true
    }
}
impl<T: OrderTransportFor<BybitHost>> Probe<T> {
    fn demo_bybit(&self) -> bool {
        true
    }
}
impl<T: OrderTransportFor<BybitLive>> Probe<T> {
    fn live_bybit(&self) -> bool {
        true
    }
}

#[test]
fn a_demo_order_transport_pairs_only_with_demo_hosts_and_a_live_one_only_with_live_hosts() {
    let demo = Probe::<SignedDemoOrderTransport>(PhantomData);
    assert!(demo.demo_binance() && demo.demo_bybit() && !demo.live_binance() && !demo.live_bybit());
    let live = Probe::<SignedLiveOrderTransport>(PhantomData);
    assert!(live.live_binance() && live.live_bybit() && !live.demo_binance() && !live.demo_bybit());
    fn _bounds<T: DemoOrderTransport, U: LiveOrderTransport>() {}
}

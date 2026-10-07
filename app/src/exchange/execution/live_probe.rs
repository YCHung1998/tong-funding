//! Task 4.2 helper: a LIVE probe against the Binance and Bybit demo/testnet accounts, run by the
//! user on the Mac (keys from the macOS Keychain). It is `#[ignore]` and does nothing unless
//! `TONG_DEMO_LIVE=I_AM_PRESENT_PLACE_DEMO_ORDERS` is set, so `cargo test` never places an order.
//! It was compiled but NEVER run from the development container (no network there).
//!
//! Each round: open a long Binance + short Bybit market order IN PARALLEL through the real
//! `DemoExecutor` (intent first, same path as the engine), wait for the fills, close both
//! reduce-only with the filled quantities, then read the positions back. Every submit writes an
//! `ORDER_LATENCY` event into the probe database; the summary prints the p50/p95/p99 that the
//! T−5 decision needs (submit part only: the engine's "entry trigger → both accepted" also
//! contains the pre-trade re-fetch, measured separately in engine-simulation design D9).
//! Finally one reduce-only order without a position shows how the exchange rejects it.
//!
//! Environment (read here only; the production code never reads the environment):
//! - `TONG_DEMO_SYMBOL` (required, e.g. `ETHUSDT`) and `TONG_DEMO_QTY` (required, the smallest
//!   lot both exchanges accept for that symbol, e.g. `0.01`),
//! - `TONG_DEMO_ROUNDS` (default 1, at most 20),
//! - `TONG_BINANCE_HOST` = `testnet` (default) or `demo` (design Open Question 1),
//! - `TONG_DEMO_DB` (default `/tmp/tong-demo-probe.db`).
#![cfg(test)]

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tong_funding_core::types::{Decimal, Exchange};

use super::account::DemoAccountView;
use super::factory::DemoExecutorFactory;
use super::http::ReqwestOrderTransport;
use crate::engine::ids::{IdPrefix, client_order_id};
use crate::engine::intent;
use crate::engine::latency::{LatencyReport, ORDER_LATENCY, latency_payload};
use crate::engine::ports::{AccountView, Executor, Leg, OrderAction, OrderRequest, OrderSide, OrderState, QueryOutcome, ServerOffsets, SubmitOutcome};
use crate::exchange::error::AdapterError;
use crate::exchange::health::clock_sync::ClockSync;
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::reqwest_transport::ReqwestTransport;
use crate::exchange::signed::binance::BinanceSignedClient;
use crate::exchange::signed::bybit::BybitSignedClient;
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost};
use crate::exchange::signed::signing::Resync;
use crate::ports::{Clock, SecretProvider, SystemClock};
use crate::store::db::Db;
use crate::store::events::EventStore;
use crate::store::secrets::BundleSecrets;

const CONFIRM: &str = "I_AM_PRESENT_PLACE_DEMO_ORDERS";

struct Offsets {
    binance: i64,
    bybit: i64,
}
impl ServerOffsets for Offsets {
    fn offset_ms(&self, exchange: Exchange) -> Option<i64> {
        match exchange {
            Exchange::Binance => Some(self.binance),
            Exchange::Bybit => Some(self.bybit),
            Exchange::Okx => None,
        }
    }
}

struct NoResync;
impl Resync for NoResync {
    fn resync(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AdapterError>> + Send + '_>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// Polls until the order is no longer open (or 10 s pass); returns the last status seen.
async fn wait_final(ex: &dyn Executor, exchange: Exchange, symbol: &str, id: &str) -> QueryOutcome {
    let mut last = QueryOutcome::NotFound;
    for _ in 0..20 {
        last = ex.query(exchange, symbol, id).await;
        if let QueryOutcome::Found(s) = &last
            && s.state != OrderState::Open
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    last
}

#[allow(clippy::too_many_arguments)]
async fn submit_logged(
    db: &Db,
    events: &EventStore,
    ex: &dyn Executor,
    clock: &dyn Clock,
    pair: &str,
    leg: Leg,
    action: OrderAction,
    req: OrderRequest,
    triggered_at: i64,
) -> SubmitOutcome {
    let (exchange, id) = (req.exchange, req.client_order_id.clone());
    match intent::submit_with_intent_clocked(db, ex, Some(clock), pair, leg, req).await {
        Ok(report) => {
            let t = report.timing.expect("clocked");
            let class = match &report.outcome {
                SubmitOutcome::Accepted(_) => "accepted",
                SubmitOutcome::Rejected { .. } => "rejected",
                SubmitOutcome::Unknown { .. } => "unknown",
            };
            let action_str = if action == OrderAction::Open { "open" } else { "close" };
            let payload = latency_payload(pair, leg.as_str(), action_str, exchange.name(), &id, t.request_sent_at_ms, t.ack_at_ms, class, Some(triggered_at), false);
            let _ = events.append(ORDER_LATENCY, Some(pair), payload);
            println!("  {action_str:<5} {:<5} {:<7} {id}  {class:<8} {} ms  {:?}", leg.as_str(), exchange.name(), t.latency_ms(), report.outcome);
            report.outcome
        }
        Err(e) => {
            println!("  intent error (nothing or unknown sent): {e}");
            SubmitOutcome::Unknown { reason: e.to_string() }
        }
    }
}

fn filled(o: &QueryOutcome) -> Decimal {
    match o {
        QueryOutcome::Found(s) => s.filled_quantity,
        QueryOutcome::NotFound | QueryOutcome::Failed { .. } => Decimal::ZERO,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "places real demo/testnet orders; run on the Mac with the user present (TODO.md)"]
async fn live_demo_probe() {
    if std::env::var("TONG_DEMO_LIVE").as_deref() != Ok(CONFIRM) {
        println!("skipped: set TONG_DEMO_LIVE={CONFIRM} to place demo orders");
        return;
    }
    let symbol = std::env::var("TONG_DEMO_SYMBOL").expect("TONG_DEMO_SYMBOL is required (e.g. ETHUSDT)");
    let qty: Decimal = std::env::var("TONG_DEMO_QTY").expect("TONG_DEMO_QTY is required").parse().expect("TONG_DEMO_QTY must be a decimal");
    let rounds: usize = std::env::var("TONG_DEMO_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 20);
    let binance_host = match std::env::var("TONG_BINANCE_HOST").as_deref() {
        Ok("demo") => BinanceHost::Demo,
        _ => BinanceHost::Testnet,
    };
    let db_path = std::env::var("TONG_DEMO_DB").unwrap_or_else(|_| "/tmp/tong-demo-probe.db".into());

    let clock = Arc::new(SystemClock);
    let db = Db::open(std::path::Path::new(&db_path), clock.clone());
    assert!(!db.is_halted(), "probe db: {:?}", db.halt_reason());
    let events = EventStore::new(db.clone());
    let secrets: Arc<dyn SecretProvider> = Arc::new(BundleSecrets::system());

    // Clock offsets from the demo hosts' public time endpoints.
    let reads = Arc::new(ReqwestTransport::signed_demo().expect("transport"));
    let sync_b = ClockSync::new(clock.clone());
    let sync_y = ClockSync::new(clock.clone());
    let b = sync_b.sync_once(reads.as_ref(), Exchange::Binance, binance_host.base_url()).await.expect("Binance serverTime");
    let y = sync_y.sync_once(reads.as_ref(), Exchange::Bybit, BybitHost::Demo.base_url()).await.expect("Bybit time");
    println!("host: Binance {binance_host:?}, Bybit Demo; offsets: Binance {} ms (rtt {}), Bybit {} ms (rtt {})", b.offset_ms, b.rtt_ms, y.offset_ms, y.rtt_ms);
    let offsets = Arc::new(Offsets { binance: b.offset_ms, bybit: y.offset_ms });

    let factory = DemoExecutorFactory::new(
        Arc::new(ReqwestOrderTransport::signed_demo().expect("order transport")),
        secrets.clone(),
        clock.clone(),
        offsets.clone(),
        Arc::new(db.clone()),
        Arc::new(RateLimiter::new(clock.clone())),
        binance_host,
    );
    let ex = factory.build().expect("demo executor (keys in the Keychain?)");
    let (ob, oy) = (b.offset_ms, y.offset_ms);
    let account = DemoAccountView::new(
        Arc::new(BinanceSignedClient::new(reads.clone(), secrets.clone(), clock.clone(), Arc::new(move || Some(ob)), Arc::new(NoResync), binance_host)),
        Arc::new(BybitSignedClient::new(reads, secrets, clock.clone(), Arc::new(move || Some(oy)), Arc::new(NoResync), BybitHost::Demo)),
    );

    for round in 0..rounds {
        let pair = format!("probe-{}-{round}", clock.now_ms());
        println!("round {} / {rounds}: {pair} {symbol} qty {qty}", round + 1);
        let req = |leg: Leg, action: OrderAction, exchange: Exchange, side: OrderSide, q: Decimal| OrderRequest {
            client_order_id: client_order_id(IdPrefix::Demo, &pair, leg, action, 0),
            exchange,
            symbol: symbol.clone(),
            side,
            quantity: q,
            reduce_only: action == OrderAction::Close,
            leverage: None,
        };
        let trigger = clock.now_ms();
        let long = req(Leg::Long, OrderAction::Open, Exchange::Binance, OrderSide::Buy, qty);
        let short = req(Leg::Short, OrderAction::Open, Exchange::Bybit, OrderSide::Sell, qty);
        let (lid, sid) = (long.client_order_id.clone(), short.client_order_id.clone());
        let _ = tokio::join!(
            submit_logged(&db, &events, &ex, clock.as_ref(), &pair, Leg::Long, OrderAction::Open, long, trigger),
            submit_logged(&db, &events, &ex, clock.as_ref(), &pair, Leg::Short, OrderAction::Open, short, trigger),
        );
        let (lf, sf) = tokio::join!(wait_final(&ex, Exchange::Binance, &symbol, &lid), wait_final(&ex, Exchange::Bybit, &symbol, &sid));
        println!("  fills: long {lf:?}\n         short {sf:?}");
        let _ = events.append("PROBE_FILLS", Some(&pair), json!({ "long": format!("{lf:?}"), "short": format!("{sf:?}") }));

        let close_trigger = clock.now_ms();
        let (lq, sq) = (filled(&lf), filled(&sf));
        let lc = req(Leg::Long, OrderAction::Close, Exchange::Binance, OrderSide::Sell, lq);
        let sc = req(Leg::Short, OrderAction::Close, Exchange::Bybit, OrderSide::Buy, sq);
        let (lcid, scid) = (lc.client_order_id.clone(), sc.client_order_id.clone());
        let ((), ()) = tokio::join!(
            async {
                if lq > Decimal::ZERO {
                    submit_logged(&db, &events, &ex, clock.as_ref(), &pair, Leg::Long, OrderAction::Close, lc, close_trigger).await;
                }
            },
            async {
                if sq > Decimal::ZERO {
                    submit_logged(&db, &events, &ex, clock.as_ref(), &pair, Leg::Short, OrderAction::Close, sc, close_trigger).await;
                }
            },
        );
        let _ = tokio::join!(wait_final(&ex, Exchange::Binance, &symbol, &lcid), wait_final(&ex, Exchange::Bybit, &symbol, &scid));
        for exchange in [Exchange::Binance, Exchange::Bybit] {
            let pos = account.positions(exchange).await;
            let orders = account.open_orders(exchange).await;
            println!("  after close {}: positions {pos:?}; open orders {orders:?}", exchange.name());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Reduce-only with no position: how does each exchange refuse it?
    let pair = format!("probe-ro-{}", clock.now_ms());
    for (leg, exchange, side) in [(Leg::Long, Exchange::Binance, OrderSide::Sell), (Leg::Short, Exchange::Bybit, OrderSide::Buy)] {
        let r = OrderRequest {
            client_order_id: client_order_id(IdPrefix::Demo, &pair, leg, OrderAction::Close, 0),
            exchange,
            symbol: symbol.clone(),
            side,
            quantity: qty,
            reduce_only: true,
            leverage: None,
        };
        let out = submit_logged(&db, &events, &ex, clock.as_ref(), &pair, leg, OrderAction::Close, r, clock.now_ms()).await;
        println!("reduce-only without a position on {}: {out:?}", exchange.name());
    }

    let rows: Vec<serde_json::Value> = events
        .list(10_000)
        .unwrap()
        .into_iter()
        .filter(|e| e.event_type == ORDER_LATENCY)
        .map(|e| e.payload)
        .collect();
    let report = LatencyReport::from_events(&rows, false);
    println!("==== latency summary ({db_path}) ====");
    println!("submit (request -> ACK, accepted only): {:?}", report.submit);
    println!("open trigger -> both legs accepted:      {:?}", report.entry_to_both_accepted);
    println!("excluded entries: {}", report.entries_excluded);
    println!("T-5 criterion (p99 < 2500 ms, submit part only): {:?}", report.t5_criterion_met());
}

/// symbol-leverage-cap / order-leverage-sync: the real demo hosts answer the leverage-cap reads and
/// the leverage-set request, and the opened positions really carry the requested leverage.
///
/// Opens ONE long Binance + ONE short Bybit market order with `leverage = L` through the real
/// `DemoExecutor` (the same path as the engine), reads both positions back (`leverage` field), closes
/// both reduce-only, and finally asks for `cap + 1` on Binance to see the refusal. Uses a tiny quantity.
/// Environment: `TONG_DEMO_SYMBOL`, `TONG_DEMO_QTY` (both required), `TONG_DEMO_LEVERAGE` (default 5),
/// `TONG_DEMO_NOTIONAL` (default 1000, only for the Binance bracket lookup), `TONG_BINANCE_HOST`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "places real demo/testnet orders; run on the Mac with the user present"]
async fn live_leverage_probe() {
    if std::env::var("TONG_DEMO_LIVE").as_deref() != Ok(CONFIRM) {
        println!("skipped: set TONG_DEMO_LIVE={CONFIRM}");
        return;
    }
    let symbol = std::env::var("TONG_DEMO_SYMBOL").expect("TONG_DEMO_SYMBOL is required");
    let qty: Decimal = std::env::var("TONG_DEMO_QTY").expect("TONG_DEMO_QTY is required").parse().expect("decimal");
    let want: Decimal = std::env::var("TONG_DEMO_LEVERAGE").ok().and_then(|v| v.parse().ok()).unwrap_or(Decimal::from(5));
    let notional: Decimal = std::env::var("TONG_DEMO_NOTIONAL").ok().and_then(|v| v.parse().ok()).unwrap_or(Decimal::from(1000));
    let binance_host = match std::env::var("TONG_BINANCE_HOST").as_deref() {
        Ok("demo") => BinanceHost::Demo,
        _ => BinanceHost::Testnet,
    };
    let clock = Arc::new(SystemClock);
    let db = Db::open(std::path::Path::new("/tmp/tong-leverage-probe.db"), clock.clone());
    let secrets: Arc<dyn SecretProvider> = Arc::new(BundleSecrets::system());
    let reads = Arc::new(ReqwestTransport::signed_demo().expect("transport"));
    let b = ClockSync::new(clock.clone()).sync_once(reads.as_ref(), Exchange::Binance, binance_host.base_url()).await.expect("Binance serverTime");
    let y = ClockSync::new(clock.clone()).sync_once(reads.as_ref(), Exchange::Bybit, BybitHost::Demo.base_url()).await.expect("Bybit time");
    let offsets = Arc::new(Offsets { binance: b.offset_ms, bybit: y.offset_ms });
    let factory = DemoExecutorFactory::new(
        Arc::new(ReqwestOrderTransport::signed_demo().expect("order transport")),
        secrets.clone(),
        clock.clone(),
        offsets,
        Arc::new(db.clone()),
        Arc::new(RateLimiter::new(clock.clone())),
        binance_host,
    );
    let ex = factory.build().expect("demo executor (keys in the Keychain?)");
    let (ob, oy) = (b.offset_ms, y.offset_ms);
    let bin = Arc::new(BinanceSignedClient::new(reads.clone(), secrets.clone(), clock.clone(), Arc::new(move || Some(ob)), Arc::new(NoResync), binance_host));
    let byb = Arc::new(BybitSignedClient::new(reads, secrets, clock.clone(), Arc::new(move || Some(oy)), Arc::new(NoResync), BybitHost::Demo));
    let account = DemoAccountView::new(bin.clone(), byb.clone());

    // 1. caps
    let caps = [account.max_leverage(Exchange::Binance, &symbol, notional).await, account.max_leverage(Exchange::Bybit, &symbol, notional).await];
    println!("leverage caps for {symbol} at notional {notional}: Binance {:?}, Bybit {:?}", caps[0], caps[1]);
    for (ex, c) in [Exchange::Binance, Exchange::Bybit].into_iter().zip(&caps) {
        let cap = c.as_ref().unwrap_or_else(|e| panic!("{} cap read failed: {e}", ex.name()));
        assert!(*cap >= Decimal::ONE, "{} cap {cap}", ex.name());
    }

    // 2. open both legs at `want`, read the leverage back
    let pair = format!("levprobe-{}", clock.now_ms());
    let mk = |leg: Leg, action: OrderAction, exchange: Exchange, side: OrderSide, leverage: Option<Decimal>, seq: u16| OrderRequest {
        client_order_id: client_order_id(IdPrefix::Demo, &pair, leg, action, seq),
        exchange,
        symbol: symbol.clone(),
        side,
        quantity: qty,
        reduce_only: action == OrderAction::Close,
        leverage,
    };
    let open_l = mk(Leg::Long, OrderAction::Open, Exchange::Binance, OrderSide::Buy, Some(want), 0);
    let open_s = mk(Leg::Short, OrderAction::Open, Exchange::Bybit, OrderSide::Sell, Some(want), 0);
    let (rl, rs) = tokio::join!(ex.submit(open_l.clone()), ex.submit(open_s.clone()));
    println!("open long Binance: {rl:?}\nopen short Bybit:  {rs:?}");
    let (lid, sid) = (open_l.client_order_id.clone(), open_s.client_order_id.clone());
    let _ = tokio::join!(wait_final(&ex, Exchange::Binance, &symbol, &lid), wait_final(&ex, Exchange::Bybit, &symbol, &sid));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bl = bin.get_positions().await.expect("Binance positions").into_iter().find(|p| p.symbol == symbol);
    let sl = byb.get_positions().await.expect("Bybit positions").items.into_iter().find(|p| p.symbol == symbol);
    println!("Binance position: {bl:?}\nBybit position:   {sl:?}");
    let leverage_of = |p: &Option<crate::exchange::signed::models::Position>| p.as_ref().and_then(|p| p.leverage);
    let (lb, ly) = (leverage_of(&bl), leverage_of(&sl));

    // 3. close both (reduce-only), whatever the assertions will say
    let close_l = mk(Leg::Long, OrderAction::Close, Exchange::Binance, OrderSide::Sell, None, 0);
    let close_s = mk(Leg::Short, OrderAction::Close, Exchange::Bybit, OrderSide::Buy, None, 0);
    let (cl, cs) = tokio::join!(ex.submit(close_l.clone()), ex.submit(close_s.clone()));
    println!("close long Binance: {cl:?}\nclose short Bybit:  {cs:?}");
    let _ = tokio::join!(wait_final(&ex, Exchange::Binance, &symbol, &close_l.client_order_id), wait_final(&ex, Exchange::Bybit, &symbol, &close_s.client_order_id));
    for exchange in [Exchange::Binance, Exchange::Bybit] {
        println!("after close {}: positions {:?}", exchange.name(), account.positions(exchange).await);
    }
    assert_eq!((lb, ly), (Some(want), Some(want)), "both positions must carry the requested leverage {want}");

    // 4. a leverage above Binance's cap must be refused by the exchange and send no order
    if let Ok(cap) = &caps[0] {
        let over = *cap + Decimal::ONE;
        if over <= Decimal::from(125) {
            let r = ex.submit(mk(Leg::Long, OrderAction::Open, Exchange::Binance, OrderSide::Buy, Some(over), 1)).await;
            println!("open with leverage {over} (cap {cap}): {r:?}");
            if !matches!(r, SubmitOutcome::Rejected { .. }) {
                // The exchange allowed more than the cap we read: never leave that position open.
                let c = ex.submit(mk(Leg::Long, OrderAction::Close, Exchange::Binance, OrderSide::Sell, None, 1)).await;
                println!("closed the unexpected position: {c:?}");
            }
            assert!(matches!(r, SubmitOutcome::Rejected { .. }), "{r:?}");
            let pos = account.positions(Exchange::Binance).await.expect("positions");
            assert!(!pos.items.iter().any(|p| p.symbol == symbol), "no position may exist after the refused order: {pos:?}");
        } else {
            println!("Binance cap {cap}: no higher leverage to try");
        }
    }
}

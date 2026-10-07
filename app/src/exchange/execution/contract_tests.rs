//! The `Executor` contract suite (task 1.4; spec "與 SimulatedExecutor 遵守同一個介面契約"): the
//! same scenarios run once against `SimulatedExecutor` and once against `DemoExecutor` over
//! recorded Binance replies. Each harness only translates "what the exchange does" into its own
//! script; the assertions are shared.
#![cfg(test)]

use std::sync::Arc;

use tong_funding_core::types::{Decimal, Exchange};

use super::endpoints::{BINANCE_ORDER_PATH, BINANCE_USER_TRADES_PATH};
use super::executor_tests::{Rig, d, okx_order_fx, rig, script_okx_mode};
use super::http::Method;
use super::http::fake::Reply;
use crate::engine::ids::{IdPrefix, client_order_id};
use crate::engine::ports::{Executor, Leg, OrderAction, OrderRequest, OrderSide, OrderState, QueryOutcome, SubmitOutcome};
use crate::engine::sim::{SimBehavior, SimPriceBook, SimulatedExecutor};
use crate::exchange::error::AdapterError;

const QTY: &str = "0.010";
const PARTIAL: &str = "0.007";

/// What the (simulated or recorded) exchange does with one order.
#[derive(Debug, Clone, Copy)]
enum Behavior {
    Fill,
    /// 70 % filled, the rest stays open until cancelled.
    Partial,
    Reject,
    /// The submit reply is lost; the order was in fact filled.
    UnknownExecuted,
    /// The submit reply is lost; the order never arrived.
    UnknownNotArrived,
}

trait Harness {
    fn exchange(&self) -> Exchange {
        Exchange::Binance
    }
    /// OKX only (okx-execution-guards): a lost submit is "not found" only after `expTime` and two
    /// consecutive lookups; this moves the clock past `expTime` and says a first lookup is needed.
    fn expire_unknown_submits(&self) -> bool {
        false
    }
    fn executor(&self) -> Arc<dyn Executor>;
    fn simulated(&self) -> bool;
    fn id(&self, seq: u16) -> String;
    fn script(&self, id: &str, b: Behavior);
}

struct Sim(Arc<SimulatedExecutor>);

impl Sim {
    fn new() -> Sim {
        let prices = Arc::new(SimPriceBook::default());
        prices.set(Exchange::Binance, "BTCUSDT", d("60000"));
        Sim(Arc::new(SimulatedExecutor::new(prices)))
    }
}

impl Harness for Sim {
    fn executor(&self) -> Arc<dyn Executor> {
        self.0.clone()
    }
    fn simulated(&self) -> bool {
        true
    }
    fn id(&self, seq: u16) -> String {
        client_order_id(IdPrefix::Sim, "contract-pair", Leg::Long, OrderAction::Open, seq)
    }
    fn script(&self, id: &str, b: Behavior) {
        let behavior = match b {
            Behavior::Fill => SimBehavior::Fill,
            Behavior::Partial => SimBehavior::Partial(d("0.7")),
            Behavior::Reject => SimBehavior::Reject("insufficient margin (scripted)".into()),
            Behavior::UnknownExecuted => SimBehavior::Unknown { executed: true },
            Behavior::UnknownNotArrived => SimBehavior::Unknown { executed: false },
        };
        self.0.script_prefix(id, behavior);
    }
}

struct Demo(Rig);

/// The same contract on OKX demo replies (hand-built from the documentation, see fixtures/okx/orders).
struct OkxDemo(Rig);

fn okx_row(id: &str, state: &str, filled: &str, fee: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&okx_order_fx("order_filled")).unwrap();
    let row = &mut v["data"][0];
    row["clOrdId"] = serde_json::json!(id);
    row["state"] = serde_json::json!(state);
    row["accFillSz"] = serde_json::json!(filled);
    row["fee"] = serde_json::json!(fee);
    row["feeCcy"] = serde_json::json!(if fee == "0" { "" } else { "USDT" });
    v.to_string()
}

fn okx_not_found() -> Reply {
    Reply::ok(&okx_order_fx("order_51603"))
}

impl Harness for OkxDemo {
    fn exchange(&self) -> Exchange {
        Exchange::Okx
    }
    fn expire_unknown_submits(&self) -> bool {
        self.0.clock.advance(6_000);
        true
    }
    fn executor(&self) -> Arc<dyn Executor> {
        self.0.ex.clone()
    }
    fn simulated(&self) -> bool {
        false
    }
    fn id(&self, seq: u16) -> String {
        let id = client_order_id(IdPrefix::Demo, "contract-pair", Leg::Long, OrderAction::Open, seq);
        self.0.ledger.0.lock().unwrap().push(id.clone());
        id
    }
    fn script(&self, id: &str, b: Behavior) {
        let t = &self.0.t;
        script_okx_mode(t);
        let (post, get, cancel) = (Method::Post, Method::Get, "/api/v5/trade/cancel-order");
        match b {
            Behavior::Fill => {
                t.replace(post, "/api/v5/trade/order", Reply::ok(&okx_order_fx("place_accepted")));
                t.replace(get, "/api/v5/trade/order?", Reply::ok(&okx_row(id, "filled", QTY, "-0.24")));
            }
            Behavior::Partial => {
                t.replace(post, "/api/v5/trade/order", Reply::ok(&okx_order_fx("place_accepted")));
                t.replace(get, "/api/v5/trade/order?", Reply::ok(&okx_row(id, "partially_filled", PARTIAL, "-0.17")));
                t.on(get, "/api/v5/trade/order?", Reply::ok(&okx_row(id, "canceled", PARTIAL, "-0.17")));
                t.replace(post, cancel, Reply::ok(&okx_order_fx("cancel_accepted")));
            }
            Behavior::Reject => {
                t.replace(post, "/api/v5/trade/order", Reply::ok(&okx_order_fx("place_rejected_51131")));
                t.replace(get, "/api/v5/trade/order?", okx_not_found());
            }
            Behavior::UnknownExecuted => {
                t.replace(post, "/api/v5/trade/order", Reply::err(AdapterError::Timeout));
                t.replace(get, "/api/v5/trade/order?", Reply::ok(&okx_row(id, "filled", QTY, "-0.24")));
            }
            Behavior::UnknownNotArrived => {
                t.replace(post, "/api/v5/trade/order", Reply::err(AdapterError::network("connection reset by peer")));
                t.replace(get, "/api/v5/trade/order?", okx_not_found());
            }
        }
    }
}

fn ack(id: &str, status: &str, executed: &str) -> String {
    format!(r#"{{"orderId":4242,"clientOrderId":"{id}","status":"{status}","executedQty":"{executed}","avgPrice":"60000"}}"#)
}

impl Harness for Demo {
    fn executor(&self) -> Arc<dyn Executor> {
        self.0.ex.clone()
    }
    fn simulated(&self) -> bool {
        false
    }
    fn id(&self, seq: u16) -> String {
        let id = client_order_id(IdPrefix::Demo, "contract-pair", Leg::Long, OrderAction::Open, seq);
        self.0.ledger.0.lock().unwrap().push(id.clone()); // written by the engine before submitting
        id
    }
    fn script(&self, id: &str, b: Behavior) {
        let t = &self.0.t;
        let post = format!("newClientOrderId={id}");
        let by_id = format!("origClientOrderId={id}");
        let not_found = || Reply::status(400, r#"{"code":-2013,"msg":"Order does not exist."}"#);
        t.on(Method::Get, BINANCE_USER_TRADES_PATH, Reply::ok(r#"[{"commission":"0.24","commissionAsset":"USDT"}]"#));
        match b {
            Behavior::Fill => {
                t.on(Method::Post, &post, Reply::ok(&ack(id, "FILLED", QTY)));
                t.on(Method::Get, &by_id, Reply::ok(&ack(id, "FILLED", QTY)));
            }
            Behavior::Partial => {
                t.on(Method::Post, &post, Reply::ok(&ack(id, "PARTIALLY_FILLED", PARTIAL)));
                t.on(Method::Get, &by_id, Reply::ok(&ack(id, "PARTIALLY_FILLED", PARTIAL)));
                t.on(Method::Get, &by_id, Reply::ok(&ack(id, "CANCELED", PARTIAL)));
                t.on(Method::Delete, &by_id, Reply::ok(&ack(id, "CANCELED", PARTIAL)));
            }
            Behavior::Reject => {
                t.on(Method::Post, &post, Reply::status(400, r#"{"code":-2019,"msg":"Margin is insufficient."}"#));
                t.on(Method::Get, &by_id, not_found());
            }
            Behavior::UnknownExecuted => {
                t.on(Method::Post, &post, Reply::err(AdapterError::Timeout));
                t.on(Method::Get, &by_id, Reply::ok(&ack(id, "FILLED", QTY)));
            }
            Behavior::UnknownNotArrived => {
                t.on(Method::Post, &post, Reply::err(AdapterError::network("connection reset by peer")));
                t.on(Method::Get, &by_id, not_found());
            }
        }
        let _ = BINANCE_ORDER_PATH;
    }
}

fn order(exchange: Exchange, id: &str) -> OrderRequest {
    OrderRequest { client_order_id: id.into(), exchange, symbol: "BTCUSDT".into(), side: OrderSide::Buy, quantity: d(QTY), reduce_only: false }
}

fn found(o: QueryOutcome) -> crate::engine::ports::OrderStatus {
    match o {
        QueryOutcome::Found(s) => s,
        other => panic!("expected Found, got {other:?}"),
    }
}

async fn contract(h: &dyn Harness) {
    let ex = h.executor();
    assert_eq!(ex.is_simulated(), h.simulated(), "only the simulator says it is simulated");

    // 1. Accepted, then found under the SAME id with its fill.
    let id = h.id(1);
    h.script(&id, Behavior::Fill);
    match ex.submit(order(h.exchange(), &id)).await {
        SubmitOutcome::Accepted(s) => assert_eq!(s.client_order_id, id, "id passed through unchanged"),
        other => panic!("fill: {other:?}"),
    }
    let s = found(ex.query(h.exchange(), "BTCUSDT", &id).await);
    assert_eq!((s.client_order_id.as_str(), s.state, s.filled_quantity), (id.as_str(), OrderState::Filled, d(QTY)));
    assert!(s.fee.is_some() && s.fee_asset.is_some(), "fill details carry the fee");

    // 2. Rejected: nothing exists, a lookup finds nothing.
    let id = h.id(2);
    h.script(&id, Behavior::Reject);
    assert!(matches!(ex.submit(order(h.exchange(), &id)).await, SubmitOutcome::Rejected { .. }));
    assert_eq!(ex.query(h.exchange(), "BTCUSDT", &id).await, QueryOutcome::NotFound);

    // 3. Unknown, but it was executed: only the lookup by the same id tells.
    let id = h.id(3);
    h.script(&id, Behavior::UnknownExecuted);
    assert!(matches!(ex.submit(order(h.exchange(), &id)).await, SubmitOutcome::Unknown { .. }), "lost reply is unknown, not rejected");
    assert_eq!(found(ex.query(h.exchange(), "BTCUSDT", &id).await).filled_quantity, d(QTY));

    // 4. Unknown, never arrived.
    let id = h.id(4);
    h.script(&id, Behavior::UnknownNotArrived);
    assert!(matches!(ex.submit(order(h.exchange(), &id)).await, SubmitOutcome::Unknown { .. }));
    if h.expire_unknown_submits() {
        assert!(matches!(ex.query(h.exchange(), "BTCUSDT", &id).await, QueryOutcome::Failed { .. }), "one confirmation is not enough");
    }
    assert_eq!(ex.query(h.exchange(), "BTCUSDT", &id).await, QueryOutcome::NotFound);

    // 5. Partial fill: open with 70 %, cancel stops it, the final lookup is terminal with 70 %.
    let id = h.id(5);
    h.script(&id, Behavior::Partial);
    assert!(matches!(ex.submit(order(h.exchange(), &id)).await, SubmitOutcome::Accepted(_)));
    let s = found(ex.query(h.exchange(), "BTCUSDT", &id).await);
    assert_eq!((s.state, s.filled_quantity), (OrderState::Open, d(PARTIAL)));
    let c = found(ex.cancel(h.exchange(), "BTCUSDT", &id).await);
    assert_eq!(c.state, OrderState::Cancelled);
    let s = found(ex.query(h.exchange(), "BTCUSDT", &id).await);
    assert_eq!((s.state, s.filled_quantity), (OrderState::Cancelled, d(PARTIAL)), "final fill after the cancel");
    assert!(s.filled_quantity > Decimal::ZERO);
}

#[tokio::test]
async fn the_simulated_executor_satisfies_the_executor_contract() {
    contract(&Sim::new()).await;
}

#[tokio::test]
async fn the_demo_executor_satisfies_the_same_contract_on_recorded_replies() {
    contract(&Demo(rig())).await;
}

#[tokio::test]
async fn the_okx_demo_path_satisfies_the_same_contract_on_documented_replies() {
    contract(&OkxDemo(rig())).await;
}

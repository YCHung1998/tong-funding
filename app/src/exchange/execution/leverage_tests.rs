//! order-leverage-sync: the executor aligns the exchange-side leverage of the symbol before an
//! opening order and sends nothing when that fails. No network: scripted `FakeOrderTransport`.
#![cfg(test)]

use tong_funding_core::types::{Decimal, Exchange};

use super::classify::SubmitClass;
use super::endpoints::*;
use super::executor_tests::{SYM, binance_ack, bybit_created, d, demo_id, req, rig};
use super::http::Method;
use super::http::fake::Reply;
use crate::engine::ports::{Leg, OrderAction, OrderRequest, OrderSide};
use crate::exchange::error::AdapterError;

fn open(exchange: Exchange, leverage: Option<&str>) -> OrderRequest {
    let (leg, side) = if exchange == Exchange::Binance { (Leg::Long, OrderSide::Buy) } else { (Leg::Short, OrderSide::Sell) };
    let mut r = req(exchange, &demo_id(leg, OrderAction::Open, 0), side, "0.019", false);
    r.leverage = leverage.map(d);
    r
}

fn binance_leverage_ok() -> Reply {
    Reply::ok(r#"{"symbol":"BTCUSDT","leverage":5,"maxNotionalValue":"1000000"}"#)
}

fn bybit_leverage_ok() -> Reply {
    Reply::ok(r#"{"retCode":0,"retMsg":"OK","result":{},"retExtInfo":{},"time":1}"#)
}

/// Index of the first request to `path` (method POST) in send order.
fn post_index(r: &super::executor_tests::Rig, path: &str) -> Option<usize> {
    r.t.requests().iter().position(|q| q.method() == Method::Post && q.full_url().contains(path))
}

#[tokio::test]
async fn binance_sets_the_leverage_first_then_sends_the_order() {
    let r = rig();
    r.t.on(Method::Post, BINANCE_LEVERAGE_PATH, binance_leverage_ok());
    let o = open(Exchange::Binance, Some("5"));
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&o.client_order_id, "FILLED", "0.019")));
    assert!(matches!(r.ex.submit_classified(&o).await, SubmitClass::Accepted(_)));
    let (lev, ord) = (post_index(&r, BINANCE_LEVERAGE_PATH).expect("leverage request"), post_index(&r, BINANCE_ORDER_PATH).expect("order request"));
    assert!(lev < ord, "leverage {lev} must precede the order {ord}");
    let url = r.t.requests()[lev].full_url().to_string();
    assert!(url.contains(&format!("symbol={SYM}")) && url.contains("leverage=5&"), "{url}");
}

#[tokio::test]
async fn bybit_sets_equal_buy_and_sell_leverage_first_then_sends_the_order() {
    let r = rig();
    r.t.on(Method::Post, BYBIT_SET_LEVERAGE_PATH, bybit_leverage_ok());
    let o = open(Exchange::Bybit, Some("5"));
    r.t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&o.client_order_id)));
    assert!(matches!(r.ex.submit_classified(&o).await, SubmitClass::Accepted(_)));
    let (lev, ord) = (post_index(&r, BYBIT_SET_LEVERAGE_PATH).expect("leverage request"), post_index(&r, BYBIT_CREATE_PATH).expect("order request"));
    assert!(lev < ord);
    let body: serde_json::Value = serde_json::from_str(r.t.requests()[lev].body().unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"category":"linear","symbol":SYM,"buyLeverage":"5","sellLeverage":"5"}));
}

#[tokio::test]
async fn bybit_leverage_not_modified_still_sends_the_order() {
    let r = rig();
    r.t.on(Method::Post, BYBIT_SET_LEVERAGE_PATH, Reply::ok(r#"{"retCode":110043,"retMsg":"Set leverage not modified","result":{}}"#));
    let o = open(Exchange::Bybit, Some("5"));
    r.t.on(Method::Post, BYBIT_CREATE_PATH, Reply::ok(&bybit_created(&o.client_order_id)));
    assert!(matches!(r.ex.submit_classified(&o).await, SubmitClass::Accepted(_)));
}

#[tokio::test]
async fn a_failed_leverage_request_means_no_order_is_sent() {
    let cases: [(Exchange, &str, &str, Reply); 6] = [
        (Exchange::Binance, BINANCE_LEVERAGE_PATH, BINANCE_ORDER_PATH, Reply::status(400, r#"{"code":-4028,"msg":"Leverage 5 is not valid"}"#)),
        (Exchange::Binance, BINANCE_LEVERAGE_PATH, BINANCE_ORDER_PATH, Reply::status(429, "")),
        (Exchange::Binance, BINANCE_LEVERAGE_PATH, BINANCE_ORDER_PATH, Reply::err(AdapterError::Timeout)),
        (Exchange::Bybit, BYBIT_SET_LEVERAGE_PATH, BYBIT_CREATE_PATH, Reply::ok(r#"{"retCode":110013,"retMsg":"Parameter Error","result":{}}"#)),
        (Exchange::Bybit, BYBIT_SET_LEVERAGE_PATH, BYBIT_CREATE_PATH, Reply::status(429, "")),
        (Exchange::Bybit, BYBIT_SET_LEVERAGE_PATH, BYBIT_CREATE_PATH, Reply::ok("<html>gateway</html>")),
    ];
    for (exchange, lev_path, order_path, reply) in cases {
        let r = rig();
        r.t.on(Method::Post, lev_path, reply.clone());
        let o = open(exchange, Some("5"));
        r.t.on(Method::Post, order_path, Reply::ok(&if exchange == Exchange::Binance { binance_ack(&o.client_order_id, "FILLED", "0.019") } else { bybit_created(&o.client_order_id) }));
        match r.ex.submit_classified(&o).await {
            SubmitClass::Rejected { code, message } => {
                assert_eq!(code, "not_sent", "{exchange:?} {reply:?}");
                assert!(message.contains("leverage"), "{message}");
            }
            other => panic!("{exchange:?} {reply:?} -> {other:?}"),
        }
        assert_eq!(r.t.count(Method::Post, order_path), 0, "{exchange:?} {reply:?}: the order must not be sent");
    }
}

#[tokio::test]
async fn a_rate_limited_leverage_request_backs_the_exchange_off() {
    let r = rig();
    r.t.on(Method::Post, BINANCE_LEVERAGE_PATH, Reply::status(429, "").with_header("Retry-After", "5"));
    assert!(matches!(r.ex.submit_classified(&open(Exchange::Binance, Some("5"))).await, SubmitClass::Rejected { .. }));
    let before = r.t.requests().len();
    let again = r.ex.submit_classified(&open(Exchange::Binance, Some("5"))).await;
    assert!(matches!(&again, SubmitClass::Rejected { message, .. } if message.contains("rate limited")), "{again:?}");
    assert_eq!(r.t.requests().len(), before, "nothing is sent during the back-off");
}

#[tokio::test]
async fn an_invalid_leverage_sends_no_request_at_all() {
    for bad in ["2.5", "0", "126", "-3"] {
        for exchange in [Exchange::Binance, Exchange::Bybit] {
            let r = rig();
            match r.ex.submit_classified(&open(exchange, Some(bad))).await {
                SubmitClass::Rejected { code, message } => assert!(code == "not_sent" && message.contains("leverage"), "{bad}: {message}"),
                other => panic!("{bad} {exchange:?}: {other:?}"),
            }
            assert!(r.t.requests().is_empty(), "{bad} {exchange:?}: {:?}", r.t.requests().len());
        }
    }
}

#[tokio::test]
async fn no_leverage_or_reduce_only_sends_no_leverage_request() {
    let r = rig();
    let none = open(Exchange::Binance, None);
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&none.client_order_id, "FILLED", "0.019")));
    assert!(matches!(r.ex.submit_classified(&none).await, SubmitClass::Accepted(_)));

    let mut close = req(Exchange::Binance, &demo_id(Leg::Long, OrderAction::Close, 0), OrderSide::Sell, "0.019", true);
    close.leverage = Some(Decimal::from(5));
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&close.client_order_id, "FILLED", "0.019")));
    assert!(matches!(r.ex.submit_classified(&close).await, SubmitClass::Accepted(_)));
    assert_eq!(r.t.count(Method::Post, BINANCE_LEVERAGE_PATH), 0);
    assert_eq!(r.t.count(Method::Post, BYBIT_SET_LEVERAGE_PATH), 0);
}

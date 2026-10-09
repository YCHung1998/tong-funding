//! order-leverage-sync: the executor aligns the exchange-side leverage of the symbol before an
//! opening order and sends nothing when that fails. No network: scripted `FakeOrderTransport`.
#![cfg(test)]

use std::sync::Arc;

use tong_funding_core::types::{Decimal, Exchange};

use super::classify::SubmitClass;
use super::endpoints::*;
use super::executor_tests::{SYM, binance_ack, bybit_created, d, demo_id, okx_order_fx, okx_req, req, rig, script_okx_mode};
use super::http::Method;
use crate::engine::ports::Executor;
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

// ---- okx-trading-enablement 3.8: OKX set-leverage (cross, net mode) before the opening order ------

fn okx_open_with(leverage: Option<&str>) -> OrderRequest {
    let mut r = okx_req(&demo_id(Leg::Short, OrderAction::Open, 0), OrderSide::Sell, "3", false);
    r.leverage = leverage.map(d);
    r
}

fn okx_requests(r: &super::executor_tests::Rig) -> Vec<String> {
    r.t.requests().iter().map(|q| format!("{:?} {}", q.method(), q.full_url().trim_start_matches("https://openapi.okx.com"))).collect()
}

#[tokio::test]
async fn okx_proves_the_demo_key_then_sets_the_leverage_then_sends_the_order() {
    let r = rig();
    script_okx_mode(&r.t);
    r.t.on(Method::Post, OKX_SET_LEVERAGE_PATH, Reply::ok(&okx_order_fx("set_leverage_ok")));
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    assert!(matches!(r.ex.submit_classified(&okx_open_with(Some("5"))).await, SubmitClass::Accepted(_)));
    assert_eq!(
        okx_requests(&r),
        ["Get /api/v5/account/config", "Post /api/v5/account/set-leverage", "Post /api/v5/trade/order"],
        "the account-config read (the demo-key proof) comes first, the leverage second, the order last"
    );
    let lev = &r.t.requests()[1];
    let body: serde_json::Value = serde_json::from_str(lev.body().unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"instId":"BTC-USDT-SWAP","lever":"5","mgnMode":"cross"}), "cross margin, net mode: no posSide");
    // it is a signed request like the order: the flag exactly once, the OKX headers, a signature over the body that was sent
    let header = |n: &str| lev.headers().iter().filter(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone()).collect::<Vec<_>>();
    assert_eq!(header("x-simulated-trading"), vec!["1".to_string()]);
    assert_eq!(header("OK-ACCESS-KEY").len(), 1);
    let want = crate::exchange::signed::signing::okx_signature("TEST_SECRET_NOT_REAL", &header("OK-ACCESS-TIMESTAMP")[0], "POST", OKX_SET_LEVERAGE_PATH, lev.body().unwrap()).unwrap();
    assert_eq!(header("OK-ACCESS-SIGN"), vec![want]);
    assert!(header("expTime").is_empty(), "set-leverage is not an order: no expTime");
    // every OKX request, set-leverage included, passed the real admission rule (the fake panics otherwise)
    for q in r.t.requests() {
        let url = reqwest::Url::parse(q.full_url()).unwrap();
        assert!(crate::exchange::reqwest_transport::HostPolicy::SignedDemo.allows(&url, q.headers()), "{}", q.full_url());
    }
}

#[tokio::test]
async fn an_okx_close_or_an_open_without_leverage_sends_no_set_leverage() {
    let r = rig();
    script_okx_mode(&r.t);
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    assert!(matches!(r.ex.submit_classified(&okx_open_with(None)).await, SubmitClass::Accepted(_)));
    let mut close = okx_req(&demo_id(Leg::Short, OrderAction::Close, 0), OrderSide::Buy, "3", true);
    close.leverage = Some(d("5"));
    assert!(matches!(r.ex.submit_classified(&close).await, SubmitClass::Accepted(_)));
    assert_eq!(r.t.count(Method::Post, OKX_SET_LEVERAGE_PATH), 0);
}

#[tokio::test]
async fn a_failed_okx_leverage_request_means_no_order_is_sent() {
    let cases: [(&str, Reply); 5] = [
        ("refused 59102", Reply::ok(&okx_order_fx("set_leverage_59102"))),
        ("rate limited", Reply::ok(&okx_order_fx("place_50011"))),
        ("timeout", Reply::err(AdapterError::Timeout)),
        ("unparsable", Reply::ok("<html>gateway</html>")),
        ("unknown 50004", Reply::ok(&okx_order_fx("place_50004"))),
    ];
    for (what, reply) in cases {
        let r = rig();
        script_okx_mode(&r.t);
        r.t.on(Method::Post, OKX_SET_LEVERAGE_PATH, reply);
        r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
        match r.ex.submit_classified(&okx_open_with(Some("5"))).await {
            SubmitClass::Rejected { code, message } => {
                assert_eq!(code, "not_sent", "{what}");
                assert!(message.contains("leverage"), "{what}: {message}");
            }
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(r.t.count(Method::Post, OKX_ORDER_PATH), 0, "{what}: the order must not be sent");
        assert_eq!(r.t.count(Method::Post, OKX_SET_LEVERAGE_PATH), 1, "{what}: exactly one attempt, no automatic retry");
    }
}

#[tokio::test]
async fn an_environment_mismatch_on_set_leverage_latches_okx_off_for_everything_that_can_open_exposure() {
    let r = rig();
    script_okx_mode(&r.t);
    r.t.on(Method::Post, OKX_SET_LEVERAGE_PATH, Reply::ok(&okx_order_fx("place_50101")));
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    assert!(matches!(r.ex.submit_classified(&okx_open_with(Some("5"))).await, SubmitClass::Rejected { .. }));
    assert_eq!(r.t.count(Method::Post, OKX_ORDER_PATH), 0);
    // latched: the next open sends nothing at all (no mode read, no leverage, no order)
    let before = r.t.requests().len();
    let again = r.ex.submit_classified(&okx_open_with(Some("5"))).await;
    assert!(matches!(&again, SubmitClass::Rejected { message, .. } if message.contains("OKX disabled")), "{again:?}");
    assert_eq!(r.t.requests().len(), before);
}

#[tokio::test]
async fn an_okx_open_that_fails_the_size_guard_requests_nothing_not_even_the_leverage() {
    let r = rig();
    script_okx_mode(&r.t);
    let mut o = okx_open_with(Some("5"));
    o.intended_base_qty = None; // an open must say its coin amount
    assert!(matches!(r.ex.submit_classified(&o).await, SubmitClass::Rejected { .. }));
    assert!(r.t.requests().is_empty(), "{:?}", okx_requests(&r));
}

#[tokio::test]
async fn an_invalid_okx_leverage_sends_no_request_at_all() {
    for bad in ["2.5", "0", "126", "-3"] {
        let r = rig();
        match r.ex.submit_classified(&okx_open_with(Some(bad))).await {
            SubmitClass::Rejected { code, message } => assert!(code == "not_sent" && message.contains("leverage"), "{bad}: {message}"),
            other => panic!("{bad}: {other:?}"),
        }
        assert!(r.t.requests().is_empty(), "{bad}");
    }
}

#[tokio::test]
async fn an_okx_leverage_without_a_proven_demo_key_is_never_sent() {
    // the account-config read answers an error: the key is not proven, so neither the leverage nor the order may go out
    let r = rig();
    r.t.on(Method::Get, "/api/v5/account/config", Reply::ok(&okx_order_fx("place_50101")));
    r.t.on(Method::Post, OKX_SET_LEVERAGE_PATH, Reply::ok(&okx_order_fx("set_leverage_ok")));
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    assert!(matches!(r.ex.submit_classified(&okx_open_with(Some("5"))).await, SubmitClass::Rejected { .. }));
    assert_eq!(r.t.count(Method::Post, OKX_SET_LEVERAGE_PATH), 0);
    assert_eq!(r.t.count(Method::Post, OKX_ORDER_PATH), 0);
}

// ---- okx-trading-enablement F1 / F2 / F3: the pre-send barrier --------------------------------------

fn pair_legs(leverage: &str) -> Vec<OrderRequest> {
    let mut bin = open(Exchange::Binance, Some(leverage));
    bin.notional_usdt = Some(d("1000"));
    let mut okx = okx_open_with(Some(leverage));
    okx.notional_usdt = Some(d("1000"));
    vec![bin, okx]
}

fn script_both_ok(r: &super::executor_tests::Rig) {
    script_okx_mode(&r.t);
    r.t.on(Method::Post, OKX_SET_LEVERAGE_PATH, Reply::ok(&okx_order_fx("set_leverage_ok")));
    r.t.on(Method::Post, BINANCE_LEVERAGE_PATH, binance_leverage_ok());
}

#[tokio::test]
async fn a_pair_is_prepared_with_the_leverage_on_both_legs_and_the_orders_do_not_set_it_again() {
    let r = rig();
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    assert_eq!((r.t.count(Method::Post, BINANCE_LEVERAGE_PATH), r.t.count(Method::Post, OKX_SET_LEVERAGE_PATH)), (1, 1));
    assert_eq!(r.t.count(Method::Post, BINANCE_ORDER_PATH) + r.t.count(Method::Post, OKX_ORDER_PATH), 0, "preparing sends no order");
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&legs[0].client_order_id, "FILLED", "0.019")));
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    for l in &legs {
        assert!(matches!(r.ex.submit_classified(l).await, SubmitClass::Accepted(_)), "{l:?}");
    }
    assert_eq!((r.t.count(Method::Post, BINANCE_LEVERAGE_PATH), r.t.count(Method::Post, OKX_SET_LEVERAGE_PATH)), (1, 1), "the barrier's leverage is not set twice");
}

#[tokio::test]
async fn if_okx_refuses_the_leverage_nothing_is_sent_to_binance() {
    let r = rig();
    script_both_ok(&r);
    r.t.replace(Method::Post, OKX_SET_LEVERAGE_PATH, Reply::ok(&okx_order_fx("set_leverage_59102")));
    let e = r.ex.prepare_open(pair_legs("5")).await.unwrap_err();
    assert!(e.contains("leverage") && e.contains("OKX"), "{e}");
    assert_eq!(r.t.count(Method::Post, BINANCE_ORDER_PATH) + r.t.count(Method::Post, OKX_ORDER_PATH), 0);
}

#[tokio::test]
async fn if_binance_refuses_the_leverage_nothing_is_sent_to_okx() {
    let r = rig();
    script_both_ok(&r);
    r.t.replace(Method::Post, BINANCE_LEVERAGE_PATH, Reply::status(400, r#"{"code":-4028,"msg":"Leverage 5 is not valid"}"#));
    let e = r.ex.prepare_open(pair_legs("5")).await.unwrap_err();
    assert!(e.contains("leverage") && e.contains("Binance"), "{e}");
    assert_eq!(r.t.count(Method::Post, BINANCE_ORDER_PATH) + r.t.count(Method::Post, OKX_ORDER_PATH), 0);
    // a refused leg is also never submitted afterwards by an engine that obeys the barrier; the
    // unchanged-leverage codes keep their meaning (Bybit 110043 counts as set)
}

#[tokio::test]
async fn okx_size_refusals_are_found_before_any_request_for_either_leg() {
    // cap derives from the pair's own notional: 500 USDT -> cap 1000 < 0.03 BTC x 60000 = 1800
    let r = rig();
    script_both_ok(&r);
    let mut legs = pair_legs("5");
    legs[1].notional_usdt = Some(d("500"));
    let e = r.ex.prepare_open(legs).await.unwrap_err();
    assert!(e.contains("exceeds the per-leg cap"), "{e}");
    assert!(r.t.requests().is_empty(), "a size refusal on the OKX leg means nothing is requested, not even for Binance: {:?}", okx_requests(&r));
    // a lowered template (the live cap) does not strand a pair whose own notional is fine
    let ok = super::executor_tests::rig_with_cap("10");
    super::executor_tests::script_one_way(&ok.t);
    script_both_ok(&ok);
    assert_eq!(ok.ex.prepare_open(pair_legs("5")).await, Ok(()));
}

#[tokio::test]
async fn missing_limits_are_found_before_any_request() {
    let r = super::executor_tests::rig_without_limits();
    super::executor_tests::script_one_way(&r.t);
    script_both_ok(&r);
    let e = r.ex.prepare_open(pair_legs("5")).await.unwrap_err();
    assert!(e.contains("limits"), "{e}");
    assert!(r.t.requests().is_empty());
}

#[tokio::test]
async fn a_set_leverage_reply_must_echo_the_requested_lever_and_cross_margin() {
    for (what, body) in [
        ("other lever", r#"{"code":"0","msg":"","data":[{"instId":"BTC-USDT-SWAP","lever":"3","mgnMode":"cross","posSide":""}]}"#),
        ("isolated", r#"{"code":"0","msg":"","data":[{"instId":"BTC-USDT-SWAP","lever":"5","mgnMode":"isolated","posSide":""}]}"#),
        ("no data", r#"{"code":"0","msg":"","data":[]}"#),
        ("no instId", r#"{"code":"0","msg":"","data":[{"lever":"5","mgnMode":"cross","posSide":""}]}"#),
        ("other instrument", r#"{"code":"0","msg":"","data":[{"instId":"ETH-USDT-SWAP","lever":"5","mgnMode":"cross","posSide":""}]}"#),
    ] {
        let r = rig();
        script_okx_mode(&r.t);
        r.t.on(Method::Post, OKX_SET_LEVERAGE_PATH, Reply::ok(body));
        r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
        match r.ex.submit_classified(&okx_open_with(Some("5"))).await {
            SubmitClass::Rejected { code, message } => assert!(code == "not_sent" && message.contains("leverage"), "{what}: {message}"),
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(r.t.count(Method::Post, OKX_ORDER_PATH), 0, "{what}");
    }
}

// ---- okx-trading-enablement round 2 (G1 / G2): a prepared pair is sent as a pair -----------------------

fn prepared(mut legs: Vec<OrderRequest>) -> Vec<OrderRequest> {
    for l in &mut legs {
        l.prepared = true; // what the engine does on `OpenPrepared` Ok
    }
    legs
}

async fn send_both(r: &super::executor_tests::Rig, legs: &[OrderRequest]) -> (SubmitClass, SubmitClass) {
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&legs[0].client_order_id, "FILLED", "0.019")));
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    (r.ex.submit_classified(&legs[0]).await, r.ex.submit_classified(&legs[1]).await)
}

fn both_sent(b: &SubmitClass, o: &SubmitClass) -> bool {
    matches!(b, SubmitClass::Accepted(_)) && matches!(o, SubmitClass::Accepted(_))
}

#[tokio::test]
async fn skeptic_mode_ttl_expiry_with_a_failing_reread_still_sends_both_prepared_legs() {
    let r = rig();
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    // the cached OKX mode reading ages past its TTL and a re-read would fail (5xx / timeout)
    r.clock.advance(crate::exchange::signed::okx::POSITION_MODE_TTL_MS + 1);
    r.t.replace(Method::Get, "/api/v5/account/config", Reply::status(503, "unavailable"));
    let (b, o) = send_both(&r, &legs).await;
    assert!(both_sent(&b, &o), "BOTH or NEITHER: {b:?} / {o:?}");
    assert_eq!(r.t.count(Method::Get, "/api/v5/account/config"), 1, "the barrier read it once; the sends do not read it again");
}

#[tokio::test]
async fn a_backoff_started_after_the_barrier_does_not_split_the_pair() {
    let r = rig();
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    for ex in [Exchange::Binance, Exchange::Okx] {
        r.limiter.on_rate_limited(ex, crate::exchange::health::ratelimit::RequestClass::Signed, Some(5_000));
    }
    let (b, o) = send_both(&r, &legs).await;
    assert!(both_sent(&b, &o), "{b:?} / {o:?}");
}

#[tokio::test]
async fn limits_that_vanish_after_the_barrier_a_stale_mark_or_an_unreadable_template_do_not_split_the_pair() {
    use std::sync::atomic::Ordering;
    let toggle = Arc::new(super::executor_tests::ToggleLimits { on: true.into(), inner: super::executor_tests::btc_limits() });
    let r = super::executor_tests::rig_custom(Some(0), Some(toggle.clone()));
    super::executor_tests::script_one_way(&r.t);
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    toggle.on.store(false, Ordering::SeqCst);
    r.clock.advance(61_000);
    let (b, o) = send_both(&r, &legs).await;
    assert!(both_sent(&b, &o), "{b:?} / {o:?}");
    // an UNPREPARED opening leg is still refused in the same situation (the executor's own guard stands)
    let r2 = super::executor_tests::rig_custom(Some(0), Some(Arc::new(super::executor_tests::ToggleLimits { on: false.into(), inner: super::executor_tests::btc_limits() })));
    super::executor_tests::script_one_way(&r2.t);
    script_both_ok(&r2);
    assert!(matches!(r2.ex.submit_classified(&pair_legs("5")[1]).await, SubmitClass::Rejected { .. }));
}

#[tokio::test]
async fn a_latch_tripped_between_the_barrier_and_the_send_is_the_accepted_residual_and_names_the_naked_leg() {
    let r = rig();
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    // the hard stop: an environment mismatch seen after the barrier (here through a leg that already went)
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::ok(&binance_ack(&legs[0].client_order_id, "FILLED", "0.019")));
    assert!(matches!(r.ex.submit_classified(&legs[0]).await, SubmitClass::Accepted(_)));
    r.t.replace(Method::Get, "/api/v5/account/config", Reply::ok(&okx_order_fx("place_50101")));
    // trip the latch with a lookup that sees 50101
    r.t.on(Method::Get, "/api/v5/trade/order?", Reply::ok(&okx_order_fx("place_50101")));
    let _ = r.ex.query(Exchange::Okx, SYM, &legs[1].client_order_id).await;
    match r.ex.submit_classified(&legs[1]).await {
        SubmitClass::Rejected { code, message } => {
            assert_eq!(code, "not_sent");
            assert!(message.contains("OKX disabled") && message.contains("naked") && message.contains("barrier"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(r.t.count(Method::Post, OKX_ORDER_PATH), 0, "the hard stop still holds");
}

#[tokio::test]
async fn any_non_success_of_a_prepared_leg_is_worded_as_a_possibly_naked_opposite_leg() {
    // OKX answers 50101 to the order itself (a code other than not_sent)
    let r = rig();
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_50101")));
    match r.ex.submit_classified(&legs[1]).await {
        SubmitClass::Rejected { code, message } => {
            assert_eq!(code, "50101");
            assert!(message.contains("naked") && message.contains("barrier"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // Binance refuses the prepared order (insufficient margin)
    let r = rig();
    script_both_ok(&r);
    let legs = prepared(pair_legs("5"));
    assert_eq!(r.ex.prepare_open(legs.clone()).await, Ok(()));
    r.t.on(Method::Post, BINANCE_ORDER_PATH, Reply::status(400, r#"{"code":-2019,"msg":"Margin is insufficient."}"#));
    match r.ex.submit_classified(&legs[0]).await {
        SubmitClass::Rejected { message, .. } => assert!(message.contains("naked"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_prepared_leg_without_a_calibrated_clock_is_not_sent_with_a_local_time_fallback() {
    let r = super::executor_tests::rig_custom(None, Some(super::executor_tests::btc_limits()));
    script_okx_mode(&r.t);
    let mut legs = prepared(pair_legs("5"));
    legs.remove(0); // only the OKX leg
    r.t.on(Method::Post, OKX_ORDER_PATH, Reply::ok(&okx_order_fx("place_accepted")));
    match r.ex.submit_classified(&legs[0]).await {
        SubmitClass::Rejected { code, message } => assert!(code == "not_sent" && message.contains("not calibrated"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(r.t.count(Method::Post, OKX_ORDER_PATH), 0);
}

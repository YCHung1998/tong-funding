//! okx-trading-enablement round 2 (G2): the REAL `DemoExecutor` (fake transports) driven by the REAL engine
//! through the pre-send barrier and the submits of a Binance / OKX pair. No network.
#![cfg(test)]

use std::sync::Arc;

use tong_funding_core::types::Exchange;

use super::binance::BinanceOrderClient;
use super::bybit::BybitOrderClient;
use super::executor::DemoExecutor;
use super::http::Method;
use super::http::fake::{FakeOrderTransport, Reply};
use super::okx::{OkxLimits, OkxLimitsSource, OkxOrderClient};
use crate::engine::actor::flow_tests::run_okx_pair_with;
use crate::exchange::health::ratelimit::RateLimiter;
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost, OkxHost};
use crate::exchange::signed::signing::load_credentials;
use crate::ports::{MemorySecrets, SecretName};

struct FixedLimits;
impl OkxLimitsSource for FixedLimits {
    fn limits(&self, _symbol: &str) -> Option<OkxLimits> {
        Some(OkxLimits { ct_val: "0.01".parse().unwrap(), lot_sz: "1".parse().unwrap(), mark_px: "100".parse().unwrap(), max_leg_notional: "100000".parse().unwrap() })
    }
}

fn fixture(rel: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/okx/{rel}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[tokio::test(start_paused = true)]
async fn the_real_executor_sets_both_leverages_then_sends_both_legs_through_the_engine() {
    let t = FakeOrderTransport::new();
    t.on(Method::Get, "/fapi/v1/positionSide/dual", Reply::ok(r#"{"dualSidePosition":false}"#));
    t.on(Method::Post, "/fapi/v1/leverage", Reply::ok(r#"{"symbol":"BTCUSDT","leverage":5,"maxNotionalValue":"1000000"}"#));
    t.on(Method::Post, "/fapi/v1/order", Reply::ok(r#"{"orderId":555,"clientOrderId":"x","status":"NEW","executedQty":"0","avgPrice":"0"}"#));
    t.on(Method::Get, "/api/v5/account/config", Reply::ok(&fixture("signed/account_config_futures_net")));
    t.on(Method::Post, "/api/v5/account/set-leverage", Reply::ok(&fixture("orders/set_leverage_ok")));
    t.on(Method::Post, "/api/v5/trade/order", Reply::ok(&fixture("orders/place_accepted")));
    let creds = |ex: Exchange| {
        let m = MemorySecrets::default().with(ex, SecretName::ApiKey, "TEST_KEY_NOT_REAL").with(ex, SecretName::ApiSecret, "TEST_SECRET_NOT_REAL").with(ex, SecretName::Passphrase, "TEST_PASS_NOT_REAL");
        Arc::new(load_credentials(&m, ex, ex == Exchange::Okx).unwrap())
    };
    let tt = t.clone();
    run_okx_pair_with(move |p| {
        let clock = Arc::new(p.clock.clone());
        Arc::new(
            DemoExecutor::new(
                BinanceOrderClient::new(Arc::new(tt.clone()), creds(Exchange::Binance), BinanceHost::Testnet),
                BybitOrderClient::new(Arc::new(tt.clone()), creds(Exchange::Bybit), BybitHost::Demo),
                clock.clone(),
                p.offsets,
                Arc::new(p.db),
                Arc::new(RateLimiter::new(clock)),
            )
            .with_okx(OkxOrderClient::new(Arc::new(tt.clone()), creds(Exchange::Okx), OkxHost::Demo))
            .with_okx_limits(Arc::new(FixedLimits)),
        )
    })
    .await;
    let reqs: Vec<String> = t.requests().iter().map(|r| format!("{:?} /{}", r.method(), r.full_url().splitn(4, '/').nth(3).unwrap_or("").split('?').next().unwrap_or(""))).collect();
    let posts: Vec<&String> = reqs.iter().filter(|r| r.starts_with("Post")).collect();
    assert_eq!(posts.len(), 4, "{reqs:?}");
    assert!(posts[..2].iter().all(|r| r.contains("leverage")), "both leverages first: {posts:?}");
    assert!(posts[2..].iter().all(|r| r.contains("order") && !r.contains("leverage")), "then both orders: {posts:?}");
    assert_eq!(t.count(Method::Get, "/api/v5/account/config"), 1, "the mode is read once, by the barrier");
}

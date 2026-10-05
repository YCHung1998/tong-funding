//! `GatedTransport`: wires rate limiting into the request path. Wrap any `HttpTransport`; every
//! request first asks the `RateLimiter` (back-off per exchange and request class) and, for
//! Binance, the `WeightGate`; a refused request returns `RateLimited` WITHOUT touching the
//! network. Responses feed the state back: 429/418 and exchange-specific rate-limit bodies start a
//! back-off, `x-mbx-used-weight-1m` updates the weight gate, a 2xx resets the back-off.
#![allow(dead_code)]

use std::sync::Arc;

use tong_funding_core::types::Exchange;

use super::ratelimit::{BackoffState, RateLimiter, WeightGate, classify_exchange_body, classify_request, rate_limit_error_at};
use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpResponse, HttpTransport};
use crate::ports::TimeSource;

/// Largest body inspected for an embedded rate-limit code.
const MAX_BODY_SNIFF: usize = 4096;

pub struct GatedTransport<T: HttpTransport> {
    inner: T,
    exchange: Exchange,
    limiter: Arc<RateLimiter>,
    weight: Arc<WeightGate>,
    clock: Arc<dyn TimeSource>,
}

impl<T: HttpTransport> GatedTransport<T> {
    pub fn new(inner: T, exchange: Exchange, limiter: Arc<RateLimiter>, weight: Arc<WeightGate>, clock: Arc<dyn TimeSource>) -> Self {
        Self { inner, exchange, limiter, weight, clock }
    }
}

impl<T: HttpTransport> HttpTransport for GatedTransport<T> {
    async fn get(&self, req: HttpRequest) -> Result<HttpResponse, AdapterError> {
        let (ex, class) = (self.exchange, classify_request(&req.url));
        if !self.limiter.allow_request(ex, class) {
            let retry_after_ms = match self.limiter.state(ex, class) {
                BackoffState::Waiting { remaining_ms, .. } => Some(remaining_ms.max(0) as u64),
                BackoffState::Clear => None,
            };
            return Err(AdapterError::RateLimited { retry_after_ms });
        }
        if ex == Exchange::Binance && !self.weight.allow(class) {
            return Err(AdapterError::RateLimited { retry_after_ms: self.weight.defer_remaining_ms() });
        }
        // Transport errors (timeout, network) pass through: they are not rate-limit signals.
        let resp = self.inner.get(req).await?;
        if ex == Exchange::Binance {
            self.weight.observe_response(&resp);
        }
        let signal = rate_limit_error_at(&resp, self.clock.now_ms()).or_else(|| {
            // Rate limits can also hide in a 2xx body; those bodies are tiny, so skip big payloads.
            (resp.status / 100 == 2 && resp.body.len() <= MAX_BODY_SNIFF).then(|| classify_exchange_body(ex, &resp.body)).flatten()
        });
        if let Some(AdapterError::RateLimited { retry_after_ms }) = signal {
            let wait = self.limiter.on_rate_limited(ex, class, retry_after_ms);
            return Err(AdapterError::RateLimited { retry_after_ms: Some(wait) });
        }
        if resp.status / 100 == 2 {
            self.limiter.on_success(ex, class);
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::exchange::health::ratelimit::RETRY_AFTER_CAP_MS;
    use crate::exchange::signed::endpoints::BINANCE_TESTNET_HOST;
    use crate::exchange::transport::FakeTransport;
    use crate::ports::ManualClock;

    const BATCH: &str = "https://pub.example/fapi/v1/premiumIndex";
    const SINGLE: &str = "https://pub.example/fapi/v1/premiumIndex?symbol=BTCUSDT";

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    struct Rig {
        clock: ManualClock,
        weight: Arc<WeightGate>,
        gated: GatedTransport<Arc<FakeTransport>>,
        fake: Arc<FakeTransport>,
    }

    impl HttpTransport for Arc<FakeTransport> {
        fn get(&self, req: HttpRequest) -> impl std::future::Future<Output = Result<HttpResponse, AdapterError>> + Send {
            self.as_ref().get(req)
        }
    }

    fn rig(exchange: Exchange, fake: FakeTransport) -> Rig {
        let clock = ManualClock::new(1_000_000);
        let ts: Arc<dyn TimeSource> = Arc::new(clock.clone());
        let weight = Arc::new(WeightGate::new(ts.clone()));
        let fake = Arc::new(fake);
        let gated = GatedTransport::new(fake.clone(), exchange, Arc::new(RateLimiter::new(ts.clone())), weight.clone(), ts);
        Rig { clock, weight, gated, fake }
    }

    fn get(r: &Rig, url: &str) -> Result<HttpResponse, AdapterError> {
        block_on(r.gated.get(HttpRequest::get(url, Duration::from_secs(1))))
    }

    fn limited(status: u16, retry_after: Option<&str>) -> Result<HttpResponse, AdapterError> {
        let mut r = HttpResponse::with_status(status, "slow down");
        if let Some(v) = retry_after {
            r.headers.push(("Retry-After".into(), v.into()));
        }
        Ok(r)
    }

    fn with_weight(used: &str) -> Result<HttpResponse, AdapterError> {
        let mut r = HttpResponse::ok("{}");
        r.headers.push(("x-mbx-used-weight-1m".into(), used.into()));
        Ok(r)
    }

    #[test]
    fn scenario_429_with_retry_after_5_sends_nothing_for_5_seconds_then_a_probe_goes_out() {
        let r = rig(Exchange::Binance, FakeTransport::new().on("premiumIndex", limited(429, Some("5"))).on("premiumIndex", Ok(HttpResponse::ok("{}"))));
        assert_eq!(get(&r, BATCH), Err(AdapterError::RateLimited { retry_after_ms: Some(5_000) }));
        assert_eq!(r.fake.requests().len(), 1);
        r.clock.advance(4_999);
        assert_eq!(get(&r, BATCH), Err(AdapterError::RateLimited { retry_after_ms: Some(1) }), "remaining wait is reported");
        assert_eq!(r.fake.requests().len(), 1, "zero new requests inside the window");
        r.clock.advance(1);
        assert!(get(&r, BATCH).is_ok());
        assert_eq!(r.fake.requests().len(), 2, "after the wait one request is allowed through");
    }

    #[test]
    fn status_418_without_header_starts_the_minimum_backoff() {
        let r = rig(Exchange::Binance, FakeTransport::new().on("premiumIndex", limited(418, None)));
        assert_eq!(get(&r, BATCH), Err(AdapterError::RateLimited { retry_after_ms: Some(1_000) }));
        assert!(matches!(get(&r, BATCH), Err(AdapterError::RateLimited { .. })));
        assert_eq!(r.fake.requests().len(), 1);
    }

    #[test]
    fn repeated_429_without_header_backs_off_exponentially_through_the_wrapper() {
        let r = rig(Exchange::Bybit, FakeTransport::new().on("tickers", limited(429, None)));
        let url = "https://pub.example/v5/market/tickers?category=linear";
        let mut waits = Vec::new();
        for _ in 0..3 {
            match get(&r, url) {
                Err(AdapterError::RateLimited { retry_after_ms: Some(w) }) => {
                    waits.push(w);
                    r.clock.advance(w as i64);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(waits, vec![1_000, 2_000, 4_000]);
        assert_eq!(r.fake.requests().len(), 3);
    }

    #[test]
    fn success_resets_the_backoff() {
        let r = rig(
            Exchange::Okx,
            FakeTransport::new().on("funding-rate", limited(429, None)).on("funding-rate", Ok(HttpResponse::ok("{}"))).on("funding-rate", limited(429, None)),
        );
        let url = "https://pub.example/api/v5/public/funding-rate?instId=ANY";
        assert!(get(&r, url).is_err());
        r.clock.advance(1_000);
        assert!(get(&r, url).is_ok(), "probe succeeds");
        assert_eq!(get(&r, url), Err(AdapterError::RateLimited { retry_after_ms: Some(1_000) }), "next failure starts from the minimum, not 2 s");
    }

    #[test]
    fn a_huge_retry_after_locks_for_at_most_one_hour() {
        let r = rig(Exchange::Binance, FakeTransport::new().on("premiumIndex", limited(429, Some("18446744073709551615"))).on("premiumIndex", Ok(HttpResponse::ok("{}"))));
        assert_eq!(get(&r, BATCH), Err(AdapterError::RateLimited { retry_after_ms: Some(RETRY_AFTER_CAP_MS) }));
        r.clock.advance(RETRY_AFTER_CAP_MS as i64);
        assert!(get(&r, BATCH).is_ok());
    }

    #[test]
    fn http_date_retry_after_is_resolved_with_the_injected_wall_clock() {
        // wall clock = 2026-10-21 07:27:55 UTC; header says 07:28:00 => 5 s
        let r = rig(Exchange::Binance, FakeTransport::new().on("premiumIndex", limited(429, Some("Wed, 21 Oct 2026 07:28:00 GMT"))));
        r.clock.set(1_792_567_675_000);
        assert_eq!(get(&r, BATCH), Err(AdapterError::RateLimited { retry_after_ms: Some(5_000) }));
    }

    #[test]
    fn backoff_of_one_class_does_not_block_another_class() {
        let r = rig(Exchange::Binance, FakeTransport::new().on("symbol=", Ok(HttpResponse::ok("{}"))).on("premiumIndex", limited(429, Some("30"))));
        assert!(get(&r, BATCH).is_err());
        assert!(get(&r, SINGLE).is_ok(), "the pre-order single-symbol re-fetch is a different class");
        assert_eq!(r.fake.requests().len(), 2);
    }

    #[test]
    fn scenario_weight_at_80_percent_defers_batch_but_single_and_signed_still_go() {
        let signed = format!("https://{BINANCE_TESTNET_HOST}/fapi/v2/balance?timestamp=1");
        let r = rig(Exchange::Binance, FakeTransport::new().on("exchangeInfo", with_weight("1950")).on("premiumIndex", Ok(HttpResponse::ok("{}"))).on("fapi/v2/balance", Ok(HttpResponse::ok("{}"))));
        r.weight.set_limit(2400);
        assert!(get(&r, "https://pub.example/fapi/v1/exchangeInfo").is_ok(), "this response reports weight 1950");
        let before = r.fake.requests().len();
        assert!(matches!(get(&r, BATCH), Err(AdapterError::RateLimited { .. })), "batch deferred");
        assert_eq!(r.fake.requests().len(), before, "no request for a deferred batch");
        assert!(get(&r, SINGLE).is_ok(), "single-symbol re-fetch is exempt");
        assert!(get(&r, &signed).is_ok(), "signed reads are exempt");
        r.clock.advance(60_000);
        assert!(get(&r, BATCH).is_ok(), "the weight window rolled over: batch resumes");
    }

    #[test]
    fn weight_headers_from_other_exchanges_are_ignored() {
        let r = rig(Exchange::Bybit, FakeTransport::new().on("tickers", with_weight("2399")));
        r.weight.set_limit(2400);
        let url = "https://pub.example/v5/market/tickers?category=linear";
        assert!(get(&r, url).is_ok());
        assert!(get(&r, url).is_ok(), "x-mbx-used-weight-1m means nothing for Bybit");
    }

    #[test]
    fn a_rate_limit_inside_a_200_body_also_starts_a_backoff() {
        let body = r#"{"retCode":10006,"retMsg":"Too many visits!","result":{},"time":1}"#;
        let r = rig(Exchange::Bybit, FakeTransport::new().on("tickers", Ok(HttpResponse::ok(body))));
        let url = "https://pub.example/v5/market/tickers?category=linear";
        assert_eq!(get(&r, url), Err(AdapterError::RateLimited { retry_after_ms: Some(1_000) }));
        assert!(get(&r, url).is_err());
        assert_eq!(r.fake.requests().len(), 1);
    }

    #[test]
    fn ordinary_responses_and_transport_errors_pass_through_untouched() {
        let r = rig(
            Exchange::Binance,
            FakeTransport::new()
                .on("premiumIndex", Ok(HttpResponse::with_status(500, "boom")))
                .on("premiumIndex", Err(AdapterError::Timeout))
                .on("premiumIndex", Ok(HttpResponse::ok("{\"a\":1}"))),
        );
        assert_eq!(get(&r, BATCH).unwrap().status, 500, "5xx is the caller's business, not a rate limit");
        assert_eq!(get(&r, BATCH), Err(AdapterError::Timeout));
        assert_eq!(get(&r, BATCH).unwrap().body, "{\"a\":1}");
        assert_eq!(r.fake.requests().len(), 3, "nothing was blocked");
    }
}

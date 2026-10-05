//! Rate-limit handling (spec: feed-health, "限流時遵守 Retry-After 並以退避重試"):
//! `Retry-After` / exponential back-off state machine per (exchange, request class), and the
//! Binance used-weight gate that defers non-essential batch polling at 80% of the limit.
//! Time comes only from the injected `Clock`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tong_funding_core::types::Exchange;

use crate::exchange::error::AdapterError;
use crate::exchange::transport::HttpResponse;
use crate::ports::TimeSource;

/// First back-off wait without `Retry-After` (proposal, unverified).
pub const BACKOFF_BASE_MS: u64 = 1_000;
/// Upper bound of the exponential wait (proposal, unverified). `Retry-After` itself is never capped.
pub const BACKOFF_CAP_MS: u64 = 60_000;
/// Defer batch polling at this share of the weight limit (spec: 80%).
pub const WEIGHT_DEFER_PERCENT: u64 = 80;
/// Binance's weight window is one minute; a reading older than this no longer describes the
/// current window (otherwise a deferral would never lift, since no response arrives to update it).
pub const WEIGHT_WINDOW_MS: i64 = 60_000;
pub const BINANCE_USED_WEIGHT_HEADER: &str = "x-mbx-used-weight-1m";
/// Longest wait honoured from any `Retry-After` / back-off input (1 hour). Larger values, including
/// `u64::MAX`, are clamped to this: a hostile or broken header must not lock an exchange forever.
pub const RETRY_AFTER_CAP_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestClass {
    /// Whole-market polling (`premiumIndex`, `tickers`, ...). Deferrable.
    Batch,
    /// Single-symbol re-fetch before an order. Never deferred by the weight gate.
    Single,
    /// Signed account reads.
    Signed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffState {
    Clear,
    Waiting { until_ms: i64, remaining_ms: i64, consecutive: u32 },
}

/// `Retry-After: <seconds>` in milliseconds, clamped to [`RETRY_AFTER_CAP_MS`] (all-digit values
/// that overflow `u64` are clamped too). Anything else, including the HTTP-date form (which needs a
/// clock: see [`parse_retry_after_at`]), is `None`.
pub fn parse_retry_after_ms(value: &str) -> Option<u64> {
    let v = value.trim();
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let secs = v.parse::<u64>().unwrap_or(u64::MAX);
    Some(secs.saturating_mul(1_000).min(RETRY_AFTER_CAP_MS))
}

/// `Retry-After` as seconds or as an HTTP-date, in ms, clamped to [`RETRY_AFTER_CAP_MS`].
/// An HTTP-date is turned into a delay against `now_wall_ms` (a date in the past is `Some(0)`).
/// Negative, fractional or non-numeric input is `None`. This is the one definition both the public
/// and the signed clients should call.
pub fn parse_retry_after_at(value: &str, now_wall_ms: i64) -> Option<u64> {
    if let Some(ms) = parse_retry_after_ms(value) {
        return Some(ms);
    }
    let when = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?.timestamp_millis();
    let delay = when.saturating_sub(now_wall_ms).max(0) as u64;
    Some(delay.min(RETRY_AFTER_CAP_MS))
}

/// Like [`rate_limit_error`] but also understands the HTTP-date form of `Retry-After`.
pub fn rate_limit_error_at(resp: &HttpResponse, now_wall_ms: i64) -> Option<AdapterError> {
    if matches!(resp.status, 429 | 418) {
        let retry_after_ms = resp.header_value("retry-after").and_then(|v| parse_retry_after_at(v, now_wall_ms));
        Some(AdapterError::RateLimited { retry_after_ms })
    } else {
        None
    }
}

/// Rate-limit signals that arrive inside an HTTP 200 / non-429 body: Binance `-1003`,
/// Bybit retCode `10006` / `10018`, OKX code `50011` / `50013`. `None` for anything else.
pub fn classify_exchange_body(exchange: Exchange, body: &str) -> Option<AdapterError> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let limited = match exchange {
        Exchange::Binance => v.get("code").and_then(serde_json::Value::as_i64) == Some(-1003),
        Exchange::Bybit => matches!(v.get("retCode").and_then(serde_json::Value::as_i64), Some(10006 | 10018)),
        Exchange::Okx => matches!(v.get("code").and_then(serde_json::Value::as_str), Some("50011" | "50013")),
    };
    limited.then_some(AdapterError::RateLimited { retry_after_ms: None })
}

/// Request class derived from the URL (pure): a signed (demo/testnet) host is `Signed`; a public
/// request naming one symbol (`symbol=`, or `instId=` other than `ANY`) is `Single`; the rest is `Batch`.
pub fn classify_request(url: &str) -> RequestClass {
    let Some((_, rest)) = url.split_once("://") else { return RequestClass::Batch };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    // The real host is after any `user@`, before any `:port`.
    let host_port = rest[..authority_end].rsplit('@').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("").to_ascii_lowercase();
    if crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS.contains(&host.as_str()) {
        return RequestClass::Signed;
    }
    let query = rest.find('?').map_or("", |q| rest[q + 1..].split('#').next().unwrap_or(""));
    let single = query.split('&').filter_map(|kv| kv.split_once('=')).any(|(k, v)| k == "symbol" || (k == "instId" && !v.eq_ignore_ascii_case("ANY")));
    if single { RequestClass::Single } else { RequestClass::Batch }
}

/// 429 and Binance's 418 become `RateLimited`; any other status is not a rate limit.
pub fn rate_limit_error(resp: &HttpResponse) -> Option<AdapterError> {
    if matches!(resp.status, 429 | 418) {
        let retry_after_ms = resp.header_value("retry-after").and_then(parse_retry_after_ms);
        Some(AdapterError::RateLimited { retry_after_ms })
    } else {
        None
    }
}

/// Back-off for one (exchange, class).
pub struct Backoff {
    clock: Arc<dyn TimeSource>,
    inner: Mutex<BackoffInner>,
}

#[derive(Default)]
struct BackoffInner {
    consecutive: u32,
    until_ms: Option<i64>,
}

impl Backoff {
    pub fn new(clock: Arc<dyn TimeSource>) -> Self {
        Self { clock, inner: Mutex::new(BackoffInner::default()) }
    }

    /// Records a rate-limited response; returns the wait in ms.
    pub fn on_rate_limited(&self, retry_after_ms: Option<u64>) -> u64 {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.consecutive = inner.consecutive.saturating_add(1);
        let exponent = (inner.consecutive - 1).min(32);
        let exponential = BACKOFF_BASE_MS.saturating_mul(1u64 << exponent).min(BACKOFF_CAP_MS);
        let wait = exponential.max(retry_after_ms.unwrap_or(0).min(RETRY_AFTER_CAP_MS));
        inner.until_ms = Some(self.clock.mono_ms().saturating_add(i64::try_from(wait).unwrap_or(i64::MAX)));
        wait
    }

    /// A successful request resets the back-off.
    pub fn on_success(&self) {
        *self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = BackoffInner::default();
    }

    pub fn allow_request(&self) -> bool {
        matches!(self.state(), BackoffState::Clear)
    }

    pub fn state(&self) -> BackoffState {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = self.clock.mono_ms();
        match inner.until_ms {
            Some(until_ms) if until_ms > now => BackoffState::Waiting { until_ms, remaining_ms: until_ms - now, consecutive: inner.consecutive },
            _ => BackoffState::Clear,
        }
    }
}

/// Back-offs keyed by (exchange, class): a 429 on one class never blocks another exchange.
pub struct RateLimiter {
    clock: Arc<dyn TimeSource>,
    map: Mutex<HashMap<(Exchange, RequestClass), Arc<Backoff>>>,
}

impl RateLimiter {
    pub fn new(clock: Arc<dyn TimeSource>) -> Self {
        Self { clock, map: Mutex::new(HashMap::new()) }
    }

    fn entry(&self, ex: Exchange, class: RequestClass) -> Arc<Backoff> {
        let mut m = self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        m.entry((ex, class)).or_insert_with(|| Arc::new(Backoff::new(self.clock.clone()))).clone()
    }

    pub fn on_rate_limited(&self, ex: Exchange, class: RequestClass, retry_after_ms: Option<u64>) -> u64 {
        self.entry(ex, class).on_rate_limited(retry_after_ms)
    }
    pub fn on_success(&self, ex: Exchange, class: RequestClass) {
        self.entry(ex, class).on_success()
    }
    pub fn allow_request(&self, ex: Exchange, class: RequestClass) -> bool {
        self.entry(ex, class).allow_request()
    }
    pub fn state(&self, ex: Exchange, class: RequestClass) -> BackoffState {
        self.entry(ex, class).state()
    }
}

/// Binance used-weight gate. The limit comes from `exchangeInfo.rateLimits`, never hard-coded.
pub struct WeightGate {
    clock: Arc<dyn TimeSource>,
    inner: Mutex<WeightInner>,
}

#[derive(Default)]
struct WeightInner {
    limit: Option<u64>,
    used: Option<(u64, i64)>,
}

impl WeightGate {
    pub fn new(clock: Arc<dyn TimeSource>) -> Self {
        Self { clock, inner: Mutex::new(WeightInner::default()) }
    }

    /// Reads the `REQUEST_WEIGHT` / `MINUTE` limit from an `exchangeInfo` body.
    pub fn set_limit_from_exchange_info(&self, body: &str) -> Result<u64, AdapterError> {
        let v: serde_json::Value = serde_json::from_str(body).map_err(|e| AdapterError::parse(format!("exchangeInfo is not JSON: {e}")))?;
        let limit = v
            .get("rateLimits")
            .and_then(serde_json::Value::as_array)
            .and_then(|a| {
                a.iter().find(|r| {
                    r.get("rateLimitType").and_then(|t| t.as_str()) == Some("REQUEST_WEIGHT")
                        && r.get("interval").and_then(|t| t.as_str()) == Some("MINUTE")
                        && r.get("intervalNum").and_then(serde_json::Value::as_u64) == Some(1)
                })
            })
            .and_then(|r| r.get("limit").and_then(serde_json::Value::as_u64))
            .filter(|l| *l > 0)
            .ok_or_else(|| AdapterError::parse("no REQUEST_WEIGHT/MINUTE limit in exchangeInfo.rateLimits"))?;
        self.set_limit(limit);
        Ok(limit)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WeightInner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_limit(&self, limit: u64) {
        self.lock().limit = Some(limit);
    }

    /// Records `x-mbx-used-weight-1m` from a response, if present and numeric.
    pub fn observe_response(&self, resp: &HttpResponse) {
        if let Some(used) = resp.header_value(BINANCE_USED_WEIGHT_HEADER).and_then(|v| v.trim().parse::<u64>().ok()) {
            self.observe_used(used);
        }
    }

    pub fn observe_used(&self, used: u64) {
        let now = self.clock.mono_ms();
        self.lock().used = Some((used, now));
    }

    pub fn limit(&self) -> Option<u64> {
        self.lock().limit
    }

    /// True when the (still current) used weight is at or above 80% of the limit.
    pub fn batch_deferred(&self) -> bool {
        let inner = self.lock();
        match (inner.limit, inner.used) {
            (Some(limit), Some((used, at))) if self.clock.mono_ms() - at < WEIGHT_WINDOW_MS => {
                used.saturating_mul(100) >= limit.saturating_mul(WEIGHT_DEFER_PERCENT)
            }
            _ => false,
        }
    }

    /// How long until the used-weight reading expires (the earliest the deferral can lift on its own).
    pub fn defer_remaining_ms(&self) -> Option<u64> {
        if !self.batch_deferred() {
            return None;
        }
        let at = self.lock().used.map(|(_, at)| at)?;
        Some((WEIGHT_WINDOW_MS - (self.clock.mono_ms() - at)).max(0) as u64)
    }

    /// Batch polling is deferred near the limit; single-symbol and signed requests never are.
    pub fn allow(&self, class: RequestClass) -> bool {
        class != RequestClass::Batch || !self.batch_deferred()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::ManualClock;

    fn backoff(clock: &ManualClock) -> Backoff {
        Backoff::new(Arc::new(clock.clone()))
    }

    #[test]
    fn retry_after_header_is_parsed_as_seconds() {
        assert_eq!(parse_retry_after_ms("5"), Some(5_000));
        assert_eq!(parse_retry_after_ms(" 120 "), Some(120_000));
        assert_eq!(parse_retry_after_ms("0"), Some(0));
        assert_eq!(parse_retry_after_ms("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after_ms(""), None);
        assert_eq!(parse_retry_after_ms("-3"), None);
    }

    #[test]
    fn status_429_and_418_become_rate_limited_with_retry_after() {
        let mut r = HttpResponse::with_status(429, "x");
        r.headers.push(("Retry-After".into(), "5".into()));
        assert_eq!(rate_limit_error(&r), Some(AdapterError::RateLimited { retry_after_ms: Some(5_000) }));
        assert_eq!(rate_limit_error(&HttpResponse::with_status(418, "")), Some(AdapterError::RateLimited { retry_after_ms: None }));
        assert_eq!(rate_limit_error(&HttpResponse::with_status(500, "")), None);
        assert_eq!(rate_limit_error(&HttpResponse::ok("")), None);
    }

    #[test]
    fn scenario_429_with_retry_after_blocks_for_five_seconds() {
        let clock = ManualClock::new(100_000);
        let b = backoff(&clock);
        assert!(b.allow_request());
        assert_eq!(b.on_rate_limited(Some(5_000)), 5_000);
        assert!(!b.allow_request());
        clock.advance(4_999);
        assert!(!b.allow_request(), "still inside the 5 s");
        assert_eq!(b.state(), BackoffState::Waiting { until_ms: 105_000, remaining_ms: 1, consecutive: 1 });
        clock.advance(1);
        assert!(b.allow_request());
        assert_eq!(b.state(), BackoffState::Clear, "an elapsed wait is no longer reported as waiting");
    }

    #[test]
    fn scenario_three_429s_without_retry_after_grow_exponentially_up_to_the_cap() {
        let clock = ManualClock::new(0);
        let b = backoff(&clock);
        let waits: Vec<u64> = (0..3).map(|_| b.on_rate_limited(None)).collect();
        assert_eq!(waits, vec![1_000, 2_000, 4_000]);
        for _ in 0..20 {
            assert!(b.on_rate_limited(None) <= BACKOFF_CAP_MS);
        }
        assert_eq!(b.on_rate_limited(None), BACKOFF_CAP_MS);
    }

    #[test]
    fn retry_after_is_a_minimum_not_a_replacement_for_a_longer_backoff() {
        let clock = ManualClock::new(0);
        let b = backoff(&clock);
        for _ in 0..3 {
            b.on_rate_limited(None);
        }
        // 4th consecutive failure: exponential = 8 s; a Retry-After of 1 s must not shorten it.
        assert_eq!(b.on_rate_limited(Some(1_000)), 8_000);
        // A long Retry-After is honoured even above the exponential cap.
        assert_eq!(b.on_rate_limited(Some(300_000)), 300_000);
    }

    #[test]
    fn scenario_success_resets_backoff_so_next_failure_starts_from_the_minimum() {
        let clock = ManualClock::new(0);
        let b = backoff(&clock);
        b.on_rate_limited(None);
        b.on_rate_limited(None);
        b.on_rate_limited(None);
        clock.advance(10_000);
        assert!(b.allow_request());
        b.on_success();
        assert_eq!(b.state(), BackoffState::Clear);
        assert_eq!(b.on_rate_limited(None), BACKOFF_BASE_MS);
    }

    #[test]
    fn backoff_is_per_exchange_and_per_class() {
        let clock = ManualClock::new(0);
        let rl = RateLimiter::new(Arc::new(clock.clone()));
        rl.on_rate_limited(Exchange::Bybit, RequestClass::Batch, Some(5_000));
        assert!(!rl.allow_request(Exchange::Bybit, RequestClass::Batch));
        assert!(rl.allow_request(Exchange::Bybit, RequestClass::Signed), "other class unaffected");
        assert!(rl.allow_request(Exchange::Binance, RequestClass::Batch), "other exchange unaffected");
        assert!(matches!(rl.state(Exchange::Bybit, RequestClass::Batch), BackoffState::Waiting { remaining_ms: 5_000, .. }));
        clock.advance(5_000);
        rl.on_success(Exchange::Bybit, RequestClass::Batch);
        assert!(rl.allow_request(Exchange::Bybit, RequestClass::Batch));
    }

    const EXCHANGE_INFO: &str = r#"{"timezone":"UTC","rateLimits":[
        {"rateLimitType":"REQUEST_WEIGHT","interval":"MINUTE","intervalNum":1,"limit":2400},
        {"rateLimitType":"ORDERS","interval":"MINUTE","intervalNum":1,"limit":1200},
        {"rateLimitType":"ORDERS","interval":"SECOND","intervalNum":10,"limit":300}],"symbols":[]}"#;

    fn weight_resp(used: &str) -> HttpResponse {
        let mut r = HttpResponse::ok("{}");
        r.headers.push(("X-MBX-USED-WEIGHT-1M".into(), used.into()));
        r
    }

    #[test]
    fn limit_comes_from_exchange_info_not_a_constant() {
        let clock = ManualClock::new(0);
        let g = WeightGate::new(Arc::new(clock));
        assert_eq!(g.limit(), None);
        assert_eq!(g.set_limit_from_exchange_info(EXCHANGE_INFO), Ok(2400));
        assert_eq!(g.limit(), Some(2400));
        assert!(g.set_limit_from_exchange_info(r#"{"rateLimits":[]}"#).is_err());
        assert!(g.set_limit_from_exchange_info("nope").is_err());
        assert_eq!(g.limit(), Some(2400), "a failed parse keeps the known limit");
    }

    #[test]
    fn scenario_weight_1950_of_2400_defers_batch_but_not_single_symbol_refetch() {
        let clock = ManualClock::new(0);
        let g = WeightGate::new(Arc::new(clock));
        g.set_limit_from_exchange_info(EXCHANGE_INFO).unwrap();
        g.observe_response(&weight_resp("1950"));
        assert!(g.batch_deferred());
        assert!(!g.allow(RequestClass::Batch));
        assert!(g.allow(RequestClass::Single), "pre-order re-fetch is essential");
        assert!(g.allow(RequestClass::Signed));
    }

    #[test]
    fn deferral_starts_exactly_at_eighty_percent() {
        let clock = ManualClock::new(0);
        let g = WeightGate::new(Arc::new(clock));
        g.set_limit(2400);
        g.observe_used(1919);
        assert!(!g.batch_deferred());
        g.observe_used(1920);
        assert!(g.batch_deferred());
    }

    #[test]
    fn deferral_lifts_when_weight_drops_or_the_window_has_rolled() {
        let clock = ManualClock::new(0);
        let g = WeightGate::new(Arc::new(clock.clone()));
        g.set_limit(2400);
        g.observe_used(2000);
        assert!(g.batch_deferred());
        clock.advance(WEIGHT_WINDOW_MS - 1);
        assert!(g.batch_deferred());
        clock.advance(1);
        assert!(!g.batch_deferred(), "no response for a full window: the reading is stale, polling may resume");
        g.observe_used(2000);
        g.observe_used(100);
        assert!(!g.batch_deferred());
    }

    #[test]
    fn unknown_limit_or_missing_header_never_defers() {
        let clock = ManualClock::new(0);
        let g = WeightGate::new(Arc::new(clock));
        g.observe_response(&weight_resp("99999"));
        assert!(!g.batch_deferred(), "limit unknown");
        g.set_limit(2400);
        g.observe_response(&HttpResponse::ok("{}"));
        g.observe_response(&weight_resp("garbage"));
        assert!(g.batch_deferred(), "earlier valid reading (99999) still stands; garbage is ignored");
    }

    // ------------------------------------------------ round 2

    /// 2026-10-21 07:28:00 UTC in ms.
    const DATE_MS: i64 = 1_792_567_680_000;
    const DATE_STR: &str = "Wed, 21 Oct 2026 07:28:00 GMT";

    #[test]
    fn retry_after_is_capped_at_one_hour_including_u64_max() {
        assert_eq!(parse_retry_after_at("3600", 0), Some(3_600_000));
        assert_eq!(parse_retry_after_at("3601", 0), Some(RETRY_AFTER_CAP_MS));
        assert_eq!(parse_retry_after_at("99999999", 0), Some(RETRY_AFTER_CAP_MS));
        assert_eq!(parse_retry_after_at("18446744073709551615", 0), Some(RETRY_AFTER_CAP_MS), "u64::MAX seconds");
        assert_eq!(parse_retry_after_ms("18446744073709551615"), Some(RETRY_AFTER_CAP_MS));
    }

    #[test]
    fn retry_after_garbage_is_none_not_a_panic_or_zero() {
        for v in ["-1", "-0", "abc", "1.5", "", "  ", "5s", "1e3", "99999999999999999999999"] {
            let got = parse_retry_after_at(v, 0);
            // the last one overflows u64 but is still all digits: clamp, never None-by-accident
            if v == "99999999999999999999999" {
                assert_eq!(got, Some(RETRY_AFTER_CAP_MS), "{v}");
            } else {
                assert_eq!(got, None, "{v}");
            }
        }
    }

    #[test]
    fn retry_after_http_date_is_a_delay_against_the_injected_wall_time() {
        assert_eq!(parse_retry_after_at(DATE_STR, DATE_MS - 5_000), Some(5_000));
        assert_eq!(parse_retry_after_at(DATE_STR, DATE_MS + 1), Some(0), "date already passed");
        assert_eq!(parse_retry_after_at(DATE_STR, DATE_MS - 10 * 3_600_000), Some(RETRY_AFTER_CAP_MS), "capped");
        assert_eq!(parse_retry_after_at("Wed, 99 Foo 2026 07:28:00 GMT", 0), None);
        assert_eq!(parse_retry_after_ms(DATE_STR), None, "no clock, no date arithmetic");
    }

    #[test]
    fn rate_limit_error_at_reads_the_date_form() {
        let mut r = HttpResponse::with_status(429, "");
        r.headers.push(("Retry-After".into(), DATE_STR.into()));
        assert_eq!(rate_limit_error_at(&r, DATE_MS - 7_000), Some(AdapterError::RateLimited { retry_after_ms: Some(7_000) }));
        let mut huge = HttpResponse::with_status(418, "");
        huge.headers.push(("Retry-After".into(), "18446744073709551615".into()));
        assert_eq!(rate_limit_error_at(&huge, 0), Some(AdapterError::RateLimited { retry_after_ms: Some(RETRY_AFTER_CAP_MS) }));
    }

    #[test]
    fn backoff_clamps_a_huge_retry_after_and_unlocks_after_the_cap() {
        let clock = ManualClock::new(0);
        let b = backoff(&clock);
        assert_eq!(b.on_rate_limited(Some(u64::MAX)), RETRY_AFTER_CAP_MS);
        clock.advance(RETRY_AFTER_CAP_MS as i64 - 1);
        assert!(!b.allow_request());
        clock.advance(1);
        assert!(b.allow_request(), "a probe request must be let through once the capped wait is over");
    }

    #[test]
    fn a_hundred_consecutive_failures_still_unlock_within_the_exponential_cap() {
        let clock = ManualClock::new(0);
        let b = backoff(&clock);
        for _ in 0..100 {
            b.on_rate_limited(None);
        }
        clock.advance(BACKOFF_CAP_MS as i64);
        assert!(b.allow_request());
    }

    #[test]
    fn backoff_windows_use_monotonic_time_so_a_wall_step_back_cannot_extend_them() {
        let clock = ManualClock::new(10_000_000);
        let b = backoff(&clock);
        b.on_rate_limited(Some(5_000));
        clock.jump_wall(-3_600_000);
        clock.advance(5_000);
        assert!(b.allow_request());
    }

    #[test]
    fn exchange_bodies_that_mean_rate_limited() {
        let limited = |ex, body: &str| classify_exchange_body(ex, body) == Some(AdapterError::RateLimited { retry_after_ms: None });
        assert!(limited(Exchange::Bybit, r#"{"retCode":10006,"retMsg":"Too many visits!","result":{},"time":1}"#));
        assert!(limited(Exchange::Bybit, r#"{"retCode":10018,"retMsg":"Exceeded the IP Rate Limit","time":1}"#));
        assert!(limited(Exchange::Okx, r#"{"code":"50011","msg":"Request too frequent","data":[]}"#));
        assert!(limited(Exchange::Okx, r#"{"code":"50013","msg":"System is busy","data":[]}"#));
        assert!(limited(Exchange::Binance, r#"{"code":-1003,"msg":"Too many requests; current limit of IP(s) is 2400 requests per minute."}"#));
    }

    #[test]
    fn exchange_bodies_that_are_not_rate_limits_are_left_alone() {
        assert_eq!(classify_exchange_body(Exchange::Bybit, r#"{"retCode":0,"retMsg":"OK","result":{}}"#), None);
        assert_eq!(classify_exchange_body(Exchange::Bybit, r#"{"retCode":10001,"retMsg":"bad param"}"#), None);
        assert_eq!(classify_exchange_body(Exchange::Okx, r#"{"code":"0","data":[]}"#), None);
        assert_eq!(classify_exchange_body(Exchange::Okx, r#"{"code":"51000","msg":"param"}"#), None);
        assert_eq!(classify_exchange_body(Exchange::Binance, r#"{"code":-1021,"msg":"recvWindow"}"#), None);
        assert_eq!(classify_exchange_body(Exchange::Binance, "[1,2,3]"), None);
        assert_eq!(classify_exchange_body(Exchange::Bybit, "not json"), None);
        // a code from another exchange is not this exchange's rate limit
        assert_eq!(classify_exchange_body(Exchange::Bybit, r#"{"code":"50011"}"#), None);
        assert_eq!(classify_exchange_body(Exchange::Okx, r#"{"retCode":10006}"#), None);
    }

    #[test]
    fn requests_are_classified_by_url() {
        use crate::exchange::signed::endpoints::{BINANCE_DEMO_HOST, BINANCE_TESTNET_HOST, BYBIT_DEMO_HOST};
        let c = classify_request;
        assert_eq!(c(&format!("https://{BINANCE_TESTNET_HOST}/fapi/v2/balance?timestamp=1&signature=x")), RequestClass::Signed);
        assert_eq!(c(&format!("https://{BINANCE_DEMO_HOST}/fapi/v1/openOrders")), RequestClass::Signed);
        assert_eq!(c(&format!("https://{BYBIT_DEMO_HOST}/v5/position/list?category=linear")), RequestClass::Signed);
        assert_eq!(c(&format!("HTTPS://{}/x", BINANCE_TESTNET_HOST.to_uppercase())), RequestClass::Signed, "host case");
        assert_eq!(c(&format!("https://{BINANCE_TESTNET_HOST}:443/x?symbol=BTCUSDT")), RequestClass::Signed, "port, and Signed beats symbol=");
        assert_eq!(c("https://pub.example/fapi/v1/premiumIndex?symbol=BTCUSDT"), RequestClass::Single);
        assert_eq!(c("https://pub.example/v5/market/tickers?category=linear&symbol=BTCUSDT"), RequestClass::Single);
        assert_eq!(c("https://pub.example/fapi/v1/premiumIndex"), RequestClass::Batch);
        assert_eq!(c("https://pub.example/v5/market/tickers?category=linear"), RequestClass::Batch);
        assert_eq!(c("https://pub.example/api/v5/public/funding-rate?instId=BTC-USDT-SWAP"), RequestClass::Single);
        assert_eq!(c("https://pub.example/api/v5/public/funding-rate?instId=ANY"), RequestClass::Batch);
        assert_eq!(c("https://pub.example/x?notsymbol=1"), RequestClass::Batch, "parameter name must match exactly");
        assert_eq!(c("https://pub.example/x?a=1&symbol=ETHUSDT&b=2"), RequestClass::Single);
        assert_eq!(c("not a url"), RequestClass::Batch);
    }

    #[test]
    fn userinfo_tricks_do_not_make_a_public_url_look_signed() {
        use crate::exchange::signed::endpoints::BINANCE_TESTNET_HOST;
        let url = format!("https://{BINANCE_TESTNET_HOST}@pub.example/fapi/v1/premiumIndex");
        assert_eq!(classify_request(&url), RequestClass::Batch);
    }
}

//! Clock offset against each exchange's server time (spec: feed-health, "serverTime 校時").
//! The offset maths is a pure function; `ClockSync` only adds state, and every "now" comes from
//! the injected `Clock`. An exchange that never synced is `Unsynced` and must not be signed for.
#![allow(dead_code)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tong_funding_core::types::Exchange;

use crate::exchange::error::AdapterError;
use crate::exchange::transport::{HttpRequest, HttpTransport};
use crate::ports::TimeSource;

/// Server-time paths (host is supplied by the caller; hosts live in the endpoint modules).
pub const BINANCE_TIME_PATH: &str = "/fapi/v1/time";
pub const BYBIT_TIME_PATH: &str = "/v5/market/time";
pub const OKX_TIME_PATH: &str = "/api/v5/public/time";

/// Re-sync period. Spec says only "定期"; 5 minutes is this implementation's proposal (unverified).
pub const RESYNC_INTERVAL_MS: i64 = 300_000;
/// Time-sync request timeout (proposal, unverified).
pub const SYNC_TIMEOUT: Duration = Duration::from_secs(2);
/// A sample older than this (monotonic time) no longer counts: the state becomes `Unsynced`.
pub const MAX_SAMPLE_AGE_MS: i64 = 3_600_000;
/// Samples claiming a clock error beyond one day are rejected as garbage.
pub const MAX_ABS_OFFSET_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSample {
    /// exchange time − local time (ms); add to local time to get exchange time.
    pub offset_ms: i64,
    pub rtt_ms: i64,
    /// Local time at which the response arrived.
    pub synced_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockStatus {
    /// Never synced: signed requests must not be sent.
    Unsynced,
    Synced { offset_ms: i64, rtt_ms: i64, age_ms: i64 },
}

/// offset = exchange − (sent + received) / 2, rtt = received − sent. All arithmetic is checked;
/// a sample is rejected if the exchange time is not positive, the RTT is negative or the offset
/// exceeds one day in either direction.
pub fn compute_sample(sent_ms: i64, received_ms: i64, exchange_ms: i64) -> Result<ClockSample, AdapterError> {
    let bad = |why: &str| AdapterError::parse(format!("rejected clock sample: {why}"));
    if exchange_ms <= 0 {
        return Err(bad("server time is not positive"));
    }
    let rtt_ms = received_ms.checked_sub(sent_ms).filter(|r| *r >= 0).ok_or_else(|| bad("negative or overflowing RTT"))?;
    // sent + rtt / 2 cannot overflow once rtt is known to be non-negative and received is valid.
    let midpoint = sent_ms.checked_add(rtt_ms / 2).ok_or_else(|| bad("midpoint overflow"))?;
    let offset_ms = exchange_ms.checked_sub(midpoint).filter(|o| o.abs() <= MAX_ABS_OFFSET_MS).ok_or_else(|| bad("offset beyond one day"))?;
    Ok(ClockSample { offset_ms, rtt_ms, synced_at_ms: received_ms })
}

/// Extracts the exchange's server time (ms) from its time endpoint body.
pub fn parse_server_time(exchange: Exchange, body: &str) -> Result<i64, AdapterError> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| AdapterError::parse(format!("time response is not JSON: {e}")))?;
    let as_ms = |x: &serde_json::Value| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()));
    let found = match exchange {
        Exchange::Binance => v.get("serverTime").and_then(as_ms),
        Exchange::Bybit => {
            if v.get("retCode").and_then(serde_json::Value::as_i64) != Some(0) {
                return Err(AdapterError::exchange(v.get("retCode").map(|c| c.to_string()).unwrap_or_default(), "time endpoint reported failure"));
            }
            v.get("time").and_then(as_ms)
        }
        Exchange::Okx => {
            if v.get("code").and_then(serde_json::Value::as_str) != Some("0") {
                return Err(AdapterError::exchange(v.get("code").map(|c| c.to_string()).unwrap_or_default(), "time endpoint reported failure"));
            }
            v.get("data").and_then(|d| d.get(0)).and_then(|d| d.get("ts")).and_then(as_ms)
        }
    };
    found.ok_or_else(|| AdapterError::parse("server time field missing"))
}

/// True for the "timestamp outside recvWindow / expired" rejections of the three exchanges
/// (Binance `-1021`, Bybit retCode `10002`, OKX `50102`).
pub fn is_timestamp_rejection(e: &AdapterError) -> bool {
    matches!(e, AdapterError::Exchange { code, .. } if matches!(code.as_str(), "-1021" | "10002" | "50102"))
}

pub struct ClockSync {
    clock: Arc<dyn TimeSource>,
    /// The sample and the monotonic time at which it was recorded.
    last: Mutex<Option<(ClockSample, i64)>>,
}

impl ClockSync {
    pub fn new(clock: Arc<dyn TimeSource>) -> Self {
        Self { clock, last: Mutex::new(None) }
    }

    /// The current sample, or `None` if never synced or older than [`MAX_SAMPLE_AGE_MS`]
    /// (monotonic age, so wall-clock steps cannot keep an old offset alive or kill a new one).
    fn last(&self) -> Option<(ClockSample, i64)> {
        let (sample, at) = (*self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner))?;
        let age = self.clock.mono_ms().saturating_sub(at);
        (0..=MAX_SAMPLE_AGE_MS).contains(&age).then_some((sample, age))
    }

    pub fn record_success(&self, sample: ClockSample) {
        let at = self.clock.mono_ms();
        *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((sample, at));
    }

    pub fn status(&self) -> ClockStatus {
        match self.last() {
            None => ClockStatus::Unsynced,
            Some((s, age_ms)) => ClockStatus::Synced { offset_ms: s.offset_ms, rtt_ms: s.rtt_ms, age_ms },
        }
    }

    /// Local time converted to exchange time; `None` while unsynced (or on overflow).
    pub fn exchange_now_ms(&self) -> Option<i64> {
        let (s, _) = self.last()?;
        self.clock.now_ms().checked_add(s.offset_ms)
    }

    /// True when |offset| > recvWindow / 2 (feeds `alert-banner`).
    pub fn skew_warning(&self, recv_window_ms: i64) -> bool {
        self.last().is_some_and(|(s, _)| s.offset_ms.unsigned_abs() > (recv_window_ms / 2).unsigned_abs())
    }

    pub fn resync_due(&self, interval_ms: i64) -> bool {
        match self.last() {
            None => true,
            Some((_, age)) => age >= interval_ms,
        }
    }

    /// One sync round trip. On failure the previous sample is kept.
    pub async fn sync_once<T: HttpTransport>(&self, transport: &T, exchange: Exchange, base_url: &str) -> Result<ClockSample, AdapterError> {
        let path = match exchange {
            Exchange::Binance => BINANCE_TIME_PATH,
            Exchange::Bybit => BYBIT_TIME_PATH,
            Exchange::Okx => OKX_TIME_PATH,
        };
        let sent = self.clock.now_ms();
        let sent_mono = self.clock.mono_ms();
        let resp = transport.get(HttpRequest::get(format!("{base_url}{path}"), SYNC_TIMEOUT)).await?;
        // RTT from the monotonic clock (immune to wall steps mid-request); the midpoint is placed
        // on the wall timeline so the offset stays "exchange − local wall".
        let rtt = self.clock.mono_ms().saturating_sub(sent_mono);
        let received = sent.saturating_add(rtt);
        if resp.status != 200 {
            return Err(AdapterError::Http { status: resp.status });
        }
        let sample = compute_sample(sent, received, parse_server_time(exchange, &resp.body)?)?;
        self.record_success(sample);
        Ok(sample)
    }
}

/// Runs `attempt`; if it is rejected for its timestamp, runs `resync` and retries exactly once.
/// A second rejection (or any other error) is returned as is.
pub async fn retry_once_after_resync<T, A, AF, R, RF>(mut attempt: A, mut resync: R) -> Result<T, AdapterError>
where
    A: FnMut() -> AF,
    AF: Future<Output = Result<T, AdapterError>>,
    R: FnMut() -> RF,
    RF: Future<Output = ()>,
{
    match attempt().await {
        Err(e) if is_timestamp_rejection(&e) => {
            resync().await;
            attempt().await
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::exchange::transport::{FakeTransport, HttpResponse};
    use crate::ports::ManualClock;

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    fn sync_with(clock: &ManualClock) -> ClockSync {
        ClockSync::new(Arc::new(clock.clone()))
    }

    #[test]
    fn offset_and_rtt_follow_the_spec_example() {
        // sent 1000, received 1200, exchange 1700 => offset 600, rtt 200
        let s = compute_sample(1000, 1200, 1700).unwrap();
        assert_eq!((s.offset_ms, s.rtt_ms, s.synced_at_ms), (600, 200, 1200));
    }

    #[test]
    fn negative_offset_when_local_clock_is_ahead() {
        let s = compute_sample(10_000, 10_100, 9_000).unwrap();
        assert_eq!((s.offset_ms, s.rtt_ms), (-1050, 100));
    }

    #[test]
    fn never_synced_is_unsynced_and_gives_no_exchange_time() {
        let clock = ManualClock::new(5_000);
        let cs = sync_with(&clock);
        assert_eq!(cs.status(), ClockStatus::Unsynced);
        assert_eq!(cs.exchange_now_ms(), None);
        assert!(!cs.skew_warning(5000));
    }

    #[test]
    fn synced_status_reports_offset_rtt_and_age_and_exchange_now() {
        let clock = ManualClock::new(1_200);
        let cs = sync_with(&clock);
        cs.record_success(compute_sample(1000, 1200, 1700).unwrap());
        clock.advance(4_000);
        assert_eq!(cs.status(), ClockStatus::Synced { offset_ms: 600, rtt_ms: 200, age_ms: 4_000 });
        assert_eq!(cs.exchange_now_ms(), Some(5_200 + 600));
    }

    #[test]
    fn failed_sync_keeps_the_old_offset_and_reports_its_age() {
        let clock = ManualClock::new(10_000);
        let cs = sync_with(&clock);
        cs.record_success(ClockSample { offset_ms: 300, rtt_ms: 80, synced_at_ms: 10_000 });
        clock.advance(60_000);
        let t = FakeTransport::new().on("/fapi/v1/time", Err(AdapterError::Timeout));
        let r = block_on(cs.sync_once(&t, Exchange::Binance, "https://h"));
        assert_eq!(r, Err(AdapterError::Timeout));
        assert_eq!(cs.status(), ClockStatus::Synced { offset_ms: 300, rtt_ms: 80, age_ms: 60_000 });
    }

    #[test]
    fn first_sync_failure_leaves_the_exchange_unsynced() {
        let clock = ManualClock::new(10_000);
        let cs = sync_with(&clock);
        let t = FakeTransport::new().on("/v5/market/time", Err(AdapterError::network("down")));
        assert!(block_on(cs.sync_once(&t, Exchange::Bybit, "https://h")).is_err());
        assert_eq!(cs.status(), ClockStatus::Unsynced);
        assert_eq!(cs.exchange_now_ms(), None);
    }

    #[test]
    fn sync_once_uses_the_clock_for_both_ends_and_the_right_path() {
        // The fake transport answers instantly, so sent == received; the offset is exchange − local.
        let clock = ManualClock::new(2_000);
        let cs = sync_with(&clock);
        let t = FakeTransport::new().on("/api/v5/public/time", Ok(HttpResponse::ok(r#"{"code":"0","msg":"","data":[{"ts":"2750"}]}"#)));
        let s = block_on(cs.sync_once(&t, Exchange::Okx, "https://h")).unwrap();
        assert_eq!((s.offset_ms, s.rtt_ms, s.synced_at_ms), (750, 0, 2_000));
        assert_eq!(t.requests()[0].url, "https://h/api/v5/public/time");
        assert_eq!(cs.status(), ClockStatus::Synced { offset_ms: 750, rtt_ms: 0, age_ms: 0 });
    }

    #[test]
    fn sync_once_rejects_an_unparseable_time_body_and_keeps_state() {
        let clock = ManualClock::new(2_000);
        let cs = sync_with(&clock);
        let t = FakeTransport::new().on("/fapi/v1/time", Ok(HttpResponse::ok("<html>")));
        assert!(matches!(block_on(cs.sync_once(&t, Exchange::Binance, "https://h")), Err(AdapterError::Parse(_))));
        assert_eq!(cs.status(), ClockStatus::Unsynced);
    }

    #[test]
    fn non_200_time_response_is_an_http_error() {
        let clock = ManualClock::new(2_000);
        let cs = sync_with(&clock);
        let t = FakeTransport::new().on("/fapi/v1/time", Ok(HttpResponse::with_status(503, "x")));
        assert_eq!(block_on(cs.sync_once(&t, Exchange::Binance, "https://h")), Err(AdapterError::Http { status: 503 }));
    }

    #[test]
    fn parses_the_three_server_time_formats() {
        assert_eq!(parse_server_time(Exchange::Binance, r#"{"serverTime":1791190833000}"#), Ok(1791190833000));
        assert_eq!(
            parse_server_time(Exchange::Bybit, r#"{"retCode":0,"retMsg":"OK","result":{"timeSecond":"1791190833","timeNano":"1791190833123456789"},"time":1791190833123}"#),
            Ok(1791190833123)
        );
        assert_eq!(parse_server_time(Exchange::Okx, r#"{"code":"0","msg":"","data":[{"ts":"1791190833456"}]}"#), Ok(1791190833456));
        assert!(parse_server_time(Exchange::Binance, "{}").is_err());
        assert!(parse_server_time(Exchange::Okx, r#"{"code":"0","data":[]}"#).is_err());
        assert!(parse_server_time(Exchange::Bybit, r#"{"retCode":10001,"retMsg":"x","time":5}"#).is_err());
    }

    #[test]
    fn skew_warning_fires_only_beyond_half_the_recv_window() {
        let clock = ManualClock::new(0);
        let cs = sync_with(&clock);
        cs.record_success(ClockSample { offset_ms: 2_500, rtt_ms: 0, synced_at_ms: 0 });
        assert!(!cs.skew_warning(5_000), "exactly half is not beyond half");
        cs.record_success(ClockSample { offset_ms: -2_501, rtt_ms: 0, synced_at_ms: 0 });
        assert!(cs.skew_warning(5_000), "negative offsets count by absolute value");
    }

    #[test]
    fn resync_is_due_when_unsynced_or_old() {
        let clock = ManualClock::new(0);
        let cs = sync_with(&clock);
        assert!(cs.resync_due(RESYNC_INTERVAL_MS));
        cs.record_success(ClockSample { offset_ms: 1, rtt_ms: 1, synced_at_ms: 0 });
        clock.set(RESYNC_INTERVAL_MS - 1);
        assert!(!cs.resync_due(RESYNC_INTERVAL_MS));
        clock.set(RESYNC_INTERVAL_MS);
        assert!(cs.resync_due(RESYNC_INTERVAL_MS));
    }

    #[test]
    fn timestamp_rejections_are_recognised_per_exchange() {
        assert!(is_timestamp_rejection(&AdapterError::exchange("-1021", "Timestamp for this request is outside of the recvWindow.")));
        assert!(is_timestamp_rejection(&AdapterError::exchange("10002", "invalid request, please check your server timestamp")));
        assert!(is_timestamp_rejection(&AdapterError::exchange("50102", "Timestamp request expired")));
        assert!(!is_timestamp_rejection(&AdapterError::exchange("-1022", "Signature invalid")));
        assert!(!is_timestamp_rejection(&AdapterError::Timeout));
    }

    #[test]
    fn rejected_once_then_ok_resyncs_once_and_retries_once() {
        let (attempts, resyncs) = (Cell::new(0), Cell::new(0));
        let r = block_on(retry_once_after_resync(
            || {
                attempts.set(attempts.get() + 1);
                let n = attempts.get();
                async move { if n == 1 { Err(AdapterError::exchange("-1021", "recvWindow")) } else { Ok(n) } }
            },
            || {
                resyncs.set(resyncs.get() + 1);
                async {}
            },
        ));
        assert_eq!((r, attempts.get(), resyncs.get()), (Ok(2), 2, 1));
    }

    #[test]
    fn rejected_twice_returns_the_error_without_a_third_attempt() {
        let (attempts, resyncs) = (Cell::new(0), Cell::new(0));
        let r: Result<(), _> = block_on(retry_once_after_resync(
            || {
                attempts.set(attempts.get() + 1);
                async { Err(AdapterError::exchange("-1021", "recvWindow")) }
            },
            || {
                resyncs.set(resyncs.get() + 1);
                async {}
            },
        ));
        assert!(matches!(r, Err(AdapterError::Exchange { .. })));
        assert_eq!((attempts.get(), resyncs.get()), (2, 1));
    }

    #[test]
    fn other_errors_and_successes_do_not_resync() {
        let resyncs = Cell::new(0);
        let r: Result<u8, _> = block_on(retry_once_after_resync(|| async { Err(AdapterError::Timeout) }, || {
            resyncs.set(resyncs.get() + 1);
            async {}
        }));
        assert_eq!(r, Err(AdapterError::Timeout));
        let ok = block_on(retry_once_after_resync(|| async { Ok::<_, AdapterError>(7) }, || {
            resyncs.set(resyncs.get() + 1);
            async {}
        }));
        assert_eq!((ok, resyncs.get()), (Ok(7), 0));
    }

    // ------------------------------------------------ round 2: sample validation

    #[test]
    fn i64_min_server_time_does_not_panic_and_is_rejected() {
        assert!(compute_sample(0, 10, i64::MIN).is_err());
        assert!(compute_sample(0, 10, i64::MAX).is_err(), "offset far beyond a day");
    }

    #[test]
    fn extreme_local_timestamps_do_not_overflow() {
        assert!(compute_sample(i64::MAX, i64::MAX, 5).is_err());
        assert!(compute_sample(i64::MIN, i64::MAX, 5).is_err());
        assert!(compute_sample(i64::MAX - 1, i64::MAX, 5).is_err());
    }

    #[test]
    fn negative_rtt_is_rejected() {
        assert!(compute_sample(2_000, 1_000, 1_500).is_err());
        assert!(compute_sample(1_000, 1_000, 1_000).is_ok(), "zero RTT is fine");
    }

    #[test]
    fn offset_beyond_one_day_is_rejected_exactly_one_day_is_not() {
        let day = 86_400_000;
        assert!(compute_sample(1_000_000, 1_000_000, 1_000_000 + day).is_ok());
        assert!(compute_sample(1_000_000, 1_000_000, 1_000_000 + day + 1).is_err());
        assert!(compute_sample(1_000_000_000, 1_000_000_000, 1_000_000_000 - day).is_ok());
        assert!(compute_sample(1_000_000_000, 1_000_000_000, 1_000_000_000 - day - 1).is_err());
    }

    #[test]
    fn non_positive_server_time_is_rejected() {
        assert!(compute_sample(0, 0, 0).is_err());
        assert!(compute_sample(0, 0, -5).is_err());
        assert!(compute_sample(0, 0, 1).is_ok());
    }

    #[test]
    fn sync_once_with_an_absurd_server_time_keeps_the_previous_state() {
        let clock = ManualClock::new(1_000_000);
        let cs = sync_with(&clock);
        let t = FakeTransport::new().on("/fapi/v1/time", Ok(HttpResponse::ok(r#"{"serverTime":0}"#)));
        assert!(matches!(block_on(cs.sync_once(&t, Exchange::Binance, "https://h")), Err(AdapterError::Parse(_))));
        assert_eq!(cs.status(), ClockStatus::Unsynced);
    }

    #[test]
    fn a_sample_older_than_one_hour_makes_the_clock_unsynced_again() {
        let clock = ManualClock::new(10_000);
        let cs = sync_with(&clock);
        cs.record_success(ClockSample { offset_ms: 300, rtt_ms: 80, synced_at_ms: 10_000 });
        clock.advance(3_600_000);
        assert!(matches!(cs.status(), ClockStatus::Synced { age_ms: 3_600_000, .. }), "exactly one hour is still synced");
        clock.advance(1);
        assert_eq!(cs.status(), ClockStatus::Unsynced);
        assert_eq!(cs.exchange_now_ms(), None, "signing must stop once the offset is that old");
        assert!(cs.resync_due(RESYNC_INTERVAL_MS));
    }

    #[test]
    fn sample_age_ignores_wall_clock_steps() {
        let clock = ManualClock::new(10_000_000);
        let cs = sync_with(&clock);
        cs.record_success(ClockSample { offset_ms: 0, rtt_ms: 0, synced_at_ms: 10_000_000 });
        clock.jump_wall(-7_200_000); // wall clock steps back two hours
        clock.advance(60_000);
        assert!(matches!(cs.status(), ClockStatus::Synced { age_ms: 60_000, .. }));
        clock.jump_wall(10 * 3_600_000); // and forward ten hours
        assert!(matches!(cs.status(), ClockStatus::Synced { age_ms: 60_000, .. }));
    }
}

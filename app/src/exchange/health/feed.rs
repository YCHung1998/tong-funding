//! Binance all-market mark price feed (`!markPrice@arr@1s`) and per-source health
//! (spec: feed-health). Layers, from pure to impure:
//! - `parse_mark_price_message`: pure parsing (Decimal, partial updates, skip bad items);
//! - `FeedHealth`: connection/freshness state machine, driven only by the injected `Clock`,
//!   writing `FETCH_ERROR` / `FEED_RECOVERED` events on state transitions only;
//! - `MarkPriceFeed::run`: the reconnect loop over an injectable `MessageSource`
//!   (scripted in tests, `TungsteniteSource` against the real stream).
#![allow(dead_code)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::str::FromStr;

use serde_json::json;
use tong_funding_core::redact::redact_secrets;
use tong_funding_core::types::Decimal;

use super::cache::{MarkPriceCache, MarkPriceEntry};
use crate::exchange::error::AdapterError;
use crate::ports::{Clock, EventSink};

pub const EVENT_FETCH_ERROR: &str = "FETCH_ERROR";
pub const EVENT_FEED_RECOVERED: &str = "FEED_RECOVERED";

/// Reconnect back-off: start 1 s, cap 30 s (spec, inherited from the Python version).
pub const RECONNECT_START_MS: u64 = 1_000;
pub const RECONNECT_CAP_MS: u64 = 30_000;
/// Freshness multiplier of the expected period (design D4; proposal, unverified).
pub const STALE_PERIOD_MULTIPLIER: i64 = 3;
/// How often the loop wakes up with no traffic to check for a silent connection.
pub const IDLE_CHECK: Duration = Duration::from_millis(500);

/// Health of one data source at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub source: String,
    pub connected: bool,
    pub last_success_at: Option<i64>,
    pub consecutive_failures: u32,
    pub expected_period_ms: i64,
    /// `max(stale_data_threshold_ms, 3 × expected period)`.
    pub stale_threshold_ms: i64,
    /// Never updated, disconnected, or older than the threshold.
    pub stale: bool,
}

pub struct FeedHealth {
    source: String,
    expected_period_ms: i64,
    stale_data_threshold_ms: i64,
    clock: Arc<dyn Clock>,
    sink: Arc<dyn EventSink>,
    inner: Mutex<HealthInner>,
}

#[derive(Default)]
struct HealthInner {
    connected: bool,
    connected_at: i64,
    last_success_at: Option<i64>,
    consecutive_failures: u32,
    /// (error kind, time the failure episode began)
    failure: Option<(String, i64)>,
}

impl FeedHealth {
    pub fn new(source: &str, expected_period_ms: i64, stale_data_threshold_ms: i64, clock: Arc<dyn Clock>, sink: Arc<dyn EventSink>) -> Self {
        Self { source: source.to_string(), expected_period_ms, stale_data_threshold_ms, clock, sink, inner: Mutex::new(HealthInner::default()) }
    }

    /// `max(stale_data_threshold_ms, 3 x expected period)`.
    pub fn stale_threshold_ms(&self) -> i64 {
        self.stale_data_threshold_ms.max(self.expected_period_ms.saturating_mul(STALE_PERIOD_MULTIPLIER))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HealthInner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The transport connected (not yet a recovery: that needs data).
    pub fn on_connected(&self) {
        let now = self.clock.now_ms();
        let mut g = self.lock();
        g.connected = true;
        g.connected_at = now;
    }

    /// A message / poll succeeded.
    pub fn on_success(&self) {
        let now = self.clock.now_ms();
        let recovered = {
            let mut g = self.lock();
            g.connected = true;
            g.last_success_at = Some(now);
            g.consecutive_failures = 0;
            g.failure.take()
        };
        // Events are emitted after the lock is released (the sink is external code).
        if let Some((_, since)) = recovered {
            self.sink.emit(EVENT_FEED_RECOVERED, None, json!({"source": self.source, "duration_ms": now - since}));
        }
    }

    /// A poll or connection attempt failed with `error`.
    pub fn on_failure(&self, error: &AdapterError) {
        self.fail(error_kind(error), &error.to_string());
    }

    /// close / error event on a live connection: effective immediately.
    pub fn on_disconnect(&self, reason: &str) {
        self.fail("Disconnected", reason);
    }

    /// Connected but silent past the threshold: marks the source disconnected and returns true
    /// (the caller must reconnect).
    pub fn check_silence(&self) -> bool {
        let now = self.clock.now_ms();
        let threshold = self.stale_threshold_ms();
        let silent = {
            let g = self.lock();
            // Measured from the later of the last message and the (re)connection.
            let reference = g.last_success_at.unwrap_or(i64::MIN).max(g.connected_at);
            g.connected && now - reference > threshold
        };
        if silent {
            self.fail("Stale", &format!("no message for more than {threshold} ms"));
        }
        silent
    }

    fn fail(&self, kind: &str, message: &str) {
        let now = self.clock.now_ms();
        let emit = {
            let mut g = self.lock();
            g.connected = false;
            g.consecutive_failures = g.consecutive_failures.saturating_add(1);
            match &g.failure {
                Some((k, _)) if k == kind => false,
                Some((_, since)) => {
                    let since = *since;
                    g.failure = Some((kind.to_string(), since));
                    true
                }
                None => {
                    g.failure = Some((kind.to_string(), now));
                    true
                }
            }
        };
        if emit {
            self.sink.emit(
                EVENT_FETCH_ERROR,
                None,
                json!({"source": self.source, "error_kind": kind, "message": redact_secrets(message)}),
            );
        }
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        let now = self.clock.now_ms();
        let threshold = self.stale_threshold_ms();
        let g = self.lock();
        let stale = !g.connected || g.last_success_at.is_none_or(|t| now - t > threshold);
        HealthSnapshot {
            source: self.source.clone(),
            connected: g.connected,
            last_success_at: g.last_success_at,
            consecutive_failures: g.consecutive_failures,
            expected_period_ms: self.expected_period_ms,
            stale_threshold_ms: threshold,
            stale,
        }
    }
}

fn error_kind(e: &AdapterError) -> &'static str {
    match e {
        AdapterError::Timeout => "Timeout",
        AdapterError::Network(_) => "Network",
        AdapterError::Http { .. } => "Http",
        AdapterError::RateLimited { .. } => "RateLimited",
        AdapterError::Exchange { .. } => "Exchange",
        AdapterError::Parse(_) => "Parse",
        AdapterError::Incomplete(_) => "Incomplete",
        AdapterError::NotConnected => "NotConnected",
    }
}

/// Parses one WebSocket text frame into entries stamped with `observed_at`.
/// Accepts a bare array or a combined-stream `{"stream":..,"data":[..]}` wrapper. Items missing
/// `s`/`r`/`T`/`p`/`E`, or with unparsable numbers, are skipped without affecting the others.
/// A frame that is not JSON or holds no array is a `Parse` error.
pub fn parse_mark_price_message(text: &str, observed_at: i64) -> Result<Vec<MarkPriceEntry>, AdapterError> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| AdapterError::parse(format!("mark price frame is not JSON: {e}")))?;
    let items = match &v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(o) => match o.get("data") {
            Some(serde_json::Value::Array(a)) => a,
            _ => return Err(AdapterError::parse("mark price frame has no array")),
        },
        _ => return Err(AdapterError::parse("mark price frame is not an array")),
    };
    Ok(items.iter().filter_map(|item| parse_item(item, observed_at)).collect())
}

fn parse_item(item: &serde_json::Value, observed_at: i64) -> Option<MarkPriceEntry> {
    let decimal = |key: &str| -> Option<Decimal> {
        match item.get(key)? {
            serde_json::Value::String(s) => Decimal::from_str(s.trim()).ok(),
            serde_json::Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
            _ => None,
        }
    };
    let symbol = item.get("s")?.as_str().filter(|s| !s.is_empty())?;
    Some(MarkPriceEntry {
        symbol: symbol.to_string(),
        funding_rate: decimal("r")?,
        next_funding_time: item.get("T")?.as_i64()?,
        mark_price: decimal("p")?,
        exchange_timestamp: item.get("E")?.as_i64()?,
        observed_at,
    })
}

/// Exponential reconnect delays 1 s, 2 s, 4 s, ... capped at 30 s.
#[derive(Debug, Default)]
pub struct ReconnectBackoff {
    next_ms: Option<u64>,
}

impl ReconnectBackoff {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn next_delay(&mut self) -> Duration {
        let ms = self.next_ms.unwrap_or(RECONNECT_START_MS);
        self.next_ms = Some(ms.saturating_mul(2).min(RECONNECT_CAP_MS));
        Duration::from_millis(ms)
    }
    pub fn reset(&mut self) {
        self.next_ms = None;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEvent {
    Text(String),
    Closed(Option<String>),
    Error(String),
    /// Nothing arrived within the wait.
    Idle,
    /// The source has nothing more to give (tests, deadline); ends the loop.
    Shutdown,
}

pub trait MessageSource: Send {
    fn connect(&mut self) -> impl Future<Output = Result<(), AdapterError>> + Send;
    fn next_event(&mut self, wait: Duration) -> impl Future<Output = FeedEvent> + Send;
}

pub trait Sleeper: Send + Sync {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send;
}

pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(d)
    }
}

/// Real WebSocket source (`tokio-tungstenite`, rustls). Read-only: nothing is ever sent but
/// protocol pongs.
pub struct TungsteniteSource {
    url: String,
    stream: Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
}

/// Handshake time limit (proposal, unverified).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

impl TungsteniteSource {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into(), stream: None }
    }
}

impl MessageSource for TungsteniteSource {
    async fn connect(&mut self) -> Result<(), AdapterError> {
        self.stream = None;
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(self.url.as_str())).await {
            Err(_) => Err(AdapterError::Timeout),
            Ok(Err(e)) => Err(AdapterError::network(e.to_string())),
            Ok(Ok((stream, _response))) => {
                self.stream = Some(stream);
                Ok(())
            }
        }
    }

    async fn next_event(&mut self, wait: Duration) -> FeedEvent {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let Some(stream) = self.stream.as_mut() else {
            return FeedEvent::Closed(Some("not connected".into()));
        };
        match tokio::time::timeout(wait, stream.next()).await {
            Err(_) => FeedEvent::Idle,
            Ok(None) => FeedEvent::Closed(None),
            Ok(Some(Err(e))) => FeedEvent::Error(e.to_string()),
            Ok(Some(Ok(msg))) => match msg {
                Message::Text(t) => FeedEvent::Text(t.as_str().to_string()),
                Message::Binary(b) => match String::from_utf8(b.to_vec()) {
                    Ok(t) => FeedEvent::Text(t),
                    Err(_) => FeedEvent::Idle,
                },
                Message::Close(frame) => FeedEvent::Closed(frame.map(|f| f.reason.to_string())),
                // Ping / Pong / raw frames: tungstenite answers pings itself while we keep polling.
                _ => FeedEvent::Idle,
            },
        }
    }
}

pub struct MarkPriceFeed {
    clock: Arc<dyn Clock>,
    health: Arc<FeedHealth>,
    cache: Arc<MarkPriceCache>,
}

impl MarkPriceFeed {
    pub const SOURCE: &'static str = "binance_ws_mark_price";

    pub fn new(clock: Arc<dyn Clock>, sink: Arc<dyn EventSink>, stale_data_threshold_ms: i64) -> Self {
        let health = Arc::new(FeedHealth::new(
            Self::SOURCE,
            crate::exchange::public::feed_endpoints::BINANCE_MARK_PRICE_PERIOD_MS,
            stale_data_threshold_ms,
            clock.clone(),
            sink,
        ));
        let cache = Arc::new(MarkPriceCache::new(clock.clone(), health.clone()));
        Self { clock, health, cache }
    }

    pub fn cache(&self) -> Arc<MarkPriceCache> {
        self.cache.clone()
    }

    pub fn health(&self) -> Arc<FeedHealth> {
        self.health.clone()
    }

    /// Handles one text frame: parse, merge into the cache, mark the source healthy.
    /// Returns true if the frame was a valid mark-price message (even an empty one).
    pub fn handle_text(&self, text: &str) -> bool {
        match parse_mark_price_message(text, self.clock.now_ms()) {
            Ok(entries) => {
                self.cache.apply(entries);
                self.health.on_success();
                true
            }
            // Subscription acks, garbage: not data, so not a sign of life either.
            Err(_) => false,
        }
    }

    /// Connect / read / reconnect until the source reports `Shutdown`.
    pub async fn run<S: MessageSource, Z: Sleeper>(&self, source: &mut S, sleeper: &Z) {
        let mut backoff = ReconnectBackoff::new();
        loop {
            match source.connect().await {
                Err(e) => {
                    self.health.on_failure(&e);
                    sleeper.sleep(backoff.next_delay()).await;
                    continue;
                }
                Ok(()) => self.health.on_connected(),
            }
            loop {
                match source.next_event(IDLE_CHECK).await {
                    FeedEvent::Text(t) => {
                        if self.handle_text(&t) {
                            backoff.reset();
                        }
                    }
                    FeedEvent::Idle => {
                        if self.health.check_silence() {
                            break;
                        }
                    }
                    FeedEvent::Closed(reason) => {
                        self.health.on_disconnect(reason.as_deref().unwrap_or("connection closed"));
                        break;
                    }
                    FeedEvent::Error(e) => {
                        self.health.on_disconnect(&e);
                        break;
                    }
                    FeedEvent::Shutdown => return,
                }
            }
            sleeper.sleep(backoff.next_delay()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::str::FromStr;

    use tong_funding_core::types::Decimal;

    use super::*;
    use crate::ports::{ManualClock, MemoryEventSink};

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    const BTC: &str = r#"{"e":"markPriceUpdate","E":1791190833000,"s":"BTCUSDT","p":"86186.20","P":"86100.1","i":"86180.5","r":"0.00005047","T":1791216000000}"#;

    // ---------------------------------------------------------------- parsing

    #[test]
    fn scenario_parse_one_message() {
        let es = parse_mark_price_message(&format!("[{BTC}]"), 1791190833123).unwrap();
        assert_eq!(es.len(), 1);
        let e = &es[0];
        assert_eq!(e.symbol, "BTCUSDT");
        assert_eq!(e.funding_rate, dec("0.00005047"));
        assert_eq!(e.next_funding_time, 1791216000000);
        assert_eq!(e.mark_price, dec("86186.20"));
        assert_eq!(e.exchange_timestamp, 1791190833000);
        assert_eq!(e.observed_at, 1791190833123);
    }

    #[test]
    fn scenario_item_missing_r_is_skipped_and_others_survive() {
        let no_r = r#"{"E":1,"s":"ETHUSDT","p":"3000.1","T":2}"#;
        let ok = r#"{"E":1,"s":"SOLUSDT","p":"150.5","r":"-0.0001","T":2}"#;
        let es = parse_mark_price_message(&format!("[{no_r},{ok}]"), 9).unwrap();
        assert_eq!(es.iter().map(|e| e.symbol.as_str()).collect::<Vec<_>>(), vec!["SOLUSDT"]);
        assert_eq!(es[0].funding_rate, dec("-0.0001"));
    }

    #[test]
    fn each_required_field_is_required_and_bad_numbers_are_skipped() {
        let items = [
            r#"{"E":1,"p":"1","r":"0.1","T":2}"#,                       // no s
            r#"{"E":1,"s":"A","r":"0.1","T":2}"#,                       // no p
            r#"{"E":1,"s":"B","p":"1","r":"0.1"}"#,                     // no T
            r#"{"s":"C","p":"1","r":"0.1","T":2}"#,                     // no E
            r#"{"E":1,"s":"D","p":"abc","r":"0.1","T":2}"#,             // bad p
            r#"{"E":1,"s":"E","p":"1","r":"","T":2}"#,                  // bad r
            r#"{"E":"x","s":"F","p":"1","r":"0.1","T":2}"#,             // bad E
            r#"{"E":1,"s":"","p":"1","r":"0.1","T":2}"#,                // empty s
            r#"{"E":1,"s":"OKUSDT","p":"1","r":"0.1","T":2}"#,
        ];
        let es = parse_mark_price_message(&format!("[{}]", items.join(",")), 5).unwrap();
        assert_eq!(es.iter().map(|e| e.symbol.as_str()).collect::<Vec<_>>(), vec!["OKUSDT"]);
    }

    #[test]
    fn combined_stream_wrapper_and_empty_array_are_accepted() {
        let wrapped = format!(r#"{{"stream":"!markPrice@arr@1s","data":[{BTC}]}}"#);
        assert_eq!(parse_mark_price_message(&wrapped, 1).unwrap().len(), 1);
        assert_eq!(parse_mark_price_message("[]", 1).unwrap().len(), 0);
    }

    #[test]
    fn non_array_or_non_json_frames_are_parse_errors() {
        assert!(matches!(parse_mark_price_message("not json", 1), Err(AdapterError::Parse(_))));
        assert!(matches!(parse_mark_price_message(r#"{"result":null,"id":1}"#, 1), Err(AdapterError::Parse(_))));
        assert!(matches!(parse_mark_price_message("42", 1), Err(AdapterError::Parse(_))));
    }

    /// First two items of a frame recorded from the real stream (wss://fstream.binance.com/market/ws/!markPrice@arr@1s,
    /// 2026-10-05 by the 25 s ignored test); includes fields we do not use (`ap`, `P`, `i`, `st`).
    const RECORDED: &str = r#"[{"e":"markPriceUpdate","E":1791201678000,"s":"BTCUSDT","p":"86141.00000000","ap":"86141.00000000","P":"86110.40591268","i":"86181.65630435","r":"0.00003779","T":1791216000000,"st":1},{"e":"markPriceUpdate","E":1791201678000,"s":"ETHUSDT","p":"2715.93000000","ap":"2715.93000000","P":"2717.36292584","i":"2717.21325581","r":"0.00006061","T":1791216000000,"st":1}]"#;

    #[test]
    fn a_frame_recorded_from_the_real_stream_parses() {
        let es = parse_mark_price_message(RECORDED, 7).unwrap();
        assert_eq!(es.len(), 2);
        assert_eq!((es[0].symbol.as_str(), es[0].mark_price, es[0].funding_rate), ("BTCUSDT", dec("86141"), dec("0.00003779")));
        assert_eq!((es[1].symbol.as_str(), es[1].exchange_timestamp), ("ETHUSDT", 1791201678000));
    }

    #[test]
    fn prices_keep_their_exact_decimal_digits() {
        let item = r#"{"E":1,"s":"PEPEUSDT","p":"0.00001234","r":"0.00010000","T":2}"#;
        let e = &parse_mark_price_message(&format!("[{item}]"), 1).unwrap()[0];
        assert_eq!(e.mark_price.to_string(), "0.00001234");
        assert_eq!(e.funding_rate.to_string(), "0.00010000");
    }

    // ----------------------------------------------------------------- health

    struct Rig {
        clock: ManualClock,
        sink: Arc<MemoryEventSink>,
        health: FeedHealth,
    }

    fn rig(period: i64, threshold: i64) -> Rig {
        let clock = ManualClock::new(1_000_000);
        let sink = Arc::new(MemoryEventSink::default());
        let health = FeedHealth::new("src", period, threshold, Arc::new(clock.clone()), sink.clone());
        Rig { clock, sink, health }
    }

    fn kinds(r: &Rig) -> Vec<(String, String)> {
        r.sink
            .events()
            .into_iter()
            .map(|e| (e.event_type, e.payload.get("error_kind").and_then(|k| k.as_str()).unwrap_or("").to_string()))
            .collect()
    }

    #[test]
    fn scenario_never_succeeded_is_stale_not_healthy() {
        let r = rig(1_000, 1_000);
        let s = r.health.snapshot();
        assert!(s.stale && !s.connected && s.last_success_at.is_none());
        r.health.on_connected();
        assert!(r.health.snapshot().stale, "connected but no data yet is still stale");
    }

    #[test]
    fn scenario_close_event_takes_effect_immediately_200ms_after_a_message() {
        let r = rig(1_000, 1_000);
        r.health.on_connected();
        r.health.on_success();
        assert!(!r.health.snapshot().stale);
        r.clock.advance(200);
        r.health.on_disconnect("close 1006");
        let s = r.health.snapshot();
        assert!(!s.connected && s.stale, "no timer needed");
        assert_eq!(s.consecutive_failures, 1);
    }

    #[test]
    fn scenario_silent_connection_is_stale_and_reconnects_beyond_three_periods() {
        let r = rig(1_000, 1_000); // threshold = max(1000, 3 * 1000) = 3000
        assert_eq!(r.health.snapshot().stale_threshold_ms, 3_000);
        r.health.on_connected();
        r.health.on_success();
        r.clock.advance(3_000);
        assert!(!r.health.snapshot().stale, "exactly at the threshold is not beyond it");
        assert!(!r.health.check_silence());
        r.clock.advance(1);
        assert!(r.health.snapshot().stale);
        assert!(r.health.check_silence(), "must ask for a reconnect");
        assert!(!r.health.snapshot().connected);
        assert_eq!(kinds(&r), vec![("FETCH_ERROR".to_string(), "Stale".to_string())]);
    }

    #[test]
    fn scenario_rest_poll_25s_ago_is_not_stale_31s_ago_is() {
        let r = rig(10_000, 1_000); // threshold = max(1000, 30000) = 30000
        r.health.on_success();
        r.clock.advance(25_000);
        assert!(!r.health.snapshot().stale);
        r.clock.advance(6_000);
        assert!(r.health.snapshot().stale);
    }

    #[test]
    fn threshold_is_the_larger_of_configured_and_three_periods() {
        assert_eq!(rig(100, 1_000).health.snapshot().stale_threshold_ms, 1_000);
        assert_eq!(rig(1_000, 5_000).health.snapshot().stale_threshold_ms, 5_000);
        assert_eq!(rig(10_000, 1_000).health.snapshot().stale_threshold_ms, 30_000);
    }

    #[test]
    fn reconnected_but_still_silent_is_not_instantly_silent_again() {
        let r = rig(1_000, 1_000);
        r.health.on_connected();
        r.health.on_success();
        r.clock.advance(10_000);
        assert!(r.health.check_silence());
        r.clock.advance(1_000);
        r.health.on_connected(); // new connection, no message yet
        r.clock.advance(2_000);
        assert!(!r.health.check_silence(), "silence is measured from the new connection");
        r.clock.advance(1_001);
        assert!(r.health.check_silence());
    }

    #[test]
    fn scenario_ten_failures_then_recovery_write_exactly_two_events() {
        let r = rig(10_000, 1_000);
        r.health.on_success();
        for _ in 0..10 {
            r.clock.advance(10_000);
            r.health.on_failure(&AdapterError::Timeout);
        }
        assert_eq!(r.health.snapshot().consecutive_failures, 10);
        r.clock.advance(10_000);
        r.health.on_success();
        let ev = r.sink.events();
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert_eq!(ev[0].event_type, "FETCH_ERROR");
        assert_eq!(ev[0].payload["source"], "src");
        assert_eq!(ev[0].payload["error_kind"], "Timeout");
        assert_eq!(ev[1].event_type, "FEED_RECOVERED");
        assert_eq!(ev[1].payload["source"], "src");
        assert_eq!(ev[1].payload["duration_ms"], 100_000, "first failure at +10 s, recovery at +110 s");
        assert_eq!(r.health.snapshot().consecutive_failures, 0);
    }

    #[test]
    fn scenario_changing_error_kind_is_a_new_transition() {
        let r = rig(10_000, 1_000);
        r.health.on_failure(&AdapterError::Timeout);
        r.health.on_failure(&AdapterError::Timeout);
        r.health.on_failure(&AdapterError::RateLimited { retry_after_ms: Some(5_000) });
        r.health.on_failure(&AdapterError::RateLimited { retry_after_ms: None });
        assert_eq!(
            kinds(&r),
            vec![("FETCH_ERROR".to_string(), "Timeout".to_string()), ("FETCH_ERROR".to_string(), "RateLimited".to_string())]
        );
    }

    #[test]
    fn event_messages_are_redacted_and_first_success_is_not_a_recovery() {
        let r = rig(10_000, 1_000);
        r.health.on_success();
        assert!(r.sink.events().is_empty(), "first success: nothing to recover from");
        r.health.on_failure(&AdapterError::network("GET https://h/x?timestamp=1&signature=deadbeef failed"));
        let msg = r.sink.events()[0].payload["message"].as_str().unwrap().to_string();
        assert!(!msg.contains("deadbeef") && msg.contains("timestamp=1"), "{msg}");
    }

    #[test]
    fn repeated_disconnect_reports_and_failed_reconnects_do_not_duplicate_events() {
        let r = rig(1_000, 1_000);
        r.health.on_connected();
        r.health.on_success();
        r.health.on_disconnect("closed");
        for _ in 0..5 {
            r.health.on_failure(&AdapterError::network("refused"));
        }
        // Disconnected -> Network is a kind change (2 events), the 4 further refusals add none.
        assert_eq!(kinds(&r).len(), 2);
        r.health.on_connected();
        r.health.on_success();
        assert_eq!(r.sink.events().last().unwrap().event_type, "FEED_RECOVERED");
        assert_eq!(r.sink.events().len(), 3);
    }

    #[test]
    fn reconnect_backoff_doubles_from_one_second_to_a_thirty_second_cap_and_resets() {
        let mut b = ReconnectBackoff::new();
        let secs: Vec<u64> = (0..8).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }

    // ------------------------------------------------------------- feed loop

    enum Step {
        Text(String),
        Closed,
        Error,
        /// Advance the clock, then report Idle.
        Idle(i64),
        Advance(i64),
    }

    struct Scripted {
        clock: ManualClock,
        connects: VecDeque<Result<(), AdapterError>>,
        steps: VecDeque<Step>,
        connect_count: usize,
    }

    impl Scripted {
        fn new(clock: &ManualClock, connects: Vec<Result<(), AdapterError>>, steps: Vec<Step>) -> Self {
            Self { clock: clock.clone(), connects: connects.into(), steps: steps.into(), connect_count: 0 }
        }
    }

    impl MessageSource for Scripted {
        async fn connect(&mut self) -> Result<(), AdapterError> {
            self.connect_count += 1;
            self.connects.pop_front().unwrap_or(Ok(()))
        }
        async fn next_event(&mut self, _wait: Duration) -> FeedEvent {
            loop {
                match self.steps.pop_front() {
                    None => return FeedEvent::Shutdown,
                    Some(Step::Text(t)) => return FeedEvent::Text(t),
                    Some(Step::Closed) => return FeedEvent::Closed(Some("scripted close".into())),
                    Some(Step::Error) => return FeedEvent::Error("scripted error".into()),
                    Some(Step::Idle(ms)) => {
                        self.clock.advance(ms);
                        return FeedEvent::Idle;
                    }
                    Some(Step::Advance(ms)) => self.clock.advance(ms),
                }
            }
        }
    }

    /// Records requested sleeps and advances the manual clock by them.
    struct RecordingSleeper {
        clock: ManualClock,
        slept: Mutex<Vec<Duration>>,
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send {
            self.slept.lock().unwrap().push(d);
            self.clock.advance(d.as_millis() as i64);
            std::future::ready(())
        }
    }

    struct Loop {
        clock: ManualClock,
        sink: Arc<MemoryEventSink>,
        feed: MarkPriceFeed,
        sleeper: RecordingSleeper,
    }

    fn make_loop() -> Loop {
        let clock = ManualClock::new(1_000_000);
        let sink = Arc::new(MemoryEventSink::default());
        let feed = MarkPriceFeed::new(Arc::new(clock.clone()), sink.clone(), 1_000);
        let sleeper = RecordingSleeper { clock: clock.clone(), slept: Mutex::new(Vec::new()) };
        Loop { clock, sink, feed, sleeper }
    }

    fn frame(symbols: &[(&str, &str)]) -> String {
        let items: Vec<String> = symbols
            .iter()
            .map(|(s, p)| format!(r#"{{"e":"markPriceUpdate","E":1791190833000,"s":"{s}","p":"{p}","r":"0.0001","T":1791216000000}}"#))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn messages_fill_the_cache_stamped_with_the_injected_clock_and_mark_the_source_healthy() {
        let l = make_loop();
        let mut src = Scripted::new(&l.clock, vec![], vec![Step::Text(frame(&[("BTCUSDT", "86186.20"), ("ETHUSDT", "3000.5")]))]);
        block_on(l.feed.run(&mut src, &l.sleeper));
        let c = l.feed.cache().get("BTCUSDT").expect("cached");
        assert_eq!(c.observed_at, 1_000_000);
        assert_eq!(c.data.mark_price, dec("86186.20"));
        assert!(c.health.connected && !c.health.stale && c.is_fresh());
        assert_eq!(l.feed.cache().len(), 2);
    }

    #[test]
    fn partial_message_updates_only_its_symbols_through_the_feed() {
        let l = make_loop();
        let mut src = Scripted::new(
            &l.clock,
            vec![],
            vec![Step::Text(frame(&[("A", "1"), ("B", "1")])), Step::Advance(1_000), Step::Text(frame(&[("A", "2")]))],
        );
        block_on(l.feed.run(&mut src, &l.sleeper));
        let cache = l.feed.cache();
        assert_eq!((cache.get("A").unwrap().observed_at, cache.get("A").unwrap().data.mark_price), (1_001_000, dec("2")));
        assert_eq!((cache.get("B").unwrap().observed_at, cache.get("B").unwrap().data.mark_price), (1_000_000, dec("1")));
    }

    #[test]
    fn unparsable_frames_are_ignored_and_do_not_count_as_a_healthy_message() {
        let l = make_loop();
        let mut src = Scripted::new(&l.clock, vec![], vec![Step::Text("garbage".into()), Step::Text(r#"{"result":null,"id":1}"#.into())]);
        block_on(l.feed.run(&mut src, &l.sleeper));
        assert!(l.feed.cache().is_empty());
        assert!(l.feed.cache().health().stale, "no valid message yet");
    }

    #[test]
    fn close_event_disconnects_at_once_reads_show_it_and_the_loop_reconnects_after_one_second() {
        let l = make_loop();
        let mut src = Scripted::new(&l.clock, vec![], vec![Step::Text(frame(&[("BTCUSDT", "1")])), Step::Advance(5_000), Step::Closed]);
        block_on(l.feed.run(&mut src, &l.sleeper));
        assert_eq!(src.connect_count, 2, "reconnected after the close");
        assert_eq!(*l.sleeper.slept.lock().unwrap(), vec![Duration::from_secs(1)]);
        let c = l.feed.cache().get("BTCUSDT").unwrap();
        // The scripted source reconnects OK but has no more messages: data is 6 s old and stale.
        assert_eq!(c.age_ms, 6_000);
        assert!(c.health.stale && !c.is_fresh());
    }

    #[test]
    fn error_event_behaves_like_close() {
        let l = make_loop();
        let mut src = Scripted::new(&l.clock, vec![], vec![Step::Text(frame(&[("A", "1")])), Step::Error]);
        block_on(l.feed.run(&mut src, &l.sleeper));
        assert_eq!(src.connect_count, 2);
        let ev = l.sink.events();
        assert_eq!(ev[0].event_type, "FETCH_ERROR");
        assert_eq!(ev[0].payload["error_kind"], "Disconnected");
    }

    #[test]
    fn silent_connection_triggers_a_reconnect_without_any_close_event() {
        let l = make_loop();
        // message, then idle wake-ups: 1500 + 1500 = 3000 (ok), +500 = 3500 > 3000 => reconnect
        let mut src = Scripted::new(
            &l.clock,
            vec![],
            vec![Step::Text(frame(&[("A", "1")])), Step::Idle(1_500), Step::Idle(1_500), Step::Idle(500)],
        );
        block_on(l.feed.run(&mut src, &l.sleeper));
        assert_eq!(src.connect_count, 2);
        assert_eq!(l.sleeper.slept.lock().unwrap().len(), 1);
        assert_eq!(l.sink.events()[0].payload["error_kind"], "Stale");
    }

    #[test]
    fn connection_failures_back_off_exponentially_and_a_message_resets_the_delay() {
        let l = make_loop();
        let refused = || Err(AdapterError::network("refused"));
        let mut src = Scripted::new(
            &l.clock,
            vec![refused(), refused(), refused(), Ok(())],
            vec![Step::Text(frame(&[("A", "1")])), Step::Closed],
        );
        block_on(l.feed.run(&mut src, &l.sleeper));
        // 1s, 2s, 4s after three refusals; message resets; close -> 1s again.
        let secs: Vec<u64> = l.sleeper.slept.lock().unwrap().iter().map(Duration::as_secs).collect();
        assert_eq!(secs, vec![1, 2, 4, 1]);
    }

    #[test]
    fn repeated_connection_failures_write_one_event_and_recovery_writes_one_more() {
        let l = make_loop();
        let refused = || Err(AdapterError::network("refused"));
        let mut src = Scripted::new(
            &l.clock,
            vec![refused(), refused(), refused(), refused(), refused(), Ok(())],
            vec![Step::Text(frame(&[("A", "1")]))],
        );
        block_on(l.feed.run(&mut src, &l.sleeper));
        let types: Vec<String> = l.sink.events().into_iter().map(|e| e.event_type).collect();
        assert_eq!(types, vec!["FETCH_ERROR", "FEED_RECOVERED"]);
        assert_eq!(l.feed.cache().health().consecutive_failures, 0);
    }

    // ------------------------------------------------- real stream (ignored)

    /// Wraps a source: records (receive time, text) and stops after `secs` of wall time.
    struct Recording<S> {
        inner: S,
        started: std::time::Instant,
        secs: u64,
        frames: Vec<(i64, String)>,
        clock: crate::ports::SystemClock,
    }

    impl<S: MessageSource> MessageSource for Recording<S> {
        async fn connect(&mut self) -> Result<(), AdapterError> {
            self.inner.connect().await
        }
        async fn next_event(&mut self, wait: Duration) -> FeedEvent {
            if self.started.elapsed() >= Duration::from_secs(self.secs) {
                return FeedEvent::Shutdown;
            }
            let ev = self.inner.next_event(wait).await;
            if let FeedEvent::Text(t) = &ev {
                self.frames.push((self.clock.now_ms(), t.clone()));
            }
            ev
        }
    }

    fn percentile(sorted: &[i64], p: f64) -> i64 {
        let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[idx]
    }

    /// Run: `cargo test -p tong-funding real_binance_stream_25s -- --ignored --nocapture`
    /// Public, read-only stream. Connects for 25 s through the real `MarkPriceFeed::run` loop.
    #[test]
    #[ignore = "network: connects to the public Binance stream for 25 seconds"]
    fn real_binance_stream_25s_update_intervals() {
        use std::collections::HashMap;
        let clock: Arc<dyn Clock> = Arc::new(crate::ports::SystemClock);
        let sink = Arc::new(MemoryEventSink::default());
        let feed = MarkPriceFeed::new(clock.clone(), sink.clone(), 1_000);
        let mut src = Recording {
            inner: TungsteniteSource::new(crate::exchange::public::feed_endpoints::BINANCE_MARK_PRICE_WS_URL),
            started: std::time::Instant::now(),
            secs: 25,
            frames: Vec::new(),
            clock: crate::ports::SystemClock,
        };
        block_on(feed.run(&mut src, &TokioSleeper));

        let mut per_symbol: HashMap<String, Vec<i64>> = HashMap::new();
        let mut sizes = Vec::new();
        for (at, text) in &src.frames {
            let entries = parse_mark_price_message(text, *at).expect("real frame parses");
            sizes.push(entries.len());
            for e in entries {
                per_symbol.entry(e.symbol).or_default().push(e.observed_at);
            }
        }
        let mut gaps: Vec<i64> = per_symbol.values().flat_map(|v| v.windows(2).map(|w| w[1] - w[0])).collect();
        gaps.sort_unstable();
        println!("frames: {}, symbols: {}, symbols per frame (min/max): {:?}/{:?}", src.frames.len(), per_symbol.len(), sizes.iter().min(), sizes.iter().max());
        println!("per-symbol update intervals over {} samples: p50 = {} ms, p95 = {} ms, min = {} ms, max = {} ms",
            gaps.len(), percentile(&gaps, 0.5), percentile(&gaps, 0.95), gaps[0], gaps[gaps.len() - 1]);
        if let Some((_, first)) = src.frames.first() {
            let head: String = first.chars().take(400).collect();
            println!("first frame head: {head}");
        }
        println!("events: {:?}", sink.events().iter().map(|e| e.event_type.clone()).collect::<Vec<_>>());
        let snap = feed.cache().health();
        println!("final health: {snap:?}");
        assert!(!src.frames.is_empty(), "no frame received in 25 s");
        assert!(feed.cache().len() > 100);
    }
}

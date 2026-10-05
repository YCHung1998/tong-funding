//! Interfaces shared across modules so they can be built and tested independently:
//! time, secrets and the event sink. Real implementations: `SystemClock` here, the Keychain
//! `SecretProvider` and the SQLite `EventSink` in `store`. Fakes live next to the traits.

#![allow(dead_code)]

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tong_funding_core::types::Exchange;

/// Injected time source (Unix milliseconds). Nothing but `SystemClock` may read the wall clock.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
    }
}

/// A clock tests can move by hand; clones share the same time.
#[derive(Clone, Default)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    pub fn new(ms: i64) -> Self {
        ManualClock(Arc::new(AtomicI64::new(ms)))
    }
    pub fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretName {
    ApiKey,
    ApiSecret,
    /// OKX only.
    Passphrase,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("secret store unavailable: {0}")]
    Unavailable(String),
}

/// Source of API credentials. `Ok(None)` = not stored; callers must treat that, and any error,
/// as "not connected" and send no request.
pub trait SecretProvider: Send + Sync {
    fn get(&self, exchange: Exchange, name: SecretName) -> Result<Option<String>, SecretError>;
}

/// Append-only business event sink (`events` table in production). Payloads must already be
/// free of secrets: run text through `tong_funding_core::redact::redact_secrets` first.
pub trait EventSink: Send + Sync {
    fn emit(&self, event_type: &str, pair_id: Option<&str>, payload: serde_json::Value);
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedEvent {
    pub event_type: String,
    pub pair_id: Option<String>,
    pub payload: serde_json::Value,
}

/// In-memory sink for tests.
#[derive(Default)]
pub struct MemoryEventSink(Mutex<Vec<RecordedEvent>>);

impl MemoryEventSink {
    pub fn events(&self) -> Vec<RecordedEvent> {
        self.0.lock().unwrap().clone()
    }
}

impl EventSink for MemoryEventSink {
    fn emit(&self, event_type: &str, pair_id: Option<&str>, payload: serde_json::Value) {
        self.0.lock().unwrap().push(RecordedEvent {
            event_type: event_type.to_string(),
            pair_id: pair_id.map(str::to_string),
            payload,
        });
    }
}

/// In-memory secrets for tests.
#[derive(Default)]
pub struct MemorySecrets(Mutex<Vec<((Exchange, &'static str), String)>>);

impl MemorySecrets {
    pub fn with(self, exchange: Exchange, name: SecretName, value: &str) -> Self {
        self.0.lock().unwrap().push(((exchange, name_key(name)), value.to_string()));
        self
    }
}

fn name_key(n: SecretName) -> &'static str {
    match n {
        SecretName::ApiKey => "key",
        SecretName::ApiSecret => "secret",
        SecretName::Passphrase => "passphrase",
    }
}

impl SecretProvider for MemorySecrets {
    fn get(&self, exchange: Exchange, name: SecretName) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().iter().find(|(k, _)| *k == (exchange, name_key(name))).map(|(_, v)| v.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told_and_clones_share_time() {
        let c = ManualClock::new(1_000);
        let d = c.clone();
        assert_eq!(c.now_ms(), 1_000);
        c.advance(250);
        assert_eq!(d.now_ms(), 1_250);
        d.set(5);
        assert_eq!(c.now_ms(), 5);
    }

    #[test]
    fn memory_secrets_distinguish_exchange_and_name() {
        let s = MemorySecrets::default().with(Exchange::Binance, SecretName::ApiKey, "k");
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("k"));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap(), None);
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn memory_event_sink_records_in_order() {
        let sink = MemoryEventSink::default();
        sink.emit("A", None, serde_json::json!({"n": 1}));
        sink.emit("B", Some("p1"), serde_json::json!({}));
        let ev = sink.events();
        assert_eq!((ev[0].event_type.as_str(), ev[1].pair_id.as_deref()), ("A", Some("p1")));
    }
}

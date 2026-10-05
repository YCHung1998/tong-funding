//! Freshness-aware cache (spec: feed-health, "快取讀取介面不得只回傳價格").
//! Every read returns [`Cached`], which always carries `observed_at`, `exchange_timestamp` and the
//! health of the owning source *at read time*; there is no price-only accessor. Cached data is
//! never usable for the pre-order check, which re-fetches a single symbol instead.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tong_funding_core::types::{Price, Rate};

use super::feed::{FeedHealth, HealthSnapshot};
use crate::ports::Clock;

/// One mark-price observation of a symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkPriceEntry {
    pub symbol: String,
    pub funding_rate: Rate,
    pub next_funding_time: i64,
    pub mark_price: Price,
    /// Binance event time `E`.
    pub exchange_timestamp: i64,
    /// Local time at which the message containing this symbol arrived.
    pub observed_at: i64,
}

/// A cached value together with everything needed to judge it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cached<T> {
    pub data: T,
    pub observed_at: i64,
    pub exchange_timestamp: i64,
    /// Health of the source at the moment of the read.
    pub health: HealthSnapshot,
    /// Read time − `observed_at`.
    pub age_ms: i64,
}

impl<T> Cached<T> {
    pub fn new(data: T, observed_at: i64, exchange_timestamp: i64, health: HealthSnapshot, now_ms: i64) -> Self {
        Self { data, observed_at, exchange_timestamp, health, age_ms: now_ms - observed_at }
    }

    /// Fresh only if the source is healthy AND this entry itself is younger than the source's
    /// stale threshold (a symbol can go quiet while the connection is fine).
    pub fn is_fresh(&self) -> bool {
        !self.health.stale && self.age_ms <= self.health.stale_threshold_ms
    }
}

/// Latest mark price per symbol. Updated per message: symbols absent from a message keep their
/// previous data and their own `observed_at`.
pub struct MarkPriceCache {
    clock: Arc<dyn Clock>,
    health: Arc<FeedHealth>,
    entries: Mutex<HashMap<String, MarkPriceEntry>>,
}

impl MarkPriceCache {
    pub fn new(clock: Arc<dyn Clock>, health: Arc<FeedHealth>) -> Self {
        Self { clock, health, entries: Mutex::new(HashMap::new()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, MarkPriceEntry>> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Merges the entries of one message (only those symbols are touched).
    pub fn apply(&self, entries: Vec<MarkPriceEntry>) {
        let mut map = self.lock();
        for e in entries {
            map.insert(e.symbol.clone(), e);
        }
    }

    fn wrap(&self, e: MarkPriceEntry, health: &HealthSnapshot, now: i64) -> Cached<MarkPriceEntry> {
        let (observed_at, exchange_timestamp) = (e.observed_at, e.exchange_timestamp);
        Cached::new(e, observed_at, exchange_timestamp, health.clone(), now)
    }

    pub fn get(&self, symbol: &str) -> Option<Cached<MarkPriceEntry>> {
        let entry = self.lock().get(symbol).cloned()?;
        Some(self.wrap(entry, &self.health.snapshot(), self.clock.now_ms()))
    }

    /// All symbols, each with the health snapshot taken once for the whole read.
    pub fn snapshot(&self) -> Vec<Cached<MarkPriceEntry>> {
        let entries: Vec<MarkPriceEntry> = self.lock().values().cloned().collect();
        let (health, now) = (self.health.snapshot(), self.clock.now_ms());
        entries.into_iter().map(|e| self.wrap(e, &health, now)).collect()
    }

    pub fn health(&self) -> HealthSnapshot {
        self.health.snapshot()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tong_funding_core::types::Decimal;

    use super::*;
    use crate::ports::{ManualClock, MemoryEventSink};

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn entry(symbol: &str, price: &str, observed_at: i64) -> MarkPriceEntry {
        MarkPriceEntry {
            symbol: symbol.into(),
            funding_rate: dec("0.0001"),
            next_funding_time: 1_791_216_000_000,
            mark_price: dec(price),
            exchange_timestamp: observed_at - 40,
            observed_at,
        }
    }

    fn setup(clock: &ManualClock) -> (Arc<FeedHealth>, MarkPriceCache) {
        let sink = Arc::new(MemoryEventSink::default());
        let health = Arc::new(FeedHealth::new("binance_ws", 1_000, 1_000, Arc::new(clock.clone()), sink));
        let cache = MarkPriceCache::new(Arc::new(clock.clone()), health.clone());
        (health, cache)
    }

    #[test]
    fn read_carries_observed_at_exchange_timestamp_health_and_age() {
        let clock = ManualClock::new(10_000);
        let (health, cache) = setup(&clock);
        health.on_connected();
        health.on_success();
        cache.apply(vec![entry("BTCUSDT", "86186.20", 10_000)]);
        clock.advance(400);
        let c = cache.get("BTCUSDT").expect("cached");
        assert_eq!((c.observed_at, c.exchange_timestamp, c.age_ms), (10_000, 9_960, 400));
        assert_eq!(c.data.mark_price, dec("86186.20"));
        assert!(c.health.connected && !c.health.stale);
        assert!(c.is_fresh());
        assert!(cache.get("NOPEUSDT").is_none());
    }

    #[test]
    fn scenario_read_after_disconnect_shows_disconnected_status_and_old_observed_at() {
        let clock = ManualClock::new(10_000);
        let (health, cache) = setup(&clock);
        health.on_connected();
        health.on_success();
        cache.apply(vec![entry("BTCUSDT", "86186.20", 10_000)]);
        clock.advance(5_000);
        health.on_disconnect("close frame");
        let c = cache.get("BTCUSDT").unwrap();
        assert!(!c.health.connected);
        assert!(c.health.stale);
        assert_eq!(c.observed_at, 10_000);
        assert_eq!(c.age_ms, 5_000);
        assert!(!c.is_fresh(), "data from a disconnected source must not be marked fresh");
    }

    #[test]
    fn stale_source_makes_every_entry_not_fresh_even_if_entry_is_young() {
        let clock = ManualClock::new(0);
        let (health, cache) = setup(&clock);
        health.on_connected();
        health.on_success();
        cache.apply(vec![entry("A", "1", 0)]);
        clock.advance(3_001); // source silent beyond max(1000, 3*1000)
        assert!(cache.get("A").unwrap().health.stale);
        assert!(!cache.get("A").unwrap().is_fresh());
    }

    #[test]
    fn a_quiet_symbol_on_a_healthy_source_is_not_fresh() {
        let clock = ManualClock::new(0);
        let (health, cache) = setup(&clock);
        health.on_connected();
        health.on_success();
        cache.apply(vec![entry("OLD", "1", 0)]);
        for t in [2_000, 4_000] {
            clock.set(t);
            health.on_success(); // the source keeps delivering other symbols
            cache.apply(vec![entry("NEW", "2", t)]);
        }
        let old = cache.get("OLD").unwrap();
        assert!(!old.health.stale, "source itself is healthy");
        assert_eq!(old.age_ms, 4_000);
        assert!(!old.is_fresh(), "entry older than the 3000 ms threshold");
        assert!(cache.get("NEW").unwrap().is_fresh());
    }

    #[test]
    fn never_updated_cache_reports_stale_health() {
        let clock = ManualClock::new(0);
        let (_health, cache) = setup(&clock);
        assert!(cache.health().stale);
        assert!(cache.is_empty());
    }

    #[test]
    fn partial_update_does_not_clear_other_symbols_and_keeps_their_own_observed_at() {
        let clock = ManualClock::new(1_000);
        let (_h, cache) = setup(&clock);
        cache.apply((0..745).map(|i| entry(&format!("S{i}USDT"), "1.5", 1_000)).collect());
        assert_eq!(cache.len(), 745);
        clock.advance(1_000);
        cache.apply((0..213).map(|i| entry(&format!("S{i}USDT"), "2.5", 2_000)).collect());
        assert_eq!(cache.len(), 745, "nothing cleared");
        let touched = cache.get("S5USDT").unwrap();
        let untouched = cache.get("S500USDT").unwrap();
        assert_eq!((touched.observed_at, touched.data.mark_price), (2_000, dec("2.5")));
        assert_eq!((untouched.observed_at, untouched.data.mark_price), (1_000, dec("1.5")));
        assert_eq!(cache.snapshot().len(), 745);
    }
}

//! Bybit public ticker WebSocket feed (`tickers.<SYMBOL>` on the v5 linear stream), the Bybit twin of
//! the Binance mark-price feed. The REST poll stays the base; this feed overlays newer rate / mark
//! price / next settlement on top of it ([`overlay_bybit`]). Layers, from pure to impure:
//! - [`parse_ticker_message`]: pure parsing (Decimal strings, snapshot vs delta, acks, pong);
//! - [`TickerCache`]: a snapshot replaces a symbol, a delta merges only the fields it carries;
//! - [`overlay_bybit`]: pure merge of complete entries onto the REST observations;
//! - [`BybitTickerFeed::run`]: connect, subscribe in chunks, ping, refresh, reconnect, health.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tong_funding_core::funding::FundingObservation;
use tong_funding_core::redact::redact_secrets;
use tong_funding_core::types::{Decimal, Price, Rate};

use super::feed::{FeedEvent, FeedHealth, HealthSnapshot, MessageSource, ReconnectBackoff, Sleeper, EVENT_FETCH_ERROR, IDLE_CHECK};
use crate::exchange::public::feed_endpoints::{BYBIT_SUBSCRIBE_CHUNK, BYBIT_WS_PING_MS};
use crate::ports::{EventSink, TimeSource};

/// Health source name of this feed (system log, `feed-health`).
pub const SOURCE: &str = "bybit_ws_ticker";
/// Expected period of the feed as a whole: the stream carries ~800 messages per second, so any gap
/// of a few seconds is a dead connection (a quiet *symbol* is not).
pub const EXPECTED_PERIOD_MS: i64 = 1_000;
/// How often the loop looks for rollovers and new symbols.
pub const REFRESH_EVERY_MS: i64 = 5_000;
/// A settlement time this far in the past without a newer one means the rollover was not seen.
pub const ROLLOVER_GRACE_MS: i64 = 15_000;
/// Most symbols re-subscribed in one refresh pass (a mass rollover is spread over several passes).
pub const MAX_REFRESH_PER_PASS: usize = 100;
/// A symbol re-subscribed for a rollover is not asked again for this long.
pub const REFRESH_RETRY_MS: i64 = 30_000;

// ---- parsing ---------------------------------------------------------------------------------

/// The ticker fields this system uses; each is `None` when the message did not carry it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TickerFields {
    pub symbol: String,
    pub mark_price: Option<Price>,
    pub funding_rate: Option<Rate>,
    pub next_funding_time: Option<i64>,
    pub funding_interval_secs: Option<i64>,
    pub volume_24h_quote: Option<Decimal>,
    /// Message `ts` (exchange time, ms).
    pub exchange_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Snapshot(TickerFields),
    Delta(TickerFields),
    SubscribeAck { success: bool, ret_msg: String },
    Pong,
    /// A well-formed message that is not a ticker (other topics, other ops).
    Other,
    Malformed(String),
}

/// An absent or empty field is `None`; a present field that cannot be read is an error (never a
/// guessed value).
fn opt_decimal(data: &Value, key: &str) -> Result<Option<Decimal>, String> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Decimal::from_str(s.trim()).map(Some).map_err(|_| format!("`{key}` is not a number: {s:?}")),
        Some(other) => Err(format!("`{key}` has the wrong type: {other}")),
    }
}

fn opt_i64(data: &Value, key: &str) -> Result<Option<i64>, String> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => s.trim().parse::<i64>().map(Some).map_err(|_| format!("`{key}` is not an integer: {s:?}")),
        Some(other) => Err(format!("`{key}` has the wrong type: {other}")),
    }
}

fn ticker_fields(data: &Value, ts: i64) -> Result<TickerFields, String> {
    let symbol = data.get("symbol").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or("ticker data has no symbol")?;
    let funding_interval_secs = opt_i64(data, "fundingIntervalHour")?.and_then(|h| h.checked_mul(3600)).filter(|s| *s > 0);
    Ok(TickerFields {
        symbol: symbol.to_string(),
        mark_price: opt_decimal(data, "markPrice")?,
        funding_rate: opt_decimal(data, "fundingRate")?,
        next_funding_time: opt_i64(data, "nextFundingTime")?,
        funding_interval_secs,
        volume_24h_quote: opt_decimal(data, "turnover24h")?,
        exchange_timestamp: ts,
    })
}

/// Parses one text frame of the public stream.
pub fn parse_ticker_message(text: &str) -> Parsed {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Parsed::Malformed(format!("not JSON: {e}")),
    };
    let Some(obj) = v.as_object() else { return Parsed::Malformed("not a JSON object".into()) };
    if let Some(topic) = obj.get("topic").and_then(Value::as_str) {
        if !topic.starts_with("tickers.") {
            return Parsed::Other;
        }
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(ts) = obj.get("ts").and_then(Value::as_i64) else { return Parsed::Malformed("ticker message has no ts".into()) };
        let Some(data) = obj.get("data") else { return Parsed::Malformed("ticker message has no data".into()) };
        return match (kind, ticker_fields(data, ts)) {
            (_, Err(e)) => Parsed::Malformed(e),
            ("snapshot", Ok(f)) => Parsed::Snapshot(f),
            ("delta", Ok(f)) => Parsed::Delta(f),
            (other, Ok(_)) => Parsed::Malformed(format!("unknown ticker message type {other:?}")),
        };
    }
    match obj.get("op").and_then(Value::as_str) {
        Some("subscribe") | Some("unsubscribe") => Parsed::SubscribeAck {
            success: obj.get("success").and_then(Value::as_bool).unwrap_or(false),
            ret_msg: obj.get("ret_msg").and_then(Value::as_str).unwrap_or("").to_string(),
        },
        Some("pong") => Parsed::Pong,
        Some("ping") if obj.get("ret_msg").and_then(Value::as_str) == Some("pong") => Parsed::Pong,
        _ => Parsed::Other,
    }
}

// ---- cache -----------------------------------------------------------------------------------

/// A complete ticker: mark price, funding rate and next settlement are all known.
#[derive(Debug, Clone, PartialEq)]
pub struct TickerEntry {
    pub symbol: String,
    pub mark_price: Price,
    pub funding_rate: Rate,
    pub next_funding_time: i64,
    pub funding_interval_secs: Option<i64>,
    pub volume_24h_quote: Option<Decimal>,
    pub exchange_timestamp: i64,
    /// Our receive time of the latest message of this symbol.
    pub observed_at: i64,
}

#[derive(Debug, Clone, Default)]
struct SymbolState {
    mark_price: Option<Price>,
    funding_rate: Option<Rate>,
    next_funding_time: Option<i64>,
    funding_interval_secs: Option<i64>,
    volume_24h_quote: Option<Decimal>,
    exchange_timestamp: i64,
    observed_at: i64,
}

impl SymbolState {
    fn merge(&mut self, f: &TickerFields, observed_at: i64) {
        self.mark_price = f.mark_price.or(self.mark_price);
        self.funding_rate = f.funding_rate.or(self.funding_rate);
        self.next_funding_time = f.next_funding_time.or(self.next_funding_time);
        self.funding_interval_secs = f.funding_interval_secs.or(self.funding_interval_secs);
        self.volume_24h_quote = f.volume_24h_quote.or(self.volume_24h_quote);
        self.exchange_timestamp = self.exchange_timestamp.max(f.exchange_timestamp);
        self.observed_at = observed_at;
    }

    fn entry(&self, symbol: &str) -> Option<TickerEntry> {
        Some(TickerEntry {
            symbol: symbol.to_string(),
            mark_price: self.mark_price?,
            funding_rate: self.funding_rate?,
            next_funding_time: self.next_funding_time?,
            funding_interval_secs: self.funding_interval_secs,
            volume_24h_quote: self.volume_24h_quote,
            exchange_timestamp: self.exchange_timestamp,
            observed_at: self.observed_at,
        })
    }
}

/// Latest ticker state per symbol. A snapshot replaces the symbol; a delta merges only the fields it
/// carries and is dropped when older than what is stored (a late message must not overwrite newer data).
#[derive(Default)]
pub struct TickerCache {
    map: Mutex<HashMap<String, SymbolState>>,
}

impl TickerCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SymbolState>> {
        self.map.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn apply_snapshot(&self, f: &TickerFields, observed_at: i64) {
        let mut state = SymbolState::default();
        state.merge(f, observed_at);
        self.lock().insert(f.symbol.clone(), state);
    }

    pub fn apply_delta(&self, f: &TickerFields, observed_at: i64) {
        let mut map = self.lock();
        let state = map.entry(f.symbol.clone()).or_default();
        if state.exchange_timestamp > f.exchange_timestamp {
            return;
        }
        state.merge(f, observed_at);
    }

    pub fn apply(&self, parsed: &Parsed, observed_at: i64) {
        match parsed {
            Parsed::Snapshot(f) => self.apply_snapshot(f, observed_at),
            Parsed::Delta(f) => self.apply_delta(f, observed_at),
            _ => {}
        }
    }

    pub fn entry(&self, symbol: &str) -> Option<TickerEntry> {
        self.lock().get(symbol)?.entry(symbol)
    }

    /// Every complete entry.
    pub fn entries(&self) -> Vec<TickerEntry> {
        self.lock().iter().filter_map(|(s, st)| st.entry(s)).collect()
    }

    /// Forgets everything (after a reconnect the snapshots arrive again; nothing stale is republished).
    pub fn clear(&self) {
        self.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Symbols whose next settlement is more than `grace_ms` in the past: the rollover was not seen.
    pub fn needs_snapshot(&self, now_ms: i64, grace_ms: i64) -> Vec<String> {
        let mut v: Vec<String> = self.lock().iter().filter(|(_, st)| st.next_funding_time.is_some_and(|t| t.saturating_add(grace_ms) < now_ms)).map(|(s, _)| s.clone()).collect();
        v.sort();
        v
    }
}

// ---- overlay ---------------------------------------------------------------------------------

/// The REST observations improved by newer WebSocket entries. Listing, rules and everything the
/// stream does not carry come from REST; a symbol REST does not list is never invented, and an entry
/// not newer than the REST observation changes nothing.
pub fn overlay_bybit(rest: &HashMap<String, FundingObservation>, entries: &[TickerEntry]) -> Vec<FundingObservation> {
    let mut out = Vec::new();
    for e in entries {
        let Some(base) = rest.get(&e.symbol) else { continue };
        if e.exchange_timestamp <= base.exchange_timestamp {
            continue;
        }
        let mut o = base.clone();
        o.funding_rate = e.funding_rate;
        o.mark_price = e.mark_price;
        o.next_funding_time = e.next_funding_time;
        o.exchange_timestamp = e.exchange_timestamp;
        o.observed_at = e.observed_at;
        if e.volume_24h_quote.is_some() {
            o.volume_24h_quote = e.volume_24h_quote;
        }
        if e.funding_interval_secs.is_some() {
            o.funding_interval_secs = e.funding_interval_secs;
        }
        out.push(o);
    }
    out
}

// ---- the feed --------------------------------------------------------------------------------

fn subscribe_requests(op: &str, symbols: &[String]) -> Vec<String> {
    symbols.chunks(BYBIT_SUBSCRIBE_CHUNK).map(|c| json!({ "op": op, "args": c.iter().map(|s| format!("tickers.{s}")).collect::<Vec<_>>() }).to_string()).collect()
}

pub struct BybitTickerFeed {
    clock: Arc<dyn TimeSource>,
    health: Arc<FeedHealth>,
    cache: Arc<TickerCache>,
    sink: Arc<dyn EventSink>,
}

impl BybitTickerFeed {
    pub fn new(clock: Arc<dyn TimeSource>, sink: Arc<dyn EventSink>, stale_data_threshold_ms: i64) -> Self {
        let health = Arc::new(FeedHealth::new(SOURCE, EXPECTED_PERIOD_MS, stale_data_threshold_ms, clock.clone(), sink.clone()));
        Self { clock, health, cache: Arc::new(TickerCache::new()), sink }
    }

    pub fn cache(&self) -> Arc<TickerCache> {
        self.cache.clone()
    }

    pub fn health(&self) -> Arc<FeedHealth> {
        self.health.clone()
    }

    pub fn health_snapshot(&self) -> HealthSnapshot {
        self.health.snapshot()
    }

    /// Handles one text frame. Returns true when it was a well-formed message (a sign of life);
    /// malformed frames are skipped and are not.
    pub fn handle_text(&self, text: &str) -> bool {
        let parsed = parse_ticker_message(text);
        match &parsed {
            Parsed::Malformed(_) => return false,
            Parsed::SubscribeAck { success: false, ret_msg } => {
                self.sink.emit(EVENT_FETCH_ERROR, None, json!({ "source": SOURCE, "error_kind": "Exchange", "message": redact_secrets(&format!("subscribe rejected: {ret_msg}")) }));
            }
            _ => self.cache.apply(&parsed, self.clock.now_ms()),
        }
        self.health.on_success();
        true
    }

    /// Connect / subscribe / read / reconnect until the source reports `Shutdown`. `symbols` is asked
    /// for the current list (the keys of the REST poll); nothing is subscribed while it is empty.
    pub async fn run<S: MessageSource, Z: Sleeper, P: Fn() -> Vec<String>>(&self, source: &mut S, symbols: P, sleeper: &Z) {
        let mut backoff = ReconnectBackoff::new();
        loop {
            let initial = loop {
                let list = symbols();
                if !list.is_empty() {
                    break list;
                }
                sleeper.sleep(Duration::from_secs(1)).await;
            };
            match source.connect().await {
                Err(e) => {
                    self.health.on_failure(&e);
                    sleeper.sleep(backoff.next_delay()).await;
                    continue;
                }
                Ok(()) => self.health.on_connected(),
            }
            self.cache.clear();
            let mut subscribed: HashSet<String> = initial.iter().cloned().collect();
            let mut asked: HashMap<String, i64> = HashMap::new();
            let mut sent_ok = true;
            for req in subscribe_requests("subscribe", &initial) {
                if let Err(e) = source.send(req).await {
                    self.health.on_disconnect(&e.to_string());
                    sent_ok = false;
                    break;
                }
            }
            let now = self.clock.mono_ms();
            let (mut last_ping, mut last_refresh) = (now, now);
            loop {
                if !sent_ok {
                    break; // the subscribe could not be sent: reconnect (clippy: `sent_ok` never changes in the loop)
                }
                match source.next_event(IDLE_CHECK).await {
                    FeedEvent::Text(t) => {
                        if self.handle_text(&t) {
                            backoff.reset();
                        }
                    }
                    FeedEvent::Idle => {}
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
                if self.health.check_silence() {
                    break;
                }
                let mono = self.clock.mono_ms();
                if mono - last_ping >= BYBIT_WS_PING_MS as i64 {
                    last_ping = mono;
                    if let Err(e) = source.send(json!({ "op": "ping" }).to_string()).await {
                        self.health.on_disconnect(&e.to_string());
                        break;
                    }
                }
                if mono - last_refresh >= REFRESH_EVERY_MS {
                    last_refresh = mono;
                    if let Err(e) = self.refresh(source, &symbols, &mut subscribed, &mut asked, mono).await {
                        self.health.on_disconnect(&e.to_string());
                        break;
                    }
                }
            }
            sleeper.sleep(backoff.next_delay()).await;
        }
    }

    /// Subscribes symbols that appeared since the last subscribe and re-subscribes (unsubscribe, then
    /// subscribe: forces a fresh snapshot) the symbols whose settlement rollover was not seen.
    async fn refresh<S: MessageSource, P: Fn() -> Vec<String>>(&self, source: &mut S, symbols: &P, subscribed: &mut HashSet<String>, asked: &mut HashMap<String, i64>, mono: i64) -> Result<(), crate::exchange::error::AdapterError> {
        let mut fresh: Vec<String> = symbols().into_iter().filter(|s| !subscribed.contains(s)).collect();
        fresh.sort();
        fresh.dedup();
        let mut budget = MAX_REFRESH_PER_PASS;
        fresh.truncate(budget);
        budget -= fresh.len();
        let mut again: Vec<String> = self.cache.needs_snapshot(self.clock.now_ms(), ROLLOVER_GRACE_MS).into_iter().filter(|s| asked.get(s).is_none_or(|at| mono - at >= REFRESH_RETRY_MS)).collect();
        again.truncate(budget);
        for s in &fresh {
            subscribed.insert(s.clone());
        }
        for s in &again {
            asked.insert(s.clone(), mono);
        }
        for req in subscribe_requests("unsubscribe", &again).into_iter().chain(subscribe_requests("subscribe", &again)).chain(subscribe_requests("subscribe", &fresh)) {
            source.send(req).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "bybit_feed_tests.rs"]
mod tests;

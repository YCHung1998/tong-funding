//! bybit-ws-feed: fixtures (real recorded frames), parser, cache, overlay and the feed loop.
use std::collections::VecDeque;
use std::future::Future;

use tong_funding_core::funding::DataStatus;
use tong_funding_core::types::Exchange;

use super::*;
use crate::exchange::error::AdapterError;
use crate::ports::{Clock, ManualClock, MemoryEventSink};

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

// Real frames recorded from the public stream (app/tests/fixtures/bybit/ws_frames.meta).
const SNAPSHOT: &str = include_str!("../../../tests/fixtures/bybit/ws_tickers_snapshot_btcusdt.json");
const DELTA_RATE: &str = include_str!("../../../tests/fixtures/bybit/ws_tickers_delta_btcusdt.json");
const DELTA_MARK: &str = include_str!("../../../tests/fixtures/bybit/ws_tickers_delta_markprice_only.json");
const ACK: &str = include_str!("../../../tests/fixtures/bybit/ws_subscribe_ack.json");
const ACK_ERR: &str = include_str!("../../../tests/fixtures/bybit/ws_subscribe_error.json");
const PONG: &str = include_str!("../../../tests/fixtures/bybit/ws_pong.json");

// ---- fixtures ----------------------------------------------------------------------------------

#[test]
fn every_recorded_frame_is_valid_json_with_the_documented_shape() {
    for (name, raw) in [("snapshot", SNAPSHOT), ("delta_rate", DELTA_RATE), ("delta_mark", DELTA_MARK), ("ack", ACK), ("ack_err", ACK_ERR), ("pong", PONG)] {
        let v: Value = serde_json::from_str(raw).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(v.is_object(), "{name}");
    }
    let s: Value = serde_json::from_str(SNAPSHOT).unwrap();
    assert_eq!((s["topic"].as_str(), s["type"].as_str()), (Some("tickers.BTCUSDT"), Some("snapshot")));
    for key in ["markPrice", "fundingRate", "nextFundingTime", "fundingIntervalHour", "turnover24h"] {
        assert!(s["data"][key].is_string(), "snapshot carries {key}");
    }
    assert_eq!(serde_json::from_str::<Value>(DELTA_RATE).unwrap()["type"], "delta");
    assert_eq!(serde_json::from_str::<Value>(ACK_ERR).unwrap()["success"], false);
}

// ---- parser --------------------------------------------------------------------------------------

#[test]
fn the_recorded_snapshot_parses_to_every_field() {
    let Parsed::Snapshot(f) = parse_ticker_message(SNAPSHOT) else { panic!("not a snapshot") };
    assert_eq!(f.symbol, "BTCUSDT");
    assert_eq!(f.mark_price, Some(dec("81827.90")));
    assert_eq!(f.funding_rate, Some(dec("0.00001539")));
    assert_eq!(f.next_funding_time, Some(1_791_993_600_000));
    assert_eq!(f.funding_interval_secs, Some(8 * 3600));
    assert_eq!(f.volume_24h_quote, Some(dec("5085103442.7473")));
    assert_eq!(f.exchange_timestamp, 1_791_992_364_230);
}

#[test]
fn a_delta_carries_only_what_changed() {
    let Parsed::Delta(rate) = parse_ticker_message(DELTA_RATE) else { panic!("not a delta") };
    assert_eq!((rate.funding_rate, rate.mark_price, rate.next_funding_time), (Some(dec("0.00001496")), None, None));
    let Parsed::Delta(mark) = parse_ticker_message(DELTA_MARK) else { panic!("not a delta") };
    assert_eq!((mark.mark_price, mark.funding_rate, mark.volume_24h_quote), (Some(dec("81832.55")), None, None));
    assert_eq!(mark.exchange_timestamp, 1_791_992_369_429);
}

#[test]
fn acks_and_pong_are_recognised() {
    assert_eq!(parse_ticker_message(ACK), Parsed::SubscribeAck { success: true, ret_msg: String::new() });
    match parse_ticker_message(ACK_ERR) {
        Parsed::SubscribeAck { success: false, ret_msg } => assert!(ret_msg.contains("NOT_A_SYMBOL"), "{ret_msg}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(parse_ticker_message(PONG), Parsed::Pong);
    assert_eq!(parse_ticker_message(r#"{"op":"pong","args":["1"]}"#), Parsed::Pong);
}

#[test]
fn empty_fields_are_none_and_unreadable_ones_make_the_message_malformed() {
    let ok = r#"{"topic":"tickers.X","type":"snapshot","ts":5,"data":{"symbol":"X","markPrice":"1","fundingRate":"","nextFundingTime":"","preOpenPrice":""}}"#;
    let Parsed::Snapshot(f) = parse_ticker_message(ok) else { panic!() };
    assert_eq!((f.funding_rate, f.next_funding_time, f.mark_price), (None, None, Some(dec("1"))));
    for bad in [
        r#"{"topic":"tickers.X","type":"delta","ts":5,"data":{"symbol":"X","markPrice":"abc"}}"#,
        r#"{"topic":"tickers.X","type":"delta","ts":5,"data":{"symbol":"X","markPrice":12.5}}"#,
        r#"{"topic":"tickers.X","type":"delta","ts":5,"data":{"symbol":"X","nextFundingTime":"soon"}}"#,
        r#"{"topic":"tickers.X","type":"delta","data":{"symbol":"X"}}"#,
        r#"{"topic":"tickers.X","type":"delta","ts":5,"data":{"markPrice":"1"}}"#,
        r#"{"topic":"tickers.X","type":"weird","ts":5,"data":{"symbol":"X"}}"#,
        "not json",
        "[1,2]",
    ] {
        assert!(matches!(parse_ticker_message(bad), Parsed::Malformed(_)), "{bad}");
    }
    assert_eq!(parse_ticker_message(r#"{"topic":"orderbook.1.X","type":"delta","ts":1,"data":{}}"#), Parsed::Other);
}

// ---- cache ---------------------------------------------------------------------------------------

fn fields(symbol: &str, ts: i64) -> TickerFields {
    TickerFields { symbol: symbol.into(), exchange_timestamp: ts, ..Default::default() }
}

fn full(symbol: &str, ts: i64, mark: &str, rate: &str, nft: i64) -> TickerFields {
    TickerFields { mark_price: Some(dec(mark)), funding_rate: Some(dec(rate)), next_funding_time: Some(nft), ..fields(symbol, ts) }
}

#[test]
fn snapshot_then_deltas_merge_field_by_field() {
    let c = TickerCache::new();
    c.apply_snapshot(&full("A", 100, "10", "0.0001", 1_000), 7);
    c.apply_delta(&TickerFields { mark_price: Some(dec("11")), ..fields("A", 110) }, 8);
    c.apply_delta(&TickerFields { funding_rate: Some(dec("0.0002")), ..fields("A", 120) }, 9);
    let e = c.entry("A").unwrap();
    assert_eq!((e.mark_price, e.funding_rate, e.next_funding_time), (dec("11"), dec("0.0002"), 1_000));
    assert_eq!((e.exchange_timestamp, e.observed_at), (120, 9));
}

#[test]
fn a_delta_before_any_snapshot_is_kept_but_not_complete_and_a_snapshot_replaces() {
    let c = TickerCache::new();
    c.apply_delta(&TickerFields { mark_price: Some(dec("5")), ..fields("A", 10) }, 1);
    assert_eq!(c.len(), 1);
    assert!(c.entry("A").is_none() && c.entries().is_empty());
    c.apply_snapshot(&full("A", 20, "6", "0.1", 99), 2);
    c.apply_delta(&TickerFields { volume_24h_quote: Some(dec("42")), ..fields("A", 21) }, 3);
    assert_eq!(c.entry("A").unwrap().volume_24h_quote, Some(dec("42")));
    c.apply_snapshot(&full("A", 30, "7", "0.2", 100), 4);
    let e = c.entry("A").unwrap();
    assert_eq!((e.mark_price, e.volume_24h_quote), (dec("7"), None), "a snapshot replaces the symbol, it does not merge");
}

#[test]
fn an_older_delta_never_overwrites_newer_data_and_other_symbols_are_untouched() {
    let c = TickerCache::new();
    c.apply_snapshot(&full("A", 100, "10", "0.1", 1), 1);
    c.apply_snapshot(&full("B", 100, "20", "0.2", 1), 1);
    c.apply_delta(&TickerFields { mark_price: Some(dec("99")), ..fields("A", 90) }, 2);
    assert_eq!(c.entry("A").unwrap().mark_price, dec("10"));
    assert_eq!(c.entry("B").unwrap().mark_price, dec("20"));
}

#[test]
fn rollover_is_detected_after_the_grace_and_a_delta_with_the_new_time_clears_it() {
    let c = TickerCache::new();
    c.apply_snapshot(&full("A", 1, "1", "0.1", 10_000), 1);
    c.apply_snapshot(&full("B", 1, "1", "0.1", 50_000), 1);
    assert!(c.needs_snapshot(25_000, 15_000).is_empty(), "exactly at the grace edge is not yet late");
    assert_eq!(c.needs_snapshot(25_001, 15_000), vec!["A".to_string()]);
    c.apply_delta(&TickerFields { next_funding_time: Some(20_000), funding_rate: Some(dec("0.2")), ..fields("A", 2) }, 2);
    assert!(c.needs_snapshot(25_001, 15_000).is_empty(), "a delta with the new nextFundingTime clears the flag");
}

// ---- overlay -------------------------------------------------------------------------------------

fn obs(symbol: &str, ts: i64) -> FundingObservation {
    FundingObservation {
        exchange: Exchange::Bybit,
        symbol: symbol.into(),
        funding_rate: dec("0.0001"),
        funding_interval_secs: Some(3600),
        next_funding_time: 1_000,
        mark_price: dec("10"),
        volume_24h_quote: Some(dec("500")),
        exchange_timestamp: ts,
        observed_at: ts + 5,
        data_status: DataStatus::Listed,
    }
}

fn entry(symbol: &str, ts: i64) -> TickerEntry {
    TickerEntry { symbol: symbol.into(), mark_price: dec("11"), funding_rate: dec("0.0003"), next_funding_time: 2_000, funding_interval_secs: None, volume_24h_quote: None, exchange_timestamp: ts, observed_at: ts + 1 }
}

#[test]
fn a_newer_entry_overlays_rate_price_and_settlement_and_keeps_the_rest_of_rest() {
    let rest: HashMap<String, FundingObservation> = [("A".to_string(), obs("A", 100))].into();
    let out = overlay_bybit(&rest, &[entry("A", 101)]);
    assert_eq!(out.len(), 1);
    let o = &out[0];
    assert_eq!((o.funding_rate, o.mark_price, o.next_funding_time, o.exchange_timestamp, o.observed_at), (dec("0.0003"), dec("11"), 2_000, 101, 102));
    assert_eq!((o.funding_interval_secs, o.volume_24h_quote), (Some(3600), Some(dec("500"))), "the entry has neither: REST values stay");
    let with = TickerEntry { funding_interval_secs: Some(8 * 3600), volume_24h_quote: Some(dec("9")), ..entry("A", 101) };
    let o = &overlay_bybit(&rest, &[with])[0];
    assert_eq!((o.funding_interval_secs, o.volume_24h_quote), (Some(8 * 3600), Some(dec("9"))));
}

#[test]
fn older_or_equal_entries_and_symbols_rest_does_not_list_change_nothing() {
    let rest: HashMap<String, FundingObservation> = [("A".to_string(), obs("A", 100))].into();
    assert!(overlay_bybit(&rest, &[entry("A", 100), entry("A", 99), entry("GHOST", 500)]).is_empty());
}

// ---- the feed loop -------------------------------------------------------------------------------

enum Step {
    Text(String),
    Closed,
    Idle(i64),
}

struct Scripted {
    clock: ManualClock,
    connects: VecDeque<Result<(), AdapterError>>,
    steps: VecDeque<Step>,
    connect_count: usize,
    sent: Arc<Mutex<Vec<String>>>,
    fail_send_after: Option<usize>,
}

impl Scripted {
    fn new(clock: &ManualClock, steps: Vec<Step>) -> Self {
        Self { clock: clock.clone(), connects: VecDeque::new(), steps: steps.into(), connect_count: 0, sent: Arc::new(Mutex::new(Vec::new())), fail_send_after: None }
    }
    fn sent(&self) -> Vec<Value> {
        self.sent.lock().unwrap().iter().map(|s| serde_json::from_str(s).unwrap()).collect()
    }
}

impl MessageSource for Scripted {
    async fn connect(&mut self) -> Result<(), AdapterError> {
        self.connect_count += 1;
        self.connects.pop_front().unwrap_or(Ok(()))
    }
    async fn send(&mut self, text: String) -> Result<(), AdapterError> {
        let mut sent = self.sent.lock().unwrap();
        if self.connect_count == 1 && self.fail_send_after.is_some_and(|n| sent.len() >= n) {
            return Err(AdapterError::network("send failed"));
        }
        sent.push(text);
        Ok(())
    }
    async fn next_event(&mut self, _wait: Duration) -> FeedEvent {
        match self.steps.pop_front() {
            None => FeedEvent::Shutdown,
            Some(Step::Text(t)) => FeedEvent::Text(t),
            Some(Step::Closed) => FeedEvent::Closed(Some("scripted close".into())),
            Some(Step::Idle(ms)) => {
                self.clock.advance(ms);
                FeedEvent::Idle
            }
        }
    }
}

struct NoSleep {
    clock: ManualClock,
    slept: Mutex<Vec<Duration>>,
}

impl Sleeper for NoSleep {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send {
        self.slept.lock().unwrap().push(d);
        self.clock.advance(d.as_millis() as i64);
        std::future::ready(())
    }
}

struct Rig {
    clock: ManualClock,
    sink: Arc<MemoryEventSink>,
    feed: BybitTickerFeed,
    sleeper: NoSleep,
}

fn rig() -> Rig {
    let clock = ManualClock::new(1_791_990_000_000);
    let sink = Arc::new(MemoryEventSink::default());
    let feed = BybitTickerFeed::new(Arc::new(clock.clone()), sink.clone(), 1_000);
    let sleeper = NoSleep { clock: clock.clone(), slept: Mutex::new(Vec::new()) };
    Rig { clock, sink, feed, sleeper }
}

fn syms(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("S{i:03}USDT")).collect()
}

fn tickers_frame(symbol: &str, kind: &str, ts: i64, data: Value) -> String {
    let mut d = data;
    d["symbol"] = json!(symbol);
    json!({ "topic": format!("tickers.{symbol}"), "type": kind, "ts": ts, "data": d }).to_string()
}

#[test]
fn eight_hundred_one_symbols_are_subscribed_in_nine_requests_of_at_most_a_hundred() {
    let r = rig();
    let mut src = Scripted::new(&r.clock, vec![]);
    let list = syms(801);
    block_on(r.feed.run(&mut src, || list.clone(), &r.sleeper));
    let sent = src.sent();
    assert_eq!(sent.len(), 9);
    let mut topics = 0;
    for req in &sent {
        assert_eq!(req["op"], "subscribe");
        let n = req["args"].as_array().unwrap().len();
        assert!((1..=100).contains(&n));
        topics += n;
    }
    assert_eq!(topics, 801);
    assert_eq!(sent[0]["args"][0], "tickers.S000USDT");
}

#[test]
fn messages_fill_the_cache_and_make_the_source_healthy() {
    let r = rig();
    let snap = tickers_frame("S000USDT", "snapshot", 1_791_989_999_900, json!({ "markPrice": "10", "fundingRate": "0.0001", "nextFundingTime": "1791993600000", "fundingIntervalHour": "8", "turnover24h": "77" }));
    let mut src = Scripted::new(&r.clock, vec![Step::Text(snap), Step::Text("garbage".into())]);
    block_on(r.feed.run(&mut src, || syms(1), &r.sleeper));
    let e = r.feed.cache().entry("S000USDT");
    // The loop ended on Shutdown without a reconnect, so the cache keeps what it received.
    assert_eq!(e.unwrap().funding_interval_secs, Some(8 * 3600));
    assert!(r.feed.health_snapshot().connected);
    assert!(!r.feed.health_snapshot().stale);
    assert_eq!(src.connect_count, 1);
}

#[test]
fn a_malformed_message_is_skipped_without_a_disconnect_and_is_no_sign_of_life() {
    let r = rig();
    assert!(!r.feed.handle_text("not json"));
    assert!(!r.feed.handle_text(r#"{"topic":"tickers.X","type":"delta","ts":1,"data":{"symbol":"X","markPrice":"bad"}}"#));
    assert!(r.feed.health_snapshot().last_success_at.is_none());
    assert!(r.feed.handle_text(PONG));
    assert!(r.feed.health_snapshot().last_success_at.is_some());
}

#[test]
fn ping_is_sent_every_twenty_seconds_and_not_more_often() {
    let r = rig();
    let pong = PONG.to_string();
    // 500 ms idle steps with a pong every 2 s (inside the 3 s silence threshold) for 45 s.
    let mut steps = Vec::new();
    for i in 0..90 {
        steps.push(Step::Idle(500));
        if i % 4 == 3 {
            steps.push(Step::Text(pong.clone()));
        }
    }
    let mut src = Scripted::new(&r.clock, steps);
    block_on(r.feed.run(&mut src, || syms(1), &r.sleeper));
    let pings = src.sent().iter().filter(|m| m["op"] == "ping").count();
    assert_eq!(pings, 2, "at +20 s and +40 s of 45 s");
    assert_eq!(src.connect_count, 1);
}

#[test]
fn close_means_reconnect_resubscribe_and_a_cleared_cache() {
    let r = rig();
    let snap = tickers_frame("S000USDT", "snapshot", 5, json!({ "markPrice": "10", "fundingRate": "0.1", "nextFundingTime": "99" }));
    let mut src = Scripted::new(&r.clock, vec![Step::Text(snap), Step::Closed]);
    block_on(r.feed.run(&mut src, || syms(2), &r.sleeper));
    assert_eq!(src.connect_count, 2, "closed, then connected again (the second round ends on Shutdown)");
    let subs = src.sent().iter().filter(|m| m["op"] == "subscribe").count();
    assert_eq!(subs, 2, "one subscribe request per connection");
    assert!(r.feed.cache().entry("S000USDT").is_none(), "nothing from the dead connection is republished");
    assert_eq!(r.sleeper.slept.lock().unwrap()[0], Duration::from_secs(1));
    let types: Vec<_> = r.sink.events().iter().map(|e| e.event_type.clone()).collect();
    assert!(types.contains(&"FETCH_ERROR".to_string()));
}

#[test]
fn silence_beyond_the_threshold_is_unhealthy_and_forces_a_reconnect_then_recovers() {
    let r = rig();
    let snap = tickers_frame("S000USDT", "snapshot", 5, json!({ "markPrice": "10", "fundingRate": "0.1", "nextFundingTime": "99" }));
    // threshold = max(1000, 3 x 1000) = 3000 ms: the 4th silent 1 s step crosses it.
    let steps = vec![Step::Idle(1_000), Step::Idle(1_000), Step::Idle(1_000), Step::Idle(1_500), Step::Text(snap)];
    let mut src = Scripted::new(&r.clock, steps);
    block_on(r.feed.run(&mut src, || syms(1), &r.sleeper));
    assert_eq!(src.connect_count, 2);
    let kinds: Vec<_> = r.sink.events().iter().map(|e| (e.event_type.clone(), e.payload["error_kind"].as_str().map(str::to_string))).collect();
    assert_eq!(kinds[0], ("FETCH_ERROR".to_string(), Some("Stale".to_string())));
    assert_eq!(kinds.last().unwrap().0, "FEED_RECOVERED");
    assert!(!r.feed.health_snapshot().stale);
}

#[test]
fn a_failed_connect_backs_off_and_a_failed_send_reconnects() {
    let r = rig();
    let mut src = Scripted::new(&r.clock, vec![]);
    src.connects = VecDeque::from([Err(AdapterError::Timeout), Err(AdapterError::Timeout)]);
    block_on(r.feed.run(&mut src, || syms(1), &r.sleeper));
    let slept: Vec<u64> = r.sleeper.slept.lock().unwrap().iter().map(|d| d.as_secs()).collect();
    assert_eq!(slept, vec![1, 2], "1 s then 2 s back-off, then the third connect works");
    assert_eq!(src.connect_count, 3);

    let r2 = rig();
    let mut bad = Scripted::new(&r2.clock, vec![]);
    bad.fail_send_after = Some(0);
    block_on(r2.feed.run(&mut bad, || syms(1), &r2.sleeper));
    assert_eq!(bad.connect_count, 2, "the first connection's subscribe failed, so it reconnected (the second works)");
    assert_eq!(r2.sink.events()[0].payload["error_kind"], "Disconnected");
}

#[test]
fn nothing_is_subscribed_while_the_symbol_list_is_empty() {
    let r = rig();
    let mut src = Scripted::new(&r.clock, vec![]);
    let calls = Arc::new(Mutex::new(0));
    let c2 = calls.clone();
    let list = move || {
        let mut n = c2.lock().unwrap();
        *n += 1;
        if *n < 3 { Vec::new() } else { syms(1) }
    };
    block_on(r.feed.run(&mut src, list, &r.sleeper));
    assert_eq!(r.sleeper.slept.lock().unwrap().len(), 2, "waited twice");
    assert_eq!(src.connect_count, 1);
    assert_eq!(src.sent().len(), 1);
}

#[test]
fn a_rejected_subscribe_is_a_fetch_error_event_and_not_a_disconnect() {
    let r = rig();
    let mut src = Scripted::new(&r.clock, vec![Step::Text(ACK_ERR.to_string())]);
    block_on(r.feed.run(&mut src, || syms(1), &r.sleeper));
    let ev = r.sink.events();
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!((ev[0].event_type.as_str(), ev[0].payload["source"].as_str()), ("FETCH_ERROR", Some("bybit_ws_ticker")));
    assert!(ev[0].payload["message"].as_str().unwrap().contains("subscribe rejected"));
    assert_eq!(src.connect_count, 1);
}

// ---- refresh: rollover and new listings ------------------------------------------------------------

/// Advances the wall clock past the settlement of `S000USDT` with a snapshot that carries it.
fn rollover_steps(r: &Rig, symbols: &[&str]) -> Vec<Step> {
    let now = r.clock.now_ms();
    let mut steps = Vec::new();
    for s in symbols {
        steps.push(Step::Text(tickers_frame(s, "snapshot", now, json!({ "markPrice": "1", "fundingRate": "0.1", "nextFundingTime": (now + 1_000).to_string() }))));
    }
    // 30 s of 1 s idle steps, each with a pong so the connection is not silent: crosses the settlement
    // and the 15 s grace, with refresh passes every 5 s.
    for i in 0..30 {
        steps.push(Step::Idle(1_000));
        steps.push(Step::Text(PONG.to_string()));
        let _ = i;
    }
    steps
}

#[test]
fn a_rollover_symbol_is_resubscribed_once_not_every_pass() {
    let r = rig();
    let steps = rollover_steps(&r, &["S000USDT"]);
    let mut src = Scripted::new(&r.clock, steps);
    block_on(r.feed.run(&mut src, || syms(2), &r.sleeper));
    let sent = src.sent();
    let unsub: Vec<_> = sent.iter().filter(|m| m["op"] == "unsubscribe").collect();
    assert_eq!(unsub.len(), 1, "asked once within the 30 s retry window");
    assert_eq!(unsub[0]["args"][0], "tickers.S000USDT");
    let subs_after: Vec<_> = sent.iter().skip_while(|m| m["op"] != "unsubscribe").filter(|m| m["op"] == "subscribe").collect();
    assert!(subs_after.iter().any(|m| m["args"][0] == "tickers.S000USDT"), "and subscribed again right after");
}

#[test]
fn a_new_symbol_is_subscribed_on_the_next_refresh() {
    let r = rig();
    let steps: Vec<Step> = (0..14).flat_map(|_| [Step::Idle(500), Step::Text(PONG.to_string())]).collect();
    let mut src = Scripted::new(&r.clock, steps);
    let calls = Arc::new(Mutex::new(0));
    let c2 = calls.clone();
    // The first two lists (wait + initial subscribe) have one symbol; later passes see a second one.
    let list = move || {
        let mut n = c2.lock().unwrap();
        *n += 1;
        if *n <= 1 { syms(1) } else { syms(2) }
    };
    block_on(r.feed.run(&mut src, list, &r.sleeper));
    let subs: Vec<String> = src.sent().iter().filter(|m| m["op"] == "subscribe").map(|m| m["args"][0].as_str().unwrap().to_string()).collect();
    assert_eq!(subs, vec!["tickers.S000USDT".to_string(), "tickers.S001USDT".to_string()], "the new one exactly once");
}

#[test]
fn a_mass_rollover_is_spread_over_several_passes() {
    let r = rig();
    let n = 250;
    let list = syms(n);
    let refs: Vec<&str> = list.iter().map(String::as_str).collect();
    let steps = rollover_steps(&r, &refs);
    let mut src = Scripted::new(&r.clock, steps);
    block_on(r.feed.run(&mut src, || list.clone(), &r.sleeper));
    let sent = src.sent();
    let per_request: Vec<usize> = sent.iter().filter(|m| m["op"] == "unsubscribe").map(|m| m["args"].as_array().unwrap().len()).collect();
    let total: usize = per_request.iter().sum();
    assert_eq!(total, n, "every rollover symbol re-subscribed");
    assert!(per_request.iter().all(|c| *c <= 100));
    // No pass handles more than MAX_REFRESH_PER_PASS symbols: more than 2 passes for 250 symbols.
    assert!(per_request.len() >= 3, "{per_request:?}");
}

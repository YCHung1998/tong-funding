//! Order latency events and their aggregation (change exchange-demo-execution, task 1.3).
//!
//! Every submit writes one `ORDER_LATENCY` event: `request_sent_at` (injected clock, just before
//! `Executor::submit`), `ack_at` (when it returned), `latency_ms`, `client_order_id`, the result
//! class and, for orders of a pair, `triggered_at` (when the entry / close was triggered). The
//! T−5 decision (engine-simulation design D9) needs "entry trigger → both legs accepted" p99 <
//! 2,500 ms: [`LatencyReport::from_events`] computes it (and per-leg submit latency) from those
//! events with the nearest-rank percentile. Pure: no clock, no database.

use serde_json::{Value, json};

/// One per submit (open or close, pair or manual).
pub const ORDER_LATENCY: &str = "ORDER_LATENCY";
/// The switch criterion for T−5 (engine-simulation design D9): p99 below half of 5 s.
pub const T5_P99_LIMIT_MS: i64 = 2_500;

/// The payload of one `ORDER_LATENCY` event. `result` is `accepted` / `rejected` / `unknown`
/// (a rate-limited submit is `unknown` with a "rate limited" reason: the engine must look it up).
#[allow(clippy::too_many_arguments)]
pub fn latency_payload(
    pair: &str,
    leg: &str,
    action: &str,
    exchange: &str,
    client_order_id: &str,
    request_sent_at: i64,
    ack_at: i64,
    result: &str,
    triggered_at: Option<i64>,
    simulated: bool,
) -> Value {
    json!({
        "pair": pair,
        "leg": leg,
        "action": action,
        "exchange": exchange,
        "client_order_id": client_order_id,
        "request_sent_at": request_sent_at,
        "ack_at": ack_at,
        "latency_ms": ack_at - request_sent_at,
        "result": result,
        "triggered_at": triggered_at,
        "trigger_to_ack_ms": triggered_at.map(|t| ack_at - t),
        "simulated": simulated,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Percentiles {
    pub n: usize,
    pub p50: i64,
    pub p95: i64,
    pub p99: i64,
    pub max: i64,
}

/// Nearest-rank percentiles (the smallest value with at least p % of the samples at or below
/// it); `None` without samples.
pub fn percentiles(samples: &[i64]) -> Option<Percentiles> {
    if samples.is_empty() {
        return None;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let n = v.len();
    let rank = |p: usize| v[(p * n).div_ceil(100).clamp(1, n) - 1];
    Some(Percentiles { n, p50: rank(50), p95: rank(95), p99: rank(99), max: v[n - 1] })
}

#[derive(Debug, Clone, PartialEq)]
pub struct LatencyReport {
    /// `latency_ms` of every accepted submit (request sent → exchange ACK).
    pub submit: Option<Percentiles>,
    /// Per entry of a pair whose two opening legs were both accepted: last ACK − trigger.
    pub entry_to_both_accepted: Option<Percentiles>,
    /// Entries left out because a leg was not accepted (rejected / unknown) or a timestamp is missing.
    pub entries_excluded: usize,
}

impl LatencyReport {
    /// From `ORDER_LATENCY` payloads (any order). Simulated events are skipped unless
    /// `include_simulated` (they measure nothing about an exchange).
    pub fn from_events(payloads: &[Value], include_simulated: bool) -> LatencyReport {
        let rows: Vec<&Value> =
            payloads.iter().filter(|p| include_simulated || p.get("simulated").and_then(Value::as_bool) != Some(true)).collect();
        let accepted = |p: &Value| p.get("result").and_then(Value::as_str) == Some("accepted");
        let submit: Vec<i64> =
            rows.iter().filter(|p| accepted(p)).filter_map(|p| p.get("latency_ms").and_then(Value::as_i64)).collect();

        // Group opening legs by (pair, triggered_at): one entry attempt.
        let mut entries: std::collections::BTreeMap<(String, i64), Vec<&Value>> = std::collections::BTreeMap::new();
        let mut excluded = 0usize;
        for p in rows.iter().filter(|p| p.get("action").and_then(Value::as_str) == Some("open")) {
            match (p.get("pair").and_then(Value::as_str), p.get("triggered_at").and_then(Value::as_i64)) {
                (Some(pair), Some(t)) if !pair.is_empty() => entries.entry((pair.to_string(), t)).or_default().push(p),
                _ => excluded += 1,
            }
        }
        let mut both = Vec::new();
        for ((_, triggered), legs) in &entries {
            let legs_ok = legs.len() == 2 && legs.iter().all(|p| accepted(p));
            let last_ack = legs.iter().filter_map(|p| p.get("ack_at").and_then(Value::as_i64)).max();
            match (legs_ok, last_ack) {
                (true, Some(ack)) => both.push(ack - triggered),
                _ => excluded += 1,
            }
        }
        LatencyReport { submit: percentiles(&submit), entry_to_both_accepted: percentiles(&both), entries_excluded: excluded }
    }

    /// `Some(true)` when the T−5 criterion holds, `None` without data.
    pub fn t5_criterion_met(&self) -> Option<bool> {
        self.entry_to_both_accepted.map(|p| p.p99 < T5_P99_LIMIT_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        assert_eq!(percentiles(&[]), None);
        assert_eq!(percentiles(&[7]), Some(Percentiles { n: 1, p50: 7, p95: 7, p99: 7, max: 7 }));
        let hundred: Vec<i64> = (1..=100).rev().collect();
        assert_eq!(percentiles(&hundred), Some(Percentiles { n: 100, p50: 50, p95: 95, p99: 99, max: 100 }));
        // 10 samples: p99 is the largest (rank ceil(9.9) = 10), p50 the 5th.
        let ten = [120, 180, 150, 900, 160, 170, 140, 130, 110, 2600];
        let p = percentiles(&ten).unwrap();
        assert_eq!((p.p50, p.p95, p.p99, p.max), (150, 2600, 2600, 2600));
    }

    fn ev(pair: &str, leg: &str, sent: i64, ack: i64, result: &str, trig: Option<i64>, sim: bool) -> Value {
        latency_payload(pair, leg, "open", "Binance", &format!("demo{pair}{leg}"), sent, ack, result, trig, sim)
    }

    #[test]
    fn entry_to_both_accepted_uses_the_later_ack_of_the_two_legs() {
        let events = vec![
            ev("p1", "long", 1_000, 1_180, "accepted", Some(900), false),
            ev("p1", "short", 1_000, 1_220, "accepted", Some(900), false),
            ev("p2", "long", 5_000, 5_300, "accepted", Some(4_800), false),
            ev("p2", "short", 5_000, 5_150, "accepted", Some(4_800), false),
            // one leg rejected: not an "accepted" sample, excluded from entry latency
            ev("p3", "long", 9_000, 9_100, "accepted", Some(8_900), false),
            ev("p3", "short", 9_000, 9_120, "rejected", Some(8_900), false),
            // simulated: ignored
            ev("s1", "long", 1, 2, "accepted", Some(0), true),
            ev("s1", "short", 1, 2, "accepted", Some(0), true),
        ];
        let r = LatencyReport::from_events(&events, false);
        assert_eq!(r.entry_to_both_accepted.unwrap(), Percentiles { n: 2, p50: 320, p95: 500, p99: 500, max: 500 });
        assert_eq!(r.entries_excluded, 1);
        let s = r.submit.unwrap();
        assert_eq!((s.n, s.p50, s.max), (5, 180, 300));
        assert_eq!(r.t5_criterion_met(), Some(true));
        let slow = vec![ev("p", "long", 0, 2_600, "accepted", Some(0), false), ev("p", "short", 0, 100, "accepted", Some(0), false)];
        assert_eq!(LatencyReport::from_events(&slow, false).t5_criterion_met(), Some(false));
        assert_eq!(LatencyReport::from_events(&[], false).t5_criterion_met(), None);
    }

    #[test]
    fn the_payload_carries_latency_and_trigger_offsets() {
        let p = latency_payload("u1", "long", "open", "Bybit", "demolo...", 0, 180, "accepted", Some(-20), false);
        assert_eq!((p["latency_ms"].as_i64(), p["trigger_to_ack_ms"].as_i64()), (Some(180), Some(200)));
        assert_eq!(p["client_order_id"], "demolo...");
    }
}

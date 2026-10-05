//! Pure entry / exit scheduling (task 2.1, design D9/D10). No clock reads: the local time and the
//! per-exchange `serverTime` offsets are parameters; the actor calls [`decide`] on every tick.
//!
//! Exchange-time rule: a pair has two legs on two exchanges, each with its own offset
//! (exchange minus local). Entry needs BOTH offsets and must hold on BOTH exchanges: it starts when
//! the leg whose exchange clock is furthest behind reaches `T − entry_lead_ms`, and the window
//! closes as soon as the leg furthest ahead reaches `T`. A missing offset never lets an entry
//! through (fail closed).

use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange;

use super::timings::{EngineTimings, MissedWindowPolicy};

/// Name of the warning event written before a missed-window cancel.
pub const ENTRY_WINDOW_MISSED: &str = "ENTRY_WINDOW_MISSED";

/// Local time plus each leg's exchange and `serverTime` offset (`None` = not calibrated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    pub local_now_ms: i64,
    pub long: (Exchange, Option<i64>),
    pub short: (Exchange, Option<i64>),
}

impl ClockReading {
    /// Exchanges whose offset is unavailable (long leg first, no duplicates).
    pub fn missing_offsets(&self) -> Vec<Exchange> {
        let mut out = Vec::new();
        for (ex, off) in [self.long, self.short] {
            if off.is_none() && !out.contains(&ex) {
                out.push(ex);
            }
        }
        out
    }

    /// `(earliest, latest)` corrected exchange time over both legs; `None` if an offset is missing.
    pub fn exchange_now_range(&self) -> Option<(i64, i64)> {
        let (a, b) = (self.long.1?, self.short.1?);
        Some((self.local_now_ms + a.min(b), self.local_now_ms + a.max(b)))
    }
}

/// The per-pair facts the scheduler needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairTiming {
    pub state: PairState,
    /// Settlement `T` (exchange time), fixed at pair creation.
    pub settlement_ms: i64,
    /// A baseline fetch was already started for this pair.
    pub baseline_requested: bool,
}

/// Why an entry is held back although its time has come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryBlock {
    /// `serverTime` offset not calibrated for these exchanges: no entry (fail closed).
    OffsetUnavailable { exchanges: Vec<Exchange> },
}

/// Which clock an exit decision was based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitClock {
    /// Both offsets known; the later-running leg's exchange time was used.
    Corrected,
    /// An offset is missing; local time was used so the exit is never stuck (late beats never).
    LocalFallback { missing: Vec<Exchange> },
}

/// What the actor should do for one pair on this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleAction {
    Nothing,
    /// Fetch the baseline prices (once).
    FetchBaseline,
    /// Send `Command::EntryTrigger`.
    Enter,
    /// Entry time has come but entry is not allowed; record an event, stay PREPARED.
    EntryBlocked(EntryBlock),
    /// The entry window passed without an entry: apply `policy` (WarnThenCancel: write an
    /// [`ENTRY_WINDOW_MISSED`] warning event, then cancel). Never enter late.
    EntryWindowMissed { policy: MissedWindowPolicy },
    /// Send `Command::AutoExit`.
    Exit { clock: ExitClock },
}

/// Decides the scheduler action for one pair. Only PREPARED pairs can enter and only RECONCILED
/// pairs exit automatically; every other state yields `Nothing`.
pub fn decide(pair: &PairTiming, timings: &EngineTimings, clock: &ClockReading) -> ScheduleAction {
    match pair.state {
        PairState::Prepared => decide_entry(pair, timings, clock),
        PairState::Reconciled => decide_exit(pair.settlement_ms, timings, clock),
        PairState::PreTradeCheck
        | PairState::Blocked
        | PairState::OrderSubmit
        | PairState::FillMonitor
        | PairState::Imbalanced
        | PairState::Closing
        | PairState::Finalized
        | PairState::Cancelled
        | PairState::PartialFailure
        | PairState::Unresolved => ScheduleAction::Nothing,
    }
}

fn decide_entry(pair: &PairTiming, timings: &EngineTimings, clock: &ClockReading) -> ScheduleAction {
    let t = pair.settlement_ms;
    let entry_at = timings.entry_at(t);
    let missed = ScheduleAction::EntryWindowMissed { policy: timings.missed_window_policy };
    let Some((earliest, latest)) = clock.exchange_now_range() else {
        // Without calibration: a past settlement is still detected on the local clock (restart
        // case); otherwise an entry time that has come is blocked, never fired.
        let now = clock.local_now_ms;
        let known_past_t = [clock.long.1, clock.short.1].into_iter().flatten().any(|off| now + off >= t);
        return if known_past_t || now >= t {
            missed
        } else if now >= entry_at {
            ScheduleAction::EntryBlocked(EntryBlock::OffsetUnavailable { exchanges: clock.missing_offsets() })
        } else if now >= timings.base_price_at(t) && !pair.baseline_requested {
            // Read-only fetch; harmless on the uncorrected clock.
            ScheduleAction::FetchBaseline
        } else {
            ScheduleAction::Nothing
        };
    };
    if latest >= t {
        missed
    } else if earliest >= entry_at {
        ScheduleAction::Enter
    } else if earliest >= timings.base_price_at(t) && !pair.baseline_requested {
        ScheduleAction::FetchBaseline
    } else {
        ScheduleAction::Nothing
    }
}

fn decide_exit(settlement_ms: i64, timings: &EngineTimings, clock: &ClockReading) -> ScheduleAction {
    let (now, exit_clock) = match clock.exchange_now_range() {
        Some((earliest, _)) => (earliest, ExitClock::Corrected),
        None => (clock.local_now_ms, ExitClock::LocalFallback { missing: clock.missing_offsets() }),
    };
    if now >= timings.exit_at(settlement_ms) {
        ScheduleAction::Exit { clock: exit_clock }
    } else {
        ScheduleAction::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    /// 12:00:00 on some day, exchange time.
    const T: i64 = 1_000 * 24 * HOUR + 12 * HOUR;

    fn hms(h: i64, m: i64, s: i64) -> i64 {
        1_000 * 24 * HOUR + h * HOUR + m * 60_000 + s * 1_000
    }

    fn prepared(baseline_requested: bool) -> PairTiming {
        PairTiming { state: PairState::Prepared, settlement_ms: T, baseline_requested }
    }

    fn clock(local_now_ms: i64, long: Option<i64>, short: Option<i64>) -> ClockReading {
        ClockReading { local_now_ms, long: (Exchange::Binance, long), short: (Exchange::Bybit, short) }
    }

    fn both(local_now_ms: i64, off: i64) -> ClockReading {
        clock(local_now_ms, Some(off), Some(off))
    }

    #[test]
    fn local_clock_two_seconds_behind_fires_at_local_11_59_48() {
        let t = EngineTimings::default();
        let p = prepared(true);
        assert_eq!(decide(&p, &t, &both(hms(11, 59, 48) - 1, 2_000)), ScheduleAction::Nothing);
        assert_eq!(decide(&p, &t, &both(hms(11, 59, 48), 2_000)), ScheduleAction::Enter);
    }

    #[test]
    fn window_start_is_inclusive_and_settlement_exclusive_default_lead() {
        let t = EngineTimings::default();
        let p = prepared(true);
        assert_eq!(decide(&p, &t, &both(T - 10_001, 0)), ScheduleAction::Nothing);
        assert_eq!(decide(&p, &t, &both(T - 10_000, 0)), ScheduleAction::Enter);
        assert_eq!(decide(&p, &t, &both(T - 1, 0)), ScheduleAction::Enter);
        assert_eq!(
            decide(&p, &t, &both(T, 0)),
            ScheduleAction::EntryWindowMissed { policy: MissedWindowPolicy::WarnThenCancel }
        );
    }

    #[test]
    fn window_boundaries_follow_a_non_default_lead() {
        let t = EngineTimings { entry_lead_ms: 5_000, ..EngineTimings::default() };
        let p = prepared(true);
        assert_eq!(decide(&p, &t, &both(T - 5_001, 0)), ScheduleAction::Nothing);
        assert_eq!(decide(&p, &t, &both(T - 5_000, 0)), ScheduleAction::Enter);
        assert_eq!(decide(&p, &t, &both(T - 1, 0)), ScheduleAction::Enter);
        assert!(matches!(decide(&p, &t, &both(T, 0)), ScheduleAction::EntryWindowMissed { .. }));
    }

    #[test]
    fn entry_must_hold_on_both_exchanges() {
        let t = EngineTimings::default();
        let p = prepared(true);
        // Long exchange 1 s ahead, short 1 s behind local: start waits for the short leg...
        assert_eq!(decide(&p, &t, &clock(T - 10_000, Some(1_000), Some(-1_000))), ScheduleAction::Nothing);
        assert_eq!(decide(&p, &t, &clock(T - 9_000, Some(1_000), Some(-1_000))), ScheduleAction::Enter);
        // ...and the window closes when the long leg reaches T.
        assert_eq!(decide(&p, &t, &clock(T - 1_001, Some(1_000), Some(-1_000))), ScheduleAction::Enter);
        assert!(matches!(
            decide(&p, &t, &clock(T - 1_000, Some(1_000), Some(-1_000))),
            ScheduleAction::EntryWindowMissed { .. }
        ));
    }

    #[test]
    fn offset_unavailable_blocks_entry_and_names_the_exchange() {
        let t = EngineTimings::default();
        let p = prepared(true);
        let blocked = |exchanges: Vec<Exchange>| {
            ScheduleAction::EntryBlocked(EntryBlock::OffsetUnavailable { exchanges })
        };
        assert_eq!(decide(&p, &t, &clock(T - 5_000, Some(0), None)), blocked(vec![Exchange::Bybit]));
        assert_eq!(
            decide(&p, &t, &clock(T - 10_000, None, None)),
            blocked(vec![Exchange::Binance, Exchange::Bybit])
        );
        // Before the entry time there is nothing to block yet.
        assert_eq!(decide(&p, &t, &clock(T - 10_001, None, Some(0))), ScheduleAction::Nothing);
    }

    #[test]
    fn restart_after_settlement_is_a_missed_window_even_without_offsets() {
        let t = EngineTimings::default();
        let p = prepared(false);
        let missed = ScheduleAction::EntryWindowMissed { policy: MissedWindowPolicy::WarnThenCancel };
        assert_eq!(decide(&p, &t, &both(T + 3 * HOUR, 0)), missed);
        assert_eq!(decide(&p, &t, &clock(T + 3 * HOUR, None, None)), missed);
        assert_eq!(decide(&p, &t, &clock(T + 3 * HOUR, Some(0), None)), missed);
    }

    #[test]
    fn baseline_is_due_from_t_minus_15_once() {
        let t = EngineTimings::default();
        assert_eq!(decide(&prepared(false), &t, &both(T - 15_001, 0)), ScheduleAction::Nothing);
        assert_eq!(decide(&prepared(false), &t, &both(T - 15_000, 0)), ScheduleAction::FetchBaseline);
        assert_eq!(decide(&prepared(false), &t, &both(T - 10_001, 0)), ScheduleAction::FetchBaseline);
        assert_eq!(decide(&prepared(true), &t, &both(T - 12_000, 0)), ScheduleAction::Nothing);
        // In the entry window the entry wins; the pre-trade check falls back to scan prices.
        assert_eq!(decide(&prepared(false), &t, &both(T - 10_000, 0)), ScheduleAction::Enter);
    }

    #[test]
    fn exit_at_t_plus_exit_delay_with_no_upper_bound() {
        let t = EngineTimings::default();
        let p = PairTiming { state: PairState::Reconciled, settlement_ms: T, baseline_requested: true };
        assert_eq!(decide(&p, &t, &both(T + 15_000 - 2_001, 2_000)), ScheduleAction::Nothing);
        let due = ScheduleAction::Exit { clock: ExitClock::Corrected };
        assert_eq!(decide(&p, &t, &both(T + 15_000 - 2_000, 2_000)), due);
        assert_eq!(decide(&p, &t, &both(T + 10 * HOUR, 0)), due);
        // Uses the leg furthest behind, so neither exchange exits before T + 15 s.
        assert_eq!(decide(&p, &t, &clock(T + 15_000, Some(1_000), Some(-1_000))), ScheduleAction::Nothing);
        let t2 = EngineTimings { exit_delay_ms: 30_000, ..EngineTimings::default() };
        assert_eq!(decide(&p, &t2, &both(T + 29_999, 0)), ScheduleAction::Nothing);
        assert_eq!(decide(&p, &t2, &both(T + 30_000, 0)), due);
    }

    #[test]
    fn exit_without_offset_falls_back_to_local_time_and_says_so() {
        let t = EngineTimings::default();
        let p = PairTiming { state: PairState::Reconciled, settlement_ms: T, baseline_requested: true };
        assert_eq!(decide(&p, &t, &clock(T + 14_999, Some(0), None)), ScheduleAction::Nothing);
        assert_eq!(
            decide(&p, &t, &clock(T + 15_000, Some(0), None)),
            ScheduleAction::Exit { clock: ExitClock::LocalFallback { missing: vec![Exchange::Bybit] } }
        );
    }

    #[test]
    fn only_prepared_enters_and_only_reconciled_exits() {
        let t = EngineTimings::default();
        for state in PairState::ALL {
            let p = PairTiming { state, settlement_ms: T, baseline_requested: true };
            let at_entry = decide(&p, &t, &both(T - 5_000, 0));
            let after = decide(&p, &t, &both(T + HOUR, 0));
            match state {
                PairState::Prepared => {
                    assert_eq!(at_entry, ScheduleAction::Enter);
                    assert!(matches!(after, ScheduleAction::EntryWindowMissed { .. }));
                }
                PairState::Reconciled => {
                    assert_eq!(at_entry, ScheduleAction::Nothing);
                    assert!(matches!(after, ScheduleAction::Exit { .. }));
                }
                _ => {
                    assert_eq!(at_entry, ScheduleAction::Nothing, "{state}");
                    assert_eq!(after, ScheduleAction::Nothing, "{state}");
                }
            }
        }
    }

    /// Engine code must take time from the injected clock only (scheduler-and-nodes spec).
    /// Patterns are split so this file does not match itself.
    #[test]
    fn engine_sources_never_read_the_system_clock() {
        let banned = [
            ["SystemTime", "::now"].concat(),
            ["Instant", "::now"].concat(),
            ["Utc", "::now"].concat(),
            ["Local", "::now"].concat(),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine");
        let mut stack = vec![dir];
        let mut scanned = 0;
        let mut hits = Vec::new();
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).expect("engine dir readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                scanned += 1;
                let src = std::fs::read_to_string(&path).expect("source readable");
                for (n, line) in src.lines().enumerate() {
                    let compact: String = line.split_whitespace().collect();
                    for b in &banned {
                        if compact.contains(b.as_str()) {
                            hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                        }
                    }
                }
            }
        }
        assert!(scanned >= 10, "scanned only {scanned} files; wrong directory?");
        assert!(hits.is_empty(), "system clock read in engine:\n{}", hits.join("\n"));
    }
}

//! Every scheduling constant in one place (design D9): the user wants these easy to change.
//! Defaults: entry T−10 s, baseline price T−15 s, exit T+15 s, Snapshot interval 250 ms.

/// What to do when a PREPARED pair's entry window has passed without an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissedWindowPolicy {
    /// Write an `ENTRY_WINDOW_MISSED` warning event, then cancel the pair. Never enter late.
    WarnThenCancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTimings {
    /// Entry fires at `T - entry_lead_ms` (exchange time). Switch to 5_000 only once the measured
    /// p99 of "entry trigger -> both legs accepted" is below 2_500 ms (design D9).
    pub entry_lead_ms: i64,
    /// The baseline price is fetched this long before the entry time.
    pub base_price_lead_ms: i64,
    /// Exit fires at `T + exit_delay_ms`.
    pub exit_delay_ms: i64,
    /// Scheduler tick period.
    pub tick_ms: i64,
    /// Minimum interval between two Snapshots pushed to the UI (unverified proposal, design D3).
    pub snapshot_min_interval_ms: i64,
    pub missed_window_policy: MissedWindowPolicy,
}

impl Default for EngineTimings {
    fn default() -> Self {
        EngineTimings {
            entry_lead_ms: 10_000,
            base_price_lead_ms: 5_000,
            exit_delay_ms: 15_000,
            tick_ms: 1_000,
            snapshot_min_interval_ms: 250,
            missed_window_policy: MissedWindowPolicy::WarnThenCancel,
        }
    }
}

impl EngineTimings {
    pub fn entry_at(&self, settlement_ms: i64) -> i64 {
        settlement_ms - self.entry_lead_ms
    }
    pub fn base_price_at(&self, settlement_ms: i64) -> i64 {
        self.entry_at(settlement_ms) - self.base_price_lead_ms
    }
    pub fn exit_at(&self, settlement_ms: i64) -> i64 {
        settlement_ms + self.exit_delay_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_entry_t_minus_10_baseline_t_minus_15_exit_t_plus_15() {
        let t = EngineTimings::default();
        let settle = 1_000_000;
        assert_eq!(t.entry_at(settle), settle - 10_000);
        assert_eq!(t.base_price_at(settle), settle - 15_000);
        assert_eq!(t.exit_at(settle), settle + 15_000);
        assert_eq!(t.tick_ms, 1_000);
        assert_eq!(t.missed_window_policy, MissedWindowPolicy::WarnThenCancel);
    }
}

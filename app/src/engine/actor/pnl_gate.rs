//! funding-pnl task 3.1, engine side: FINALIZED needs the closed confirmation AND "PnL computed"
//! (core `PnlGate`). After `CLOSE_CONFIRMED` an EXCHANGE_DEMO pair stays CLOSING while its PnL
//! waits for missing funding entries, at most `funding::PNL_RETRY_WINDOW_MS` (injected clock);
//! then `PAIR_PNL_COMPUTED` is written (INCOMPLETE with the missing items if still missing, user
//! decision 2026-10-05) and only after that the pair lands FINALIZED. A SIMULATION pair has no
//! PnL by design: it finalizes with `PnlGate::NotApplicableSimulated` right away, so its event
//! sequence is unchanged.
//!
//! The wait is in memory: after a restart the CLOSING pair goes to the reconciler, which records
//! the PnL with whatever data exists (final attempt) before finalizing; late entries then lead to
//! `PAIR_PNL_RECOMPUTED`.

use serde_json::json;
use tong_funding_core::pair::{ManualEvent, PairState, PnlGate, SystemEvent, next};

use super::{Actor, CommandReply};
use crate::funding::PNL_RETRY_WINDOW_MS;
use crate::funding::pnl_record::{PnlAttempt, settle_pnl};

/// Written once per pair while its PnL waits for funding entries (system log visibility).
pub const PNL_PENDING: &str = "PNL_PENDING";

impl Actor {
    /// CLOSING with the closed confirmation recorded and the PnL not yet settled.
    pub(super) fn awaiting_pnl(&self, pair: &str) -> bool {
        self.flows.get(pair).is_some_and(|f| f.pnl_wait_since.is_some())
    }

    /// Right after `CLOSE_CONFIRMED`: start the PnL step and try it at once.
    pub(super) fn begin_pnl_wait(&mut self, pair: &str) {
        let now = self.clock.now_ms();
        self.flows.entry(pair.to_string()).or_default().pnl_wait_since.get_or_insert(now);
        self.finalize_closed(pair);
    }

    /// One attempt (also every tick while waiting): settle the PnL, then FINALIZED.
    pub(super) fn finalize_closed(&mut self, pair: &str) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        if view.state != PairState::Closing {
            return;
        }
        if view.simulated {
            if self.land(pair, SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::NotApplicableSimulated }, json!({ "source": "flat check", "pnl": "not applicable (SIMULATION)" })).is_ok() {
                self.flows.entry(pair.to_string()).or_default().pnl_wait_since = None;
            }
            return;
        }
        let now = self.clock.now_ms();
        let since = self.flows.get(pair).and_then(|f| f.pnl_wait_since).unwrap_or(now);
        let final_attempt = now >= since.saturating_add(PNL_RETRY_WINDOW_MS);
        match settle_pnl(&self.db, pair, now, final_attempt) {
            Ok(PnlAttempt::Recorded { event_id, status }) => {
                let detail = json!({ "source": "flat check", "pnl_event_id": event_id, "pnl_status": status });
                if self.land(pair, SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::Recorded }, detail).is_ok() {
                    self.flows.entry(pair.to_string()).or_default().pnl_wait_since = None;
                }
            }
            Ok(PnlAttempt::Waiting { reasons }) => {
                let payload = json!({ "reasons": reasons, "until_ms": since.saturating_add(PNL_RETRY_WINDOW_MS) });
                self.report_once(pair, PNL_PENDING.to_string(), PNL_PENDING, payload);
            }
            // A store problem: stays CLOSING and is retried on the next tick (never finalized
            // without the PnL event).
            Err(e) => self.report_once(pair, format!("{PNL_PENDING}:error"), PNL_PENDING, json!({ "error": e })),
        }
    }

    /// The user's "confirmed closed" on a locked pair: the PnL is recorded first (final attempt:
    /// whatever data exists), then the transition. A refused transition writes no PnL event.
    pub(super) fn confirm_closed(&mut self, pair: &str, verified_flat: bool) -> CommandReply {
        let Some(view) = self.pairs.get(pair).cloned() else { return CommandReply::Rejected(format!("unknown pair {pair}")) };
        let pnl = if view.simulated {
            PnlGate::NotApplicableSimulated
        } else if verified_flat && next(view.state, ManualEvent::ConfirmClosed { verified_flat, pnl: PnlGate::Recorded }).is_ok() {
            match settle_pnl(&self.db, pair, self.clock.now_ms(), true) {
                Ok(PnlAttempt::Recorded { .. }) => PnlGate::Recorded,
                Ok(PnlAttempt::Waiting { .. }) => PnlGate::Missing,
                Err(e) => return CommandReply::Rejected(format!("PnL not recorded, not finalized: {e}")),
            }
        } else {
            PnlGate::Missing
        };
        self.transition(pair, ManualEvent::ConfirmClosed { verified_flat, pnl }, json!({ "source": "user" }))
    }
}

//! loss-circuit-breaker inside the actor (child of `actor`, so it reaches the actor's private
//! state). The arithmetic is `core::loss_breaker`, the store side is `engine::loss_breaker`; this
//! file decides WHEN to look (after a `CLOSE_CONFIRMED`, after a PnL result, at start, and whenever
//! one of the watched events moved) and what a trip does: kill switch ON together with
//! `LOSS_BREAKER_TRIPPED`, a Live engine disarmed, one notification. Opening is stopped and nothing
//! else: closing, exits of pairs in progress and manual handling keep working.
//! Rule of this file: when something cannot be read or judged, stop opening.

use super::*;
use crate::engine::alert::BreakerNotice;
use crate::engine::live_guard::live_db_defaults;
use crate::engine::loss_breaker::{self as lb, Assessment, LOSS_BREAKER_TRIPPED};

/// Result of the notification, for the system log.
pub const LOSS_BREAKER_NOTIFY: &str = "LOSS_BREAKER_NOTIFY";

impl Actor {
    /// The stored limits; when the settings cannot be read, the defaults of this process (Live is the
    /// stricter 5 USDT / 2 losses).
    pub(super) fn breaker_limits(&self) -> (Decimal, u32) {
        match self.load_risk() {
            Ok((risk, _)) => (risk.loss_breaker_max_loss_usdt, risk.loss_breaker_max_consecutive_losses),
            Err(_) => {
                let risk = if self.live_on() { live_db_defaults().risk } else { RiskConfig::default() };
                (risk.loss_breaker_max_loss_usdt, risk.loss_breaker_max_consecutive_losses)
            }
        }
    }

    /// Looks at the current interval. Without `force` it does so only when one of the watched events
    /// (closes, PnL results, kill switch changes) is newer than the last look.
    pub(super) fn breaker_check(&mut self, force: bool) {
        let Ok(mark) = lb::watermark(&self.db) else { return };
        if !force && self.breaker_mark == Some(mark) {
            return;
        }
        let (max_loss, max_consecutive) = self.breaker_limits();
        let now = self.clock.now_ms();
        match lb::assess(&self.db, max_loss, max_consecutive, now) {
            Ok(a) => {
                let must_trip = a.state.tripped && a.recorded_trip.is_none() && !self.db.kill_switch_halted();
                self.breaker = Some(a.view());
                if must_trip {
                    self.trip_breaker(&a);
                    // the trip is on record now: show it at once
                    if let Ok(after) = lb::assess(&self.db, max_loss, max_consecutive, now) {
                        self.breaker = Some(after.view());
                    }
                }
            }
            // Cannot tell what the interval is worth: stop opening (unless it is already stopped).
            Err(e) => {
                if !self.db.is_halted() && !self.db.kill_switch_halted() {
                    let payload = json!({ "reason": "UNREADABLE", "reason_text": "無法評估熔斷", "error": e });
                    if let Err(w) = self.db.set_kill_switch_with_event(true, LOSS_BREAKER_TRIPPED, &payload)
                        && !self.db.is_halted()
                    {
                        self.db.halt(HaltReason::KillSwitchReadFailed(format!("breaker lock could not be written: {w}")));
                    }
                    self.live_disarm();
                }
            }
        }
        self.breaker_mark = lb::watermark(&self.db).ok();
        self.dirty = true;
    }

    /// Kill switch ON and `LOSS_BREAKER_TRIPPED` in ONE transaction; a Live engine is disarmed; the
    /// user is told. Closing stays open.
    fn trip_breaker(&mut self, a: &Assessment) {
        let payload = a.trip_payload();
        if let Err(e) = self.db.set_kill_switch_with_event(true, LOSS_BREAKER_TRIPPED, &payload)
            && !self.db.is_halted()
        {
            self.db.halt(HaltReason::KillSwitchReadFailed(format!("breaker trip could not be written: {e}")));
        }
        if self.live_on() {
            let why = a.state.reason.map_or("loss circuit breaker", |r| r.text());
            self.live.last_lock_reason = Some(format!("loss circuit breaker: {why}"));
            self.live_disarm();
        }
        self.dirty = true;
        self.notify_breaker(a);
    }

    /// One notification off the actor (a notifier may block); its failure changes nothing but one record.
    fn notify_breaker(&self, a: &Assessment) {
        let Some(reason) = a.state.reason else { return };
        let notice = BreakerNotice {
            identity: self.identity,
            reason,
            loss_used: a.state.loss_used(),
            max_loss: a.max_loss,
            consecutive: a.state.consecutive,
            max_consecutive: a.max_consecutive,
            provisional: a.pairs.iter().any(|p| !matches!(p.amount, tong_funding_core::loss_breaker::PairAmount::Net(_))),
        };
        let (notifier, events) = (self.notifier.clone(), self.events.clone());
        tokio::task::spawn_blocking(move || {
            let result = notifier.notify_breaker(&notice);
            let mut payload = json!({ "ok": result.is_ok() });
            if let Err(e) = result {
                payload["error"] = json!(e);
            }
            let _ = events.append(LOSS_BREAKER_NOTIFY, None, payload);
        });
    }
}

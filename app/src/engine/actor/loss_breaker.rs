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
use tong_funding_core::loss_breaker::TripReason;

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

    /// Looks at the current interval, but only when one of the watched events (closes, hand-confirmed
    /// finalizations, PnL results, kill switch changes, risk settings, trips) is newer than the last
    /// look. Fail closed: when the events cannot be read at all, new entries stop.
    pub(super) fn breaker_check(&mut self) {
        let mark = match lb::watermark(&self.db) {
            Ok(m) => m,
            Err(e) => return self.breaker_unreadable(format!("watermark: {e}")),
        };
        if self.breaker_mark == Some(mark) {
            return;
        }
        let (max_loss, max_consecutive) = self.breaker_limits();
        match lb::assess(&self.db, max_loss, max_consecutive, self.clock.now_ms()) {
            Ok(a) => {
                // A limit reached with no trip on record for this interval is recorded and told even when
                // the kill switch is already ON (stopped by hand, by the watchdog): the status then names the breaker.
                let must_trip = a.state.tripped && a.recorded_trip.is_none();
                self.breaker = Some(a.view());
                if must_trip {
                    self.trip_breaker(&a);
                }
            }
            // Cannot tell what the interval is worth.
            Err(e) => return self.breaker_unreadable(e),
        }
        self.breaker_mark = lb::watermark(&self.db).ok();
        self.dirty = true;
    }

    /// The interval cannot be read: stop opening (unless it is already stopped), write why, tell the user.
    fn breaker_unreadable(&mut self, error: String) {
        self.dirty = true;
        if self.db.is_halted() || self.db.kill_switch_halted() {
            return;
        }
        let reason = TripReason::Unreadable;
        let payload = json!({ "reason": reason.code(), "reason_text": reason.text(), "error": error });
        self.lock_new_entries(LOSS_BREAKER_TRIPPED, &payload, format!("loss circuit breaker: {}", reason.text()));
        let (max_loss, max_consecutive) = self.breaker_limits();
        self.breaker = Some(lb::BreakerView {
            start_ms: self.clock.now_ms(),
            net_sum: Decimal::ZERO,
            loss_used: Decimal::ZERO,
            consecutive: 0,
            max_loss,
            max_consecutive,
            tripped: true,
            reason: Some(reason),
            counted: 0,
        });
        self.send_breaker_notice(BreakerNotice { identity: self.identity, reason, loss_used: Decimal::ZERO, max_loss, consecutive: 0, max_consecutive, provisional: false });
    }

    /// Kill switch ON and `LOSS_BREAKER_TRIPPED` in ONE transaction; a Live engine is disarmed; the
    /// user is told. Closing stays open.
    fn trip_breaker(&mut self, a: &Assessment) {
        let why = a.state.reason.map_or("loss circuit breaker", |r| r.text());
        self.lock_new_entries(LOSS_BREAKER_TRIPPED, &a.trip_payload(), format!("loss circuit breaker: {why}"));
        self.notify_breaker(a);
    }

    /// One notification off the actor (a notifier may block); its failure changes nothing but one record.
    fn notify_breaker(&self, a: &Assessment) {
        let Some(reason) = a.state.reason else { return };
        self.send_breaker_notice(BreakerNotice {
            identity: self.identity,
            reason,
            loss_used: a.state.loss_used(),
            max_loss: a.max_loss,
            consecutive: a.state.consecutive,
            max_consecutive: a.max_consecutive,
            provisional: a.pairs.iter().any(|p| !matches!(p.amount, tong_funding_core::loss_breaker::PairAmount::Net(_))),
        });
    }

    fn send_breaker_notice(&self, notice: BreakerNotice) {
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

    /// An existing Live database whose stored settings predate the breaker has neither limit: a missing
    /// field would load as the looser Demo default (15 / 3). Once, at a Live start, the missing field(s)
    /// are written as the Live defaults (5 / 2) through the settings path, with `RISK_CONFIG_UPDATED`
    /// (before / after). A value that is there is never touched.
    pub(super) fn live_backfill_breaker_defaults(&mut self) {
        if !self.live_on() {
            return;
        }
        let Ok(Some(entry)) = self.db.config_get(CONFIG_RISK) else { return };
        let Value::Object(mut stored) = entry.value else { return };
        let Ok(defaults) = serde_json::to_value(live_db_defaults().risk) else { return };
        let mut changed = false;
        for key in ["loss_breaker_max_loss_usdt", "loss_breaker_max_consecutive_losses"] {
            if !stored.contains_key(key) {
                stored.insert(key.to_string(), defaults[key].clone());
                changed = true;
            }
        }
        if !changed {
            return;
        }
        let overrides = match self.db.config_get(CONFIG_RISK_OVERRIDES) {
            Ok(Some(e)) => e.value,
            Ok(None) => json!({}),
            Err(_) => return,
        };
        // not armed at a start, so the frozen-caps check passes; a value that does not validate is refused here too
        let _ = self.save_risk_settings(Value::Object(stored), overrides);
    }
}

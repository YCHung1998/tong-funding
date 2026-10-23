//! live-guardrails inside the actor (child of `actor`, so it reaches the actor's private state).
//! Everything here is a no-op for `ProcessIdentity::Demo`. For Live: the start lock, `Arm` and the
//! preflight, the leg-notional / pairs-per-arming caps, the automatic locks, the frozen settings
//! and `FlattenLeg`. Rule of this file: when something cannot be read or judged, refuse and lock.

use super::*;
use crate::engine::live_guard::{
    self, FrozenCaps, LEG_FLATTEN, LEG_FLATTEN_ABORTED, LIVE_ARM_REFUSED, LIVE_ARMED, LIVE_AUTO_LOCK, LIVE_PREFLIGHT, LIVE_STARTUP_LOCK, LiveStatus,
};
use crate::engine::preflight;

impl Actor {
    pub(super) fn live_on(&self) -> bool {
        self.identity.is_live()
    }

    /// Armed = a successful `Arm` since the last lock / start (frozen caps in memory).
    pub(super) fn armed(&self) -> bool {
        self.live_on() && self.live.frozen.is_some()
    }

    pub(super) fn live_status(&self) -> LiveStatus {
        LiveStatus {
            is_live: self.live_on(),
            armed: self.armed(),
            frozen: self.live.frozen.clone(),
            pairs_entered: self.live.pairs_entered,
            preflight_running: self.live.preflight_running,
            preflight: self.live.preflight.clone(),
            last_lock_reason: self.live.last_lock_reason.clone(),
        }
    }

    pub(super) fn publish_live(&self) {
        self.live_tx.send_replace(self.live_status());
    }

    /// Live start: the kill switch is forced ON, whatever the database said; a stored AUTO trigger
    /// is taken back to MANUAL. A lock that cannot be written halts the store (fail closed).
    pub(super) fn live_startup(&mut self) {
        if !self.live_on() {
            return;
        }
        let payload = json!({ "reason": "live start: kill switch forced ON" });
        if let Err(e) = self.db.set_kill_switch_with_event(true, LIVE_STARTUP_LOCK, &payload) {
            self.db.halt(HaltReason::KillSwitchReadFailed(format!("live start lock could not be written: {e}")));
        }
        // pairs already in an alert state at start were answered by this lock
        for v in self.pairs.values() {
            if alert::is_alert_state(v.state) {
                self.live.locked_alerts.insert((v.internal_uuid.clone(), v.state.as_str()));
            }
        }
        self.publish_live();
    }

    /// The one automatic lock: frozen caps dropped (disarmed in memory first), kill switch ON and a
    /// `LIVE_AUTO_LOCK` event with the reason, in one transaction. Closing and cancelling stay open.
    pub(super) fn lock_live(&mut self, reason: &str) {
        if !self.live_on() {
            return;
        }
        self.live.frozen = None;
        self.live.last_lock_reason = Some(reason.to_string());
        let payload = json!({ "reason": reason, "pairs_entered": self.live.pairs_entered });
        if let Err(e) = self.db.set_kill_switch_with_event(true, LIVE_AUTO_LOCK, &payload)
            && !self.db.is_halted()
        {
            self.db.halt(HaltReason::KillSwitchReadFailed(format!("auto lock could not be written: {e}")));
        }
        self.dirty = true;
        self.publish_live();
    }

    /// Error texts of an event (HTTP status / exchange answers) that lock an armed engine:
    /// authentication errors and rate limits (HTTP 418 / 429).
    pub(super) fn watch_event_errors(&mut self, ev: &Event) {
        if !self.armed() {
            return;
        }
        for (source, text) in event_errors(ev) {
            if let Some(cause) = live_guard::classify_error(&text) {
                // the reason names the cause and the source; the exchange text itself stays out of the event
                self.lock_live(&format!("{} ({source})", cause.as_str()));
                return;
            }
        }
    }

    // ---- settings ----

    fn live_risk(&self) -> Result<RiskConfig, String> {
        self.load_risk().map(|(r, _)| r)
    }

    /// The per-leg cap in force now (stored cap, built-in ceiling, frozen value): `Err` = refuse.
    pub(super) fn live_leg_cap(&self) -> Result<Decimal, String> {
        live_guard::effective_leg_cap(&self.live_risk()?, self.live.frozen.as_ref())
    }

    /// Adding a pair / saving the template: the per-leg notional must be within the cap.
    pub(super) fn live_notional_check(&self, notional: Decimal, what: &str) -> Result<(), String> {
        if !self.live_on() {
            return Ok(());
        }
        let cap = self.live_leg_cap().map_err(|e| format!("live: cannot judge {what} ({e})"))?;
        if notional > cap {
            return Err(format!("live: {what} {} USDT per leg is above the live cap {} USDT (live_max_leg_notional_usdt)", notional.normalize(), cap.normalize()));
        }
        Ok(())
    }

    /// `AddPrepared` under Live: the entry snapshot's notional must be readable and within the cap.
    pub(super) fn live_add_prepared_check(&self, entry: &Value) -> Result<(), String> {
        if !self.live_on() {
            return Ok(());
        }
        let snap = EntrySnapshot::from_json(entry).map_err(|e| format!("live: entry notional unreadable ({e})"))?;
        self.live_notional_check(snap.notional_usdt, "entry notional")
    }

    /// Saving risk settings (`risk` JSON, optionally `overrides`) while armed: no cap may go up.
    pub(super) fn live_settings_check(&self, risk: Option<&Value>, overrides: Option<&Value>) -> Result<(), String> {
        let Some(frozen) = self.live.frozen.as_ref().filter(|_| self.live_on()) else { return Ok(()) };
        if let Some(risk) = risk {
            let new = RiskConfig::from_json(&risk.to_string()).map_err(|e| format!("risk: {e}"))?;
            if let Some(field) = frozen.first_raised_by(&new) {
                return Err(format!("live: armed, `{field}` cannot be raised; lock first (kill switch ON), then change it and Arm again (lowering is allowed)"));
            }
        }
        if let Some(ov) = overrides {
            let parsed = parse_overrides(ov).map_err(|e| format!("risk_overrides: {e}"))?;
            if let Some((ex, v)) = parsed.iter().find_map(|(ex, o)| o.max_leverage.filter(|v| *v > frozen.max_leverage).map(|v| (ex, v))) {
                return Err(format!("live: armed, the {} max_leverage override {} is above the frozen {}; lock first, then Arm again", ex.name(), v.normalize(), frozen.max_leverage.normalize()));
            }
        }
        Ok(())
    }

    /// Node 0 stage of an entry under Live (`RiskLimits`: the pair's notional, leverage and the
    /// pairs-per-arming count). `Some(reason)` = block.
    pub(super) fn live_entry_block(&self, entry: &EntrySnapshot) -> Option<String> {
        if !self.live_on() {
            return None;
        }
        let Some(frozen) = self.live.frozen.as_ref() else { return Some("live is not armed".into()) };
        let risk = match self.live_risk() {
            Ok(r) => r,
            Err(e) => return Some(format!("live caps unreadable ({e})")),
        };
        match live_guard::effective_leg_cap(&risk, Some(frozen)) {
            Ok(cap) if entry.notional_usdt > cap => {
                return Some(format!("pair notional {} USDT per leg is above the live cap {} USDT", entry.notional_usdt.normalize(), cap.normalize()));
            }
            Ok(_) => {}
            Err(e) => return Some(e),
        }
        if entry.leverage > frozen.max_leverage {
            return Some(format!("leverage {}x is above the frozen max_leverage {}x", entry.leverage.normalize(), frozen.max_leverage.normalize()));
        }
        match live_guard::effective_pairs_cap(&risk, Some(frozen)) {
            Ok(cap) if self.live.pairs_entered >= cap => Some(format!("{} pair(s) already entered since arming (live_max_pairs_per_arming {cap}): Arm again", self.live.pairs_entered)),
            Ok(_) => None,
            Err(e) => Some(e),
        }
    }

    /// Right after an `OrderRequest` exists and right before it is sent: a non-reduce-only order may
    /// not exceed the cap at `quantity x worst protection price`. `prices` = the leg's base and latest prices.
    pub(super) fn live_order_check(&self, req: &OrderRequest, unit_base: Decimal, worst_price: Option<Decimal>) -> Result<(), String> {
        if !self.live_on() || req.reduce_only {
            return Ok(());
        }
        let cap = self.live_leg_cap().map_err(|e| format!("live cap unknown ({e})"))?;
        live_guard::check_leg_notional(cap, req.quantity * unit_base, worst_price)
    }

    /// Worst price of one leg for `live_order_check`: the larger of its base prices (baseline, scan)
    /// and its latest prices (pre-trade, the market feed), plus the 1 % protection buffer.
    pub(super) fn live_worst_price(&self, pair: &str, leg: Leg, exchange: Exchange, symbol: &str, scan: Decimal, pretrade: Decimal) -> Option<Decimal> {
        let baseline = self.flows.get(pair).and_then(|f| f.baseline.as_ref()).and_then(|(l, s)| match leg {
            Leg::Long => l.as_ref().ok(),
            Leg::Short => s.as_ref().ok(),
        });
        let feed = self.market_rx.borrow().get(&(exchange, symbol.to_string())).copied();
        let mut candidates = vec![scan, pretrade];
        candidates.extend(baseline.map(|q| q.price));
        candidates.extend(feed);
        live_guard::worst_protection_price(&candidates)
    }

    /// Last gate before the opening legs are sent (after the pre-send barrier): armed, and every leg
    /// still within the cap as it stands NOW (a cap lowered meanwhile applies).
    pub(super) fn live_final_check(&self, pair: &str, set: &OrderSet) -> Result<(), String> {
        if !self.live_on() {
            return Ok(());
        }
        if !self.armed() {
            return Err("live is not armed".into());
        }
        let worst = self.flows.get(pair).map(|f| f.live_worst_price).unwrap_or_default();
        for leg in Leg::BOTH {
            if let Some(o) = set.legs[idx(leg)].as_ref() {
                self.live_order_check(&o.req, o.unit_base, worst[idx(leg)]).map_err(|e| format!("{}: {e}", leg.as_str()))?;
            }
        }
        Ok(())
    }

    /// A pair reached ORDER_SUBMIT: it takes one of this arming's entries (reserved until its opening
    /// orders are handed to the executor, so concurrent pairs cannot overshoot the cap).
    pub(super) fn live_pair_entered(&mut self, pair: &str) {
        if self.live_on() && self.live.unsent.insert(pair.to_string()) {
            self.live.pairs_entered += 1;
            self.publish_live();
        }
    }

    /// The pair's opening orders were handed to the executor: the entry is used for good.
    pub(super) fn live_pair_sent(&mut self, pair: &str) {
        self.live.unsent.remove(pair);
    }

    /// The pair left ORDER_SUBMIT without any order sent ("nothing sent"): the entry is given back.
    pub(super) fn live_pair_left_order_submit(&mut self, pair: &str) {
        if self.live_on() && self.live.unsent.remove(pair) {
            self.live.pairs_entered = self.live.pairs_entered.saturating_sub(1);
            self.publish_live();
        }
    }

    /// After the opening orders were handed to the executor: at the cap, lock (the pair itself
    /// completes; the next entry is refused).
    pub(super) fn live_lock_if_cap_reached(&mut self) {
        if !self.armed() {
            return;
        }
        match self.live_risk().and_then(|r| live_guard::effective_pairs_cap(&r, self.live.frozen.as_ref())) {
            Ok(cap) if self.live.pairs_entered >= cap => {
                self.lock_live(&format!("pairs per arming reached ({} of {cap})", self.live.pairs_entered));
            }
            Ok(_) => {}
            Err(e) => self.lock_live(&format!("pairs-per-arming cap unreadable ({e})")),
        }
    }

    // ---- preflight and Arm ----

    pub(super) fn run_preflight_command(&mut self) -> CommandReply {
        if !self.live_on() {
            return CommandReply::Rejected("preflight is only available under the Live identity".into());
        }
        if self.live.preflight_running {
            return CommandReply::Rejected("a preflight is already running".into());
        }
        self.live.preflight_running = true;
        self.publish_live();
        let (probes, account, offsets, tx) = (self.probes.clone(), self.account.clone(), self.offsets.clone(), self.event_tx.clone());
        let risk = self.live_risk();
        let now = self.clock.now_ms();
        tokio::spawn(async move {
            let report = preflight::run_preflight(probes.as_ref(), account.as_ref(), offsets.as_ref(), risk, now).await;
            let _ = tx.send(Event::PreflightDone { report }).await;
        });
        CommandReply::Accepted
    }

    pub(super) fn on_preflight_done(&mut self, report: preflight::PreflightReport) {
        self.live.preflight_running = false;
        self.note(LIVE_PREFLIGHT, None, report.to_json());
        self.live.preflight = Some(report);
        self.publish_live();
    }

    fn arm_refused(&self, why: String) -> CommandReply {
        self.note(LIVE_ARM_REFUSED, None, json!({ "reason": why }));
        CommandReply::Rejected(format!("Arm refused: {why}"))
    }

    pub(super) fn arm(&mut self, preflight_id: &str, caps_hash: &str, confirmation: &str, checklist_ack: bool) -> CommandReply {
        if !self.live_on() {
            return CommandReply::Rejected("Arm is only available under the Live identity".into());
        }
        if !checklist_ack {
            return self.arm_refused("the manual checklist was not acknowledged".into());
        }
        if !live_guard::confirmation_matches(confirmation) {
            return self.arm_refused("the confirmation sentence does not match".into());
        }
        let now = self.clock.now_ms();
        let Some(report) = self.live.preflight.clone() else {
            return self.arm_refused("no preflight has been run".into());
        };
        if let Err(why) = report.accepts(preflight_id, now) {
            return self.arm_refused(why);
        }
        let risk = match self.live_risk() {
            Ok(r) => r,
            Err(e) => return self.arm_refused(format!("settings unreadable ({e})")),
        };
        let Some(current) = live_guard::caps_hash(&risk) else {
            return self.arm_refused("live caps are not set".into());
        };
        if current != caps_hash {
            return self.arm_refused("caps_hash does not match the caps stored now".into());
        }
        if report.caps_hash.as_deref() != Some(current.as_str()) {
            return self.arm_refused("the caps changed after the preflight: run it again".into());
        }
        let frozen = match FrozenCaps::from_risk(&risk) {
            Ok(f) => f,
            Err(e) => return self.arm_refused(e),
        };
        if let Some(r) = &self.reconciliation_pending {
            return self.arm_refused(format!("reconciliation is pending ({r})"));
        }
        let payload = json!({
            "caps_hash": current,
            "preflight_issued_at_ms": report.issued_at_ms,
            "caps": {
                "live_max_leg_notional_usdt": frozen.leg_notional_usdt.normalize().to_string(),
                "live_max_pairs_per_arming": frozen.pairs_per_arming,
                "max_concurrent_pairs": frozen.max_concurrent_pairs,
                "max_leverage": frozen.max_leverage.normalize().to_string(),
            },
        });
        if let Err(e) = self.db.set_kill_switch_with_event(false, LIVE_ARMED, &payload) {
            return self.arm_refused(format!("cannot lift the kill switch: {e}"));
        }
        self.live.frozen = Some(frozen);
        self.live.pairs_entered = 0;
        self.live.unsent.clear();
        self.live.preflight = None; // single use
        self.live.last_lock_reason = None;
        self.dirty = true;
        self.publish_live();
        CommandReply::Accepted
    }

    /// The ONE way an automatic safeguard stops new entries (the watchdog, the breaker's trip, the breaker
    /// that cannot read): kill switch ON together with its reason event in one transaction (a write
    /// that fails halts the store: fail closed), a Live engine disarmed with `last_lock_reason` set so
    /// the arming panel says why. Closing and manual handling stay open.
    pub(super) fn lock_new_entries(&mut self, event_type: &str, payload: &Value, live_reason: String) {
        if let Err(e) = self.db.set_kill_switch_with_event(true, event_type, payload)
            && !self.db.is_halted()
        {
            self.db.halt(HaltReason::KillSwitchReadFailed(format!("{event_type} lock could not be written: {e}")));
        }
        if self.live_on() {
            self.live.last_lock_reason = Some(live_reason);
            self.live_disarm();
        }
        self.dirty = true;
    }

    /// `SetKillSwitch { on: true }` by hand: disarmed in memory too.
    pub(super) fn live_disarm(&mut self) {
        if self.live_on() {
            self.live.frozen = None;
            self.publish_live();
        }
    }

    // ---- FlattenLeg ----

    pub(super) fn flatten_pending(&self, pair: &str) -> bool {
        self.flows.get(pair).is_some_and(|f| f.close.as_ref().is_some_and(|s| s.legs.iter().flatten().any(|o| !o.is_over())))
    }

    pub(super) fn flatten_leg(&mut self, pair: &str, leg: Leg) -> CommandReply {
        if !self.live_on() {
            return CommandReply::Rejected("FlattenLeg is only available under the Live identity".into());
        }
        let Some(view) = self.pairs.get(pair).cloned() else {
            return CommandReply::Rejected(format!("unknown pair {pair}"));
        };
        if !alert::is_alert_state(view.state) {
            return CommandReply::Rejected(format!("pair is {}: FlattenLeg applies to PARTIAL_FAILURE / IMBALANCED / UNRESOLVED", view.state));
        }
        if let Some(why) = self.executor_mismatch(&view) {
            return CommandReply::Rejected(why);
        }
        // one check or flatten at a time per pair: while the naked-leg watchdog works on it, the user waits
        if self.watchdog_busy(pair) {
            return CommandReply::Rejected("the naked-leg watchdog is checking or flattening this pair right now; try again in a few seconds".into());
        }
        let flow = self.flows.entry(pair.to_string()).or_default();
        if flow.flatten_inflight[idx(leg)] {
            return CommandReply::Rejected(format!("a flatten of the {} leg is already in progress", leg.as_str()));
        }
        // the pair's own orders on this leg: open fills minus earlier close fills is what it holds
        let orders: Vec<PairOrder> = match self.db.list_intents_for_pair(pair) {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| {
                    let (l, action) = ids::leg_action_of(&row.client_order_id)?;
                    let rejected = IntentState::parse(&row.state) == Some(IntentState::Failed);
                    (l == leg).then_some(PairOrder { client_order_id: row.client_order_id, action, rejected })
                })
                .collect(),
            Err(e) => return CommandReply::Rejected(format!("order intents unreadable: {e}")),
        };
        if let Some(flow) = self.flows.get_mut(pair) {
            flow.flatten_inflight[idx(leg)] = true;
        }
        let exchange = match leg {
            Leg::Long => view.long_exchange,
            Leg::Short => view.short_exchange,
        };
        let (account, executor, tx) = (self.account_for(view.simulated), self.executor.clone(), self.event_tx.clone());
        let (sym, pair) = (view.symbol.clone(), pair.to_string());
        tokio::spawn(async move {
            let (position, recorded) = tokio::join!(signed_position(account.as_ref(), exchange, &sym), recorded_fill(executor.as_ref(), exchange, &sym, &orders));
            let _ = tx.send(Event::FlattenPositionFetched { pair, leg, position, recorded }).await;
        });
        CommandReply::Accepted
    }

    pub(super) fn flatten_aborted(&mut self, pair: &str, leg: Leg, reason: String, detail: Option<Value>, by: FlattenBy) {
        let mut payload = json!({ "leg": leg.as_str(), "reason": reason, "by": by.as_str() });
        if let Some(d) = detail {
            payload["detail"] = d;
        }
        self.note(LEG_FLATTEN_ABORTED, Some(pair), payload);
    }

    /// The one place a flatten order is sent, for the Live manual `FlattenLeg` and for the naked-leg
    /// watchdog: a reduce-only market close of `qty` (already sized by `plan_leg_close`) of ONE leg.
    /// The order intent is landed first; if that fails nothing is sent. The pair state is not changed.
    /// Returns the `client_order_id`, or why nothing was sent.
    pub(super) fn send_flatten(&mut self, view: &PairView, plan: FlattenPlan, eff: &EffectiveConfig, by: FlattenBy, attempt: Option<u32>) -> Result<String, String> {
        let FlattenPlan { leg, qty, position } = plan;
        let pair = view.internal_uuid.as_str();
        let exchange = match leg {
            Leg::Long => view.long_exchange,
            Leg::Short => view.short_exchange,
        };
        // A close of this leg that is not over yet is never overwritten (nor doubled): refuse.
        if let Some(open) = self.flows.get(pair).and_then(|f| f.close.as_ref()).and_then(|s| s.legs[idx(leg)].as_ref()).filter(|o| !o.is_over()) {
            return Err(format!("a close order of the {} leg ({}) is already in flight; nothing sent", leg.as_str(), open.req.client_order_id));
        }
        let prefix = self.id_prefix();
        let req = self.close_request(prefix, pair, leg, exchange, &view.symbol, qty);
        // intent first: if it cannot be landed nothing is sent
        intent::land_intents(&self.db, pair, &[(leg, &req)]).map_err(|why| format!("order intent not landed ({why}); nothing sent"))?;
        let mut payload = json!({
            "leg": leg.as_str(),
            "exchange": exchange.name(),
            "symbol": view.symbol,
            "client_order_id": req.client_order_id,
            "quantity": dstr(qty),
            "position": dstr(position),
            "simulated": view.simulated,
            "by": by.as_str(),
        });
        if let Some(n) = attempt {
            payload["attempt"] = json!(n);
        }
        self.note(LEG_FLATTEN, Some(pair), payload);
        let now = self.clock.now_ms();
        let client_order_id = req.client_order_id.clone();
        let flow = self.flows.entry(pair.to_string()).or_default();
        flow.close_triggered_ms = Some(now);
        let set = flow.close.get_or_insert_with(|| OrderSet { sent_at_ms: now, eff: eff.clone(), legs: [None, None], cancel_started: false });
        set.legs[idx(leg)] = Some(LegOrder::new(req.clone(), Decimal::ONE));
        self.spawn_submit_inner(pair.to_string(), leg, OrderAction::Close, req, Some(now), true);
        Ok(client_order_id)
    }

    /// Size the leg's reduce-only close with `plan_leg_close` (min of recorded fill and position,
    /// the same imbalance check as every close) and send it. The pair state is not changed.
    pub(super) fn on_flatten_position(&mut self, pair: &str, leg: Leg, position: Result<Decimal, String>, recorded: Result<Decimal, String>) {
        if let Some(flow) = self.flows.get_mut(pair) {
            flow.flatten_inflight[idx(leg)] = false;
        }
        let manual = FlattenBy::Manual;
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        // The watchdog got to the pair while the reads were out: it decides, one close only.
        if self.watchdog_busy(pair) {
            return self.flatten_aborted(pair, leg, "the naked-leg watchdog is checking or flattening this pair; nothing sent".into(), None, manual);
        }
        if !alert::is_alert_state(view.state) {
            return self.flatten_aborted(pair, leg, format!("pair is {} now; nothing sent", view.state), None, manual);
        }
        let position = match position {
            Ok(p) => p,
            Err(e) => return self.flatten_aborted(pair, leg, format!("position unreadable: {e}; nothing sent"), None, manual),
        };
        // like every close: never held back by unreadable settings (defaults then)
        let (risk, overrides) = self.load_risk().unwrap_or_else(|_| (RiskConfig::default(), RiskOverrides::new()));
        let eff = effective_for_pair(&risk, &overrides, view.long_exchange, view.short_exchange);
        let exchange = match leg {
            Leg::Long => view.long_exchange,
            Leg::Short => view.short_exchange,
        };
        let qty = match plan_leg_close(leg, exchange, &view.symbol, view.simulated, eff.max_leg_imbalance_pct, position, recorded) {
            Ok((qty, _residual)) => qty,
            Err(LegClosePlanError::WrongSide(reason)) => return self.flatten_aborted(pair, leg, format!("{reason}; nothing sent"), None, manual),
            Err(LegClosePlanError::Mismatch(alert)) => {
                self.note(CLOSE_QUANTITY_MISMATCH, Some(pair), alert.clone());
                let reason = alert["reason"].as_str().unwrap_or("position and recorded fill disagree").to_string();
                return self.flatten_aborted(pair, leg, format!("{reason}; nothing sent"), Some(alert), manual);
            }
        };
        if qty.is_zero() {
            return self.flatten_aborted(pair, leg, "nothing to close on this leg (already flat)".into(), None, manual);
        }
        if let Err(why) = self.send_flatten(&view, FlattenPlan { leg, qty, position }, &eff, manual, None) {
            self.flatten_aborted(pair, leg, why, None, manual);
        }
    }
}

/// What a flatten sends: the leg, how much (already sized by `plan_leg_close`) and the position it was
/// sized against (for the record).
#[derive(Debug, Clone, Copy)]
pub(super) struct FlattenPlan {
    pub(super) leg: Leg,
    pub(super) qty: Decimal,
    pub(super) position: Decimal,
}

/// Who asked for a flatten order (written as `by` on `LEG_FLATTEN` / `LEG_FLATTEN_ABORTED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlattenBy {
    /// The user pressed the flatten button (Live only).
    Manual,
    /// The naked-leg watchdog (authorization B).
    Watchdog,
}

impl FlattenBy {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            FlattenBy::Manual => "manual",
            FlattenBy::Watchdog => "watchdog",
        }
    }
}

type Texts = Vec<(&'static str, String)>;

fn errs<T>(source: &'static str, r: &Result<T, String>, out: &mut Texts) {
    if let Err(e) = r {
        out.push((source, e.clone()));
    }
}

fn submit_texts(source: &'static str, o: &SubmitOutcome, out: &mut Texts) {
    match o {
        SubmitOutcome::Rejected { reason } | SubmitOutcome::Unknown { reason } => out.push((source, reason.clone())),
        SubmitOutcome::Accepted(_) => {}
    }
}

fn query_texts(source: &'static str, o: &QueryOutcome, out: &mut Texts) {
    if let QueryOutcome::Failed { reason } = o {
        out.push((source, reason.clone()));
    }
}

/// Every error text an event carries (exhaustive on purpose: a new event must be classified here).
fn event_errors(ev: &Event) -> Texts {
    let mut out = Texts::new();
    match ev {
        Event::BaselineFetched { long, short, .. } | Event::PretradeFetched { long, short, .. } | Event::RecheckFetched { long, short, .. } => {
            errs("market data", long, &mut out);
            errs("market data", short, &mut out);
        }
        Event::MarginFetched { long, short, .. } => {
            errs("margin read", long, &mut out);
            errs("margin read", short, &mut out);
        }
        Event::EntryContextFetched { long_rules, short_rules, long_foreign, short_foreign, leverage_caps, .. } => {
            errs("order rules", long_rules, &mut out);
            errs("order rules", short_rules, &mut out);
            errs("account read", long_foreign, &mut out);
            errs("account read", short_foreign, &mut out);
            for r in leverage_caps.iter().flatten() {
                errs("leverage cap read", r, &mut out);
            }
        }
        Event::ClosePositionsFetched { long, short, long_recorded, short_recorded, long_reference, short_reference, .. } => {
            for r in [long, short, long_recorded, short_recorded] {
                errs("close position read", r, &mut out);
            }
            errs("close reference", long_reference, &mut out);
            errs("close reference", short_reference, &mut out);
        }
        Event::Submitted { outcome, .. } | Event::ManualSubmitted { outcome, .. } => submit_texts("order submit", outcome, &mut out),
        Event::Queried { outcome, .. } | Event::ManualCancelled { outcome, .. } => query_texts("order lookup", outcome, &mut out),
        Event::CancelChecked { cancel, after, .. } => {
            query_texts("order cancel", cancel, &mut out);
            query_texts("order lookup", after, &mut out);
        }
        Event::FlatChecked { flat, .. } => errs("flat check", flat, &mut out),
        Event::ConfirmChecked { result, .. } => errs("flat check", result, &mut out),
        Event::EarlyExitFetched { long, short, .. } => {
            for q in [long, short].into_iter().flatten() {
                errs("market data", q, &mut out);
            }
        }
        Event::EarlyPositionFetched { position, recorded, .. } | Event::FlattenPositionFetched { position, recorded, .. } => {
            errs("position read", position, &mut out);
            errs("position read", recorded, &mut out);
        }
        Event::WatchdogProbed { probe, .. } => {
            for l in &probe.legs {
                errs("position read", &l.read.position, &mut out);
                errs("order lookup", &l.read.recorded, &mut out);
            }
        }
        Event::WatchdogCancelled { cancel, after, .. } => {
            query_texts("order cancel", cancel, &mut out);
            query_texts("order lookup", after, &mut out);
        }
        Event::OpenPrepared { result, .. } => errs("pre-send barrier", result, &mut out),
        Event::ReconciliationDone { result } => errs("reconciliation", result, &mut out),
        Event::PreflightDone { .. } => {}
    }
    out
}

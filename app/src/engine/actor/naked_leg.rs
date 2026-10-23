//! naked-leg-watchdog inside the actor (child of `actor`, so it reaches the actor's private state).
//! The pure decision and the progress rebuilt from events live in `engine::watchdog`; this file reads
//! the exchange, writes the events and sends the one kind of order the watchdog may send: a
//! reduce-only market close of ONE leg of the pair, through the same path as the manual flatten.
//! Rule of this file: it never changes the pair's state, and when anything is unknown it sends
//! nothing, locks the engine (kill switch ON) and says so.

use super::live::{FlattenBy, FlattenPlan};
use super::*;
use crate::engine::live_guard::LEG_FLATTEN_ABORTED;
use crate::engine::watchdog as wd;

/// What the watchdog is doing for a pair right now.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Phase {
    /// Nothing in flight: the next round starts at `next_round_ms`.
    Idle,
    /// A round is out; its result arrives as `Event::WatchdogProbed` (the probe has its own time limit).
    Verifying,
    /// The flatten order of the bare leg was sent and is awaited.
    Flattening(Flattening),
}

/// The flatten order the watchdog has in flight.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Flattening {
    pub(super) leg: Leg,
    pub(super) quantity: Decimal,
    pub(super) client_order_id: String,
    /// Which attempt of this entry it is (1-based).
    pub(super) attempt: u32,
    /// After this the order counts as not confirmed (the pair's effective order timeout).
    pub(super) timeout_at_ms: i64,
}

/// Progress of the watchdog for one alert entry of a pair (memory only; rebuilt from events).
#[derive(Debug, Clone)]
pub(super) struct WatchdogFlow {
    /// Numbers the entry: a result that carries an older number belongs to an earlier entry and is dropped.
    pub(super) entry_no: u64,
    /// The alert state this entry belongs to.
    pub(super) state: PairState,
    /// When the pair entered the state (local clock): the first round is due `WATCHDOG_GRACE_MS` later.
    pub(super) alert_since_ms: i64,
    pub(super) next_round_ms: i64,
    pub(super) rounds: u32,
    pub(super) attempts_sent: u32,
    pub(super) waited_rounds: u32,
    /// Rounds that could not read what they need since the last flatten round (the third is `Unknown`).
    pub(super) read_failed_rounds: u32,
    /// A conclusion that ends the rounds was reached: no further automatic action for this entry.
    pub(super) done: bool,
    pub(super) phase: Phase,
    /// The flatten orders this entry sent itself (the others in flight are the user's), with the fill
    /// each result reported: what an `Unknown` notice and the card list.
    pub(super) sent: Vec<wd::SentOrder>,
}

impl WatchdogFlow {
    /// The pair entered `state` at `now`: nothing happened yet, the first round waits out the grace.
    fn fresh(entry_no: u64, state: PairState, now: i64) -> WatchdogFlow {
        WatchdogFlow { entry_no, state, alert_since_ms: now, next_round_ms: now.saturating_add(wd::WATCHDOG_GRACE_MS), rounds: 0, attempts_sent: 0, waited_rounds: 0, read_failed_rounds: 0, done: false, phase: Phase::Idle, sent: Vec::new() }
    }

    /// Rebuilt from the stored events of the current entry.
    fn restored(entry_no: u64, state: PairState, p: &wd::Progress) -> WatchdogFlow {
        WatchdogFlow {
            entry_no,
            state,
            alert_since_ms: p.alert_since_ms,
            next_round_ms: p.next_round_ms(),
            rounds: p.rounds,
            attempts_sent: p.attempts_sent,
            waited_rounds: p.waited_rounds,
            read_failed_rounds: p.read_failed_rounds,
            done: p.terminal,
            phase: Phase::Idle,
            sent: p.sent.clone(),
        }
    }
}

impl Actor {
    /// `land` calls this after every state change: an alert state starts a fresh entry at `now`, any
    /// other state drops the progress.
    pub(super) fn watchdog_state_changed(&mut self, pair: &str, to: PairState) {
        if alert::is_alert_state(to) {
            self.watchdog_entries += 1;
            let w = WatchdogFlow::fresh(self.watchdog_entries, to, self.clock.now_ms());
            self.flows.entry(pair.to_string()).or_default().watchdog = Some(w);
        } else if let Some(flow) = self.flows.get_mut(pair) {
            flow.watchdog = None;
        }
    }

    /// Makes sure the pair's current alert entry has its progress in memory, rebuilding it from the
    /// stored events when it has none (after a restart, or when the reconciler moved the pair).
    /// `false` = the pair is not in an alert state (or the events could not be read).
    pub(super) fn ensure_watchdog(&mut self, pair: &str) -> bool {
        let Some(state) = self.pairs.get(pair).map(|v| v.state).filter(|s| alert::is_alert_state(*s)) else { return false };
        if self.flows.get(pair).and_then(|f| f.watchdog.as_ref()).is_some_and(|w| w.state == state) {
            return true;
        }
        match wd::load_progress(&self.db, pair, state, self.clock.now_ms()) {
            Ok(progress) => {
                self.watchdog_entries += 1;
                let w = WatchdogFlow::restored(self.watchdog_entries, state, &progress);
                self.flows.entry(pair.to_string()).or_default().watchdog = Some(w);
                true
            }
            // unreadable events: no round (a store that cannot be read cannot record a decision either)
            Err(_) => false,
        }
    }

    /// Once per tick for a pair in an alert state: starts the round that is due, if any. Never
    /// changes the pair's state.
    pub(super) fn watchdog_tick(&mut self, view: &PairView, now: i64) {
        let pair = view.internal_uuid.as_str();
        // A pair the startup reconciler still owns is its to decide first.
        if self.reconcile_scope.contains(pair) {
            return;
        }
        // A pair of the other execution mode can neither be read nor closed by this engine: say so once.
        if let Some(why) = self.executor_mismatch(view) {
            let payload = json!({
                "kind": wd::KIND_SKIPPED,
                "reason": why,
                "alert_state": view.state.as_str(),
                "pair_id": view.pair_id,
                "symbol": view.symbol,
                "simulated": view.simulated,
            });
            self.report_once(pair, format!("{}:{}", wd::NAKED_LEG_WATCHDOG, wd::KIND_SKIPPED), wd::NAKED_LEG_WATCHDOG, payload);
            return;
        }
        // A store that cannot be written can neither record a decision nor land an order intent.
        if self.db.is_halted() || !self.ensure_watchdog(pair) {
            return;
        }
        let Some((phase, done, next_round_ms)) = self.flows.get(pair).and_then(|f| f.watchdog.as_ref()).map(|w| (w.phase.clone(), w.done, w.next_round_ms)) else { return };
        if done {
            return;
        }
        match phase {
            // A flatten of the user's (reading, or its order not over) goes first: two closes of one
            // position are never sent. The round comes when that is over.
            Phase::Idle if now >= next_round_ms && self.user_flatten_active(pair, now) => {}
            Phase::Idle if now >= next_round_ms => self.start_round(view, now),
            Phase::Idle => {}
            Phase::Verifying => {}
            Phase::Flattening(f) => self.check_flatten(view, &f, now),
        }
    }

    /// The watchdog is checking or flattening this pair right now (the manual flatten waits).
    pub(super) fn watchdog_busy(&self, pair: &str) -> bool {
        self.flows.get(pair).and_then(|f| f.watchdog.as_ref()).is_some_and(|w| !w.done && w.phase != Phase::Idle)
    }

    /// The user's manual flatten is reading the exchange, or a flatten order the user sent (a manual
    /// `FlattenLeg`) is not over yet and still inside its order timeout. Nothing else counts (review
    /// round 2, B1): an ordinary close order whose result never came, or a manual order past its
    /// timeout, is left to the watchdog's own round (lookup -> read failed -> ... -> Unknown, locked).
    pub(super) fn user_flatten_active(&self, pair: &str, now: i64) -> bool {
        let Some(flow) = self.flows.get(pair) else { return false };
        if flow.flatten_inflight.iter().any(|b| *b) {
            return true;
        }
        let Some(set) = flow.close.as_ref() else { return false };
        flow.flattens.iter().filter(|f| f.by == FlattenBy::Manual && now < f.deadline_ms).any(|f| set.legs.iter().flatten().any(|o| o.req.client_order_id == f.client_order_id && !o.is_over()))
    }

    /// The pair's own orders per leg (open and close), from its order intents.
    fn pair_orders_by_leg(&self, pair: &str) -> Result<[Vec<PairOrder>; 2], String> {
        let rows = self.db.list_intents_for_pair(pair).map_err(|e| e.to_string())?;
        let mut orders: [Vec<PairOrder>; 2] = [Vec::new(), Vec::new()];
        for row in rows {
            if let Some((leg, action)) = ids::leg_action_of(&row.client_order_id) {
                let rejected = IntentState::parse(&row.state) == Some(IntentState::Failed);
                orders[idx(leg)].push(PairOrder { client_order_id: row.client_order_id, action, rejected });
            }
        }
        Ok(orders)
    }

    /// Marks a round as started; returns the number of the entry it belongs to.
    fn begin_round(&mut self, pair: &str) -> Option<u64> {
        let w = self.flows.get_mut(pair)?.watchdog.as_mut()?;
        w.phase = Phase::Verifying;
        w.rounds = w.rounds.saturating_add(1);
        Some(w.entry_no)
    }

    /// One round: both legs' positions and every one of the pair's orders are read (in parallel, off
    /// the actor); the result comes back as `Event::WatchdogProbed`.
    fn start_round(&mut self, view: &PairView, now: i64) {
        let pair = view.internal_uuid.clone();
        let orders = match self.pair_orders_by_leg(&pair) {
            Ok(o) => o,
            Err(why) => {
                // Without the pair's own order list nothing can be proven.
                self.begin_round(&pair);
                let decision = wd::Decision::Unknown { reason: format!("order intents unreadable: {why}") };
                return self.finish_round(view, decision, None, now);
            }
        };
        let Some(entry_no) = self.begin_round(&pair) else { return };
        let (account, executor, tx) = (self.account_for(view.simulated), self.executor.clone(), self.event_tx.clone());
        let legs = [(Leg::Long, view.long_exchange), (Leg::Short, view.short_exchange)];
        let symbol = view.symbol.clone();
        tokio::spawn(async move {
            // a task that ends without a result (a panic) still reports one: an unreadable round
            let lost = wd::WatchdogProbe::unreadable(legs, "the verification task ended without a result");
            let mut guard = LostTaskGuard::new(tx.clone(), Event::WatchdogProbed { pair: pair.clone(), entry_no, probe: lost });
            let limit = Duration::from_millis(wd::WATCHDOG_PROBE_TIMEOUT_MS as u64);
            let probe = match tokio::time::timeout(limit, probe_pair(account.as_ref(), executor.as_ref(), &symbol, legs, orders)).await {
                Ok(p) => p,
                Err(_) => wd::WatchdogProbe::unreadable(legs, &format!("the verification round did not finish within {} s", wd::WATCHDOG_PROBE_TIMEOUT_MS / 1000)),
            };
            guard.disarm();
            let _ = tx.send(Event::WatchdogProbed { pair, entry_no, probe }).await;
        });
    }

    /// A verification round came back: decide, record, and apply the conclusion to the progress.
    pub(super) fn on_watchdog_probed(&mut self, pair: &str, entry_no: u64, probe: wd::WatchdogProbe) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        let Some(w) = self.flows.get(pair).and_then(|f| f.watchdog.clone()) else { return };
        // A late result of an earlier entry, of a pair that left the state, or of a round that is not
        // in flight is dropped.
        if w.entry_no != entry_no || w.state != view.state || w.done || !matches!(w.phase, Phase::Verifying) {
            return;
        }
        let now = self.clock.now_ms();
        // like every close: never held back by unreadable settings (defaults then)
        let (risk, overrides) = self.load_risk().unwrap_or_else(|_| (RiskConfig::default(), RiskOverrides::new()));
        let tolerance = effective_for_pair(&risk, &overrides, view.long_exchange, view.short_exchange).max_leg_imbalance_pct;
        let input = wd::DecideInput {
            symbol: &view.symbol,
            simulated: view.simulated,
            tolerance_pct: tolerance,
            legs: [probe.legs[0].read.clone(), probe.legs[1].read.clone()],
            waited_rounds: w.waited_rounds,
            read_failed_rounds: w.read_failed_rounds,
            attempts_sent: w.attempts_sent,
        };
        let decision = wd::decide(&input);
        self.finish_round(&view, decision, Some((&probe, tolerance)), now);
    }

    /// Writes the round's record and applies its conclusion to the progress (never to the pair).
    fn finish_round(&mut self, view: &PairView, decision: wd::Decision, evidence: Option<(&wd::WatchdogProbe, Decimal)>, now: i64) {
        let pair = view.internal_uuid.as_str();
        let Some(w) = self.flows.get(pair).and_then(|f| f.watchdog.clone()) else { return };
        let payload = round_payload(view, &w, &decision, evidence);
        let recorded = self.events.append(wd::NAKED_LEG_WATCHDOG, Some(pair), payload).is_ok();
        if let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
            w.phase = Phase::Idle;
        }
        self.dirty = true;
        // the ONE place the run counters change (independent of each other, see `wd::RoundCounts`)
        if let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
            let counts = wd::RoundCounts { waited: w.waited_rounds, read_failed: w.read_failed_rounds }.after(decision.conclusion());
            (w.waited_rounds, w.read_failed_rounds) = (counts.waited, counts.read_failed);
        }
        // The action of the conclusion comes only after its record.
        match decision {
            // Something could not be read: nothing is decided yet, the next round (5 s later) looks
            // again; the third failure is `Unknown` (see `wd::decide`).
            wd::Decision::ReadFailed { .. } => {
                if let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
                    w.next_round_ms = now.saturating_add(wd::WATCHDOG_INTERVAL_MS);
                }
            }
            wd::Decision::Flatten { leg, qty } => {
                let position = evidence.and_then(|(probe, _)| probe.legs[idx(leg)].read.position.clone().ok()).unwrap_or_default();
                self.begin_flatten(view, leg, qty, position, recorded, now);
            }
            // An order that is not over is cancelled; the next round (5 s later) looks again.
            wd::Decision::WaitForCancel => {
                if let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
                    w.next_round_ms = now.saturating_add(wd::WATCHDOG_INTERVAL_MS);
                }
                if let Some((probe, _)) = evidence {
                    self.cancel_unfinished(view, probe);
                }
            }
            // these end the rounds
            wd::Decision::Hedged | wd::Decision::Flat => self.end_rounds(pair),
            // Not provable (a lookup failed, a position does not match, the attempts are used up):
            // nothing is sent, new entries stop, the user is told.
            wd::Decision::Unknown { reason } => {
                self.end_rounds(pair);
                self.watchdog_lock(view, &reason);
            }
        }
    }

    /// No further automatic action for this entry until the user acts.
    fn end_rounds(&mut self, pair: &str) {
        if let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
            w.done = true;
        }
    }

    /// The situation could not be proven and nothing was sent: new entries stop (kill switch ON,
    /// written with `NAKED_LEG_WATCHDOG_LOCK` in ONE transaction), a Live engine is disarmed, and the
    /// user is told. Closing and every manual action stay available.
    fn watchdog_lock(&mut self, view: &PairView, reason: &str) {
        let payload = json!({
            "pair": view.internal_uuid,
            "pair_id": view.pair_id,
            "symbol": view.symbol,
            "alert_state": view.state.as_str(),
            "simulated": view.simulated,
            "reason": reason,
        });
        self.lock_new_entries(wd::NAKED_LEG_WATCHDOG_LOCK, &payload, format!("naked-leg watchdog: {reason}"));
        let sent = self.flows.get(&view.internal_uuid).and_then(|f| f.watchdog.as_ref()).map(|w| w.sent.clone()).unwrap_or_default();
        self.notify_watchdog(view, alert::WatchdogOutcome::Locked { reason: reason.to_string(), sent });
    }

    /// One notification about what the watchdog did, off the actor (a notifier may block). Its failure
    /// changes nothing but one record.
    fn notify_watchdog(&self, view: &PairView, outcome: alert::WatchdogOutcome) {
        let label = match &outcome {
            alert::WatchdogOutcome::Flattened { .. } => "FLATTENED",
            alert::WatchdogOutcome::Locked { .. } => "LOCKED",
        };
        let notice = alert::WatchdogNotice {
            pair: view.internal_uuid.clone(),
            pair_id: view.pair_id.clone(),
            symbol: view.symbol.clone(),
            simulated: view.simulated,
            identity: self.identity,
            outcome,
        };
        let (notifier, events) = (self.notifier.clone(), self.events.clone());
        tokio::task::spawn_blocking(move || {
            let result = notifier.notify_watchdog(&notice);
            let mut payload = json!({ "kind": wd::KIND_NOTIFY_RESULT, "ok": result.is_ok(), "outcome": label });
            if let Err(e) = result {
                payload["error"] = json!(e);
            }
            let _ = events.append(wd::NAKED_LEG_WATCHDOG, Some(&notice.pair), payload);
        });
    }

    /// Sends the one reduce-only close of the bare leg (through the same path as the manual flatten:
    /// intent first, nothing sent when it cannot be landed) and waits for its result.
    pub(super) fn begin_flatten(&mut self, view: &PairView, leg: Leg, qty: Decimal, position: Decimal, decision_recorded: bool, now: i64) {
        let pair = view.internal_uuid.as_str();
        let Some(attempts_sent) = self.flows.get(pair).and_then(|f| f.watchdog.as_ref()).map(|w| w.attempts_sent) else {
            return;
        };
        let attempt = attempts_sent.saturating_add(1);
        // Whatever happens, this is attempt `attempt`: one that could not even be sent (no record, no
        // intent, a close already in flight) uses it up too, so three of them end in `Unknown`.
        let aborted = |this: &mut Actor, why: String| {
            this.watchdog_attempt_aborted(pair, leg, why, attempt);
            if let Some(w) = this.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
                w.attempts_sent = attempt;
                w.next_round_ms = now.saturating_add(wd::WATCHDOG_INTERVAL_MS);
            }
        };
        // no record, no order
        if !decision_recorded {
            return aborted(self, "the conclusion could not be recorded; nothing sent".into());
        }
        // like every close: never held back by unreadable settings (defaults then)
        let (risk, overrides) = self.load_risk().unwrap_or_else(|_| (RiskConfig::default(), RiskOverrides::new()));
        let eff = effective_for_pair(&risk, &overrides, view.long_exchange, view.short_exchange);
        match self.send_flatten(view, FlattenPlan { leg, qty, position }, &eff, FlattenBy::Watchdog, Some(attempt)) {
            Ok(client_order_id) => {
                if let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) {
                    w.attempts_sent = attempt;
                    let exchange = match leg {
                        Leg::Long => view.long_exchange,
                        Leg::Short => view.short_exchange,
                    };
                    w.sent.push(wd::SentOrder { leg, exchange: exchange.name().to_string(), quantity: qty, client_order_id: client_order_id.clone(), filled: None });
                    w.phase = Phase::Flattening(Flattening { leg, quantity: qty, client_order_id, attempt, timeout_at_ms: fill::timeout_at_ms(&eff, now) });
                }
            }
            Err(why) => aborted(self, why),
        }
    }

    /// `LEG_FLATTEN_ABORTED` of the watchdog's attempt number `attempt` (the number is what a restart counts).
    fn watchdog_attempt_aborted(&mut self, pair: &str, leg: Leg, reason: String, attempt: u32) {
        let payload = json!({ "leg": leg.as_str(), "reason": reason, "by": FlattenBy::Watchdog.as_str(), "attempt": attempt });
        self.note(LEG_FLATTEN_ABORTED, Some(pair), payload);
        self.dirty = true;
    }

    /// Looks at the flatten order in flight: filled, refused, ended without being filled, or not
    /// confirmed within the order timeout. Anything but "still waiting" ends the attempt.
    fn check_flatten(&mut self, view: &PairView, f: &Flattening, now: i64) {
        let pair = view.internal_uuid.as_str();
        let order = self.flows.get(pair).and_then(|fl| fl.close.as_ref()).and_then(|s| s.legs[idx(f.leg)].as_ref()).filter(|o| o.req.client_order_id == f.client_order_id);
        let (result, filled): (Option<Result<(), String>>, Option<Decimal>) = match order {
            None => (Some(Err("the order is no longer tracked".into())), None),
            Some(o) => {
                let filled = o.status.as_ref().map(|s| s.filled_quantity);
                let result = if let Some(reason) = o.submit_rejected() {
                    Some(Err(format!("order rejected: {reason}")))
                } else if o.is_over() && !o.query_inflight {
                    match &o.status {
                        Some(s) if s.state == OrderState::Filled && s.filled_quantity >= o.req.quantity => Some(Ok(())),
                        Some(s) => Some(Err(format!("order ended {:?} with {} of {} filled", s.state, dstr(s.filled_quantity), dstr(o.req.quantity)))),
                        None => Some(Err("order ended without a status".into())),
                    }
                } else if now >= f.timeout_at_ms {
                    Some(Err("order not confirmed within the order timeout".into()))
                } else {
                    None // still in flight
                };
                (result, filled)
            }
        };
        if let Some(result) = result {
            self.finish_attempt(view, f, result, filled, now);
        }
    }

    /// Records what became of the flatten order and schedules the next verification, 5 s later.
    fn finish_attempt(&mut self, view: &PairView, f: &Flattening, result: Result<(), String>, filled: Option<Decimal>, now: i64) {
        let pair = view.internal_uuid.as_str();
        let mut payload = json!({
            "kind": wd::KIND_ATTEMPT_RESULT,
            "ok": result.is_ok(),
            "attempt": f.attempt,
            "leg": f.leg.as_str(),
            "quantity": dstr(f.quantity),
            "client_order_id": f.client_order_id,
            "alert_state": view.state.as_str(),
            "pair_id": view.pair_id,
            "symbol": view.symbol,
            "simulated": view.simulated,
        });
        if let Some(filled) = filled {
            payload["filled"] = json!(dstr(filled));
        }
        if let Err(why) = &result {
            payload["reason"] = json!(why);
        }
        self.note(wd::NAKED_LEG_WATCHDOG, Some(pair), payload);
        if let Some(w) = self.flows.get_mut(pair).and_then(|fl| fl.watchdog.as_mut()) {
            w.phase = Phase::Idle;
            w.next_round_ms = now.saturating_add(wd::WATCHDOG_INTERVAL_MS);
            if let Some(o) = w.sent.iter_mut().find(|o| o.client_order_id == f.client_order_id) {
                o.filled = filled;
            }
        }
        self.dirty = true;
        if result.is_ok() {
            let exchange = match f.leg {
                Leg::Long => view.long_exchange,
                Leg::Short => view.short_exchange,
            };
            self.notify_watchdog(view, alert::WatchdogOutcome::Flattened { leg: f.leg, exchange, quantity: f.quantity });
        }
    }

    /// Cancels the pair's own orders that are not over (never a position, never anybody else's
    /// order), then looks each one up again.
    fn cancel_unfinished(&self, view: &PairView, probe: &wd::WatchdogProbe) {
        for l in &probe.legs {
            for id in l.open_order_ids() {
                let (executor, db, tx) = (self.executor.clone(), self.db.clone(), self.event_tx.clone());
                let (pair, symbol, exchange) = (view.internal_uuid.clone(), view.symbol.clone(), l.read.exchange);
                tokio::spawn(async move {
                    let cancel = executor.cancel(exchange, &symbol, &id).await;
                    let after = executor.query(exchange, &symbol, &id).await;
                    // keep the intent in step with what the exchange reports (state changes only)
                    if let QueryOutcome::Found(status) = &after
                        && let Ok(Some(row)) = db.get_intent(&id)
                        && IntentState::parse(&row.state) != Some(intent_state_of(status))
                    {
                        let _ = intent::record_query_outcome(&db, &id, &after, executor.is_simulated());
                    }
                    let _ = tx.send(Event::WatchdogCancelled { pair, exchange, client_order_id: id, cancel, after }).await;
                });
            }
        }
    }

    /// What the watchdog's cancel and the lookup after it said (a record; the next round decides).
    pub(super) fn on_watchdog_cancelled(&mut self, pair: &str, exchange: Exchange, client_order_id: &str, cancel: &QueryOutcome, after: &QueryOutcome) {
        let verdict = match after {
            QueryOutcome::Found(s) if s.state != OrderState::Open => "final",
            QueryOutcome::Found(_) => "still open after cancel",
            QueryOutcome::NotFound | QueryOutcome::Failed { .. } => "final state not confirmed",
        };
        let (symbol, simulated) = self.pairs.get(pair).map(|v| (v.symbol.clone(), v.simulated)).unwrap_or_default();
        let payload = json!({
            "client_order_id": client_order_id,
            "exchange": exchange.name(),
            "symbol": symbol,
            "cancel": query_json(cancel),
            "after": query_json(after),
            "verdict": verdict,
            "simulated": simulated,
            "by": "watchdog",
        });
        self.note(ORDER_CANCEL_RESULT, Some(pair), payload);
        self.dirty = true;
    }
}

/// The record of one round: the conclusion and the basis it was drawn from.
fn round_payload(view: &PairView, w: &WatchdogFlow, decision: &wd::Decision, evidence: Option<(&wd::WatchdogProbe, Decimal)>) -> Value {
    let mut p = json!({
        "kind": wd::KIND_ROUND,
        "conclusion": decision.conclusion().as_str(),
        "round": w.rounds,
        "alert_state": view.state.as_str(),
        "alert_since_ms": w.alert_since_ms,
        "pair_id": view.pair_id,
        "symbol": view.symbol,
        "simulated": view.simulated,
        "waited_rounds": w.waited_rounds,
        "read_failed_rounds": w.read_failed_rounds,
        "attempts_sent": w.attempts_sent,
    });
    if let Some((probe, tolerance)) = evidence {
        p["tolerance_pct"] = json!(dstr(tolerance));
        p["legs"] = wd::evidence(probe);
    }
    match decision {
        wd::Decision::Flatten { leg, qty } => {
            p["leg"] = json!(leg.as_str());
            p["quantity"] = json!(dstr(*qty));
            p["attempt"] = json!(w.attempts_sent.saturating_add(1));
        }
        wd::Decision::Unknown { reason } | wd::Decision::ReadFailed { reason } => p["reason"] = json!(reason),
        wd::Decision::Hedged | wd::Decision::Flat | wd::Decision::WaitForCancel => {}
    }
    p
}

/// Both legs, in parallel.
async fn probe_pair(account: &dyn AccountView, executor: &dyn Executor, symbol: &str, legs: [(Leg, Exchange); 2], orders: [Vec<PairOrder>; 2]) -> wd::WatchdogProbe {
    let [long_orders, short_orders] = orders;
    let (long, short) = tokio::join!(probe_leg(account, executor, symbol, legs[0], &long_orders), probe_leg(account, executor, symbol, legs[1], &short_orders));
    wd::WatchdogProbe { legs: [long, short] }
}

/// One leg: its position and every one of the pair's orders on it, looked up by `client_order_id`.
async fn probe_leg(account: &dyn AccountView, executor: &dyn Executor, symbol: &str, (leg, exchange): (Leg, Exchange), orders: &[PairOrder]) -> wd::LegProbe {
    let position = signed_position(account, exchange, symbol);
    let lines = async {
        let mut lines = Vec::with_capacity(orders.len());
        for o in orders {
            lines.push(order_line(executor, exchange, symbol, o).await);
        }
        lines
    };
    let (position, lines) = tokio::join!(position, lines);
    wd::LegProbe::from_reads(leg, exchange, position, lines)
}

async fn order_line(executor: &dyn Executor, exchange: Exchange, symbol: &str, o: &PairOrder) -> wd::OrderLine {
    let mut line = wd::OrderLine { client_order_id: o.client_order_id.clone(), action: o.action, rejected_intent: o.rejected, state: None, filled: None, open: false, error: None };
    if o.rejected {
        return line; // never existed on the exchange: no lookup
    }
    match executor.query(exchange, symbol, &o.client_order_id).await {
        QueryOutcome::Found(status) => {
            line.state = Some(format!("{:?}", status.state));
            line.filled = Some(status.filled_quantity);
            line.open = status.state == OrderState::Open;
        }
        QueryOutcome::NotFound => line.error = Some(format!("order {} not found", o.client_order_id)),
        QueryOutcome::Failed { reason } => line.error = Some(format!("order {} lookup failed: {reason}", o.client_order_id)),
    }
    line
}

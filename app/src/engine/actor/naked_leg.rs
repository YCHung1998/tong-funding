//! naked-leg-watchdog inside the actor (child of `actor`, so it reaches the actor's private state).
//! The pure decision and the progress rebuilt from events live in `engine::watchdog`; this file reads
//! the exchange, writes the events and sends the one kind of order the watchdog may send: a
//! reduce-only market close of ONE leg of the pair, through the same path as the manual flatten.
//! Rule of this file: it never changes the pair's state, and when anything is unknown it sends
//! nothing, locks the engine (kill switch ON) and says so.

use super::*;
use crate::engine::watchdog as wd;

/// What the watchdog is doing for a pair right now.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Phase {
    /// Nothing in flight: the next round starts at `next_round_ms`.
    Idle,
    /// A round was started at `started_ms`; its result arrives as `Event::WatchdogProbed`.
    Verifying { started_ms: i64 },
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
    /// A conclusion that ends the rounds was reached: no further automatic action for this entry.
    pub(super) done: bool,
    pub(super) phase: Phase,
}

impl WatchdogFlow {
    /// The pair entered `state` at `now`: nothing happened yet, the first round waits out the grace.
    fn fresh(entry_no: u64, state: PairState, now: i64) -> WatchdogFlow {
        WatchdogFlow { entry_no, state, alert_since_ms: now, next_round_ms: now.saturating_add(wd::WATCHDOG_GRACE_MS), rounds: 0, attempts_sent: 0, waited_rounds: 0, done: false, phase: Phase::Idle }
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
            done: p.terminal,
            phase: Phase::Idle,
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
            Phase::Idle if now >= next_round_ms => self.start_round(view, now),
            Phase::Idle => {}
            // The round's result is overdue (its event was lost): give it up, never wait for ever.
            Phase::Verifying { started_ms } if now.saturating_sub(started_ms) >= wd::WATCHDOG_PROBE_TIMEOUT_MS + REPORT_SLACK_MS => {
                let decision = wd::Decision::Unknown { reason: "the verification round did not report back".into() };
                self.finish_round(view, decision, None, now);
            }
            Phase::Verifying { .. } => {}
        }
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
    fn begin_round(&mut self, pair: &str, now: i64) -> Option<u64> {
        let w = self.flows.get_mut(pair)?.watchdog.as_mut()?;
        w.phase = Phase::Verifying { started_ms: now };
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
                self.begin_round(&pair, now);
                let decision = wd::Decision::Unknown { reason: format!("order intents unreadable: {why}") };
                return self.finish_round(view, decision, None, now);
            }
        };
        let Some(entry_no) = self.begin_round(&pair, now) else { return };
        let (account, executor, tx) = (self.account_for(view.simulated), self.executor.clone(), self.event_tx.clone());
        let legs = [(Leg::Long, view.long_exchange), (Leg::Short, view.short_exchange)];
        let symbol = view.symbol.clone();
        tokio::spawn(async move {
            let limit = Duration::from_millis(wd::WATCHDOG_PROBE_TIMEOUT_MS as u64);
            let probe = match tokio::time::timeout(limit, probe_pair(account.as_ref(), executor.as_ref(), &symbol, legs, orders)).await {
                Ok(p) => p,
                Err(_) => wd::WatchdogProbe::unreadable(legs, &format!("the verification round did not finish within {} s", wd::WATCHDOG_PROBE_TIMEOUT_MS / 1000)),
            };
            let _ = tx.send(Event::WatchdogProbed { pair, entry_no, probe }).await;
        });
    }

    /// A verification round came back: decide, record, and apply the conclusion to the progress.
    pub(super) fn on_watchdog_probed(&mut self, pair: &str, entry_no: u64, probe: wd::WatchdogProbe) {
        let Some(view) = self.pairs.get(pair).cloned() else { return };
        let Some(w) = self.flows.get(pair).and_then(|f| f.watchdog.clone()) else { return };
        // A late result of an earlier entry, of a pair that left the state, or of a round that is not
        // in flight is dropped.
        if w.entry_no != entry_no || w.state != view.state || w.done || !matches!(w.phase, Phase::Verifying { .. }) {
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
            attempts_sent: w.attempts_sent,
        };
        let decision = wd::decide(&input);
        self.finish_round(&view, decision, Some((&probe, tolerance)), now);
    }

    /// Writes the round's record and applies its conclusion to the progress (never to the pair).
    fn finish_round(&mut self, view: &PairView, decision: wd::Decision, evidence: Option<(&wd::WatchdogProbe, Decimal)>, now: i64) {
        let pair = view.internal_uuid.as_str();
        let Some(w) = self.flows.get(pair).and_then(|f| f.watchdog.clone()) else { return };
        let conclusion = decision.conclusion();
        let payload = round_payload(view, &w, &decision, evidence);
        let _ = self.events.append(wd::NAKED_LEG_WATCHDOG, Some(pair), payload);
        let Some(w) = self.flows.get_mut(pair).and_then(|f| f.watchdog.as_mut()) else { return };
        w.phase = Phase::Idle;
        match conclusion {
            wd::Conclusion::WaitForCancel => {
                w.waited_rounds = w.waited_rounds.saturating_add(1);
                w.next_round_ms = now.saturating_add(wd::WATCHDOG_INTERVAL_MS);
            }
            // task 2.2 records the conclusions; acting on them comes with 2.3 - 2.5
            wd::Conclusion::Flatten | wd::Conclusion::Hedged | wd::Conclusion::Flat | wd::Conclusion::Unknown => {
                w.waited_rounds = 0;
                w.done = true;
            }
        }
        self.dirty = true;
    }
}

/// How long after the probe's own limit a missing result is given up (a lost event).
const REPORT_SLACK_MS: i64 = 10_000;

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
        wd::Decision::Unknown { reason } => p["reason"] = json!(reason),
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

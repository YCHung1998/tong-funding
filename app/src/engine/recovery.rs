//! Restart reconciliation (crash-recovery spec, design D12, decision 7; task 4.2).
//!
//! Runs once at startup, before the actor accepts any exposure-opening command, and again (by
//! the actor) while the result is still pending. It is READ-ONLY towards the exchange: it only
//! calls `Executor::query` and `AccountView::{positions, open_orders}`; it never submits, cancels
//! or closes anything. What it writes is the store: query results (`intent::record_query_outcome`),
//! "not sent" marks, pair transitions decided by core `next()` (via `transition::land_then_act`)
//! and alert events.
//!
//! Which pairs are looked at: every pair with an unfinished intent, plus every pair in an
//! in-flight state (PRE_TRADE_CHECK, ORDER_SUBMIT, FILL_MONITOR, CLOSING) even without one (a
//! crash can land the state but no intent yet, or settle every intent but not the state), plus
//! RECONCILED pairs of an interrupted simulation, plus unfinished manual orders (`manual-...`
//! intents without a pair row). With a `scope` (always, from the actor) only the pairs that were
//! in flight at startup are looked at: pairs started after startup are live and never touched,
//! however often a pending reconciliation is retried.
//!
//! PRE_TRADE_CHECK at restart: no order exists yet (intent-first; intents only follow
//! CheckPassed), so the pair goes to BLOCKED (`CheckFailed`) with a `PRE_TRADE_CHECK_INTERRUPTED`
//! event, simulated or not, without any exchange access.
//!
//! Simulated pairs (envelope `simulated`, or any `sim`-prefixed intent) are never queried: the
//! simulated ledger is not persisted, so in-flight and RECONCILED ones go to UNRESOLVED with a
//! `SIMULATION_INTERRUPTED` event (decision 7).
//!
//! Demo pairs: unfinished intents are queried by `client_order_id` and recorded; then the legs of
//! the current phase (open orders in ORDER_SUBMIT / FILL_MONITOR, close orders in CLOSING) are
//! compared with the positions and open orders of their symbols. Rules (spec table):
//! - both legs filled, positions match, no open order → normal flow: ORDER_SUBMIT →
//!   FILL_MONITOR (`BothLegsSubmitted`), then `fill::fill_decision` on the fills in base coin
//!   (OKX contract value from `MarketData::order_rules`) and the pair's effective settings →
//!   RECONCILED / IMBALANCED; CLOSING with both close legs filled and both flat → FINALIZED;
//! - both legs not filled (rejected / cancelled with zero fill, no intent, or not found while the
//!   order was at most SUBMITTED) AND no position AND no open order on either leg → CANCELLED
//!   (`BothSubmitsFailed` from ORDER_SUBMIT, `TimeoutNoFills` from FILL_MONITOR); "not found"
//!   intents are then marked not sent (CANCELLED + `ORDER_INTENT_NOT_SENT`) — design D12;
//! - one leg filled (or partly) and the other confirmed not filled, positions matching →
//!   PARTIAL_FAILURE (`RestartFoundPartial`) + `RECONCILE_ALERT`;
//! - anything else (position or open-order mismatch, live order, tampered row) → UNRESOLVED
//!   (`RestartUndetermined`) + `RECONCILE_ALERT`.
//!
//! Manual orders: queried and recorded; one the exchange does not know, with no position and no
//! open order on its symbol, is marked not sent; one it does not know while the symbol carries
//! exposure raises a `RECONCILE_ALERT` (kept for a human). Simulated manual orders are left as is.
//!
//! Missing data is never read as "no exposure": a failed query, an unavailable exchange (no key,
//! SIMULATION mode, simulated executor) or an incomplete / failed listing leaves the pair
//! untouched and makes the report `pending`. The actor then refuses exposure-opening commands in
//! EXCHANGE_DEMO (decision 8; SIMULATION entries stay allowed) and retries periodically.
//!
//! Core gap (core is not changed): CLOSING has no way back to RECONCILED, so a CLOSING pair whose
//! close orders were never sent goes to UNRESOLVED (a human sends `RequestClose` again).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{Value, json};
use tong_funding_core::pair::{PairState, SystemEvent};
use tong_funding_core::risk::{ExecutionMode, effective_for_pair};
use tong_funding_core::types::{Decimal, Exchange};

use super::actor::{PairEnvelope, load_risk_config};
use super::fill::{self, FillDecision, LegFill};
use super::ids::IdPrefix;
use super::intent::record_query_outcome;
use super::ports::{
    AccountOrder, AccountPosition, AccountView, BoxFut, Executor, Leg, Listed, MarketData, OrderAction, OrderState, QueryOutcome,
    ReconcileContext, StartupReconciler,
};
use super::transition::{self, TransitionError};
use crate::ports::Clock;
use crate::store::db::Db;
use crate::store::events::EventStore;
use crate::store::state::{IntentRow, IntentState, PairRow};

/// A pair went to PARTIAL_FAILURE or UNRESOLVED on restart, or an unexpected live intent was found.
pub const RECONCILE_ALERT: &str = "RECONCILE_ALERT";
/// A simulated pair was in flight when the program stopped (decision 7).
pub const SIMULATION_INTERRUPTED: &str = "SIMULATION_INTERRUPTED";
/// An intent that the exchange does not know, with no exposure on either leg: never sent (D12).
pub const ORDER_INTENT_NOT_SENT: &str = "ORDER_INTENT_NOT_SENT";
/// Summary of one reconciliation run (written only when there was something to reconcile).
pub const RECONCILE_FINISHED: &str = "RECONCILE_FINISHED";
/// A pair was in PRE_TRADE_CHECK when the program stopped: no order was sent; it is BLOCKED.
pub const PRE_TRADE_CHECK_INTERRUPTED: &str = "PRE_TRADE_CHECK_INTERRUPTED";
/// `pair_uuid` prefix of manual orders (no pair row; see `Actor::manual_order`).
pub const MANUAL_PAIR_PREFIX: &str = "manual-";

/// The order-capable executor and account view of EXCHANGE_DEMO. The caller passes
/// `Err(reason)` when they cannot be built (keys unavailable, ...); a simulated executor is
/// treated the same way (it cannot know demo orders).
pub struct ExchangeAccess<'a> {
    pub executor: &'a dyn Executor,
    pub account: &'a dyn AccountView,
}

/// What reconciliation decided for one pair.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Everything matched; the pair continues on its normal path. `fills` (long, short) is set
    /// for a pair now in FILL_MONITOR so the actor can run `fill::fill_decision` at once.
    Normal { fills: Option<(LegFill, LegFill)> },
    /// Nothing was sent / filled and there is no exposure.
    Cancelled,
    PartialFailure,
    Unresolved,
    /// A simulated pair was in flight (UNRESOLVED where core allows it).
    SimulationInterrupted,
    /// The pre-trade check was interrupted; no order was sent (BLOCKED).
    CheckInterrupted,
    /// No transition applies (locked or settled pair); query results were recorded.
    Kept,
    /// Could not be decided now (see `reason`); nothing was transitioned.
    Pending,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PairOutcome {
    pub pair: String,
    /// `None` when the pair row is missing or its state unreadable.
    pub before: Option<PairState>,
    pub after: Option<PairState>,
    pub verdict: Verdict,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileReport {
    /// `Some(reason)`: reconciliation is not complete; the actor must keep refusing every
    /// exposure-opening command with `Blocker::ReconciliationPending(reason)` and retry later.
    /// `None`: nothing to reconcile, or everything was resolved.
    pub pending: Option<String>,
    pub pairs: Vec<PairOutcome>,
    /// `Executor::query` calls made.
    pub queries: usize,
    pub finished_at_ms: i64,
}

/// Everything one reconciliation run needs.
pub struct ReconcileInput<'a> {
    pub db: &'a Db,
    /// Demo access; `Err(reason)` when there is none (keys unavailable, SIMULATION mode).
    pub exchange: Result<ExchangeAccess<'a>, String>,
    /// Order rules (OKX contract value) for the fill decision; `None` = unavailable (an OKX leg
    /// then keeps its pair pending in FILL_MONITOR).
    pub market: Option<&'a dyn MarketData>,
    /// Only these pairs / `pair_uuid`s; `None` = the whole store.
    pub scope: Option<&'a BTreeSet<String>>,
    pub clock: &'a dyn Clock,
}

/// Reconcile the whole store, without order rules (tests and tools).
pub async fn reconcile_startup(db: &Db, exchange: Result<ExchangeAccess<'_>, String>, clock: &dyn Clock) -> ReconcileReport {
    reconcile(ReconcileInput { db, exchange, market: None, scope: None, clock }).await
}

/// Reconcile the store with the exchange after a restart. See the module docs for the rules.
pub async fn reconcile(input: ReconcileInput<'_>) -> ReconcileReport {
    let ReconcileInput { db, exchange, market, scope, clock } = input;
    let mut run = Run {
        db,
        events: EventStore::new(db.clone()),
        market,
        now_ms: clock.now_ms(),
        queries: 0,
        accounts: HashMap::new(),
        store_failed: false,
    };
    let mut outcomes = Vec::new();
    let mut fatal: Option<String> = None;
    match (db.list_unfinished_intents(), db.list_pairs()) {
        (Ok(unfinished), Ok(pairs)) => {
            let exchange = match exchange {
                Ok(a) if a.executor.is_simulated() => Err("only a simulated executor is available".to_string()),
                other => other,
            };
            for cand in candidates(unfinished, pairs).into_iter().filter(|c| scope.is_none_or(|s| s.contains(&c.uuid))) {
                if run.store_failed {
                    break;
                }
                outcomes.push(run.pair(cand, &exchange).await);
            }
        }
        (Err(e), _) => fatal = Some(format!("cannot list unfinished order intents: {e}")),
        (_, Err(e)) => fatal = Some(format!("cannot list pairs: {e}")),
    }
    let mut reasons: Vec<String> = fatal.into_iter().collect();
    reasons.extend(outcomes.iter().filter(|o| o.verdict == Verdict::Pending).map(|o| format!("pair {}: {}", o.pair, o.reason)));
    if run.store_failed {
        reasons.push("store write failed; reconciliation stopped".into());
    }
    let pending = if reasons.is_empty() { None } else { Some(reasons.join("; ")) };
    if !outcomes.is_empty() || pending.is_some() {
        let summary: Vec<Value> = outcomes
            .iter()
            .map(|o| json!({ "pair": o.pair, "verdict": verdict_str(&o.verdict), "after": o.after.map(PairState::as_str) }))
            .collect();
        // Best effort: on a halted store this cannot be written (the halt is the record).
        let _ = run.events.append(RECONCILE_FINISHED, None, json!({ "pending": pending, "pairs": summary, "queries": run.queries }));
    }
    ReconcileReport { pending, pairs: outcomes, queries: run.queries, finished_at_ms: clock.now_ms() }
}

fn verdict_str(v: &Verdict) -> &'static str {
    match v {
        Verdict::Normal { .. } => "NORMAL",
        Verdict::Cancelled => "CANCELLED",
        Verdict::PartialFailure => "PARTIAL_FAILURE",
        Verdict::Unresolved => "UNRESOLVED",
        Verdict::SimulationInterrupted => "SIMULATION_INTERRUPTED",
        Verdict::CheckInterrupted => "CHECK_INTERRUPTED",
        Verdict::Kept => "KEPT",
        Verdict::Pending => "PENDING",
    }
}

/// One pair to reconcile: its row (if any) and its unfinished intents.
struct Candidate {
    uuid: String,
    row: Option<PairRow>,
    unfinished: Vec<IntentRow>,
}

/// In-flight states: an order of the pair may be live on the exchange.
const fn in_flight(state: PairState) -> bool {
    match state {
        PairState::OrderSubmit | PairState::FillMonitor | PairState::Closing => true,
        PairState::Prepared
        | PairState::PreTradeCheck
        | PairState::Blocked
        | PairState::Reconciled
        | PairState::Imbalanced
        | PairState::Finalized
        | PairState::Cancelled
        | PairState::PartialFailure
        | PairState::Unresolved => false,
    }
}

fn envelope(row: &PairRow) -> Option<PairEnvelope> {
    serde_json::from_value(row.entry.clone()).ok()
}

/// Pairs with unfinished intents, in-flight pairs, and RECONCILED simulated pairs; oldest pair
/// first, then intents whose pair row is missing.
fn candidates(unfinished: Vec<IntentRow>, pairs: Vec<PairRow>) -> Vec<Candidate> {
    let mut by_pair: BTreeMap<String, Vec<IntentRow>> = BTreeMap::new();
    for i in unfinished {
        by_pair.entry(i.pair_uuid.clone()).or_default().push(i);
    }
    let mut out = Vec::new();
    for row in pairs {
        let unfinished = by_pair.remove(&row.internal_uuid).unwrap_or_default();
        let wanted = !unfinished.is_empty()
            || match row.status.parse::<PairState>() {
                Ok(s) => {
                    in_flight(s)
                        || s == PairState::PreTradeCheck
                        || (s == PairState::Reconciled && envelope(&row).is_some_and(|e| e.simulated))
                }
                Err(_) => false, // unreadable state without live intents: the actor's loader halts on it
            };
        if wanted {
            out.push(Candidate { uuid: row.internal_uuid.clone(), row: Some(row), unfinished });
        }
    }
    out.extend(by_pair.into_iter().map(|(uuid, unfinished)| Candidate { uuid, row: None, unfinished }));
    out
}

fn exchange_named(s: &str) -> Option<Exchange> {
    Exchange::ALL.into_iter().find(|e| e.name() == s)
}

fn leg_named(s: &str) -> Option<Leg> {
    Leg::BOTH.into_iter().find(|l| l.as_str() == s)
}

/// The action encoded in an id made by `ids::client_order_id` (`<prefix><l|s><o|c>...`).
fn action_of(client_order_id: &str) -> Option<OrderAction> {
    let prefix = IdPrefix::of(client_order_id)?;
    match client_order_id.as_bytes().get(prefix.as_str().len() + 1) {
        Some(b'o') => Some(OrderAction::Open),
        Some(b'c') => Some(OrderAction::Close),
        Some(_) | None => None,
    }
}

/// What is known about one intent after the query.
#[derive(Debug, Clone)]
enum IntentView {
    Filled(Decimal),
    /// Confirmed not filled (rejected, cancelled with zero fill, or terminal in the store).
    NotFilled,
    /// The exchange does not know it and it was at most SUBMITTED: never sent IF no exposure.
    NotFound,
    /// Cancelled / rejected after a partial fill.
    Partial(Decimal),
    /// Still live on the exchange.
    Working,
    /// The query failed: undecidable now.
    Unknown(String),
    /// The row does not make sense (tampered state, unparsable field, ack'd but not found).
    Mismatch(String),
}

impl IntentView {
    fn failed_query(&self) -> Option<String> {
        match self {
            IntentView::Unknown(reason) => Some(reason.clone()),
            IntentView::Filled(_)
            | IntentView::NotFilled
            | IntentView::NotFound
            | IntentView::Partial(_)
            | IntentView::Working
            | IntentView::Mismatch(_) => None,
        }
    }
}

/// One leg of the current phase, aggregated over its intents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegKind {
    Filled,
    /// No fill at all (includes "no intent": intent-first means nothing was sent).
    Zero,
    Partial,
}

#[derive(Debug, Clone)]
struct LegState {
    kind: LegKind,
    /// Sum of the stored quantities of the intents that filled completely.
    requested: Decimal,
    /// Filled quantity as reported by the exchange (or the stored quantity of a FILLED intent).
    filled: Decimal,
    exchange: Exchange,
    symbol: String,
}

enum Decision {
    /// `fills` (long, short) for the open phase.
    Normal { fills: Option<(LegFill, LegFill)> },
    Cancelled,
    PartialFailure,
    Unresolved(String),
    Pending(String),
}

/// Positions and open orders of one exchange, fetched once per run.
type AccountData = Result<(Listed<AccountPosition>, Listed<AccountOrder>), String>;

struct Run<'a> {
    db: &'a Db,
    events: EventStore,
    market: Option<&'a dyn MarketData>,
    now_ms: i64,
    queries: usize,
    accounts: HashMap<Exchange, AccountData>,
    /// A store write failed (the store is halted): stop.
    store_failed: bool,
}

impl Run<'_> {
    fn outcome(cand: &Candidate, before: Option<PairState>, after: Option<PairState>, verdict: Verdict, reason: impl Into<String>) -> PairOutcome {
        PairOutcome { pair: cand.uuid.clone(), before, after, verdict, reason: reason.into() }
    }

    async fn pair(&mut self, cand: Candidate, exchange: &Result<ExchangeAccess<'_>, String>) -> PairOutcome {
        let Some(row) = cand.row.as_ref() else {
            return self.orphan(&cand, exchange).await;
        };
        let state = match row.status.parse::<PairState>() {
            Ok(s) => s,
            Err(e) => return Self::outcome(&cand, None, None, Verdict::Pending, format!("pair state unreadable: {e}")),
        };
        let Some(env) = envelope(row) else {
            return Self::outcome(&cand, Some(state), Some(state), Verdict::Pending, "pair entry unreadable");
        };
        let all = match self.db.list_intents_for_pair(&cand.uuid) {
            Ok(v) => v,
            Err(e) => return Self::outcome(&cand, Some(state), Some(state), Verdict::Pending, format!("cannot list intents: {e}")),
        };
        if state == PairState::PreTradeCheck && all.is_empty() {
            return self.check_interrupted(&cand, state);
        }
        let simulated = env.simulated || all.iter().any(|i| IdPrefix::of(&i.client_order_id) == Some(IdPrefix::Sim));
        if simulated {
            return self.simulated(&cand, state);
        }
        let access = match exchange {
            Ok(a) => a,
            Err(reason) => {
                return Self::outcome(&cand, Some(state), Some(state), Verdict::Pending, format!("exchange access unavailable: {reason}"));
            }
        };
        let views = match self.query_unfinished(access, &cand.unfinished).await {
            Ok(v) => v,
            Err(()) => return Self::outcome(&cand, Some(state), Some(state), Verdict::Pending, "store write failed"),
        };
        if let Some(reason) = views.values().find_map(IntentView::failed_query) {
            return Self::outcome(&cand, Some(state), Some(state), Verdict::Pending, format!("order query failed: {reason}"));
        }
        let phase = match state {
            PairState::OrderSubmit | PairState::FillMonitor => Some(OrderAction::Open),
            PairState::Closing => Some(OrderAction::Close),
            PairState::Prepared
            | PairState::PreTradeCheck
            | PairState::Blocked
            | PairState::Reconciled
            | PairState::Imbalanced
            | PairState::Finalized
            | PairState::Cancelled
            | PairState::PartialFailure
            | PairState::Unresolved => None,
        };
        let Some(phase) = phase else {
            return self.kept(&cand, state);
        };
        let decision = self.decide(access, row, &env, phase, &all, &views).await;
        self.apply(&cand, state, phase, decision)
    }

    /// Intents whose pair row is missing: record what the exchange says, alert, no transition.
    async fn orphan(&mut self, cand: &Candidate, exchange: &Result<ExchangeAccess<'_>, String>) -> PairOutcome {
        if cand.uuid.starts_with(MANUAL_PAIR_PREFIX) {
            return self.manual(cand, exchange).await;
        }
        if cand.unfinished.iter().all(|i| IdPrefix::of(&i.client_order_id) == Some(IdPrefix::Sim)) {
            return Self::outcome(cand, None, None, Verdict::Kept, "simulated intents without a pair row");
        }
        let access = match exchange {
            Ok(a) => a,
            Err(reason) => return Self::outcome(cand, None, None, Verdict::Pending, format!("exchange access unavailable: {reason}")),
        };
        let demo: Vec<IntentRow> =
            cand.unfinished.iter().filter(|i| IdPrefix::of(&i.client_order_id) != Some(IdPrefix::Sim)).cloned().collect();
        let views = match self.query_unfinished(access, &demo).await {
            Ok(v) => v,
            Err(()) => return Self::outcome(cand, None, None, Verdict::Pending, "store write failed"),
        };
        if let Some(reason) = views.values().find_map(IntentView::failed_query) {
            return Self::outcome(cand, None, None, Verdict::Pending, format!("order query failed: {reason}"));
        }
        self.alert(&cand.uuid, "KEPT", "unfinished order intents without a pair row", json!({}));
        Self::outcome(cand, None, None, Verdict::Kept, "unfinished order intents without a pair row")
    }

    /// Manual orders (no pair row): query and record; a "not found" one is marked not sent only
    /// when its symbol carries no exposure; anything that does not add up raises one alert.
    async fn manual(&mut self, cand: &Candidate, exchange: &Result<ExchangeAccess<'_>, String>) -> PairOutcome {
        let demo: Vec<IntentRow> =
            cand.unfinished.iter().filter(|i| IdPrefix::of(&i.client_order_id) != Some(IdPrefix::Sim)).cloned().collect();
        if demo.is_empty() {
            return Self::outcome(cand, None, None, Verdict::Kept, "simulated manual orders are not queried");
        }
        let access = match exchange {
            Ok(a) => a,
            Err(reason) => return Self::outcome(cand, None, None, Verdict::Pending, format!("exchange access unavailable: {reason}")),
        };
        let views = match self.query_unfinished(access, &demo).await {
            Ok(v) => v,
            Err(()) => return Self::outcome(cand, None, None, Verdict::Pending, "store write failed"),
        };
        if let Some(reason) = views.values().find_map(IntentView::failed_query) {
            return Self::outcome(cand, None, None, Verdict::Pending, format!("order query failed: {reason}"));
        }
        let mut problems = Vec::new();
        for i in &demo {
            match views.get(&i.client_order_id) {
                Some(IntentView::NotFound) => {
                    let Some(ex) = exchange_named(&i.exchange) else {
                        problems.push(format!("{}: unknown exchange {:?}", i.client_order_id, i.exchange));
                        continue;
                    };
                    let (p, o) = match self.account(access, ex).await {
                        Ok(x) => x,
                        Err(e) => return Self::outcome(cand, None, None, Verdict::Pending, e),
                    };
                    let exposed = p.items.iter().any(|x| x.exchange == ex && x.symbol == i.symbol && !x.quantity.is_zero())
                        || o.items.iter().any(|x| x.exchange == ex && x.symbol == i.symbol && x.remaining_quantity > Decimal::ZERO);
                    if exposed {
                        problems.push(format!("{} not found on the exchange while {} carries exposure", i.client_order_id, i.symbol));
                    } else if !self.mark_one_not_sent(&cand.uuid, i) {
                        return Self::outcome(cand, None, None, Verdict::Pending, "store write failed");
                    }
                }
                Some(IntentView::Mismatch(r)) => problems.push(format!("{}: {r}", i.client_order_id)),
                Some(
                    IntentView::Filled(_)
                    | IntentView::NotFilled
                    | IntentView::Partial(_)
                    | IntentView::Working
                    | IntentView::Unknown(_),
                )
                | None => {}
            }
        }
        if problems.is_empty() {
            return Self::outcome(cand, None, None, Verdict::Kept, "manual orders reconciled");
        }
        let reason = problems.join("; ");
        self.alert(&cand.uuid, "KEPT", "manual order does not add up", json!({ "problems": problems }));
        Self::outcome(cand, None, None, Verdict::Kept, reason)
    }

    /// PRE_TRADE_CHECK without any intent: nothing was sent. CheckFailed → BLOCKED; left alone
    /// (kept) if core does not allow it.
    fn check_interrupted(&mut self, cand: &Candidate, state: PairState) -> PairOutcome {
        let reason = "restart during the pre-trade check; no order was sent";
        let detail = json!({ "block": reason, "source": "restart reconciliation" });
        match transition::land_then_act(&self.events, &cand.uuid, state, SystemEvent::CheckFailed, detail, |to| to) {
            Ok((to, _)) => {
                if self.events.append(PRE_TRADE_CHECK_INTERRUPTED, Some(&cand.uuid), json!({ "reason": reason })).is_err() {
                    self.store_failed = true;
                }
                Self::outcome(cand, Some(state), Some(to), Verdict::CheckInterrupted, reason)
            }
            Err(TransitionError::Illegal(e)) => {
                Self::outcome(cand, Some(state), Some(state), Verdict::Kept, format!("CheckFailed not allowed: {e}"))
            }
            Err(e) => self.transition_failed(cand, state, e),
        }
    }

    /// Decision 7: never query; in-flight → UNRESOLVED with a `SIMULATION_INTERRUPTED` event.
    fn simulated(&mut self, cand: &Candidate, state: PairState) -> PairOutcome {
        let note = |run: &mut Self, transitioned: bool| {
            let payload = json!({ "state": state.as_str(), "transitioned": transitioned, "reason": "simulation interrupted by a restart; simulated positions are not persisted" });
            if run.events.append(SIMULATION_INTERRUPTED, Some(&cand.uuid), payload).is_err() {
                run.store_failed = true;
            }
        };
        match state {
            // RECONCILED too: its simulated position died with the process (decision 7).
            PairState::OrderSubmit | PairState::FillMonitor | PairState::Closing | PairState::Reconciled => {
                let detail = json!({ "reason": "simulation interrupted", "simulated": true });
                match transition::land_then_act(&self.events, &cand.uuid, state, SystemEvent::RestartUndetermined, detail, |to| to) {
                    Ok((to, _)) => {
                        note(self, true);
                        Self::outcome(cand, Some(state), Some(to), Verdict::SimulationInterrupted, "simulation interrupted")
                    }
                    Err(e) => self.transition_failed(cand, state, e),
                }
            }
            PairState::Prepared
            | PairState::PreTradeCheck
            | PairState::Blocked
            | PairState::Imbalanced
            | PairState::Finalized
            | PairState::Cancelled
            | PairState::PartialFailure
            | PairState::Unresolved => Self::outcome(cand, Some(state), Some(state), Verdict::Kept, "simulated pair without exposure to reconcile"),
        }
    }

    /// Not in an in-flight state: only the query results were recorded. Live intents on a pair that
    /// is not locked are unexpected, so they raise an alert.
    fn kept(&mut self, cand: &Candidate, state: PairState) -> PairOutcome {
        if !state.is_locked() && !cand.unfinished.is_empty() {
            self.alert(&cand.uuid, "KEPT", "unfinished order intents on a pair that is not in flight", json!({ "state": state.as_str() }));
        }
        Self::outcome(cand, Some(state), Some(state), Verdict::Kept, "no restart transition applies")
    }

    fn alert(&mut self, pair: &str, verdict: &str, reason: &str, extra: Value) {
        let payload = json!({ "verdict": verdict, "reason": reason, "detail": extra });
        if self.events.append(RECONCILE_ALERT, Some(pair), payload).is_err() {
            self.store_failed = true;
        }
    }

    /// Query every unfinished demo intent by its `client_order_id` and record the result.
    /// `Err(())` = a store write failed (the store is halted).
    async fn query_unfinished(&mut self, access: &ExchangeAccess<'_>, unfinished: &[IntentRow]) -> Result<HashMap<String, IntentView>, ()> {
        let mut out = HashMap::new();
        for i in unfinished {
            let Some(exchange) = exchange_named(&i.exchange) else {
                out.insert(i.client_order_id.clone(), IntentView::Mismatch(format!("unknown exchange {:?}", i.exchange)));
                continue;
            };
            self.queries += 1;
            let q = access.executor.query(exchange, &i.symbol, &i.client_order_id).await;
            let stored = IntentState::parse(&i.state).filter(|s| s.is_unfinished());
            let view = match stored {
                // A tampered state must not be written through (it would be an illegal transition).
                None => IntentView::Mismatch(format!("stored intent state {:?} is not a known unfinished state", i.state)),
                Some(st) => match recordable(st, &q) {
                    Err(why) => IntentView::Mismatch(why),
                    Ok(false) => view_of_query(st, &q),
                    Ok(true) => {
                        if record_query_outcome(self.db, &i.client_order_id, &q, false).is_err() {
                            self.store_failed = true;
                            return Err(());
                        }
                        view_of_query(st, &q)
                    }
                },
            };
            out.insert(i.client_order_id.clone(), view);
        }
        Ok(out)
    }

    async fn account(&mut self, access: &ExchangeAccess<'_>, exchange: Exchange) -> AccountData {
        if let Some(a) = self.accounts.get(&exchange) {
            return a.clone();
        }
        let positions = access.account.positions(exchange).await;
        let orders = access.account.open_orders(exchange).await;
        let data = match (positions, orders) {
            (Ok(p), Ok(o)) if p.complete && o.complete => Ok((p, o)),
            (Ok(_), Ok(_)) => Err(format!("{} positions / open orders listing incomplete", exchange.name())),
            (Err(e), _) => Err(format!("{} positions unavailable: {e}", exchange.name())),
            (_, Err(e)) => Err(format!("{} open orders unavailable: {e}", exchange.name())),
        };
        self.accounts.insert(exchange, data.clone());
        data
    }

    async fn decide(
        &mut self,
        access: &ExchangeAccess<'_>,
        row: &PairRow,
        env: &PairEnvelope,
        phase: OrderAction,
        all: &[IntentRow],
        views: &HashMap<String, IntentView>,
    ) -> Decision {
        let mut legs = Vec::with_capacity(2);
        let mut mismatch: Option<String> = None;
        for leg in Leg::BOTH {
            let default_exchange = match leg {
                Leg::Long => env.long_exchange,
                Leg::Short => env.short_exchange,
            };
            let mine: Vec<&IntentRow> =
                all.iter().filter(|i| leg_named(&i.leg) == Some(leg) && action_of(&i.client_order_id) == Some(phase)).collect();
            match leg_state(leg, &mine, views, default_exchange, &row.symbol) {
                Ok(l) => legs.push(l),
                Err(why) => {
                    mismatch.get_or_insert(why);
                    legs.push(LegState {
                        kind: LegKind::Zero,
                        requested: Decimal::ZERO,
                        filled: Decimal::ZERO,
                        exchange: default_exchange,
                        symbol: row.symbol.clone(),
                    });
                }
            }
        }
        // Intents of this pair whose leg or action cannot be read are a mismatch too.
        if let Some(bad) = all.iter().find(|i| leg_named(&i.leg).is_none() || action_of(&i.client_order_id).is_none()) {
            mismatch.get_or_insert(format!("intent {} has an unreadable leg or action", bad.client_order_id));
        }
        // Exposure data first: missing data is never "no exposure", even for a mismatch verdict.
        let mut exposure = Vec::with_capacity(2);
        for l in &legs {
            match self.account(access, l.exchange).await {
                Ok((p, o)) => {
                    let pos: Decimal =
                        p.items.iter().filter(|x| x.exchange == l.exchange && x.symbol == l.symbol).map(|x| x.quantity).sum();
                    let live = o.items.iter().any(|x| x.exchange == l.exchange && x.symbol == l.symbol && x.remaining_quantity > Decimal::ZERO);
                    exposure.push((pos, live));
                }
                Err(e) => return Decision::Pending(e),
            }
        }
        if let Some(why) = mismatch {
            return Decision::Unresolved(why);
        }
        let (long, short) = (&legs[0], &legs[1]);
        let ((lpos, llive), (spos, slive)) = (exposure[0], exposure[1]);
        if llive || slive {
            return Decision::Unresolved("open order on a leg's symbol".into());
        }
        match phase {
            OrderAction::Open => {
                if lpos != long.filled || spos != -short.filled {
                    return Decision::Unresolved(format!(
                        "positions (long {lpos}, short {spos}) do not match fills (long {}, short {})",
                        long.filled, short.filled
                    ));
                }
                match (long.kind, short.kind) {
                    (LegKind::Filled, LegKind::Filled) => {
                        let (long, short) = (long.clone(), short.clone());
                        match (self.unit_base(&long).await, self.unit_base(&short).await) {
                            (Ok(lu), Ok(su)) => {
                                let fill = |l: &LegState, u: Decimal| LegFill::Known { requested: l.requested * u, filled: l.filled * u };
                                Decision::Normal { fills: Some((fill(&long, lu), fill(&short, su))) }
                            }
                            (Err(e), _) | (_, Err(e)) => Decision::Pending(e),
                        }
                    }
                    (LegKind::Zero, LegKind::Zero) => Decision::Cancelled,
                    (LegKind::Filled | LegKind::Partial, LegKind::Zero | LegKind::Partial | LegKind::Filled)
                    | (LegKind::Zero, LegKind::Filled | LegKind::Partial) => Decision::PartialFailure,
                }
            }
            OrderAction::Close => match (long.kind, short.kind) {
                (LegKind::Filled, LegKind::Filled) if lpos.is_zero() && spos.is_zero() => Decision::Normal { fills: None },
                (LegKind::Filled, LegKind::Zero) if lpos.is_zero() && spos < Decimal::ZERO => Decision::PartialFailure,
                (LegKind::Zero, LegKind::Filled) if spos.is_zero() && lpos > Decimal::ZERO => Decision::PartialFailure,
                (LegKind::Filled | LegKind::Zero | LegKind::Partial, LegKind::Filled | LegKind::Zero | LegKind::Partial) => {
                    Decision::Unresolved(format!(
                        "close legs ({:?}, {:?}) with positions (long {lpos}, short {spos}) do not show a clean close",
                        long.kind, short.kind
                    ))
                }
            },
        }
    }

    /// Base coin per order unit (OKX contract value, else 1), as the actor sizes and compares legs.
    async fn unit_base(&self, leg: &LegState) -> Result<Decimal, String> {
        match leg.exchange {
            Exchange::Binance | Exchange::Bybit => Ok(Decimal::ONE),
            Exchange::Okx => {
                let market = self.market.ok_or_else(|| "OKX order rules unavailable (no market data)".to_string())?;
                let rules = market.order_rules(leg.exchange, &leg.symbol).await.map_err(|e| format!("OKX order rules unavailable: {e}"))?;
                Ok(rules.okx_ct_val.unwrap_or(Decimal::ONE))
            }
        }
    }

    /// FILL_MONITOR with both legs fully filled: the regular fill decision (RECONCILED /
    /// IMBALANCED) with the pair's effective settings. `Err(reason)` leaves it in FILL_MONITOR.
    fn settle_fills(&mut self, pair: &str, fills: (LegFill, LegFill)) -> Result<PairState, String> {
        let row = self.db.get_pair(pair).map_err(|e| format!("cannot read pair: {e}"))?.ok_or("pair row missing")?;
        let env = envelope(&row).ok_or("pair entry unreadable")?;
        let (risk, overrides) = load_risk_config(self.db).map_err(|e| format!("settings unreadable: {e}"))?;
        let eff = effective_for_pair(&risk, &overrides, env.long_exchange, env.short_exchange);
        match fill::fill_decision(&eff, self.now_ms, self.now_ms, fills.0, fills.1) {
            FillDecision::Wait => Err("fill decision still waiting".into()),
            FillDecision::Transition(ev) => {
                let detail = json!({ "source": "restart reconciliation", "long": format!("{:?}", fills.0), "short": format!("{:?}", fills.1) });
                match transition::land_then_act(&self.events, pair, PairState::FillMonitor, ev, detail, |to| to) {
                    Ok((to, _)) => Ok(to),
                    Err(e) => {
                        if matches!(e, TransitionError::StoreFailed(_)) {
                            self.store_failed = true;
                        }
                        Err(format!("fill decision not applied: {e}"))
                    }
                }
            }
        }
    }

    fn apply(&mut self, cand: &Candidate, state: PairState, phase: OrderAction, decision: Decision) -> PairOutcome {
        let (event, verdict, reason): (Option<SystemEvent>, Verdict, String) = match decision {
            Decision::Pending(r) => return Self::outcome(cand, Some(state), Some(state), Verdict::Pending, r),
            Decision::Normal { fills } => match (phase, state) {
                (OrderAction::Open, PairState::OrderSubmit) => {
                    (Some(SystemEvent::BothLegsSubmitted), Verdict::Normal { fills }, "both legs filled".into())
                }
                // Already FILL_MONITOR: stays; the actor's fill decision takes it from here.
                (OrderAction::Open, _) => (None, Verdict::Normal { fills }, "both legs filled".into()),
                (OrderAction::Close, _) => {
                    (Some(SystemEvent::ClosedConfirmed { verified_flat: true }), Verdict::Normal { fills }, "both legs closed and flat".into())
                }
            },
            Decision::Cancelled => {
                let event = match state {
                    PairState::OrderSubmit => SystemEvent::BothSubmitsFailed,
                    // Only FILL_MONITOR reaches here besides ORDER_SUBMIT; for any other state
                    // core refuses it and the pair stays pending (never a guessed transition).
                    PairState::FillMonitor
                    | PairState::Prepared
                    | PairState::PreTradeCheck
                    | PairState::Blocked
                    | PairState::Reconciled
                    | PairState::Imbalanced
                    | PairState::Closing
                    | PairState::Finalized
                    | PairState::Cancelled
                    | PairState::PartialFailure
                    | PairState::Unresolved => SystemEvent::TimeoutNoFills,
                };
                (Some(event), Verdict::Cancelled, "nothing filled and no exposure".into())
            }
            Decision::PartialFailure => (Some(SystemEvent::RestartFoundPartial), Verdict::PartialFailure, "one leg filled, the other not".into()),
            Decision::Unresolved(why) => (Some(SystemEvent::RestartUndetermined), Verdict::Unresolved, why),
        };
        // Not-sent marks land before the pair transition: a crash in between re-derives the same
        // CANCELLED verdict from the (now terminal) intents on the next start.
        if verdict == Verdict::Cancelled && !self.mark_not_sent(&cand.uuid, phase) {
            return Self::outcome(cand, Some(state), Some(state), Verdict::Pending, "store write failed");
        }
        let after = match event {
            None => state,
            Some(ev) => {
                let detail = json!({ "source": "restart reconciliation", "reason": reason });
                match transition::land_then_act(&self.events, &cand.uuid, state, ev, detail, |to| to) {
                    Ok((to, _)) => to,
                    Err(e) => return self.transition_failed(cand, state, e),
                }
            }
        };
        // Back on the normal path: FILL_MONITOR continues with the regular fill decision.
        let after = match &verdict {
            Verdict::Normal { fills: Some(fills) } if after == PairState::FillMonitor => match self.settle_fills(&cand.uuid, *fills) {
                Ok(to) => to,
                Err(why) => return Self::outcome(cand, Some(state), Some(after), Verdict::Pending, why),
            },
            _ => after,
        };
        match verdict {
            Verdict::PartialFailure | Verdict::Unresolved => {
                self.alert(&cand.uuid, after.as_str(), &reason, json!({ "from": state.as_str() }));
            }
            Verdict::Normal { .. }
            | Verdict::Cancelled
            | Verdict::SimulationInterrupted
            | Verdict::CheckInterrupted
            | Verdict::Kept
            | Verdict::Pending => {}
        }
        Self::outcome(cand, Some(state), Some(after), verdict, reason)
    }

    fn transition_failed(&mut self, cand: &Candidate, state: PairState, e: TransitionError) -> PairOutcome {
        match &e {
            TransitionError::StoreFailed(_) => self.store_failed = true,
            TransitionError::Illegal(_) | TransitionError::Stale { .. } => {}
        }
        Self::outcome(cand, Some(state), Some(state), Verdict::Pending, format!("transition not applied: {e}"))
    }

    /// Mark the phase's "not found" intents (at most SUBMITTED) as never sent. `false` = a store
    /// write failed.
    fn mark_not_sent(&mut self, pair: &str, phase: OrderAction) -> bool {
        let Ok(all) = self.db.list_intents_for_pair(pair) else {
            self.store_failed = true;
            return false;
        };
        for i in all.iter().filter(|i| action_of(&i.client_order_id) == Some(phase)) {
            let st = IntentState::parse(&i.state);
            if !matches!(st, Some(IntentState::Intended | IntentState::Submitted)) {
                continue;
            }
            if !self.mark_one_not_sent(pair, i) {
                return false;
            }
        }
        true
    }

    /// One "not found, no exposure" intent → CANCELLED + `ORDER_INTENT_NOT_SENT`. `false` = a
    /// store write failed.
    fn mark_one_not_sent(&mut self, pair: &str, i: &IntentRow) -> bool {
        let ok = self.db.update_intent_state(&i.client_order_id, IntentState::Cancelled, None).is_ok()
            && self
                .events
                .append(
                    ORDER_INTENT_NOT_SENT,
                    Some(pair),
                    json!({ "client_order_id": i.client_order_id, "from": i.state, "reason": "not found on the exchange; no position or open order" }),
                )
                .is_ok();
        if !ok {
            self.store_failed = true;
        }
        ok
    }
}

/// Whether `record_query_outcome` may write this result: `Ok(false)` for a failed query (left
/// as is, so retries do not pile up events), `Err` when the found order state is not a legal
/// next state of the stored one (e.g. an INTENDED intent reported filled): writing it would be an
/// illegal transition and halt the store, so it is treated as a mismatch instead.
fn recordable(stored: IntentState, q: &QueryOutcome) -> Result<bool, String> {
    match q {
        QueryOutcome::Failed { .. } => Ok(false),
        QueryOutcome::NotFound => Ok(true),
        QueryOutcome::Found(st) => {
            let target = match st.state {
                OrderState::Open => IntentState::Acknowledged,
                OrderState::Filled => IntentState::Filled,
                OrderState::Cancelled => IntentState::Cancelled,
                OrderState::Rejected => IntentState::Failed,
            };
            if target == stored || stored.can_transition_to(target) {
                Ok(true)
            } else {
                Err(format!("exchange reports {:?} for an intent stored as {}", st.state, stored.as_str()))
            }
        }
    }
}

fn view_of_query(stored: IntentState, q: &QueryOutcome) -> IntentView {
    match q {
        QueryOutcome::Found(st) => match st.state {
            OrderState::Filled => IntentView::Filled(st.filled_quantity),
            OrderState::Open => IntentView::Working,
            OrderState::Cancelled | OrderState::Rejected if st.filled_quantity > Decimal::ZERO => IntentView::Partial(st.filled_quantity),
            OrderState::Cancelled | OrderState::Rejected => IntentView::NotFilled,
        },
        QueryOutcome::NotFound => match stored {
            IntentState::Intended | IntentState::Submitted => IntentView::NotFound,
            // The exchange acknowledged it once; not finding it now is a contradiction.
            IntentState::Acknowledged => IntentView::Mismatch("acknowledged order not found on the exchange".into()),
            IntentState::Filled | IntentState::Cancelled | IntentState::Failed => {
                IntentView::Mismatch("terminal intent listed as unfinished".into())
            }
        },
        QueryOutcome::Failed { reason } => IntentView::Unknown(reason.clone()),
    }
}

/// The view of a stored intent that was not queried (terminal in the store).
fn view_of_stored(i: &IntentRow) -> IntentView {
    match IntentState::parse(&i.state) {
        Some(IntentState::Filled) => match i.quantity.parse::<Decimal>() {
            Ok(q) => IntentView::Filled(q),
            Err(_) => IntentView::Mismatch(format!("intent {} quantity unreadable", i.client_order_id)),
        },
        // A cancel after a partial fill would show up as a position mismatch.
        Some(IntentState::Cancelled | IntentState::Failed) => IntentView::NotFilled,
        Some(IntentState::Intended | IntentState::Submitted | IntentState::Acknowledged) | None => {
            IntentView::Mismatch(format!("intent {} was not reconciled", i.client_order_id))
        }
    }
}

/// Aggregate one leg of the phase. `Err` = the leg's rows make no sense (→ UNRESOLVED).
fn leg_state(
    leg: Leg,
    intents: &[&IntentRow],
    views: &HashMap<String, IntentView>,
    default_exchange: Exchange,
    symbol: &str,
) -> Result<LegState, String> {
    let mut exchange = default_exchange;
    let mut sym = symbol.to_string();
    if let Some(first) = intents.first() {
        exchange = exchange_named(&first.exchange).ok_or_else(|| format!("{} leg: unknown exchange {:?}", leg.as_str(), first.exchange))?;
        sym = first.symbol.clone();
    }
    let mut st = LegState { kind: LegKind::Zero, requested: Decimal::ZERO, filled: Decimal::ZERO, exchange, symbol: sym };
    let (mut any_filled, mut any_partial) = (false, false);
    for i in intents {
        if exchange_named(&i.exchange) != Some(st.exchange) || i.symbol != st.symbol {
            return Err(format!("{} leg intents disagree on exchange / symbol", leg.as_str()));
        }
        let view = views.get(&i.client_order_id).cloned().unwrap_or_else(|| view_of_stored(i));
        match view {
            IntentView::Filled(q) => {
                any_filled = true;
                st.filled += q;
                st.requested += i.quantity.parse::<Decimal>().unwrap_or(q);
            }
            IntentView::Partial(q) => {
                any_partial = true;
                st.filled += q;
            }
            // Marked not sent later, only if the whole pair turns out to have no exposure.
            IntentView::NotFilled | IntentView::NotFound => {}
            IntentView::Working => return Err(format!("{} leg order {} is still live", leg.as_str(), i.client_order_id)),
            IntentView::Unknown(r) => return Err(format!("{} leg order {} unknown: {r}", leg.as_str(), i.client_order_id)),
            IntentView::Mismatch(r) => return Err(format!("{} leg: {r}", leg.as_str())),
        }
    }
    st.kind = if any_partial {
        LegKind::Partial
    } else if any_filled {
        LegKind::Filled
    } else {
        LegKind::Zero
    };
    Ok(st)
}

/// The production [`StartupReconciler`]: [`reconcile`] over the actor's startup scope, with
/// demo access only in EXCHANGE_DEMO (the simulator is never demo access) and `Ok` only when
/// nothing is left pending.
pub struct RecoveryReconciler {
    market: Arc<dyn MarketData>,
    clock: Arc<dyn Clock>,
}

impl RecoveryReconciler {
    pub fn new(market: Arc<dyn MarketData>, clock: Arc<dyn Clock>) -> RecoveryReconciler {
        RecoveryReconciler { market, clock }
    }
}

impl StartupReconciler for RecoveryReconciler {
    fn reconcile(&self, ctx: ReconcileContext) -> BoxFut<'static, Result<(), String>> {
        let (market, clock) = (self.market.clone(), self.clock.clone());
        Box::pin(async move {
            let exchange = match ctx.execution_mode {
                ExecutionMode::ExchangeDemo => Ok(ExchangeAccess { executor: ctx.executor.as_ref(), account: ctx.account.as_ref() }),
                ExecutionMode::Simulation => Err("SIMULATION mode: no order-capable executor to query demo orders".to_string()),
            };
            let input =
                ReconcileInput { db: &ctx.db, exchange, market: Some(market.as_ref()), scope: Some(&ctx.scope), clock: clock.as_ref() };
            reconcile(input).await.pending.map_or(Ok(()), Err)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::actor::PairEnvelope;
    use crate::engine::ids::{IdPrefix, client_order_id};
    use crate::engine::ports::{
        AccountOrder, AccountPosition, BoxFut, Leg, Listed, OrderAction, OrderRequest, OrderState, OrderStatus, QueryOutcome,
        SubmitOutcome,
    };
    use std::sync::Arc;
    use crate::ports::ManualClock;
    use crate::store::db::test_support::{clock, open_tmp, tempdir};
    use crate::store::events::EventStore;
    use crate::store::state::{IntentState, NewIntent, NewPair};
    use futures_util::FutureExt;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tong_funding_core::types::{Decimal, Exchange};

    const PAIR: &str = "6f1c2d3e-4b5a-4c6d-8e7f-0123456789ab";
    const SYM: &str = "BTCUSDT";
    const LONG_EX: Exchange = Exchange::Binance;
    const SHORT_EX: Exchange = Exchange::Bybit;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// A fake exchange: answers queries from a table (default `NotFound`), lists positions and
    /// open orders per exchange (default: complete and empty), and counts every call.
    #[derive(Default)]
    struct FakeExchange {
        orders: Mutex<HashMap<String, QueryOutcome>>,
        fail_queries: Mutex<Option<String>>,
        positions: Mutex<HashMap<Exchange, Result<Listed<AccountPosition>, String>>>,
        open_orders: Mutex<HashMap<Exchange, Result<Listed<AccountOrder>, String>>>,
        submits: AtomicUsize,
        cancels: AtomicUsize,
        queries: Mutex<Vec<String>>,
        account_reads: AtomicUsize,
    }

    impl FakeExchange {
        fn filled(&self, id: &str, qty: &str) {
            let st = OrderStatus {
                client_order_id: id.into(),
                exchange_order_id: Some(format!("EX-{id}")),
                filled_quantity: d(qty),
                avg_price: Some(d("60000")),
                state: OrderState::Filled,
            };
            self.orders.lock().unwrap().insert(id.into(), QueryOutcome::Found(st));
        }
        fn position(&self, ex: Exchange, qty: &str) {
            let items = vec![AccountPosition { exchange: ex, symbol: SYM.into(), quantity: d(qty) }];
            self.positions.lock().unwrap().insert(ex, Ok(Listed { items, complete: true }));
        }
        fn queries(&self) -> Vec<String> {
            self.queries.lock().unwrap().clone()
        }
        fn account_reads(&self) -> usize {
            self.account_reads.load(Ordering::SeqCst)
        }
        /// Reconciliation is read-only towards the exchange: never a submit or a cancel.
        fn assert_read_only(&self) {
            assert_eq!(self.submits.load(Ordering::SeqCst), 0, "reconciliation must never submit");
            assert_eq!(self.cancels.load(Ordering::SeqCst), 0, "reconciliation must never cancel");
        }
    }

    impl Executor for FakeExchange {
        fn is_simulated(&self) -> bool {
            false
        }
        fn submit(&self, _req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(SubmitOutcome::Rejected { reason: "fake".into() }))
        }
        fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(QueryOutcome::NotFound))
        }
        fn query(&self, _: Exchange, _: &str, id: &str) -> BoxFut<'_, QueryOutcome> {
            self.queries.lock().unwrap().push(id.to_string());
            let out = match self.fail_queries.lock().unwrap().clone() {
                Some(reason) => QueryOutcome::Failed { reason },
                None => self.orders.lock().unwrap().get(id).cloned().unwrap_or(QueryOutcome::NotFound),
            };
            Box::pin(std::future::ready(out))
        }
    }

    impl AccountView for FakeExchange {
        fn positions(&self, ex: Exchange) -> BoxFut<'_, Result<Listed<AccountPosition>, String>> {
            self.account_reads.fetch_add(1, Ordering::SeqCst);
            let r = self.positions.lock().unwrap().get(&ex).cloned().unwrap_or(Ok(Listed { items: vec![], complete: true }));
            Box::pin(std::future::ready(r))
        }
        fn open_orders(&self, ex: Exchange) -> BoxFut<'_, Result<Listed<AccountOrder>, String>> {
            self.account_reads.fetch_add(1, Ordering::SeqCst);
            let r = self.open_orders.lock().unwrap().get(&ex).cloned().unwrap_or(Ok(Listed { items: vec![], complete: true }));
            Box::pin(std::future::ready(r))
        }
        fn available_margin(&self, _: Exchange) -> BoxFut<'_, Result<Decimal, String>> {
            Box::pin(std::future::ready(Err("not used".into())))
        }
    }

    fn seed_pair(db: &Db, uuid: &str, state: PairState, simulated: bool) {
        let env = PairEnvelope { long_exchange: LONG_EX, short_exchange: SHORT_EX, settlement_ms: 2_000_000, simulated, scan: json!({}) };
        let p = NewPair {
            internal_uuid: uuid.into(),
            pair_id: format!("pid-{uuid}"),
            symbol: SYM.into(),
            status: state,
            entry: serde_json::to_value(env).unwrap(),
        };
        db.add_pair_if_not_pending(&p).unwrap();
    }

    /// Land an intent the way `intent::land_intent` does, stopping at `upto`.
    fn seed_intent(db: &Db, prefix: IdPrefix, leg: Leg, action: OrderAction, qty: &str, upto: IntentState) -> String {
        let cid = client_order_id(prefix, PAIR, leg, action, 0);
        let (ex, side) = match (leg, action) {
            (Leg::Long, OrderAction::Open) => (LONG_EX, "BUY"),
            (Leg::Long, OrderAction::Close) => (LONG_EX, "SELL"),
            (Leg::Short, OrderAction::Open) => (SHORT_EX, "SELL"),
            (Leg::Short, OrderAction::Close) => (SHORT_EX, "BUY"),
        };
        db.create_intent(&NewIntent {
            client_order_id: cid.clone(),
            pair_uuid: PAIR.into(),
            leg: leg.as_str().into(),
            exchange: ex.name().into(),
            symbol: SYM.into(),
            side: side.into(),
            quantity: qty.into(),
        })
        .unwrap();
        match upto {
            IntentState::Intended => {}
            IntentState::Submitted => db.update_intent_state(&cid, IntentState::Submitted, None).unwrap(),
            IntentState::Filled => {
                db.update_intent_state(&cid, IntentState::Submitted, None).unwrap();
                db.update_intent_state(&cid, IntentState::Filled, Some("EX-old")).unwrap();
            }
            other => panic!("seed_intent: {other:?} not needed"),
        }
        cid
    }

    fn run(db: &Db, fx: &FakeExchange) -> ReconcileReport {
        let c = ManualClock::new(5_000_000);
        reconcile_startup(db, Ok(ExchangeAccess { executor: fx, account: fx }), &c).now_or_never().expect("fakes resolve at once")
    }

    fn pair_state(db: &Db) -> String {
        db.get_pair(PAIR).unwrap().unwrap().status
    }

    fn intent_state(db: &Db, cid: &str) -> String {
        db.get_intent(cid).unwrap().unwrap().state
    }

    fn events(db: &Db, ty: &str) -> Vec<serde_json::Value> {
        EventStore::new(db.clone()).list(1_000).unwrap().into_iter().filter(|e| e.event_type == ty).map(|e| e.payload).collect()
    }

    fn transitions(db: &Db) -> usize {
        events(db, crate::engine::transition::PAIR_TRANSITION).len()
    }

    fn only_outcome(r: &ReconcileReport) -> &PairOutcome {
        assert_eq!(r.pairs.len(), 1, "{r:?}");
        &r.pairs[0]
    }

    // spec "沒有未結束意圖"
    #[test]
    fn no_unfinished_intents_means_ready_at_once() {
        let (_d, db, _) = open_tmp();
        let fx = FakeExchange::default();
        let r = run(&db, &fx);
        assert_eq!(r.pending, None);
        assert!(r.pairs.is_empty());
        assert_eq!((r.queries, fx.account_reads()), (0, 0));
        assert_eq!(r.finished_at_ms, 5_000_000, "time comes from the injected clock");
        // A finished pair with only terminal intents changes nothing either.
        seed_pair(&db, PAIR, PairState::Finalized, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Filled);
        let r = run(&db, &fx);
        assert_eq!(r.pending, None);
        assert!(r.pairs.is_empty(), "{r:?}");
        assert!(fx.queries().is_empty());
        fx.assert_read_only();
    }

    // spec "在「已意圖」時終止"
    #[test]
    fn stopped_at_intended_not_found_and_no_exposure_is_not_sent_and_cancelled() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Intended);
        let fx = FakeExchange::default();
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(fx.queries(), vec![l.clone()], "queried by its client_order_id");
        assert_eq!(intent_state(&db, &l), "CANCELLED", "marked not sent");
        assert_eq!(events(&db, ORDER_INTENT_NOT_SENT).len(), 1);
        assert_eq!(pair_state(&db), "CANCELLED");
        let o = only_outcome(&r);
        assert_eq!((o.before, o.after, &o.verdict), (Some(PairState::OrderSubmit), Some(PairState::Cancelled), &Verdict::Cancelled));
        assert!(fx.account_reads() > 0, "exposure was checked before calling it not sent");
        fx.assert_read_only();
        assert!(db.list_unfinished_intents().unwrap().is_empty());
    }

    // spec "在「已送出」時終止，訂單實際已成交" — restarted on the same database file.
    #[test]
    fn stopped_at_submitted_but_both_legs_filled_continues_normally_without_resubmitting() {
        let dir = tempdir();
        let path = dir.path().join("funding.db");
        let (l, s) = {
            let (c, _) = clock(1_000_000);
            let db = Db::open(&path, c);
            seed_pair(&db, PAIR, PairState::OrderSubmit, false);
            let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
            let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
            (l, s)
        }; // process killed
        let (c, _) = clock(1_100_000);
        let db = Db::open(&path, c);
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.filled(&s, "0.01");
        fx.position(LONG_EX, "0.01");
        fx.position(SHORT_EX, "-0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!((intent_state(&db, &l), intent_state(&db, &s)), ("FILLED".into(), "FILLED".into()));
        assert_eq!(db.get_intent(&l).unwrap().unwrap().exchange_order_id, Some(format!("EX-{l}")));
        assert_eq!(pair_state(&db), "RECONCILED", "back on the normal path: FILL_MONITOR, then the fill decision");
        let o = only_outcome(&r);
        let known = LegFill::Known { requested: d("0.01"), filled: d("0.01") };
        assert_eq!(o.verdict, Verdict::Normal { fills: Some((known, known)) });
        assert_eq!(o.after, Some(PairState::Reconciled));
        assert!(events(&db, RECONCILE_ALERT).is_empty());
        fx.assert_read_only();
    }

    // spec "一腿成交、另一腿查無"
    #[test]
    fn long_filled_short_not_found_without_position_is_partial_failure_with_alert() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.position(LONG_EX, "0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "PARTIAL_FAILURE");
        assert_eq!(only_outcome(&r).verdict, Verdict::PartialFailure);
        let long = db.get_intent(&l).unwrap().unwrap();
        assert_eq!((long.state.as_str(), long.quantity.as_str()), ("FILLED", "0.01"), "long leg data kept");
        assert_eq!(long.exchange_order_id, Some(format!("EX-{l}")));
        assert_eq!(intent_state(&db, &s), "SUBMITTED", "not proven unsent while the other leg has exposure");
        let alerts = events(&db, RECONCILE_ALERT);
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert_eq!(alerts[0]["verdict"], "PARTIAL_FAILURE");
        fx.assert_read_only();
    }

    // spec "持倉對不上"
    #[test]
    fn both_filled_but_position_differs_is_unresolved_with_alert() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.filled(&s, "0.01");
        fx.position(LONG_EX, "0.01");
        fx.position(SHORT_EX, "-0.02");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "UNRESOLVED");
        assert_eq!(only_outcome(&r).verdict, Verdict::Unresolved);
        let alerts = events(&db, RECONCILE_ALERT);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0]["verdict"], "UNRESOLVED");
        fx.assert_read_only();
    }

    // spec "交易所不可連線"
    #[test]
    fn all_queries_failing_keeps_reconciliation_pending_and_transitions_nothing() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        *fx.fail_queries.lock().unwrap() = Some("connection refused".into());
        let r = run(&db, &fx);
        let why = r.pending.clone().expect("must stay pending");
        assert!(why.contains("connection refused"), "{why}");
        assert_eq!(fx.queries().len(), 2);
        assert_eq!(pair_state(&db), "ORDER_SUBMIT");
        assert_eq!((intent_state(&db, &l), intent_state(&db, &s)), ("SUBMITTED".into(), "SUBMITTED".into()));
        assert_eq!(transitions(&db), 0, "nothing transitioned");
        assert_eq!(only_outcome(&r).verdict, Verdict::Pending);
        fx.assert_read_only();
    }

    #[test]
    fn unavailable_exchange_access_keeps_pending_without_any_call() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let c = ManualClock::new(5_000_000);
        let r = reconcile_startup(&db, Err("api key unavailable".into()), &c).now_or_never().unwrap();
        assert!(r.pending.as_deref().is_some_and(|p| p.contains("api key unavailable")), "{r:?}");
        assert_eq!(r.queries, 0);
        assert_eq!(pair_state(&db), "ORDER_SUBMIT");
    }

    #[test]
    fn a_simulated_executor_is_never_asked_about_demo_orders() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        // NullExecutor's calls never resolve: awaiting it would make `now_or_never` return None.
        let sim = crate::engine::gate::test_support::NullExecutor::sim();
        let fx = FakeExchange::default();
        let c = ManualClock::new(5_000_000);
        let r = reconcile_startup(&db, Ok(ExchangeAccess { executor: sim.as_ref(), account: &fx }), &c)
            .now_or_never()
            .expect("must not await the simulated executor");
        assert!(r.pending.is_some(), "{r:?}");
        assert_eq!(pair_state(&db), "ORDER_SUBMIT");
    }

    // spec "模擬中斷" (decision 7)
    #[test]
    fn interrupted_simulation_is_unresolved_with_an_event_and_no_exchange_request() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, true);
        let l = seed_intent(&db, IdPrefix::Sim, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert!(fx.queries().is_empty(), "sim intents are never queried");
        assert_eq!(fx.account_reads(), 0);
        assert_eq!(pair_state(&db), "UNRESOLVED");
        assert_eq!(only_outcome(&r).verdict, Verdict::SimulationInterrupted);
        assert_eq!(events(&db, SIMULATION_INTERRUPTED).len(), 1);
        assert_eq!(intent_state(&db, &l), "SUBMITTED", "a simulated order is not claimed sent or unsent");
        fx.assert_read_only();
        // Even without exchange access (SIMULATION after a key failure) it completes.
        let (_d2, db2, _) = open_tmp();
        seed_pair(&db2, PAIR, PairState::Closing, true);
        let c = ManualClock::new(5_000_000);
        let r = reconcile_startup(&db2, Err("no keys".into()), &c).now_or_never().unwrap();
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(db2.get_pair(PAIR).unwrap().unwrap().status, "UNRESOLVED", "in-flight sim pair without intents too");
    }

    #[test]
    fn a_reconciled_simulated_pair_becomes_unresolved_once() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::Reconciled, true);
        let c = ManualClock::new(5_000_000);
        let r = reconcile_startup(&db, Err("no keys".into()), &c).now_or_never().unwrap();
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "UNRESOLVED", "its simulated ledger is gone (decision 7)");
        assert_eq!(only_outcome(&r).verdict, Verdict::SimulationInterrupted);
        assert_eq!(events(&db, SIMULATION_INTERRUPTED).len(), 1);
        // A second restart finds nothing in flight and writes nothing more.
        reconcile_startup(&db, Err("no keys".into()), &c).now_or_never().unwrap();
        assert_eq!(events(&db, SIMULATION_INTERRUPTED).len(), 1);
    }

    #[test]
    fn a_sim_prefixed_intent_marks_the_pair_simulated_even_if_the_envelope_says_otherwise() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        seed_intent(&db, IdPrefix::Sim, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert!(fx.queries().is_empty());
        assert_eq!(pair_state(&db), "UNRESOLVED");
    }

    #[test]
    fn incomplete_position_listing_is_never_read_as_no_exposure() {
        for (label, listing) in [("incomplete", Ok(Listed { items: vec![], complete: false })), ("error", Err("timeout".to_string()))] {
            let (_d, db, _) = open_tmp();
            seed_pair(&db, PAIR, PairState::OrderSubmit, false);
            let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
            let fx = FakeExchange::default();
            fx.positions.lock().unwrap().insert(SHORT_EX, listing);
            let r = run(&db, &fx);
            assert!(r.pending.is_some(), "{label}: {r:?}");
            assert_eq!(pair_state(&db), "ORDER_SUBMIT", "{label}: never CANCELLED on missing data");
            assert_eq!(intent_state(&db, &l), "SUBMITTED", "{label}: not marked unsent");
            fx.assert_read_only();
        }
    }

    #[test]
    fn incomplete_open_order_listing_is_never_read_as_no_exposure() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.open_orders.lock().unwrap().insert(LONG_EX, Ok(Listed { items: vec![], complete: false }));
        let r = run(&db, &fx);
        assert!(r.pending.is_some(), "{r:?}");
        assert_eq!(pair_state(&db), "ORDER_SUBMIT");
    }

    #[test]
    fn an_open_order_on_the_symbol_is_exposure() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        let o = AccountOrder { exchange: LONG_EX, symbol: SYM.into(), client_order_id: None, remaining_quantity: d("0.01") };
        fx.open_orders.lock().unwrap().insert(LONG_EX, Ok(Listed { items: vec![o], complete: true }));
        let r = run(&db, &fx);
        assert_eq!(r.pending, None);
        assert_eq!(pair_state(&db), "UNRESOLVED");
        assert_eq!(events(&db, RECONCILE_ALERT).len(), 1);
        fx.assert_read_only();
    }

    #[test]
    fn order_submit_without_any_intent_and_no_exposure_is_cancelled() {
        // Crash after CheckPassed landed, before the first intent: intent-first means nothing was sent.
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        let fx = FakeExchange::default();
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "CANCELLED");
        assert!(fx.queries().is_empty());
    }

    #[test]
    fn both_open_orders_rejected_is_cancelled() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        for cid in [&l, &s] {
            let st = OrderStatus {
                client_order_id: cid.clone(),
                exchange_order_id: None,
                filled_quantity: Decimal::ZERO,
                avg_price: None,
                state: OrderState::Rejected,
            };
            fx.orders.lock().unwrap().insert(cid.clone(), QueryOutcome::Found(st));
        }
        let r = run(&db, &fx);
        assert_eq!(r.pending, None);
        assert_eq!(pair_state(&db), "CANCELLED");
        assert_eq!((intent_state(&db, &l), intent_state(&db, &s)), ("FAILED".into(), "FAILED".into()));
        assert!(events(&db, ORDER_INTENT_NOT_SENT).is_empty(), "rejections are recorded as such");
    }

    #[test]
    fn closing_with_both_close_legs_filled_and_flat_is_finalized() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::Closing, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Filled);
        seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Filled);
        let lc = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Close, "0.01", IntentState::Submitted);
        let sc = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Close, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&lc, "0.01");
        fx.filled(&sc, "0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(fx.queries(), vec![lc, sc], "only unfinished intents are queried");
        assert_eq!(pair_state(&db), "FINALIZED");
        fx.assert_read_only();
    }

    #[test]
    fn closing_with_one_close_leg_filled_and_the_other_still_open_is_partial_failure() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::Closing, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Filled);
        seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Filled);
        let lc = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Close, "0.01", IntentState::Submitted);
        seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Close, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&lc, "0.01");
        fx.position(SHORT_EX, "-0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "PARTIAL_FAILURE");
        assert_eq!(events(&db, RECONCILE_ALERT).len(), 1);
        fx.assert_read_only();
    }

    #[test]
    fn closing_whose_close_orders_were_never_sent_goes_to_a_human() {
        // Core has no CLOSING -> RECONCILED and reconciliation never re-sends: UNRESOLVED.
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::Closing, false);
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Filled);
        seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Filled);
        let fx = FakeExchange::default();
        fx.position(LONG_EX, "0.01");
        fx.position(SHORT_EX, "-0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "UNRESOLVED");
        assert_eq!(events(&db, RECONCILE_ALERT).len(), 1);
        fx.assert_read_only();
    }

    #[test]
    fn a_second_run_is_idempotent() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.filled(&s, "0.01");
        fx.position(LONG_EX, "0.01");
        fx.position(SHORT_EX, "-0.01");
        assert_eq!(run(&db, &fx).pending, None);
        let before = transitions(&db);
        assert_eq!(before, 2, "FILL_MONITOR, then RECONCILED");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "RECONCILED");
        assert_eq!(transitions(&db), before, "no second transition");
        assert_eq!(fx.queries().len(), 2, "terminal intents are not queried again");
    }

    #[test]
    fn a_tampered_intent_state_is_unresolved_and_not_written_through() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        db.with_raw_conn_for_tests(|c| Ok(c.execute("UPDATE order_intents SET state = 'filled' WHERE client_order_id = ?1", [&l])?))
            .unwrap();
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.position(LONG_EX, "0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        assert_eq!(pair_state(&db), "UNRESOLVED");
    }

    #[test]
    fn an_intended_intent_reported_filled_is_unresolved_and_does_not_halt_the_store() {
        // INTENDED -> FILLED is not a legal intent transition; writing it would halt the store.
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Intended);
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.position(LONG_EX, "0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert!(!db.is_halted(), "{:?}", db.halt_reason());
        assert_eq!(intent_state(&db, &l), "INTENDED");
        assert_eq!(pair_state(&db), "UNRESOLVED");
        assert_eq!(events(&db, RECONCILE_ALERT).len(), 1);
    }

    #[test]
    fn a_partly_filled_cancelled_leg_with_the_other_leg_unfilled_is_partial_failure() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        let st = OrderStatus {
            client_order_id: l.clone(),
            exchange_order_id: Some("E".into()),
            filled_quantity: d("0.004"),
            avg_price: None,
            state: OrderState::Cancelled,
        };
        fx.orders.lock().unwrap().insert(l.clone(), QueryOutcome::Found(st));
        fx.position(LONG_EX, "0.004");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "PARTIAL_FAILURE");
    }

    #[test]
    fn reconciliation_is_a_send_future_so_it_can_be_spawned() {
        fn assert_send<T: Send>(_: &T) {}
        let (_d, db, _) = open_tmp();
        let fx = FakeExchange::default();
        let c = ManualClock::new(1);
        let fut = reconcile_startup(&db, Ok(ExchangeAccess { executor: &fx, account: &fx }), &c);
        assert_send(&fut);
    }

    // ---- wave 3: PRE_TRADE_CHECK, manual orders, fill decision, scope, the StartupReconciler ----

    /// Order rules: OKX contract value 0.01, or unavailable.
    struct Rules(Option<Decimal>);
    impl MarketData for Rules {
        fn refetch(&self, _: Exchange, _: &str) -> BoxFut<'_, Result<crate::engine::ports::FreshQuote, String>> {
            Box::pin(std::future::ready(Err("not used".into())))
        }
        fn order_rules(&self, _: Exchange, _: &str) -> BoxFut<'_, Result<crate::engine::ports::OrderRules, String>> {
            let r = self.0.map(|ct| crate::engine::ports::OrderRules {
                lot: tong_funding_core::quantity::LotSize { step_size: d("1"), min_qty: d("1") },
                okx_ct_val: Some(ct),
            });
            Box::pin(std::future::ready(r.ok_or_else(|| "rules down".to_string())))
        }
    }

    fn run_with(db: &Db, fx: &FakeExchange, market: Option<&dyn MarketData>, scope: Option<&BTreeSet<String>>) -> ReconcileReport {
        let c = ManualClock::new(5_000_000);
        let input = ReconcileInput { db, exchange: Ok(ExchangeAccess { executor: fx, account: fx }), market, scope, clock: &c };
        reconcile(input).now_or_never().expect("fakes resolve at once")
    }

    #[test]
    fn a_pair_stopped_in_pre_trade_check_is_blocked_without_any_exchange_call() {
        for simulated in [false, true] {
            let (_d, db, _) = open_tmp();
            seed_pair(&db, PAIR, PairState::PreTradeCheck, simulated);
            let c = ManualClock::new(5_000_000);
            // No exchange access at all: nothing was sent, so none is needed.
            let r = reconcile_startup(&db, Err("no keys".into()), &c).now_or_never().unwrap();
            assert_eq!(r.pending, None, "simulated={simulated}: {r:?}");
            assert_eq!(pair_state(&db), "BLOCKED", "simulated={simulated}");
            let o = only_outcome(&r);
            assert_eq!((o.before, o.after, &o.verdict), (Some(PairState::PreTradeCheck), Some(PairState::Blocked), &Verdict::CheckInterrupted));
            let ev = events(&db, PRE_TRADE_CHECK_INTERRUPTED);
            assert_eq!(ev.len(), 1);
            assert!(ev[0]["reason"].as_str().unwrap().contains("no order was sent"));
            assert_eq!(r.queries, 0);
            assert!(events(&db, RECONCILE_ALERT).is_empty());
        }
    }

    const MANUAL: &str = "manual-1700000000000-1";

    fn seed_manual(db: &Db, prefix: IdPrefix, exchange: Exchange, upto: IntentState) -> String {
        let cid = client_order_id(prefix, MANUAL, Leg::Long, OrderAction::Open, 0);
        db.create_intent(&NewIntent {
            client_order_id: cid.clone(),
            pair_uuid: MANUAL.into(),
            leg: "long".into(),
            exchange: exchange.name().into(),
            symbol: SYM.into(),
            side: "BUY".into(),
            quantity: "0.01".into(),
        })
        .unwrap();
        if upto == IntentState::Submitted {
            db.update_intent_state(&cid, IntentState::Submitted, None).unwrap();
        }
        cid
    }

    #[test]
    fn a_filled_manual_order_is_recorded_without_an_alert() {
        let (_d, db, _) = open_tmp();
        let m = seed_manual(&db, IdPrefix::Demo, LONG_EX, IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&m, "0.01");
        fx.position(LONG_EX, "0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(fx.queries(), vec![m.clone()]);
        assert_eq!(intent_state(&db, &m), "FILLED");
        assert_eq!(only_outcome(&r).verdict, Verdict::Kept);
        assert!(events(&db, RECONCILE_ALERT).is_empty(), "a manual order that resolves cleanly is not an orphan alert");
        assert!(db.list_unfinished_intents().unwrap().is_empty());
        fx.assert_read_only();
    }

    #[test]
    fn an_unknown_manual_order_without_exposure_is_marked_not_sent() {
        let (_d, db, _) = open_tmp();
        let m = seed_manual(&db, IdPrefix::Demo, LONG_EX, IntentState::Submitted);
        let fx = FakeExchange::default();
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(intent_state(&db, &m), "CANCELLED");
        assert_eq!(events(&db, ORDER_INTENT_NOT_SENT).len(), 1);
        assert!(events(&db, RECONCILE_ALERT).is_empty());
        fx.assert_read_only();
    }

    #[test]
    fn an_unknown_manual_order_while_its_symbol_has_exposure_is_kept_with_an_alert() {
        let (_d, db, _) = open_tmp();
        let m = seed_manual(&db, IdPrefix::Demo, LONG_EX, IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.position(LONG_EX, "0.01");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(intent_state(&db, &m), "SUBMITTED", "not proven unsent");
        assert_eq!(events(&db, RECONCILE_ALERT).len(), 1);
        assert!(events(&db, ORDER_INTENT_NOT_SENT).is_empty());
    }

    #[test]
    fn manual_orders_need_exchange_access_and_simulated_ones_are_never_queried() {
        let (_d, db, _) = open_tmp();
        seed_manual(&db, IdPrefix::Demo, LONG_EX, IntentState::Submitted);
        let c = ManualClock::new(5_000_000);
        let r = reconcile_startup(&db, Err("no keys".into()), &c).now_or_never().unwrap();
        assert!(r.pending.as_deref().is_some_and(|p| p.contains("no keys")), "{r:?}");

        let (_d2, db2, _) = open_tmp();
        let s = seed_manual(&db2, IdPrefix::Sim, LONG_EX, IntentState::Submitted);
        let fx = FakeExchange::default();
        let r = run(&db2, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert!(fx.queries().is_empty());
        assert_eq!(intent_state(&db2, &s), "SUBMITTED");
    }

    /// FILL_MONITOR pair with long on OKX (1 contract of 0.01 BTC) and short on Bybit (0.01).
    fn okx_pair(db: &Db) -> (String, String) {
        let env = PairEnvelope { long_exchange: Exchange::Okx, short_exchange: SHORT_EX, settlement_ms: 2_000_000, simulated: false, scan: json!({}) };
        let p = NewPair {
            internal_uuid: PAIR.into(),
            pair_id: "pid".into(),
            symbol: SYM.into(),
            status: PairState::FillMonitor,
            entry: serde_json::to_value(env).unwrap(),
        };
        db.add_pair_if_not_pending(&p).unwrap();
        let mut ids = Vec::new();
        for (leg, ex, side, qty) in [(Leg::Long, Exchange::Okx, "BUY", "1"), (Leg::Short, SHORT_EX, "SELL", "0.01")] {
            let cid = client_order_id(IdPrefix::Demo, PAIR, leg, OrderAction::Open, 0);
            db.create_intent(&NewIntent {
                client_order_id: cid.clone(),
                pair_uuid: PAIR.into(),
                leg: leg.as_str().into(),
                exchange: ex.name().into(),
                symbol: SYM.into(),
                side: side.into(),
                quantity: qty.into(),
            })
            .unwrap();
            db.update_intent_state(&cid, IntentState::Submitted, None).unwrap();
            ids.push(cid);
        }
        (ids[0].clone(), ids[1].clone())
    }

    #[test]
    fn the_fill_decision_compares_legs_in_base_coin_with_the_okx_contract_value() {
        let (_d, db, _) = open_tmp();
        let (l, s) = okx_pair(&db);
        let fx = FakeExchange::default();
        fx.filled(&l, "1");
        fx.filled(&s, "0.01");
        fx.position(Exchange::Okx, "1");
        fx.position(SHORT_EX, "-0.01");
        // Rules unavailable: the base quantities are unknown, so the pair waits in FILL_MONITOR.
        let r = run_with(&db, &fx, Some(&Rules(None)), None);
        assert!(r.pending.as_deref().is_some_and(|p| p.contains("OKX order rules")), "{r:?}");
        assert_eq!(pair_state(&db), "FILL_MONITOR");
        // 1 contract x 0.01 = 0.01 BTC on both legs: within tolerance.
        let r = run_with(&db, &fx, Some(&Rules(Some(d("0.01")))), None);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "RECONCILED");
        let known = LegFill::Known { requested: d("0.01"), filled: d("0.01") };
        assert_eq!(only_outcome(&r).verdict, Verdict::Normal { fills: Some((known, known)) });
        fx.assert_read_only();
    }

    #[test]
    fn both_legs_filled_but_far_apart_is_imbalanced() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::FillMonitor, false);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let s = seed_intent(&db, IdPrefix::Demo, Leg::Short, OrderAction::Open, "0.02", IntentState::Submitted);
        let fx = FakeExchange::default();
        fx.filled(&l, "0.01");
        fx.filled(&s, "0.02");
        fx.position(LONG_EX, "0.01");
        fx.position(SHORT_EX, "-0.02");
        let r = run(&db, &fx);
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(pair_state(&db), "IMBALANCED");
    }

    #[test]
    fn pairs_outside_the_scope_are_never_touched() {
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false); // started after startup: live
        let other = "7f1c2d3e-4b5a-4c6d-8e7f-0123456789ab";
        seed_pair(&db, other, PairState::FillMonitor, true); // in flight at startup
        seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = FakeExchange::default();
        let scope: BTreeSet<String> = [other.to_string()].into();
        let r = run_with(&db, &fx, None, Some(&scope));
        assert_eq!(r.pending, None, "{r:?}");
        assert_eq!(only_outcome(&r).pair, other);
        assert_eq!(db.get_pair(other).unwrap().unwrap().status, "UNRESOLVED");
        assert_eq!(pair_state(&db), "ORDER_SUBMIT");
        assert!(fx.queries().is_empty(), "the live pair's intent is not queried");
    }

    fn ctx(db: &Db, fx: &Arc<FakeExchange>, mode: ExecutionMode, scope: &[&str]) -> ReconcileContext {
        ReconcileContext {
            db: db.clone(),
            executor: fx.clone(),
            account: fx.clone(),
            execution_mode: mode,
            scope: scope.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_startup_reconciler_has_demo_access_only_in_exchange_demo() {
        let sim_pair = "7f1c2d3e-4b5a-4c6d-8e7f-0123456789ab";
        let (_d, db, _) = open_tmp();
        seed_pair(&db, PAIR, PairState::OrderSubmit, false);
        seed_pair(&db, sim_pair, PairState::FillMonitor, true);
        let l = seed_intent(&db, IdPrefix::Demo, Leg::Long, OrderAction::Open, "0.01", IntentState::Submitted);
        let fx = Arc::new(FakeExchange::default());
        let rec = RecoveryReconciler::new(Arc::new(Rules(None)), Arc::new(ManualClock::new(5_000_000)));

        // SIMULATION: the executor handed over is not demo access; the sim pair still resolves.
        let r = rec.reconcile(ctx(&db, &fx, ExecutionMode::Simulation, &[PAIR, sim_pair])).now_or_never().unwrap();
        let why = r.expect_err("demo pair cannot be reconciled in SIMULATION");
        assert!(why.contains("SIMULATION"), "{why}");
        assert!(fx.queries().is_empty());
        assert_eq!(db.get_pair(sim_pair).unwrap().unwrap().status, "UNRESOLVED");
        assert_eq!(pair_state(&db), "ORDER_SUBMIT");

        // EXCHANGE_DEMO: queried and resolved.
        fx.filled(&l, "0.01");
        fx.position(LONG_EX, "0.01");
        let r = rec.reconcile(ctx(&db, &fx, ExecutionMode::ExchangeDemo, &[PAIR, sim_pair])).now_or_never().unwrap();
        assert_eq!(r, Ok(()));
        assert_eq!(pair_state(&db), "PARTIAL_FAILURE", "long filled, short never sent");
        fx.assert_read_only();
    }
}

//! Intent-first order submission (crash-recovery spec, design D4; task 4.1). What a spawned I/O
//! task runs for one order; the actor itself never awaits any of this.
//!
//! Sequence: (1) write the intent (`INTENDED`) and mark it `SUBMITTED` — both before the
//! executor is called, so a crash at any point after the call has started leaves a durable trace
//! that the order may exist; (2) call `Executor::submit`; (3) record the outcome. A failure in (1)
//! means the executor is never called and the store is halted. An `Unknown` outcome keeps the
//! intent `SUBMITTED` with an `ORDER_RESULT_UNKNOWN` event (never `FAILED`), is never resubmitted
//! under a new id, and is followed by one `query` under the same id.
//!
//! The DB steps ([`land_intent`], [`record_submit_outcome`], [`record_query_outcome`]) and the
//! executor call are separate functions so each can be tested (and crash-injected) on its own.
//! The DB steps are synchronous SQLite writes; they run inside the spawned task, never the actor.

use serde_json::json;

use super::ports::{Executor, Leg, OrderRequest, OrderSide, OrderState, OrderStatus, QueryOutcome, SubmitOutcome};
use crate::ports::Clock;
use crate::store::db::{Db, StoreError};
use crate::store::events::EventStore;
use crate::store::state::{IntentState, NewIntent};

/// `submit` returned `Unknown`: the intent stays `SUBMITTED` until a query settles it.
pub const EV_ORDER_RESULT_UNKNOWN: &str = "ORDER_RESULT_UNKNOWN";
/// A query by `client_order_id` did not settle the intent (not found or failed).
pub const EV_ORDER_QUERY_INCONCLUSIVE: &str = "ORDER_QUERY_INCONCLUSIVE";

#[derive(Debug, thiserror::Error)]
pub enum IntentError {
    /// The intent could not be landed: the executor was NOT called and the store is halted.
    #[error("order intent could not be written; order not sent: {0}")]
    NotLanded(StoreError),
    /// The executor was called but its result could not be recorded: the store is halted and
    /// the intent must be reconciled (it is `SUBMITTED` or later in the database).
    #[error("order result could not be recorded: {0}")]
    ResultNotRecorded(StoreError),
}

/// What happened to one order.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmitReport {
    pub outcome: SubmitOutcome,
    /// The follow-up query under the same id, made only when `outcome` was `Unknown`.
    pub query: Option<QueryOutcome>,
    /// Intent state in the store afterwards.
    pub state: IntentState,
    /// When the submit call started / returned on the injected clock (`None` without a clock).
    pub timing: Option<SubmitTiming>,
}

/// Latency of one submit call (exchange-demo-execution task 1.3): `request_sent_at_ms` is read
/// right before `Executor::submit` (after the intent landed), `ack_at_ms` when it returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitTiming {
    pub request_sent_at_ms: i64,
    pub ack_at_ms: i64,
}

impl SubmitTiming {
    pub fn latency_ms(&self) -> i64 {
        self.ack_at_ms - self.request_sent_at_ms
    }
}

pub fn side_str(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    }
}

/// The `order_intents` row for `req`.
pub fn new_intent(pair_uuid: &str, leg: Leg, req: &OrderRequest) -> NewIntent {
    NewIntent {
        client_order_id: req.client_order_id.clone(),
        pair_uuid: pair_uuid.to_string(),
        leg: leg.as_str().to_string(),
        exchange: req.exchange.name().to_string(),
        symbol: req.symbol.clone(),
        side: side_str(req.side).to_string(),
        quantity: req.quantity.to_string(),
    }
}

/// Make sure the store is halted after a failed write, whatever the failure was (some store
/// errors, e.g. a duplicate id or an illegal transition, do not halt by themselves).
/// NOTE: the store has no dedicated halt reason for order-intent writes yet, so this reuses
/// `EventWriteFailed` with an "order intent" label (see report / needed store change).
fn halt_on(db: &Db, what: &str, e: StoreError) -> StoreError {
    if !db.is_halted() {
        let _ = db.event_write_failed(what, &e);
    }
    e
}

/// Step 1, DB only: write `INTENDED`, then mark `SUBMITTED`. On any error the store is halted
/// and the caller MUST NOT call the executor.
pub fn land_intent(db: &Db, pair_uuid: &str, leg: Leg, req: &OrderRequest) -> Result<(), IntentError> {
    db.create_intent(&new_intent(pair_uuid, leg, req))
        .map_err(|e| IntentError::NotLanded(halt_on(db, "order intent (INTENDED)", e)))?;
    db.update_intent_state(&req.client_order_id, IntentState::Submitted, None)
        .map_err(|e| IntentError::NotLanded(halt_on(db, "order intent (SUBMITTED)", e)))
}

/// The intent state an exchange-reported order state settles to.
fn settled_state(status: &OrderStatus) -> IntentState {
    match status.state {
        OrderState::Open => IntentState::Acknowledged,
        OrderState::Filled => IntentState::Filled,
        OrderState::Cancelled => IntentState::Cancelled,
        OrderState::Rejected => IntentState::Failed,
    }
}

fn record(db: &Db, client_order_id: &str, state: IntentState, exchange_order_id: Option<&str>) -> Result<IntentState, IntentError> {
    db.update_intent_state(client_order_id, state, exchange_order_id)
        .map(|()| state)
        .map_err(|e| IntentError::ResultNotRecorded(halt_on(db, "order intent result", e)))
}

/// Append an event about an intent without changing its state; returns the (unchanged) state.
fn note(db: &Db, event_type: &str, client_order_id: &str, payload: serde_json::Value) -> Result<IntentState, IntentError> {
    let fail = |e| IntentError::ResultNotRecorded(halt_on(db, "order intent result", e));
    let row = db.get_intent(client_order_id).map_err(fail)?.ok_or_else(|| fail(StoreError::IntentNotFound(client_order_id.to_string())))?;
    let state = IntentState::parse(&row.state)
        .ok_or_else(|| fail(StoreError::IllegalIntentTransition { from: row.state.clone(), to: "(unchanged)".into() }))?;
    EventStore::new(db.clone())
        .append(event_type, Some(&row.pair_uuid), payload)
        .map_err(|e| IntentError::ResultNotRecorded(halt_on(db, event_type, e)))?;
    Ok(state)
}

/// Step 3, DB only: record what `submit` said. `Unknown` leaves the intent `SUBMITTED` and adds
/// an `ORDER_RESULT_UNKNOWN` event. On any error the store is halted.
pub fn record_submit_outcome(db: &Db, req: &OrderRequest, outcome: &SubmitOutcome, simulated: bool) -> Result<IntentState, IntentError> {
    let id = req.client_order_id.as_str();
    match outcome {
        SubmitOutcome::Accepted(status) => record(db, id, settled_state(status), status.exchange_order_id.as_deref()),
        SubmitOutcome::Rejected { .. } => record(db, id, IntentState::Failed, None),
        SubmitOutcome::Unknown { reason } => {
            note(db, EV_ORDER_RESULT_UNKNOWN, id, json!({ "client_order_id": id, "reason": reason, "simulated": simulated }))
        }
    }
}

/// DB only: record a query by `client_order_id`. `Found` settles the intent; `NotFound` and
/// `Failed` leave it `SUBMITTED` (not finding an order is not proof it never existed) and add an
/// `ORDER_QUERY_INCONCLUSIVE` event. On any error the store is halted.
pub fn record_query_outcome(db: &Db, client_order_id: &str, outcome: &QueryOutcome, simulated: bool) -> Result<IntentState, IntentError> {
    match outcome {
        QueryOutcome::Found(status) => record(db, client_order_id, settled_state(status), status.exchange_order_id.as_deref()),
        QueryOutcome::NotFound | QueryOutcome::Failed { .. } => {
            let detail = match outcome {
                QueryOutcome::Failed { reason } => reason.as_str(),
                _ => "not found",
            };
            let payload = json!({ "client_order_id": client_order_id, "result": detail, "simulated": simulated });
            note(db, EV_ORDER_QUERY_INCONCLUSIVE, client_order_id, payload)
        }
    }
}

/// The whole sequence for one order, as run by a spawned task.
pub async fn submit_with_intent(
    db: &Db,
    executor: &dyn Executor,
    pair_uuid: &str,
    leg: Leg,
    req: OrderRequest,
) -> Result<SubmitReport, IntentError> {
    submit_with_intent_clocked(db, executor, None, pair_uuid, leg, req).await
}

/// Same, also timing the submit call on `clock` (`SubmitReport::timing`).
pub async fn submit_with_intent_clocked(
    db: &Db,
    executor: &dyn Executor,
    clock: Option<&dyn Clock>,
    pair_uuid: &str,
    leg: Leg,
    req: OrderRequest,
) -> Result<SubmitReport, IntentError> {
    land_intent(db, pair_uuid, leg, &req)?;
    let simulated = executor.is_simulated();
    let request_sent_at_ms = clock.map(|c| c.now_ms());
    let outcome = executor.submit(req.clone()).await;
    let timing = request_sent_at_ms.zip(clock).map(|(sent, c)| SubmitTiming { request_sent_at_ms: sent, ack_at_ms: c.now_ms() });
    let state = record_submit_outcome(db, &req, &outcome, simulated)?;
    if !matches!(outcome, SubmitOutcome::Unknown { .. }) {
        return Ok(SubmitReport { outcome, query: None, state, timing });
    }
    // Result unknown: look it up under the SAME id; never resubmit (crash-recovery spec).
    let query = executor.query(req.exchange, &req.symbol, &req.client_order_id).await;
    let state = record_query_outcome(db, &req.client_order_id, &query, simulated)?;
    Ok(SubmitReport { outcome, query: Some(query), state, timing })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ids::{IdPrefix, client_order_id};
    use crate::engine::ports::{BoxFut, OrderAction};
    use crate::engine::sim::{SimBehavior, SimPriceBook, SimulatedExecutor};
    use crate::store::db::HaltReason;
    use crate::store::db::test_support::open_tmp;
    use crate::store::state::IntentRow;
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tong_funding_core::types::{Decimal, Exchange};

    const PAIR: &str = "pair-1";

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn req(leg: Leg, seq: u16) -> OrderRequest {
        OrderRequest {
            client_order_id: client_order_id(IdPrefix::Sim, PAIR, leg, OrderAction::Open, seq),
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            side: OrderSide::Buy,
            quantity: d("0.019"),
            reduce_only: false,
            leverage: None,
        }
    }

    type Hook = Box<dyn Fn(&Db) + Send + Sync>;

    /// Wraps a `SimulatedExecutor`; records the store row seen at `submit` entry and every call.
    struct Spy {
        inner: SimulatedExecutor,
        db: Db,
        seen_at_submit: Mutex<Vec<Option<IntentRow>>>,
        submits: AtomicUsize,
        queries: Mutex<Vec<String>>,
        on_submit: Option<Hook>,
    }

    impl Spy {
        fn new(db: &Db) -> Spy {
            let prices = Arc::new(SimPriceBook::default());
            prices.set(Exchange::Binance, "BTCUSDT", d("60000"));
            Spy {
                inner: SimulatedExecutor::new(prices),
                db: db.clone(),
                seen_at_submit: Mutex::default(),
                submits: AtomicUsize::new(0),
                queries: Mutex::default(),
                on_submit: None,
            }
        }
        fn submits(&self) -> usize {
            self.submits.load(Ordering::SeqCst)
        }
    }

    impl Executor for Spy {
        fn is_simulated(&self) -> bool {
            true
        }
        fn submit(&self, req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            self.seen_at_submit.lock().unwrap().push(self.db.get_intent(&req.client_order_id).ok().flatten());
            if let Some(h) = &self.on_submit {
                h(&self.db);
            }
            self.inner.submit(req)
        }
        fn cancel(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome> {
            self.inner.cancel(exchange, symbol, client_order_id)
        }
        fn query(&self, exchange: Exchange, symbol: &str, client_order_id: &str) -> BoxFut<'_, QueryOutcome> {
            self.queries.lock().unwrap().push(client_order_id.to_string());
            self.inner.query(exchange, symbol, client_order_id)
        }
    }

    fn run(db: &Db, spy: &Spy, r: OrderRequest) -> Result<SubmitReport, IntentError> {
        submit_with_intent(db, spy, PAIR, Leg::Long, r).now_or_never().expect("simulated run resolves immediately")
    }

    fn state(db: &Db, id: &str) -> String {
        db.get_intent(id).unwrap().expect("intent row").state
    }

    fn events(db: &Db, ty: &str) -> Vec<serde_json::Value> {
        EventStore::new(db.clone()).list(1_000).unwrap().into_iter().filter(|e| e.event_type == ty).map(|e| e.payload).collect()
    }

    /// Make every INSERT into `events` fail (same injection as the store's own tests).
    fn break_event_inserts(db: &Db) {
        db.with_raw_conn_for_tests(|c| {
            Ok(c.execute_batch("CREATE TRIGGER inject_fail BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected failure'); END")?)
        })
        .unwrap();
    }

    // (a) spec "意圖先於呼叫"
    #[test]
    fn the_intent_is_in_the_store_when_submit_is_entered() {
        let (_d, db, _) = open_tmp();
        let spy = Spy::new(&db);
        let r = req(Leg::Long, 0);
        let report = run(&db, &spy, r.clone()).unwrap();
        assert_eq!(spy.submits(), 1);
        let seen = spy.seen_at_submit.lock().unwrap().clone();
        let row = seen[0].clone().expect("intent row must exist when submit is entered");
        assert_eq!((row.pair_uuid.as_str(), row.leg.as_str(), row.exchange.as_str()), (PAIR, "long", "Binance"));
        assert_eq!((row.symbol.as_str(), row.side.as_str(), row.quantity.as_str()), ("BTCUSDT", "BUY", "0.019"));
        assert_eq!(row.state, "SUBMITTED", "marked before the call");
        // ... and it was INTENDED first (the transition event says so).
        let ev = events(&db, "ORDER_INTENT_STATE");
        assert!(ev.iter().any(|e| e["client_order_id"] == r.client_order_id.as_str() && e["from"] == "INTENDED" && e["to"] == "SUBMITTED"), "{ev:?}");
        assert_eq!(report.state, IntentState::Filled);
        assert_eq!(state(&db, &r.client_order_id), "FILLED");
        assert!(db.get_intent(&r.client_order_id).unwrap().unwrap().exchange_order_id.is_some_and(|e| e.starts_with("sim-")));
    }

    // (b) spec "意圖寫入失敗": three ways to fail the write.
    #[test]
    fn a_halted_store_means_no_call() {
        let (_d, db, _) = open_tmp();
        db.halt(HaltReason::ConfigReadFailed("injected".into()));
        let spy = Spy::new(&db);
        let r = run(&db, &spy, req(Leg::Long, 0));
        assert!(matches!(r, Err(IntentError::NotLanded(_))), "{r:?}");
        assert_eq!(spy.submits(), 0);
        assert!(db.is_halted());
    }

    #[test]
    fn a_failed_intent_insert_means_no_call_and_halts_the_store() {
        let (_d, db, _) = open_tmp();
        let r = req(Leg::Long, 0);
        db.create_intent(&new_intent(PAIR, Leg::Long, &r)).unwrap(); // the id is already taken
        let spy = Spy::new(&db);
        let res = run(&db, &spy, r);
        assert!(matches!(res, Err(IntentError::NotLanded(StoreError::DuplicateClientOrderId(_)))), "{res:?}");
        assert_eq!(spy.submits(), 0, "executor must not be called");
        assert!(db.is_halted(), "an intent write failure halts the store");
    }

    #[test]
    fn a_failed_submitted_mark_means_no_call_and_halts_the_store() {
        let (_d, db, _) = open_tmp();
        break_event_inserts(&db); // INSERT intent succeeds, INTENDED -> SUBMITTED (with event) fails
        let spy = Spy::new(&db);
        let r = req(Leg::Long, 0);
        let res = run(&db, &spy, r.clone());
        assert!(matches!(res, Err(IntentError::NotLanded(_))), "{res:?}");
        assert_eq!(spy.submits(), 0);
        assert!(db.is_halted());
    }

    // (c) spec "逾時後以原 id 查詢"
    #[test]
    fn unknown_result_is_queried_under_the_same_id_and_never_resubmitted_or_failed() {
        let (_d, db, _) = open_tmp();
        let spy = Spy::new(&db);
        let r = req(Leg::Long, 0);
        spy.inner.set_default(SimBehavior::Unknown { executed: true });
        let report = run(&db, &spy, r.clone()).unwrap();
        assert!(matches!(report.outcome, SubmitOutcome::Unknown { .. }));
        assert_eq!(spy.submits(), 1, "never resubmitted");
        assert_eq!(*spy.queries.lock().unwrap(), vec![r.client_order_id.clone()], "queried once, same id");
        assert!(matches!(report.query, Some(QueryOutcome::Found(_))));
        assert_eq!(report.state, IntentState::Filled);
        assert_eq!(state(&db, &r.client_order_id), "FILLED");
        let unknown = events(&db, EV_ORDER_RESULT_UNKNOWN);
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0]["client_order_id"], r.client_order_id.as_str());
        assert_eq!(unknown[0]["simulated"], true);
    }

    #[test]
    fn unknown_result_not_found_by_query_stays_submitted() {
        let (_d, db, _) = open_tmp();
        let spy = Spy::new(&db);
        let r = req(Leg::Long, 0);
        spy.inner.set_default(SimBehavior::Unknown { executed: false });
        let report = run(&db, &spy, r.clone()).unwrap();
        assert_eq!(spy.submits(), 1);
        assert_eq!(*spy.queries.lock().unwrap(), vec![r.client_order_id.clone()]);
        assert_eq!(report.query, Some(QueryOutcome::NotFound));
        assert_eq!(report.state, IntentState::Submitted);
        assert_eq!(state(&db, &r.client_order_id), "SUBMITTED", "not FAILED: not finding it is not proof");
        assert_eq!(events(&db, EV_ORDER_QUERY_INCONCLUSIVE).len(), 1);
        assert_eq!(db.list_unfinished_intents().unwrap().len(), 1, "left for reconciliation");
    }

    #[test]
    fn unknown_result_with_failing_query_stays_submitted() {
        let (_d, db, _) = open_tmp();
        let spy = Spy::new(&db);
        spy.inner.set_default(SimBehavior::Unknown { executed: true });
        spy.inner.fail_queries(Some("unreachable".into()));
        let r = req(Leg::Long, 0);
        let report = run(&db, &spy, r.clone()).unwrap();
        assert_eq!(report.query, Some(QueryOutcome::Failed { reason: "unreachable".into() }));
        assert_eq!(state(&db, &r.client_order_id), "SUBMITTED");
        assert_eq!(spy.submits(), 1);
    }

    #[test]
    fn rejected_is_failed_and_partial_is_acknowledged() {
        let (_d, db, _) = open_tmp();
        let spy = Spy::new(&db);
        let rej = req(Leg::Long, 0);
        spy.inner.script_prefix(&rej.client_order_id, SimBehavior::Reject("no".into()));
        let report = run(&db, &spy, rej.clone()).unwrap();
        assert_eq!((report.state, report.query), (IntentState::Failed, None));
        assert_eq!(state(&db, &rej.client_order_id), "FAILED");
        let part = req(Leg::Long, 1);
        spy.inner.script_prefix(&part.client_order_id, SimBehavior::Partial(d("0.5")));
        let report = run(&db, &spy, part.clone()).unwrap();
        assert_eq!(report.state, IntentState::Acknowledged);
        assert_eq!(state(&db, &part.client_order_id), "ACKNOWLEDGED");
        assert!(spy.queries.lock().unwrap().is_empty(), "known outcomes are not queried");
    }

    #[test]
    fn a_result_that_cannot_be_recorded_halts_the_store() {
        let (_d, db, _) = open_tmp();
        let mut spy = Spy::new(&db);
        spy.on_submit = Some(Box::new(break_event_inserts));
        let r = req(Leg::Long, 0);
        let res = run(&db, &spy, r.clone());
        assert!(matches!(res, Err(IntentError::ResultNotRecorded(_))), "{res:?}");
        assert_eq!(spy.submits(), 1);
        assert!(db.is_halted());
    }

    #[test]
    fn steps_work_separately_land_then_record() {
        let (_d, db, _) = open_tmp();
        let r = req(Leg::Short, 0);
        land_intent(&db, PAIR, Leg::Short, &r).unwrap();
        assert_eq!(state(&db, &r.client_order_id), "SUBMITTED");
        assert_eq!(db.get_intent(&r.client_order_id).unwrap().unwrap().leg, "short");
        let status = OrderStatus {
            client_order_id: r.client_order_id.clone(),
            exchange_order_id: Some("X1".into()),
            filled_quantity: d("0.019"),
            avg_price: Some(d("1")),
            fee: None,
            fee_asset: None,
            state: OrderState::Filled,
        };
        let st = record_submit_outcome(&db, &r, &SubmitOutcome::Accepted(status.clone()), false).unwrap();
        assert_eq!(st, IntentState::Filled);
        assert_eq!(db.get_intent(&r.client_order_id).unwrap().unwrap().exchange_order_id.as_deref(), Some("X1"));
        // A late query that finds the same filled order is an idempotent no-op.
        assert_eq!(record_query_outcome(&db, &r.client_order_id, &QueryOutcome::Found(status), false).unwrap(), IntentState::Filled);
        assert!(!db.is_halted());
    }

    #[test]
    fn record_query_outcome_maps_every_order_state() {
        let (_d, db, _) = open_tmp();
        let cases = [
            (OrderState::Open, IntentState::Acknowledged),
            (OrderState::Filled, IntentState::Filled),
            (OrderState::Cancelled, IntentState::Cancelled),
            (OrderState::Rejected, IntentState::Failed),
        ];
        for (seq, (os, want)) in cases.into_iter().enumerate() {
            let r = req(Leg::Long, seq as u16);
            land_intent(&db, PAIR, Leg::Long, &r).unwrap();
            let status = OrderStatus {
                client_order_id: r.client_order_id.clone(),
                exchange_order_id: None,
                filled_quantity: Decimal::ZERO,
                avg_price: None,
                fee: None,
                fee_asset: None,
                state: os,
            };
            assert_eq!(record_query_outcome(&db, &r.client_order_id, &QueryOutcome::Found(status), true).unwrap(), want, "{os:?}");
            assert_eq!(state(&db, &r.client_order_id), want.as_str());
        }
    }

    #[test]
    fn the_whole_sequence_is_a_send_future_so_it_can_be_spawned() {
        fn assert_send<T: Send>(_: &T) {}
        let (_d, db, _) = open_tmp();
        let spy = Spy::new(&db);
        let fut = submit_with_intent(&db, &spy, PAIR, Leg::Long, req(Leg::Long, 0));
        assert_send(&fut);
    }
}

//! Land-then-act pair transitions (design D4, D13; task 1.3). Every pair state change is decided
//! by core `next()`; a legal one is written to the store (status + one immutable event, one
//! transaction) and only after that succeeds does its attached action run. An illegal one is
//! recorded and the state is kept. A failed write halts the store, which makes the gate refuse
//! every exposure-opening command from then on.

use serde_json::{Value, json};
use tong_funding_core::pair::{Event as PairEvent, IllegalTransition, PairState, next};

use crate::store::db::StoreError;
use crate::store::events::EventStore;

/// Event written with every landed transition.
pub const PAIR_TRANSITION: &str = "PAIR_TRANSITION";
/// Event written when core `next()` refuses a transition (state kept).
pub const ILLEGAL_TRANSITION: &str = "ILLEGAL_TRANSITION";

/// An "open pair" may still hold exposure: it counts against `max_concurrent_pairs` and blocks
/// switching `execution_mode`. Locked states (PARTIAL_FAILURE / IMBALANCED / UNRESOLVED) count
/// (design D13). Exhaustive on purpose: a new state must be classified here.
pub const fn is_open(state: PairState) -> bool {
    match state {
        PairState::Finalized | PairState::Cancelled | PairState::Blocked => false,
        PairState::Prepared
        | PairState::PreTradeCheck
        | PairState::OrderSubmit
        | PairState::FillMonitor
        | PairState::Reconciled
        | PairState::Imbalanced
        | PairState::Closing
        | PairState::PartialFailure
        | PairState::Unresolved => true,
    }
}

#[derive(Debug)]
pub enum TransitionError {
    /// Core `next()` refused it; the state is unchanged and `ILLEGAL_TRANSITION` was recorded
    /// (unless the store could not write it, in which case the store is now halted).
    Illegal(IllegalTransition),
    /// The stored row is missing or no longer in the expected state; nothing was written.
    Stale { expected: PairState },
    /// The store could not land the change and is now halted; the action did not run.
    StoreFailed(StoreError),
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionError::Illegal(e) => write!(f, "{e}"),
            TransitionError::Stale { expected } => write!(f, "pair is not in {expected} in the store"),
            TransitionError::StoreFailed(e) => write!(f, "store write failed, engine halted: {e}"),
        }
    }
}

/// Apply `event` to a pair the engine believes is in `from`: decide with `next()`, land the
/// change, then run `act(to)`. `act` never runs unless the write committed.
pub fn land_then_act<T>(
    store: &EventStore,
    pair: &str,
    from: PairState,
    event: impl Into<PairEvent>,
    detail: Value,
    act: impl FnOnce(PairState) -> T,
) -> Result<(PairState, T), TransitionError> {
    let event = event.into();
    let to = match next(from, event) {
        Ok(to) => to,
        Err(illegal) => {
            let payload = json!({ "from": from.as_str(), "event": format!("{event:?}"), "detail": detail });
            // A failed write here halts the store (EventStore::append); the state is kept either way.
            let _ = store.append(ILLEGAL_TRANSITION, Some(pair), payload);
            return Err(TransitionError::Illegal(illegal));
        }
    };
    let payload = json!({ "from": from.as_str(), "to": to.as_str(), "event": format!("{event:?}"), "detail": detail });
    match store.db().transition_pair(pair, from, to, PAIR_TRANSITION, &payload) {
        Ok(true) => Ok((to, act(to))),
        Ok(false) => Err(TransitionError::Stale { expected: from }),
        Err(e) => Err(TransitionError::StoreFailed(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ports::{BoxFut, Executor, OrderRequest, OrderSide, QueryOutcome, SubmitOutcome};
    use crate::store::db::test_support::open_tmp;
    use crate::store::db::{Db, HaltReason};
    use crate::store::state::NewPair;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tong_funding_core::pair::{ManualEvent, SystemEvent};
    use tong_funding_core::types::{Decimal, Exchange};

    /// Counts submits; never resolves them (the count is what matters here).
    #[derive(Default)]
    struct CountingExecutor(AtomicUsize);
    impl Executor for CountingExecutor {
        fn is_simulated(&self) -> bool {
            true
        }
        fn submit(&self, _req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
        fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            Box::pin(std::future::pending())
        }
        fn query(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            Box::pin(std::future::pending())
        }
    }

    fn seed(db: &Db, uuid: &str, state: PairState) {
        let p = NewPair { internal_uuid: uuid.into(), pair_id: "p".into(), symbol: format!("S{uuid}"), status: state, entry: json!({}) };
        db.add_pair_if_not_pending(&p).unwrap();
    }

    fn status(db: &Db, uuid: &str) -> String {
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        plain.query_row("SELECT status FROM pairs WHERE internal_uuid = ?1", [uuid], |r| r.get(0)).unwrap()
    }

    fn count_events(db: &Db, ty: &str) -> i64 {
        let plain = rusqlite::Connection::open(db.path()).unwrap();
        plain.query_row("SELECT COUNT(*) FROM events WHERE event_type = ?1", [ty], |r| r.get(0)).unwrap()
    }

    fn break_event_inserts(db: &Db) {
        db.with_raw_conn_for_tests(|c| {
            Ok(c.execute_batch("CREATE TRIGGER inject_fail BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'injected failure'); END")?)
        })
        .unwrap();
    }

    fn order() -> OrderRequest {
        OrderRequest {
            client_order_id: "sim-x".into(),
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            side: OrderSide::Buy,
            quantity: Decimal::new(1, 3),
            reduce_only: false,
            leverage: None,
        }
    }

    #[test]
    fn open_pairs_are_everything_but_finalized_cancelled_blocked() {
        let closed = [PairState::Finalized, PairState::Cancelled, PairState::Blocked];
        for s in PairState::ALL {
            assert_eq!(is_open(s), !closed.contains(&s), "{s}");
        }
        assert!(is_open(PairState::PartialFailure) && is_open(PairState::Imbalanced) && is_open(PairState::Unresolved));
    }

    #[test]
    fn a_legal_transition_lands_before_the_action_runs() {
        let (_d, db, _) = open_tmp();
        let store = EventStore::new(db.clone());
        seed(&db, "u1", PairState::PreTradeCheck);
        let exec = CountingExecutor::default();
        let (to, seen) = land_then_act(&store, "u1", PairState::PreTradeCheck, SystemEvent::CheckPassed, json!({}), |to| {
            let landed = status(&db, "u1");
            drop(exec.submit(order()));
            (to, landed)
        })
        .unwrap()
        .1;
        assert_eq!(to, PairState::OrderSubmit);
        assert_eq!(seen, "ORDER_SUBMIT", "the action must see the landed state");
        assert_eq!(exec.0.load(Ordering::SeqCst), 1);
        assert_eq!(count_events(&db, PAIR_TRANSITION), 1);
    }

    #[test]
    fn a_failed_write_halts_the_store_and_never_calls_the_executor() {
        let (_d, db, _) = open_tmp();
        let store = EventStore::new(db.clone());
        seed(&db, "u1", PairState::PreTradeCheck);
        break_event_inserts(&db);
        let exec = CountingExecutor::default();
        let r = land_then_act(&store, "u1", PairState::PreTradeCheck, SystemEvent::CheckPassed, json!({}), |_| {
            drop(exec.submit(order()));
        });
        assert!(matches!(r, Err(TransitionError::StoreFailed(_))), "{r:?}");
        assert_eq!(exec.0.load(Ordering::SeqCst), 0, "Executor must not be called");
        assert!(matches!(db.halt_reason(), Some(HaltReason::EventWriteFailed(_))));
        assert_eq!(status(&db, "u1"), "PRE_TRADE_CHECK");
    }

    #[test]
    fn an_illegal_transition_keeps_the_state_and_records_an_event() {
        let (_d, db, _) = open_tmp();
        let store = EventStore::new(db.clone());
        seed(&db, "u1", PairState::PartialFailure);
        let mut ran = false;
        let r = land_then_act(&store, "u1", PairState::PartialFailure, SystemEvent::FillsWithinTolerance, json!({}), |_| ran = true);
        assert!(matches!(r, Err(TransitionError::Illegal(_))), "{r:?}");
        assert!(!ran);
        assert_eq!(status(&db, "u1"), "PARTIAL_FAILURE");
        assert_eq!(count_events(&db, ILLEGAL_TRANSITION), 1);
        assert_eq!(count_events(&db, PAIR_TRANSITION), 0);
        assert!(!db.is_halted());
    }

    #[test]
    fn a_stale_expected_state_writes_nothing_and_does_not_act() {
        let (_d, db, _) = open_tmp();
        let store = EventStore::new(db.clone());
        seed(&db, "u1", PairState::Reconciled);
        let mut ran = false;
        // The engine thinks PREPARED, the store says RECONCILED.
        let r = land_then_act(&store, "u1", PairState::Prepared, ManualEvent::Cancel, json!({}), |_| ran = true);
        assert!(matches!(r, Err(TransitionError::Stale { expected: PairState::Prepared })), "{r:?}");
        assert!(!ran);
        assert_eq!(status(&db, "u1"), "RECONCILED");
        assert_eq!(count_events(&db, PAIR_TRANSITION), 0);
    }
}

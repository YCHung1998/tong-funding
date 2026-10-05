//! Gating (task 3.2; execution-modes spec): what refuses exposure-opening commands (kill switch,
//! halted store, pending reconciliation), and the two independent mode switches. `trigger_mode`
//! and `execution_mode` live in separate store flags; changing one never touches the other.

use std::sync::Arc;

use serde_json::json;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};

use super::command::{Blocker, Command};
use super::ports::{Executor, ExecutorFactory};
use crate::store::db::{Db, HaltReason};
use crate::store::state::{FLAG_EXECUTION_MODE, FLAG_TRIGGER_MODE};

pub const TRIGGER_MODE_CHANGED: &str = "TRIGGER_MODE_CHANGED";
pub const EXECUTION_MODE_CHANGED: &str = "EXECUTION_MODE_CHANGED";
/// Stored mode was EXCHANGE_DEMO at startup but the executor could not be built.
pub const EXECUTION_MODE_FALLBACK: &str = "EXECUTION_MODE_FALLBACK";

pub fn trigger_mode_str(m: TriggerMode) -> &'static str {
    match m {
        TriggerMode::Auto => "AUTO",
        TriggerMode::Manual => "MANUAL",
    }
}

pub fn execution_mode_str(m: ExecutionMode) -> &'static str {
    match m {
        ExecutionMode::Simulation => "SIMULATION",
        ExecutionMode::ExchangeDemo => "EXCHANGE_DEMO",
    }
}

/// Everything that currently refuses exposure-opening commands. Reads the kill switch fresh from
/// the store; any read failure halts the store and counts as halted (fail closed).
pub fn current_blockers(db: &Db, reconciliation_pending: Option<&str>) -> Vec<Blocker> {
    let kill = db.kill_switch_halted();
    let mut out = Vec::new();
    match db.halt_reason() {
        Some(HaltReason::KillSwitchReadFailed(r)) => out.push(Blocker::KillSwitchUnreadable(r)),
        Some(other) => out.push(Blocker::StoreHalted(other.to_string())),
        None if kill => out.push(Blocker::KillSwitch),
        None => {}
    }
    if let Some(r) = reconciliation_pending {
        out.push(Blocker::ReconciliationPending(r.to_string()));
    }
    out
}

/// The blockers that refuse exposure in `mode` (decision 8): a pending reconciliation is about
/// demo orders, so it refuses only in EXCHANGE_DEMO; SIMULATION entries stay allowed (simulated
/// pairs never need the exchange to reconcile). The kill switch and a halted store refuse in
/// both modes. The Snapshot keeps showing every blocker.
pub fn refusing(blockers: &[Blocker], mode: ExecutionMode) -> Vec<Blocker> {
    blockers
        .iter()
        .filter(|b| match b {
            Blocker::ReconciliationPending(_) => mode == ExecutionMode::ExchangeDemo,
            Blocker::KillSwitch | Blocker::KillSwitchUnreadable(_) | Blocker::StoreHalted(_) => true,
        })
        .cloned()
        .collect()
}

/// `Err(reason)` when `cmd` must be refused. Only `opens_exposure()` commands are ever refused
/// here; closing, cancelling and settings always pass (the kill switch never forces a close).
pub fn admit(cmd: &Command, blockers: &[Blocker]) -> Result<(), String> {
    if !cmd.opens_exposure() || blockers.is_empty() {
        return Ok(());
    }
    let why: Vec<String> = blockers.iter().map(describe).collect();
    Err(format!("refused: {}", why.join("; ")))
}

pub fn describe(b: &Blocker) -> String {
    match b {
        Blocker::KillSwitch => "kill switch is on".into(),
        Blocker::KillSwitchUnreadable(r) => format!("kill switch unreadable (treated as on): {r}"),
        Blocker::StoreHalted(r) => format!("store halted: {r}"),
        Blocker::ReconciliationPending(r) => format!("reconciliation pending: {r}"),
    }
}

/// Persisted modes. Missing or unrecognised values fall back to the safe pair MANUAL / SIMULATION
/// (with a warning); a read failure halts the store and also yields the safe pair.
pub fn load_modes(db: &Db) -> (TriggerMode, ExecutionMode, Vec<String>) {
    let mut warnings = Vec::new();
    let mut read = |key: &str| match db.flag_get(key) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("cannot read {key}: {e}"));
            None
        }
    };
    let (trigger, execution) = (read(FLAG_TRIGGER_MODE), read(FLAG_EXECUTION_MODE));
    let trigger = match trigger.as_deref().map(str::parse::<TriggerMode>) {
        None => TriggerMode::Manual,
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            warnings.push(format!("{e}; using MANUAL"));
            TriggerMode::Manual
        }
    };
    let execution = match execution.as_deref().map(str::parse::<ExecutionMode>) {
        None => ExecutionMode::Simulation,
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            warnings.push(format!("{e}; using SIMULATION"));
            ExecutionMode::Simulation
        }
    };
    (trigger, execution, warnings)
}

/// Persist a new `trigger_mode` (flag + event in one transaction). Never touches `execution_mode`.
pub fn set_trigger_mode(db: &Db, current: TriggerMode, target: TriggerMode) -> Result<(), String> {
    let payload = json!({ "from": trigger_mode_str(current), "to": trigger_mode_str(target) });
    db.set_flag_with_event(FLAG_TRIGGER_MODE, trigger_mode_str(target), TRIGGER_MODE_CHANGED, &payload)
        .map_err(|e| format!("cannot save trigger_mode: {e}"))
}

/// Result of a `SetExecutionMode` request.
pub enum ModeSwitch {
    /// Already in the requested mode; nothing done (the factory was not called).
    Unchanged,
    /// Switched and persisted. `executor` is the factory-built one for EXCHANGE_DEMO, `None` when
    /// back to SIMULATION (the caller drops the order-capable executor and uses the simulator).
    Switched { mode: ExecutionMode, executor: Option<Arc<dyn Executor>> },
    /// Mode unchanged; reason for the user.
    Refused(String),
}

/// Switch `execution_mode`: allowed only when every open pair belongs to `target` (decision
/// 2026-10-05 evening; `other_mode_open` = open pairs created in the OTHER mode, counted by the
/// caller from `PairView::simulated`). Thus demo pairs left open by a startup fallback to
/// SIMULATION can be taken back to EXCHANGE_DEMO, while a pair is never traded through the other
/// mode's executor. Switching to EXCHANGE_DEMO builds the executor through the factory first (the
/// key check; failure keeps SIMULATION). The factory is called only for an actual, otherwise
/// allowed switch to EXCHANGE_DEMO. Never touches `trigger_mode`.
pub fn switch_execution_mode(
    db: &Db,
    factory: &dyn ExecutorFactory,
    current: ExecutionMode,
    target: ExecutionMode,
    other_mode_open: usize,
) -> ModeSwitch {
    if current == target {
        return ModeSwitch::Unchanged;
    }
    if other_mode_open > 0 {
        let other = match target {
            ExecutionMode::ExchangeDemo => ExecutionMode::Simulation,
            ExecutionMode::Simulation => ExecutionMode::ExchangeDemo,
        };
        return ModeSwitch::Refused(format!(
            "{other_mode_open} open pair(s) belong to {}; finish or cancel them before switching execution_mode to {}",
            execution_mode_str(other),
            execution_mode_str(target)
        ));
    }
    let executor = match target {
        ExecutionMode::ExchangeDemo => match factory.create(target) {
            Ok(e) => Some(e),
            Err(e) => return ModeSwitch::Refused(format!("未連線 (not connected), staying in SIMULATION: {e}")),
        },
        ExecutionMode::Simulation => None,
    };
    let payload = json!({ "from": execution_mode_str(current), "to": execution_mode_str(target) });
    match db.set_flag_with_event(FLAG_EXECUTION_MODE, execution_mode_str(target), EXECUTION_MODE_CHANGED, &payload) {
        // On failure `executor` is dropped here: no order-capable instance outlives a refused switch.
        Err(e) => ModeSwitch::Refused(format!("cannot save execution_mode: {e}")),
        Ok(()) => ModeSwitch::Switched { mode: target, executor },
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::engine::ports::{BoxFut, OrderRequest, QueryOutcome, SubmitOutcome};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tong_funding_core::types::Exchange;

    /// An executor whose calls never resolve; counts submits.
    #[derive(Default)]
    pub struct NullExecutor {
        pub simulated: bool,
        pub submits: AtomicUsize,
    }
    impl NullExecutor {
        pub fn sim() -> Arc<NullExecutor> {
            Arc::new(NullExecutor { simulated: true, submits: AtomicUsize::new(0) })
        }
    }
    impl Executor for NullExecutor {
        fn is_simulated(&self) -> bool {
            self.simulated
        }
        fn submit(&self, _req: OrderRequest) -> BoxFut<'_, SubmitOutcome> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
        fn cancel(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            Box::pin(std::future::pending())
        }
        fn query(&self, _: Exchange, _: &str, _: &str) -> BoxFut<'_, QueryOutcome> {
            Box::pin(std::future::pending())
        }
    }

    /// Factory that counts calls and either fails with `fail` or builds a non-simulated executor.
    pub struct CountingFactory {
        pub calls: AtomicUsize,
        pub fail: Mutex<Option<String>>,
    }
    impl CountingFactory {
        pub fn ok() -> Arc<CountingFactory> {
            Arc::new(CountingFactory { calls: AtomicUsize::new(0), fail: Mutex::new(None) })
        }
        pub fn failing(reason: &str) -> Arc<CountingFactory> {
            Arc::new(CountingFactory { calls: AtomicUsize::new(0), fail: Mutex::new(Some(reason.into())) })
        }
        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }
    impl ExecutorFactory for CountingFactory {
        fn create(&self, _mode: ExecutionMode) -> Result<Arc<dyn Executor>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.fail.lock().unwrap().clone() {
                Some(r) => Err(r),
                None => Ok(Arc::new(NullExecutor { simulated: false, submits: AtomicUsize::new(0) })),
            }
        }
    }

    pub fn delete_kill_switch_row(db: &Db) {
        db.with_conn(|c| Ok(c.execute("DELETE FROM system_flags WHERE key = 'kill_switch'", [])?)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::engine::command::{ManualOrder, NewPreparedPair};
    use crate::engine::ports::OrderSide;
    use crate::store::db::test_support::open_tmp;
    use tong_funding_core::types::{Decimal, Exchange};

    fn p() -> String {
        "u1".into()
    }

    fn every_command() -> Vec<Command> {
        vec![
            Command::Tick,
            Command::AddPrepared(NewPreparedPair {
                internal_uuid: p(),
                pair_id: "p".into(),
                symbol: "BTCUSDT".into(),
                long_exchange: Exchange::Binance,
                short_exchange: Exchange::Bybit,
                settlement_ms: 0,
                entry: json!({}),
            }),
            Command::EntryTrigger { pair: p() },
            Command::ManualEnter { pair: p() },
            Command::ManualOrder(ManualOrder {
                exchange: Exchange::Binance,
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                quantity: Decimal::ONE,
                reduce_only: false,
            }),
            Command::ManualOrder(ManualOrder {
                exchange: Exchange::Binance,
                symbol: "BTCUSDT".into(),
                side: OrderSide::Sell,
                quantity: Decimal::ONE,
                reduce_only: true,
            }),
            Command::AutoExit { pair: p() },
            Command::ManualExit { pair: p() },
            Command::ManualClose { pair: p() },
            Command::ConfirmClosed { pair: p(), verified_flat: true },
            Command::CancelPrepared { pair: p(), reason: "r".into() },
            Command::SetTriggerMode(TriggerMode::Auto),
            Command::SetExecutionMode(ExecutionMode::ExchangeDemo),
            Command::UpdateConfig { key: "risk".into(), value: json!({}) },
            Command::SetKillSwitch { on: false },
        ]
    }

    fn flag(db: &Db, key: &str) -> Option<String> {
        db.flag_get(key).unwrap()
    }

    #[test]
    fn halted_refuses_exactly_the_exposure_opening_commands() {
        for blockers in [
            vec![Blocker::KillSwitch],
            vec![Blocker::KillSwitchUnreadable("x".into())],
            vec![Blocker::StoreHalted("x".into())],
            vec![Blocker::ReconciliationPending("x".into())],
        ] {
            for c in every_command() {
                let r = admit(&c, &blockers);
                assert_eq!(r.is_err(), c.opens_exposure(), "{c:?} with {blockers:?}: {r:?}");
            }
        }
        for c in every_command() {
            assert_eq!(admit(&c, &[]), Ok(()), "{c:?} without blockers");
        }
    }

    #[test]
    fn halted_still_accepts_a_manual_close_and_the_entry_trigger_is_refused() {
        let b = [Blocker::KillSwitch];
        assert_eq!(admit(&Command::ManualClose { pair: p() }, &b), Ok(()));
        assert_eq!(admit(&Command::AutoExit { pair: p() }, &b), Ok(()), "auto exit runs while halted (decision 2)");
        let r = admit(&Command::EntryTrigger { pair: p() }, &b).unwrap_err();
        assert!(r.contains("kill switch"), "{r}");
    }

    #[test]
    fn blockers_reflect_the_kill_switch() {
        let (_d, db, _) = open_tmp();
        assert_eq!(current_blockers(&db, None), vec![]);
        db.set_kill_switch(true).unwrap();
        assert_eq!(current_blockers(&db, None), vec![Blocker::KillSwitch]);
        db.set_kill_switch(false).unwrap();
        assert_eq!(
            current_blockers(&db, Some("2 intents")),
            vec![Blocker::ReconciliationPending("2 intents".into())]
        );
    }

    // decision 8: unfinished demo reconciliation refuses exposure only in EXCHANGE_DEMO.
    #[test]
    fn pending_reconciliation_refuses_only_in_exchange_demo_and_the_rest_refuses_in_both() {
        let pending = Blocker::ReconciliationPending("demo pair p1".into());
        let enter = Command::ManualEnter { pair: p() };
        let demo = refusing(std::slice::from_ref(&pending), ExecutionMode::ExchangeDemo);
        assert_eq!(demo, vec![pending.clone()]);
        assert!(admit(&enter, &demo).unwrap_err().contains("reconciliation pending"));
        let sim = refusing(std::slice::from_ref(&pending), ExecutionMode::Simulation);
        assert_eq!(sim, vec![]);
        assert_eq!(admit(&enter, &sim), Ok(()));
        for other in [Blocker::KillSwitch, Blocker::KillSwitchUnreadable("x".into()), Blocker::StoreHalted("y".into())] {
            for mode in [ExecutionMode::Simulation, ExecutionMode::ExchangeDemo] {
                let r = refusing(&[pending.clone(), other.clone()], mode);
                assert!(r.contains(&other), "{other:?} refuses in {mode:?}");
                assert!(admit(&enter, &r).is_err());
            }
        }
    }

    #[test]
    fn a_kill_switch_read_failure_counts_as_halted() {
        let (_d, db, _) = open_tmp();
        delete_kill_switch_row(&db);
        let b = current_blockers(&db, None);
        assert!(matches!(b.as_slice(), [Blocker::KillSwitchUnreadable(_)]), "{b:?}");
        assert!(admit(&Command::ManualEnter { pair: p() }, &b).is_err());
        assert!(admit(&Command::ManualClose { pair: p() }, &b).is_ok());
    }

    #[test]
    fn a_halted_store_is_a_blocker() {
        let (_d, db, _) = open_tmp();
        db.halt(HaltReason::EventWriteFailed("disk full".into()));
        let b = current_blockers(&db, None);
        assert!(matches!(b.as_slice(), [Blocker::StoreHalted(r)] if r.contains("disk full")), "{b:?}");
    }

    #[test]
    fn modes_default_to_manual_and_simulation_and_load_what_was_stored() {
        let (_d, db, _) = open_tmp();
        let (t, e, w) = load_modes(&db);
        assert_eq!((t, e), (TriggerMode::Manual, ExecutionMode::Simulation));
        assert!(w.is_empty(), "{w:?}");
        db.flag_set(FLAG_TRIGGER_MODE, "AUTO").unwrap();
        db.flag_set(FLAG_EXECUTION_MODE, "EXCHANGE_DEMO").unwrap();
        assert_eq!(load_modes(&db).0, TriggerMode::Auto);
        assert_eq!(load_modes(&db).1, ExecutionMode::ExchangeDemo);
        db.flag_set(FLAG_EXECUTION_MODE, "LIVE").unwrap();
        let (_, e, w) = load_modes(&db);
        assert_eq!(e, ExecutionMode::Simulation, "unknown value falls back to SIMULATION");
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn trigger_and_execution_modes_are_independent() {
        let (_d, db, _) = open_tmp();
        db.flag_set(FLAG_EXECUTION_MODE, "SIMULATION").unwrap();
        set_trigger_mode(&db, TriggerMode::Manual, TriggerMode::Auto).unwrap();
        assert_eq!(flag(&db, FLAG_TRIGGER_MODE).as_deref(), Some("AUTO"));
        assert_eq!(flag(&db, FLAG_EXECUTION_MODE).as_deref(), Some("SIMULATION"), "execution_mode untouched");

        let f = CountingFactory::ok();
        let r = switch_execution_mode(&db, f.as_ref(), ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, 0);
        assert!(matches!(r, ModeSwitch::Switched { mode: ExecutionMode::ExchangeDemo, executor: Some(_) }));
        assert_eq!(flag(&db, FLAG_EXECUTION_MODE).as_deref(), Some("EXCHANGE_DEMO"));
        assert_eq!(flag(&db, FLAG_TRIGGER_MODE).as_deref(), Some("AUTO"), "trigger_mode untouched");

        let r = switch_execution_mode(&db, f.as_ref(), ExecutionMode::ExchangeDemo, ExecutionMode::Simulation, 0);
        assert!(matches!(r, ModeSwitch::Switched { mode: ExecutionMode::Simulation, executor: None }));
        assert_eq!(flag(&db, FLAG_EXECUTION_MODE).as_deref(), Some("SIMULATION"));
        assert_eq!(f.calls(), 1, "switching back to SIMULATION does not call the factory");
    }

    #[test]
    fn pairs_of_the_target_mode_do_not_hold_the_switch_back() {
        // The caller passes only pairs of the OTHER mode; pairs of the target mode are fine.
        let (_d, db, _) = open_tmp();
        let f = CountingFactory::ok();
        let r = switch_execution_mode(&db, f.as_ref(), ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, 0);
        assert!(matches!(r, ModeSwitch::Switched { mode: ExecutionMode::ExchangeDemo, executor: Some(_) }));
        assert_eq!(f.calls(), 1, "the key check still runs");
    }

    #[test]
    fn switching_is_refused_while_a_pair_is_open() {
        let (_d, db, _) = open_tmp();
        let f = CountingFactory::ok();
        for (cur, tgt) in [
            (ExecutionMode::Simulation, ExecutionMode::ExchangeDemo),
            (ExecutionMode::ExchangeDemo, ExecutionMode::Simulation),
        ] {
            let r = switch_execution_mode(&db, f.as_ref(), cur, tgt, 1);
            let other = execution_mode_str(cur);
            assert!(matches!(&r, ModeSwitch::Refused(why) if why.contains("open") && why.contains(other)), "{cur:?}->{tgt:?}");
        }
        assert_eq!(f.calls(), 0, "refused before the factory");
        assert_eq!(flag(&db, FLAG_EXECUTION_MODE), None);
    }

    #[test]
    fn a_factory_failure_keeps_simulation_and_reports_the_reason() {
        let (_d, db, _) = open_tmp();
        let f = CountingFactory::failing("keychain: item not found");
        let r = switch_execution_mode(&db, f.as_ref(), ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, 0);
        assert!(matches!(&r, ModeSwitch::Refused(why) if why.contains("keychain: item not found")));
        assert_eq!(f.calls(), 1);
        assert_eq!(flag(&db, FLAG_EXECUTION_MODE), None, "nothing persisted");
    }

    #[test]
    fn no_factory_call_without_a_switch_to_exchange_demo() {
        let (_d, db, _) = open_tmp();
        let f = CountingFactory::ok();
        let r = switch_execution_mode(&db, f.as_ref(), ExecutionMode::Simulation, ExecutionMode::Simulation, 3);
        assert!(matches!(r, ModeSwitch::Unchanged));
        assert_eq!(f.calls(), 0);
    }

    #[test]
    fn a_persist_failure_drops_the_new_executor_and_keeps_the_mode() {
        let (_d, db, _) = open_tmp();
        db.halt(HaltReason::EventWriteFailed("x".into()));
        let f = CountingFactory::ok();
        let r = switch_execution_mode(&db, f.as_ref(), ExecutionMode::Simulation, ExecutionMode::ExchangeDemo, 0);
        assert!(matches!(r, ModeSwitch::Refused(_)));
        assert!(set_trigger_mode(&db, TriggerMode::Manual, TriggerMode::Auto).is_err());
    }
}

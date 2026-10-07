use super::*;
use crate::engine::command::{Alert as EngineAlert, Blocker, Notice, PairView, Snapshot};
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::Exchange;

fn view(uuid: &str, symbol: &str, state: PairState, simulated: bool) -> PairView {
    PairView {
        internal_uuid: uuid.into(),
        pair_id: format!("pid-{uuid}"),
        symbol: symbol.into(),
        long_exchange: Exchange::Binance,
        short_exchange: Exchange::Bybit,
        state,
        settlement_ms: 9_000,
        simulated,
        flat_confirmed: false,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        now_ms: 1_000,
        trigger_mode: TriggerMode::Manual,
        execution_mode: ExecutionMode::ExchangeDemo,
        pairs: vec![
            view("u1", "BTCUSDT", PairState::Prepared, false),
            view("u2", "ETHUSDT", PairState::PartialFailure, true),
            view("u3", "SOLUSDT", PairState::Finalized, true),
        ],
        blockers: vec![Blocker::KillSwitch],
        prices: vec![(Exchange::Binance, "BTCUSDT".into(), "100".parse().unwrap())],
        notices: vec![Notice { code: "EXECUTION_MODE_FALLBACK".into(), message: "m".into() }],
        alerts: vec![EngineAlert {
            pair: "u2".into(),
            pair_id: "pid-u2".into(),
            symbol: "ETHUSDT".into(),
            state: PairState::PartialFailure,
            simulated: true,
            reason: Some("LEG_REJECTED".into()),
        }],
    }
}

#[test]
fn engine_snapshot_maps_modes_pairs_blockers_notices_and_alerts() {
    let e = engine_state(&snapshot());
    assert_eq!((e.now_ms, e.trigger_mode, e.execution_mode), (1_000, TriggerMode::Manual, ExecutionMode::ExchangeDemo));
    assert_eq!(e.pairs.len(), 3, "every engine pair is kept (the pages filter by state)");
    assert_eq!(e.blockers, vec![Blocker::KillSwitch]);
    assert_eq!(e.notices.len(), 1);
    assert_eq!(e.alerts[0].pair, "u2");
    assert_eq!(e.pair("u2").map(|p| p.simulated), Some(true));
}

#[test]
fn ui_pairs_come_from_the_engine_and_closed_pairs_are_dropped() {
    let infos = pair_infos(&engine_state(&snapshot()));
    let ids: Vec<&str> = infos.iter().map(|p| p.pair_id.as_str()).collect();
    assert_eq!(ids, vec!["pid-u1", "pid-u2"]);
    assert_eq!(infos[1].state, Ok(PairState::PartialFailure));
}

#[test]
fn applying_an_engine_update_replaces_the_store_pairs_and_an_outage_clears_the_engine() {
    let mut snap = UiSnapshot::default();
    apply_update(&mut snap, SourceUpdate::Engine(engine_state(&snapshot())));
    assert_eq!(snap.pairs.len(), 2);
    assert!(snap.engine.is_some());
    apply_update(&mut snap, SourceUpdate::EngineUnavailable("no db".into()));
    assert!(snap.engine.is_none());
    assert_eq!(snap.engine_error.as_deref(), Some("no db"));
}

#[test]
fn refusing_blockers_follow_the_engine_gate() {
    let mut e = engine_state(&snapshot());
    e.blockers = vec![Blocker::ReconciliationPending("x".into())];
    e.execution_mode = ExecutionMode::Simulation;
    assert!(refusing_blockers(&e).is_empty(), "a pending demo reconciliation does not refuse SIMULATION");
    e.execution_mode = ExecutionMode::ExchangeDemo;
    assert_eq!(refusing_blockers(&e).len(), 1);
}

#[test]
fn command_replies_are_capped() {
    let mut snap = UiSnapshot::default();
    for i in 0..(MAX_REPLIES + 5) {
        apply_update(&mut snap, SourceUpdate::CommandResult(CommandOutcome { label: format!("c{i}"), reply: crate::engine::command::CommandReply::Accepted, at: i as i64 }));
    }
    assert_eq!(snap.replies.len(), MAX_REPLIES);
    assert_eq!(snap.replies.last().unwrap().label, format!("c{}", MAX_REPLIES + 4));
}

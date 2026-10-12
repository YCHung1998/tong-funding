use super::*;
use crate::store::funding_slots::SlotLeg;
use tong_funding_core::types::Exchange;

const T: i64 = 1_791_302_400_000;
const M: i64 = 60_000;

fn row() -> SlotRow {
    SlotRow::new("p1", SlotLeg::Long, Exchange::Binance, "NMRUSDT", T, first_check_ms(T), T + 10_000)
}

fn not_found() -> CheckOutcome {
    CheckOutcome::NotFound { rate: RateCheck::NonZero }
}

fn failed() -> CheckOutcome {
    CheckOutcome::Failed { error: "timeout".into() }
}

fn found() -> CheckOutcome {
    CheckOutcome::Found { amount: "11.58677324".parse().unwrap(), dedupe_key: "binance:FUNDING_FEE:1".into(), exchange_id: "1".into(), first_seen_ms: T + 206_000 }
}

fn progress(s: Step) -> SlotRow {
    match s {
        Step::Progress(r) => r,
        Step::Settled(r) => panic!("expected progress, got settled {r:?}"),
    }
}

/// Runs the checks at the given minutes (all "not found") like the real loop would.
fn run(minutes: &[i64], outcome: impl Fn() -> CheckOutcome) -> SlotRow {
    let mut r = row();
    for m in minutes {
        r = progress(step(&r, T + m * M, outcome()));
    }
    r
}

#[test]
fn funding_slots_tracing_the_first_check_is_five_minutes_after_the_settlement() {
    assert_eq!(first_check_ms(T), T + 5 * M);
}

#[test]
fn funding_slots_tracing_stage_1_checks_every_5_minutes_up_to_30() {
    let mut r = row();
    let mut seen = vec![];
    for m in [5, 10, 15, 20, 25] {
        r = progress(step(&r, T + m * M, not_found()));
        assert_eq!(r.state, SlotState::Tracing1, "after the +{m} check");
        seen.push((r.next_check_ms - T) / M);
    }
    assert_eq!(seen, vec![10, 15, 20, 25, 30]);
}

#[test]
fn funding_slots_tracing_the_30_minute_check_moves_an_unfound_slot_to_stage_2() {
    let r = run(&[5, 10, 15, 20, 25, 30], not_found);
    assert_eq!((r.state, (r.next_check_ms - T) / M), (SlotState::Tracing2, 40));
    assert_eq!(r.manual_reason, None);
}

#[test]
fn funding_slots_tracing_stage_2_checks_every_10_minutes_up_to_60() {
    let mut r = run(&[5, 10, 15, 20, 25, 30], not_found);
    let mut seen = vec![];
    for m in [40, 50] {
        r = progress(step(&r, T + m * M, not_found()));
        assert_eq!(r.state, SlotState::Tracing2, "after the +{m} check");
        seen.push((r.next_check_ms - T) / M);
    }
    assert_eq!(seen, vec![50, 60]);
}

#[test]
fn funding_slots_tracing_unfound_at_60_minutes_is_manual_check_not_found() {
    let r = run(&[5, 10, 15, 20, 25, 30, 40, 50, 60], not_found);
    assert_eq!((r.state, r.manual_reason), (SlotState::ManualCheck, Some(ManualReason::NotFound)));
    assert_eq!((r.next_check_ms - T) / M, 120, "then the hourly sweep");
}

#[test]
fn funding_slots_tracing_found_in_stage_1_settles_with_the_ledger_facts() {
    let Step::Settled(r) = step(&row(), T + 5 * M, found()) else { panic!("not settled") };
    assert_eq!(r.state, SlotState::Settled);
    assert_eq!((r.resolution, r.amount, r.ledger_dedupe_key.as_deref(), r.first_seen_ms), (Some(Resolution::Ledger), Some("11.58677324".parse().unwrap()), Some("binance:FUNDING_FEE:1"), Some(T + 206_000)));
}

#[test]
fn funding_slots_tracing_found_in_stage_2_settles() {
    let r = run(&[5, 10, 15, 20, 25, 30], not_found);
    assert!(matches!(step(&r, T + 40 * M, found()), Step::Settled(_)));
}

#[test]
fn funding_slots_tracing_found_late_in_manual_check_settles() {
    let r = run(&[5, 10, 15, 20, 25, 30, 40, 50, 60], not_found);
    assert!(matches!(step(&r, T + 2 * 60 * M, found()), Step::Settled(_)));
}

#[test]
fn funding_slots_tracing_failed_checks_never_become_not_found() {
    // Offline for the whole of stage 2: the deadline check itself failed.
    let r = run(&[5, 10, 15, 20, 25, 30], not_found);
    let r = [40, 50, 60].iter().fold(r, |r, m| progress(step(&r, T + m * M, failed())));
    assert_eq!((r.state, r.manual_reason), (SlotState::ManualCheck, Some(ManualReason::FetchFailed)));
    assert_eq!((r.checks_ok, r.checks_failed), (6, 3));
    assert_eq!(r.last_error.as_deref(), Some("timeout"));
}

#[test]
fn funding_slots_tracing_a_failed_check_in_stage_1_keeps_the_stage_and_advances_the_schedule() {
    let r = progress(step(&row(), T + 5 * M, failed()));
    assert_eq!((r.state, (r.next_check_ms - T) / M, r.checks_failed, r.checks_ok), (SlotState::Tracing1, 10, 1, 0));
    // A later completed check clears the error.
    let r = progress(step(&r, T + 10 * M, not_found()));
    assert_eq!(r.last_error, None);
}

#[test]
fn funding_slots_tracing_a_late_check_is_judged_by_when_it_ran() {
    // The machine slept: the first check runs 80 minutes after the settlement and finds the row.
    assert!(matches!(step(&row(), T + 80 * M, found()), Step::Settled(_)));
    // …or finds nothing: straight to manual check, not back to stage 1.
    let r = progress(step(&row(), T + 80 * M, not_found()));
    assert_eq!((r.state, r.manual_reason), (SlotState::ManualCheck, Some(ManualReason::NotFound)));
    // A check 31 minutes in belongs to stage 2 and the next one is the 40 minute mark.
    let r = progress(step(&row(), T + 31 * M, not_found()));
    assert_eq!((r.state, (r.next_check_ms - T) / M), (SlotState::Tracing2, 40));
}

#[test]
fn funding_slots_tracing_a_zero_rate_with_no_row_settles_as_zero() {
    let out = CheckOutcome::NotFound { rate: RateCheck::Zero { source: "binance /fapi/v1/fundingRate".into() } };
    let Step::Settled(r) = step(&row(), T + 5 * M, out) else { panic!("not settled") };
    assert_eq!((r.resolution, r.amount, r.zero_rate_source.as_deref(), r.ledger_dedupe_key), (Some(Resolution::ZeroRate), Some(Decimal::ZERO), Some("binance /fapi/v1/fundingRate"), None));
}

#[test]
fn funding_slots_tracing_a_non_zero_or_unknown_rate_does_not_settle() {
    for rate in [RateCheck::NonZero, RateCheck::Unknown] {
        assert!(matches!(step(&row(), T + 5 * M, CheckOutcome::NotFound { rate }), Step::Progress(_)));
    }
}

#[test]
fn funding_slots_tracing_an_ambiguous_row_is_never_linked() {
    let r = progress(step(&row(), T + 5 * M, CheckOutcome::Ambiguous { detail: "also inside pair p2".into() }));
    assert_eq!((r.state, r.ledger_dedupe_key.clone()), (SlotState::Tracing1, None));
    assert!(r.last_error.unwrap().contains("ambiguous"));
    let r = run(&[5, 10, 15, 20, 25, 30, 40, 50], not_found);
    let r = progress(step(&r, T + 60 * M, CheckOutcome::Ambiguous { detail: "x".into() }));
    assert_eq!(r.manual_reason, Some(ManualReason::Ambiguous));
}

#[test]
fn funding_slots_tracing_the_sweep_is_hourly_and_stops_after_three_days() {
    let r = run(&[5, 10, 15, 20, 25, 30, 40, 50, 60], not_found);
    let r = progress(step(&r, T + 120 * M, not_found()));
    assert_eq!((r.state, (r.next_check_ms - T) / M), (SlotState::ManualCheck, 180));
    let last = progress(step(&r, T + SWEEP_END_MS - M, not_found()));
    assert_eq!(last.next_check_ms, T + SWEEP_END_MS - M + SWEEP_STEP_MS);
    let end = progress(step(&r, T + SWEEP_END_MS + M, not_found()));
    assert_eq!(end.next_check_ms, NEVER_MS, "no check is scheduled after 3 days");
}

#[test]
fn funding_slots_tracing_a_recheck_request_makes_the_slot_due_now_without_changing_its_state() {
    let r = run(&[5, 10, 15, 20, 25, 30, 40, 50, 60], not_found);
    let q = request_recheck(&r, T + 150 * M);
    assert_eq!((q.next_check_ms, q.state, q.manual_reason), (T + 150 * M, SlotState::ManualCheck, Some(ManualReason::NotFound)));
}

#[test]
fn funding_slots_tracing_stage_boundaries() {
    assert_eq!(stage_at(T, T + 29 * M + 59_000), SlotState::Tracing1);
    assert_eq!(stage_at(T, T + 30 * M), SlotState::Tracing2);
    assert_eq!(stage_at(T, T + 59 * M + 59_000), SlotState::Tracing2);
    assert_eq!(stage_at(T, T + 60 * M), SlotState::ManualCheck);
}

#[test]
fn funding_slots_tracing_delay_percentiles_use_the_nearest_rank() {
    assert_eq!(delay_percentiles(&[]), None);
    assert_eq!(delay_percentiles(&[206_000]), Some((206_000, 206_000, 206_000)));
    let twenty: Vec<i64> = (1..=20).map(|n| n * 10_000).collect();
    assert_eq!(delay_percentiles(&twenty), Some((100_000, 190_000, 200_000)));
    let shuffled = [30, 10, 20].map(|n| n * 1_000);
    assert_eq!(delay_percentiles(&shuffled), Some((20_000, 30_000, 30_000)));
}

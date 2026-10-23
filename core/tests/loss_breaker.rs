//! loss-circuit-breaker 1.1: the pure evaluation (accumulation, trailing losing run, profit
//! resets the run, unknown trips, threshold boundaries).
use std::str::FromStr;
use tong_funding_core::loss_breaker::*;
use tong_funding_core::types::Decimal;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn amounts(v: &[&str]) -> Vec<Option<Decimal>> {
    v.iter().map(|s| Some(d(s))).collect()
}

#[test]
fn empty_interval_is_zero_and_does_not_trip() {
    let s = evaluate(&[], d("15"), 3);
    assert_eq!((s.net_sum, s.consecutive, s.unknown_count, s.tripped, s.reason), (d("0"), 0, 0, false, None));
    assert_eq!(s.loss_used(), d("0"));
}

#[test]
fn three_losses_in_a_row_trip_on_the_third() {
    // spec scenario: limit 3, -0.5, -0.3, -1.2
    let two = evaluate(&amounts(&["-0.5", "-0.3"]), d("15"), 3);
    assert!(!two.tripped && two.consecutive == 2);
    let s = evaluate(&amounts(&["-0.5", "-0.3", "-1.2"]), d("15"), 3);
    assert!(s.tripped);
    assert_eq!(s.reason, Some(TripReason::ConsecutiveLosses));
    assert_eq!((s.consecutive, s.net_sum), (3, d("-2.0")));
}

#[test]
fn a_profit_resets_the_losing_run() {
    // spec scenario: -0.5, +0.2, -0.3, -1.2 with limit 3 -> trailing run is 2
    let s = evaluate(&amounts(&["-0.5", "0.2", "-0.3", "-1.2"]), d("15"), 3);
    assert!(!s.tripped, "{s:?}");
    assert_eq!(s.consecutive, 2);
}

#[test]
fn exactly_zero_also_resets_the_run_and_is_not_a_loss() {
    let s = evaluate(&amounts(&["-1", "-1", "0", "-1"]), d("15"), 3);
    assert_eq!(s.consecutive, 1);
    assert!(!s.tripped);
}

#[test]
fn the_cumulative_threshold_trips_exactly_at_the_boundary_and_not_just_before() {
    let before = evaluate(&amounts(&["-14.99"]), d("15"), 99);
    assert!(!before.tripped);
    let at = evaluate(&amounts(&["-10", "-5"]), d("15"), 99);
    assert!(at.tripped && at.reason == Some(TripReason::MaxLoss), "{at:?}");
    let past = evaluate(&amounts(&["-15.01"]), d("15"), 99);
    assert!(past.tripped && past.reason == Some(TripReason::MaxLoss));
}

#[test]
fn profits_offset_losses_in_the_cumulative_sum() {
    // -10, +8, -9 => -11, still inside 15
    let s = evaluate(&amounts(&["-10", "8", "-9"]), d("15"), 99);
    assert_eq!(s.net_sum, d("-11"));
    assert_eq!(s.loss_used(), d("11"));
    assert!(!s.tripped);
    // net positive: nothing is used
    let up = evaluate(&amounts(&["3", "-1"]), d("15"), 99);
    assert_eq!(up.net_sum, d("2"));
    assert_eq!(up.loss_used(), d("0"));
}

#[test]
fn the_consecutive_limit_trips_exactly_at_the_count() {
    assert!(!evaluate(&amounts(&["-0.1", "-0.1"]), d("15"), 3).tripped);
    assert!(evaluate(&amounts(&["-0.1", "-0.1", "-0.1"]), d("15"), 3).tripped);
    // limit 1: one loss is enough, a profit is not
    assert!(evaluate(&amounts(&["-0.01"]), d("15"), 1).tripped);
    assert!(!evaluate(&amounts(&["0.01"]), d("15"), 1).tripped);
}

#[test]
fn an_unknown_amount_trips_with_the_unknown_reason_first() {
    // spec scenario: a closed pair whose fill price cannot be had
    let s = evaluate(&[Some(d("0.5")), None], d("15"), 3);
    assert!(s.tripped);
    assert_eq!(s.reason, Some(TripReason::UnknownPnl));
    assert_eq!(s.unknown_count, 1);
    assert_eq!(s.net_sum, d("0.5"), "unknown adds nothing to the sum");
    // unknown wins over an also-exceeded cumulative loss: fail closed with the most basic reason
    let both = evaluate(&[Some(d("-20")), None], d("15"), 3);
    assert_eq!(both.reason, Some(TripReason::UnknownPnl));
}

#[test]
fn cumulative_loss_is_reported_before_the_consecutive_run_when_both_hold() {
    let s = evaluate(&amounts(&["-6", "-6", "-6"]), d("15"), 3);
    assert_eq!(s.reason, Some(TripReason::MaxLoss));
}

#[test]
fn a_lower_threshold_is_never_looser_than_a_higher_one() {
    // direction check for the risk page's "越低越嚴" badge: same history, a lower limit trips
    // whenever a higher limit does.
    let history = amounts(&["-3", "1", "-4", "-2"]);
    for (lo, hi) in [("5", "15"), ("6", "7"), ("1", "2")] {
        let (lo, hi) = (d(lo), d(hi));
        assert!(evaluate(&history, lo, 9).tripped as u8 >= evaluate(&history, hi, 9).tripped as u8);
    }
    assert!(evaluate(&history, d("5"), 9).tripped && !evaluate(&history, d("15"), 9).tripped);
    for (lo, hi) in [(1_u32, 2_u32), (2, 3), (3, 9)] {
        assert!(evaluate(&history, d("999"), lo).tripped as u8 >= evaluate(&history, d("999"), hi).tripped as u8);
    }
    assert!(evaluate(&history, d("999"), 2).tripped && !evaluate(&history, d("999"), 3).tripped);
}

#[test]
fn reason_codes_and_texts_are_stable() {
    assert_eq!(TripReason::UnknownPnl.code(), "UNKNOWN_PNL");
    assert_eq!(TripReason::MaxLoss.code(), "MAX_LOSS");
    assert_eq!(TripReason::ConsecutiveLosses.code(), "CONSECUTIVE_LOSSES");
    assert_eq!(TripReason::UnknownPnl.text(), "損益未知");
    assert_eq!(TripReason::ConsecutiveLosses.text(), "連虧達上限");
}

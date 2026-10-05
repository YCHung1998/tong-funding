use tong_funding_core::pair::{
    next, Event, IllegalTransition, ManualEvent as M, PairState as S, SystemEvent as E,
};

fn sys(e: E) -> Event {
    Event::System(e)
}
fn man(e: M) -> Event {
    Event::Manual(e)
}

/// The spec's transition table, written independently of the implementation (one entry per
/// (from-state, event) pair; multi-state rows are expanded).
fn table() -> Vec<(S, Event, S)> {
    let locked = [S::PartialFailure, S::Imbalanced, S::Unresolved];
    let in_flight = [S::OrderSubmit, S::FillMonitor, S::Closing];
    let mut t = vec![
        (S::Prepared, sys(E::StartCheck), S::PreTradeCheck),
        (S::Prepared, sys(E::Cancel), S::Cancelled),
        (S::Prepared, man(M::Cancel), S::Cancelled),
        (S::PreTradeCheck, sys(E::CheckPassed), S::OrderSubmit),
        (S::PreTradeCheck, sys(E::CheckFailed), S::Blocked),
        (S::OrderSubmit, sys(E::BothLegsSubmitted), S::FillMonitor),
        (S::OrderSubmit, sys(E::BothSubmitsFailed), S::Cancelled),
        (S::OrderSubmit, sys(E::OneLegSubmitFailed), S::PartialFailure),
        (S::FillMonitor, sys(E::FillsWithinTolerance), S::Reconciled),
        (S::FillMonitor, sys(E::FillsExceedTolerance), S::Imbalanced),
        (S::FillMonitor, sys(E::TimeoutNoFills), S::Cancelled),
        (S::FillMonitor, sys(E::TimeoutPartialFill), S::PartialFailure),
        (S::FillMonitor, sys(E::TimeoutUndetermined), S::Unresolved),
        (S::Reconciled, sys(E::ScheduledClose), S::Closing),
        (S::Reconciled, man(M::RequestClose), S::Closing),
        (S::Closing, sys(E::ClosedConfirmed { verified_flat: true }), S::Finalized),
        (S::Closing, sys(E::CloseFailed), S::PartialFailure),
        // engine-simulation: a simulated pair's ledger is gone after a restart.
        (S::Reconciled, sys(E::RestartUndetermined), S::Unresolved),
    ];
    for s in in_flight {
        t.push((s, sys(E::RestartFoundPartial), S::PartialFailure));
        t.push((s, sys(E::RestartUndetermined), S::Unresolved));
    }
    for s in locked {
        t.push((s, man(M::RequestClose), S::Closing));
        t.push((s, man(M::ConfirmClosed { verified_flat: true }), S::Finalized));
    }
    t
}

fn all_events() -> Vec<Event> {
    E::ALL
        .iter()
        .copied()
        .map(Event::System)
        .chain(M::ALL.iter().copied().map(Event::Manual))
        .collect()
}

fn run(state: S, e: Event) -> Result<S, IllegalTransition> {
    next(state, e)
}

#[test]
fn table_has_expected_row_count() {
    // 17 single rows + 3 in-flight * 2 + 3 locked * 2 = 29
    assert_eq!(table().len(), 29);
}

#[test]
fn every_table_row_succeeds() {
    for (from, ev, to) in table() {
        assert_eq!(run(from, ev), Ok(to), "{from} on {ev:?}");
    }
}

#[test]
fn everything_outside_the_table_is_rejected_exhaustively() {
    let t = table();
    let mut rejected = 0;
    for from in S::ALL {
        for ev in all_events() {
            let expected = t.iter().find(|(f, e, _)| *f == from && *e == ev).map(|r| r.2);
            match expected {
                Some(to) => assert_eq!(run(from, ev), Ok(to), "{from} on {ev:?}"),
                None => {
                    assert_eq!(
                        run(from, ev),
                        Err(IllegalTransition { from, event: ev }),
                        "{from} on {ev:?} must be rejected"
                    );
                    rejected += 1;
                }
            }
        }
    }
    assert_eq!(rejected, 12 * 24 - 29);
}

#[test]
fn no_duplicate_rows_in_oracle_table() {
    let t = table();
    for (i, a) in t.iter().enumerate() {
        for b in &t[i + 1..] {
            assert!(!(a.0 == b.0 && a.1 == b.1), "duplicate {a:?}");
        }
    }
}

#[test]
fn locked_states_reject_every_system_event() {
    let mut checked = 0;
    for from in [S::PartialFailure, S::Imbalanced, S::Unresolved] {
        for ev in E::ALL {
            let r = next(from, ev);
            assert_eq!(r, Err(IllegalTransition { from, event: Event::System(ev) }));
            checked += 1;
        }
    }
    assert_eq!(checked, 3 * E::ALL.len());
}

#[test]
fn system_event_all_is_complete_and_ordered() {
    // Exhaustive match with no wildcard: adding a variant breaks compilation here too.
    for (i, e) in E::ALL.iter().enumerate() {
        let name = match e {
            E::StartCheck | E::CheckPassed | E::CheckFailed | E::Cancel => "check",
            E::BothLegsSubmitted | E::BothSubmitsFailed | E::OneLegSubmitFailed => "submit",
            E::FillsWithinTolerance
            | E::FillsExceedTolerance
            | E::TimeoutNoFills
            | E::TimeoutPartialFill
            | E::TimeoutUndetermined => "fill",
            E::ScheduledClose | E::ClosedConfirmed { .. } | E::CloseFailed => "close",
            E::RestartFoundPartial | E::RestartUndetermined => "restart",
            E::Retry | E::Recheck => "misc",
        };
        assert!(!name.is_empty());
        assert_eq!(e.ordinal(), i);
    }
    // Both verified_flat values present.
    assert!(E::ALL.contains(&E::ClosedConfirmed { verified_flat: true }));
    assert!(E::ALL.contains(&E::ClosedConfirmed { verified_flat: false }));
}

// ---- Scenarios -------------------------------------------------------------

#[test]
fn scenario_normal_path() {
    let steps = [
        (sys(E::StartCheck), S::PreTradeCheck),
        (sys(E::CheckPassed), S::OrderSubmit),
        (sys(E::BothLegsSubmitted), S::FillMonitor),
        (sys(E::FillsWithinTolerance), S::Reconciled),
        (sys(E::ScheduledClose), S::Closing),
        (sys(E::ClosedConfirmed { verified_flat: true }), S::Finalized),
    ];
    let mut st = S::Prepared;
    for (ev, expect) in steps {
        st = next(st, ev).unwrap();
        assert_eq!(st, expect);
    }
}

#[test]
fn scenario_check_failed_then_blocked_is_terminal() {
    let st = next(S::PreTradeCheck, E::CheckFailed).unwrap();
    assert_eq!(st, S::Blocked);
    for ev in all_events() {
        assert!(next(S::Blocked, ev).is_err());
    }
}

#[test]
fn scenario_illegal_transition_rejected_state_unchanged() {
    let err = next(S::Prepared, E::FillsWithinTolerance).unwrap_err();
    assert_eq!(err.from, S::Prepared);
    assert_eq!(err.event, sys(E::FillsWithinTolerance));
    // Pure: calling again from the same state gives the same answer.
    assert!(next(S::Prepared, E::FillsWithinTolerance).is_err());
}

#[test]
fn scenario_reconciled_rejects_both_legs_submitted() {
    assert!(next(S::Reconciled, E::BothLegsSubmitted).is_err());
}

#[test]
fn scenario_both_submits_failed_cancels() {
    assert_eq!(next(S::OrderSubmit, E::BothSubmitsFailed), Ok(S::Cancelled));
}

#[test]
fn scenario_imbalance_goes_to_imbalanced() {
    assert_eq!(next(S::FillMonitor, E::FillsExceedTolerance), Ok(S::Imbalanced));
}

#[test]
fn scenario_close_failure_goes_partial() {
    assert_eq!(next(S::Closing, E::CloseFailed), Ok(S::PartialFailure));
}

#[test]
fn scenario_restart_single_leg() {
    assert_eq!(next(S::FillMonitor, E::RestartFoundPartial), Ok(S::PartialFailure));
    assert_eq!(next(S::OrderSubmit, E::RestartUndetermined), Ok(S::Unresolved));
    assert_eq!(next(S::Reconciled, E::RestartUndetermined), Ok(S::Unresolved));
    assert!(next(S::Reconciled, E::RestartFoundPartial).is_err(), "only 'undetermined' leaves RECONCILED on restart");
}

#[test]
fn scenario_manual_confirm_without_verification_is_rejected() {
    for from in [S::PartialFailure, S::Imbalanced, S::Unresolved] {
        let r = next(from, M::ConfirmClosed { verified_flat: false });
        assert!(r.is_err());
        // state "stays": caller keeps `from`; verify retry with verification succeeds.
        assert_eq!(next(from, M::ConfirmClosed { verified_flat: true }), Ok(S::Finalized));
    }
}

#[test]
fn scenario_one_leg_failed_goes_partial() {
    assert_eq!(next(S::OrderSubmit, E::OneLegSubmitFailed), Ok(S::PartialFailure));
}

#[test]
fn scenario_system_events_cannot_leave_partial_failure() {
    for ev in [
        E::TimeoutNoFills,
        E::TimeoutPartialFill,
        E::TimeoutUndetermined,
        E::Retry,
        E::Recheck,
        E::ClosedConfirmed { verified_flat: true },
        E::ScheduledClose,
    ] {
        assert!(next(S::PartialFailure, ev).is_err(), "{ev:?}");
    }
}

#[test]
fn scenario_manual_request_close_leaves_partial_failure() {
    assert_eq!(next(S::PartialFailure, M::RequestClose), Ok(S::Closing));
}

#[test]
fn scenario_timeout_splits_by_fill_status() {
    assert_eq!(next(S::FillMonitor, E::TimeoutNoFills), Ok(S::Cancelled));
    assert_eq!(next(S::FillMonitor, E::TimeoutPartialFill), Ok(S::PartialFailure));
    assert_eq!(next(S::FillMonitor, E::TimeoutUndetermined), Ok(S::Unresolved));
}

#[test]
fn scenario_finalized_requires_flat_confirmation() {
    assert!(next(S::Closing, E::ClosedConfirmed { verified_flat: false }).is_err());
    assert_eq!(next(S::Closing, E::ClosedConfirmed { verified_flat: true }), Ok(S::Finalized));
}

#[test]
fn finalized_only_reachable_with_flat_confirmation() {
    for from in S::ALL {
        for ev in all_events() {
            if next(from, ev) == Ok(S::Finalized) {
                let flat = matches!(
                    ev,
                    Event::System(E::ClosedConfirmed { verified_flat: true })
                        | Event::Manual(M::ConfirmClosed { verified_flat: true })
                );
                assert!(flat, "{from} on {ev:?} reached FINALIZED without verification");
            }
        }
    }
}

#[test]
fn terminal_states_have_no_exit() {
    for from in [S::Blocked, S::Cancelled, S::Finalized] {
        assert!(from.is_terminal());
        for ev in all_events() {
            assert!(next(from, ev).is_err(), "{from} on {ev:?}");
        }
    }
}

#[test]
fn only_manual_events_leave_locked_states() {
    for from in S::ALL.into_iter().filter(|s| s.is_locked()) {
        for ev in all_events() {
            if next(from, ev).is_ok() {
                assert!(matches!(ev, Event::Manual(_)), "{from} left by {ev:?}");
            }
        }
    }
}

// ---- String mapping --------------------------------------------------------

#[test]
fn state_strings_match_spec_names_and_roundtrip() {
    let expected = [
        (S::Prepared, "PREPARED"),
        (S::PreTradeCheck, "PRE_TRADE_CHECK"),
        (S::Blocked, "BLOCKED"),
        (S::OrderSubmit, "ORDER_SUBMIT"),
        (S::FillMonitor, "FILL_MONITOR"),
        (S::Reconciled, "RECONCILED"),
        (S::Imbalanced, "IMBALANCED"),
        (S::Closing, "CLOSING"),
        (S::Finalized, "FINALIZED"),
        (S::Cancelled, "CANCELLED"),
        (S::PartialFailure, "PARTIAL_FAILURE"),
        (S::Unresolved, "UNRESOLVED"),
    ];
    assert_eq!(expected.len(), S::ALL.len());
    for (st, s) in expected {
        assert_eq!(st.as_str(), s);
        assert_eq!(s.parse::<S>(), Ok(st));
        assert_eq!(st.to_string(), s);
    }
    assert!("partial_failure".parse::<S>().is_err());
    assert!("".parse::<S>().is_err());
}

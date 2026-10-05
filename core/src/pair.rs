//! Pair lifecycle state machine: 12 states, one pure transition function `next`.
//!
//! "Locked" states (`PartialFailure`, `Imbalanced`, `Unresolved`) can only be left by a
//! [`ManualEvent`]; no [`SystemEvent`] is ever accepted there (spec: pair-lifecycle).

use thiserror::Error;

/// The closed set of pair states; names round-trip with the Python-era strings via [`PairState::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairState {
    Prepared,
    PreTradeCheck,
    Blocked,
    OrderSubmit,
    FillMonitor,
    Reconciled,
    Imbalanced,
    Closing,
    Finalized,
    Cancelled,
    PartialFailure,
    Unresolved,
}

impl PairState {
    /// Every state, for exhaustive tests and UI enumeration.
    pub const ALL: [PairState; 12] = [
        PairState::Prepared,
        PairState::PreTradeCheck,
        PairState::Blocked,
        PairState::OrderSubmit,
        PairState::FillMonitor,
        PairState::Reconciled,
        PairState::Imbalanced,
        PairState::Closing,
        PairState::Finalized,
        PairState::Cancelled,
        PairState::PartialFailure,
        PairState::Unresolved,
    ];

    /// The canonical upper-snake string used in logs and the database.
    pub const fn as_str(self) -> &'static str {
        match self {
            PairState::Prepared => "PREPARED",
            PairState::PreTradeCheck => "PRE_TRADE_CHECK",
            PairState::Blocked => "BLOCKED",
            PairState::OrderSubmit => "ORDER_SUBMIT",
            PairState::FillMonitor => "FILL_MONITOR",
            PairState::Reconciled => "RECONCILED",
            PairState::Imbalanced => "IMBALANCED",
            PairState::Closing => "CLOSING",
            PairState::Finalized => "FINALIZED",
            PairState::Cancelled => "CANCELLED",
            PairState::PartialFailure => "PARTIAL_FAILURE",
            PairState::Unresolved => "UNRESOLVED",
        }
    }

    /// Terminal states have no outgoing transition.
    pub const fn is_terminal(self) -> bool {
        matches!(self, PairState::Blocked | PairState::Cancelled | PairState::Finalized)
    }

    /// States that only a human event can leave (single-leg failure, imbalance, unknown).
    pub const fn is_locked(self) -> bool {
        matches!(
            self,
            PairState::PartialFailure | PairState::Imbalanced | PairState::Unresolved
        )
    }
}

impl std::fmt::Display for PairState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned when parsing a string that is not one of the 12 state names.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown pair state: {0:?}")]
pub struct UnknownPairState(pub String);

impl std::str::FromStr for PairState {
    type Err = UnknownPairState;

    /// Parses the exact canonical string (case-sensitive).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PairState::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| UnknownPairState(s.to_string()))
    }
}

/// The "PnL computed" confirmation that entering `FINALIZED` needs (funding-pnl, pair-lifecycle
/// MODIFIED "FINALIZED 須確認已平倉"). `Recorded` points at a `PAIR_PNL_COMPUTED` /
/// `PAIR_PNL_RECOMPUTED` event, whether that result is COMPLETE or INCOMPLETE. A SIMULATION pair
/// never gets a PnL (pnl-accounting: no real fills, no funding ledger), so for it the confirmation
/// is the explicit `NotApplicableSimulated`; the engine gives it only to simulated pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PnlGate {
    /// No PnL event exists: entering FINALIZED is refused.
    Missing,
    /// A PnL event of the pair exists (COMPLETE or INCOMPLETE).
    Recorded,
    /// The pair ran in SIMULATION: there is no actual PnL by design.
    NotApplicableSimulated,
}

impl PnlGate {
    pub const ALL: [PnlGate; 3] = [PnlGate::Missing, PnlGate::Recorded, PnlGate::NotApplicableSimulated];

    /// True when the confirmation allows FINALIZED.
    pub const fn is_satisfied(self) -> bool {
        match self {
            PnlGate::Missing => false,
            PnlGate::Recorded | PnlGate::NotApplicableSimulated => true,
        }
    }
}

/// Events produced automatically by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    /// Pre-trade check begins.
    StartCheck,
    /// Pre-trade check passed.
    CheckPassed,
    /// Pre-trade check failed.
    CheckFailed,
    /// Entry cancelled by the engine (conditions worsened / entry window missed).
    Cancel,
    /// Both legs submitted.
    BothLegsSubmitted,
    /// Both legs failed to submit (no exposure).
    BothSubmitsFailed,
    /// One leg failed to submit while the other was submitted.
    OneLegSubmitFailed,
    /// Both legs filled, imbalance within tolerance.
    FillsWithinTolerance,
    /// Both legs filled, imbalance beyond tolerance.
    FillsExceedTolerance,
    /// Fill timeout, both legs filled zero.
    TimeoutNoFills,
    /// Fill timeout, one leg filled and the other not completely.
    TimeoutPartialFill,
    /// Fill timeout, fill status cannot be determined.
    TimeoutUndetermined,
    /// Scheduled close begins.
    ScheduledClose,
    /// Close finished; `verified_flat` = both legs at zero position and no open orders; `pnl` =
    /// the "PnL computed" confirmation (funding-pnl). Both are needed for FINALIZED.
    ClosedConfirmed { verified_flat: bool, pnl: PnlGate },
    /// A leg failed to close, or closed only partially.
    CloseFailed,
    /// Restart reconciliation found a single filled leg / partial state.
    RestartFoundPartial,
    /// Restart reconciliation could not determine the state.
    RestartUndetermined,
    /// Auto-retry (accepted by no state; present so exhaustive tests cover it).
    Retry,
    /// Re-check (accepted by no state; present so exhaustive tests cover it).
    Recheck,
}

impl SystemEvent {
    /// Every system event (every `verified_flat` x `pnl` combination); `ALL[i].ordinal() == i` is
    /// checked at compile time.
    pub const ALL: [SystemEvent; 24] = [
        SystemEvent::StartCheck,
        SystemEvent::CheckPassed,
        SystemEvent::CheckFailed,
        SystemEvent::Cancel,
        SystemEvent::BothLegsSubmitted,
        SystemEvent::BothSubmitsFailed,
        SystemEvent::OneLegSubmitFailed,
        SystemEvent::FillsWithinTolerance,
        SystemEvent::FillsExceedTolerance,
        SystemEvent::TimeoutNoFills,
        SystemEvent::TimeoutPartialFill,
        SystemEvent::TimeoutUndetermined,
        SystemEvent::ScheduledClose,
        SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::Missing },
        SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::Recorded },
        SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::NotApplicableSimulated },
        SystemEvent::ClosedConfirmed { verified_flat: false, pnl: PnlGate::Missing },
        SystemEvent::ClosedConfirmed { verified_flat: false, pnl: PnlGate::Recorded },
        SystemEvent::ClosedConfirmed { verified_flat: false, pnl: PnlGate::NotApplicableSimulated },
        SystemEvent::CloseFailed,
        SystemEvent::RestartFoundPartial,
        SystemEvent::RestartUndetermined,
        SystemEvent::Retry,
        SystemEvent::Recheck,
    ];

    /// Exhaustive slot index: adding a variant stops compilation here until it gets a slot and an `ALL` entry.
    pub const fn ordinal(self) -> usize {
        match self {
            SystemEvent::StartCheck => 0,
            SystemEvent::CheckPassed => 1,
            SystemEvent::CheckFailed => 2,
            SystemEvent::Cancel => 3,
            SystemEvent::BothLegsSubmitted => 4,
            SystemEvent::BothSubmitsFailed => 5,
            SystemEvent::OneLegSubmitFailed => 6,
            SystemEvent::FillsWithinTolerance => 7,
            SystemEvent::FillsExceedTolerance => 8,
            SystemEvent::TimeoutNoFills => 9,
            SystemEvent::TimeoutPartialFill => 10,
            SystemEvent::TimeoutUndetermined => 11,
            SystemEvent::ScheduledClose => 12,
            SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::Missing } => 13,
            SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::Recorded } => 14,
            SystemEvent::ClosedConfirmed { verified_flat: true, pnl: PnlGate::NotApplicableSimulated } => 15,
            SystemEvent::ClosedConfirmed { verified_flat: false, pnl: PnlGate::Missing } => 16,
            SystemEvent::ClosedConfirmed { verified_flat: false, pnl: PnlGate::Recorded } => 17,
            SystemEvent::ClosedConfirmed { verified_flat: false, pnl: PnlGate::NotApplicableSimulated } => 18,
            SystemEvent::CloseFailed => 19,
            SystemEvent::RestartFoundPartial => 20,
            SystemEvent::RestartUndetermined => 21,
            SystemEvent::Retry => 22,
            SystemEvent::Recheck => 23,
        }
    }
}

const _: () = {
    let mut i = 0;
    while i < SystemEvent::ALL.len() {
        assert!(SystemEvent::ALL[i].ordinal() == i);
        i += 1;
    }
};

/// Events that can only originate from a user action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualEvent {
    /// User removes / cancels a prepared pair.
    Cancel,
    /// User asks to close the pair.
    RequestClose,
    /// User confirms the pair is closed; `verified_flat` = both legs verified at zero and no open
    /// orders; `pnl` = the "PnL computed" confirmation (funding-pnl), needed as for the system path.
    ConfirmClosed { verified_flat: bool, pnl: PnlGate },
}

impl ManualEvent {
    /// Every manual event (every `verified_flat` x `pnl` combination).
    pub const ALL: [ManualEvent; 8] = [
        ManualEvent::Cancel,
        ManualEvent::RequestClose,
        ManualEvent::ConfirmClosed { verified_flat: true, pnl: PnlGate::Missing },
        ManualEvent::ConfirmClosed { verified_flat: true, pnl: PnlGate::Recorded },
        ManualEvent::ConfirmClosed { verified_flat: true, pnl: PnlGate::NotApplicableSimulated },
        ManualEvent::ConfirmClosed { verified_flat: false, pnl: PnlGate::Missing },
        ManualEvent::ConfirmClosed { verified_flat: false, pnl: PnlGate::Recorded },
        ManualEvent::ConfirmClosed { verified_flat: false, pnl: PnlGate::NotApplicableSimulated },
    ];

    /// Exhaustive slot index, same purpose as [`SystemEvent::ordinal`].
    pub const fn ordinal(self) -> usize {
        match self {
            ManualEvent::Cancel => 0,
            ManualEvent::RequestClose => 1,
            ManualEvent::ConfirmClosed { verified_flat: true, pnl: PnlGate::Missing } => 2,
            ManualEvent::ConfirmClosed { verified_flat: true, pnl: PnlGate::Recorded } => 3,
            ManualEvent::ConfirmClosed { verified_flat: true, pnl: PnlGate::NotApplicableSimulated } => 4,
            ManualEvent::ConfirmClosed { verified_flat: false, pnl: PnlGate::Missing } => 5,
            ManualEvent::ConfirmClosed { verified_flat: false, pnl: PnlGate::Recorded } => 6,
            ManualEvent::ConfirmClosed { verified_flat: false, pnl: PnlGate::NotApplicableSimulated } => 7,
        }
    }
}

const _: () = {
    let mut i = 0;
    while i < ManualEvent::ALL.len() {
        assert!(ManualEvent::ALL[i].ordinal() == i);
        i += 1;
    }
};

/// An input to [`next`], typed by origin so system and manual events cannot be confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    System(SystemEvent),
    Manual(ManualEvent),
}

impl From<SystemEvent> for Event {
    fn from(e: SystemEvent) -> Self {
        Event::System(e)
    }
}

impl From<ManualEvent> for Event {
    fn from(e: ManualEvent) -> Self {
        Event::Manual(e)
    }
}

/// The event is not allowed from `from`; the caller's state is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("illegal pair transition: {from} on {event:?}")]
pub struct IllegalTransition {
    pub from: PairState,
    pub event: Event,
}

/// The single pure transition function; only the spec's closed transition table succeeds.
pub fn next(state: PairState, event: impl Into<Event>) -> Result<PairState, IllegalTransition> {
    let event = event.into();
    use Event::{Manual, System};
    use PairState as S;
    let to = match (state, event) {
        (S::Prepared, System(SystemEvent::StartCheck)) => Some(S::PreTradeCheck),
        (S::Prepared, System(SystemEvent::Cancel)) | (S::Prepared, Manual(ManualEvent::Cancel)) => {
            Some(S::Cancelled)
        }
        (S::PreTradeCheck, System(SystemEvent::CheckPassed)) => Some(S::OrderSubmit),
        (S::PreTradeCheck, System(SystemEvent::CheckFailed)) => Some(S::Blocked),
        (S::OrderSubmit, System(SystemEvent::BothLegsSubmitted)) => Some(S::FillMonitor),
        (S::OrderSubmit, System(SystemEvent::BothSubmitsFailed)) => Some(S::Cancelled),
        (S::OrderSubmit, System(SystemEvent::OneLegSubmitFailed)) => Some(S::PartialFailure),
        (S::FillMonitor, System(SystemEvent::FillsWithinTolerance)) => Some(S::Reconciled),
        (S::FillMonitor, System(SystemEvent::FillsExceedTolerance)) => Some(S::Imbalanced),
        (S::FillMonitor, System(SystemEvent::TimeoutNoFills)) => Some(S::Cancelled),
        (S::FillMonitor, System(SystemEvent::TimeoutPartialFill)) => Some(S::PartialFailure),
        (S::FillMonitor, System(SystemEvent::TimeoutUndetermined)) => Some(S::Unresolved),
        (S::Reconciled, System(SystemEvent::ScheduledClose))
        | (S::Reconciled, Manual(ManualEvent::RequestClose)) => Some(S::Closing),
        // FINALIZED needs both confirmations: flat AND "PnL computed" (funding-pnl).
        (S::Closing, System(SystemEvent::ClosedConfirmed { verified_flat: true, pnl })) if pnl.is_satisfied() => {
            Some(S::Finalized)
        }
        (S::Closing, System(SystemEvent::CloseFailed)) => Some(S::PartialFailure),
        (S::OrderSubmit | S::FillMonitor | S::Closing, System(SystemEvent::RestartFoundPartial)) => {
            Some(S::PartialFailure)
        }
        (S::OrderSubmit | S::FillMonitor | S::Closing, System(SystemEvent::RestartUndetermined)) => {
            Some(S::Unresolved)
        }
        // A simulated pair loses its in-memory ledger on restart (engine-simulation decision 7).
        (S::Reconciled, System(SystemEvent::RestartUndetermined)) => Some(S::Unresolved),
        (S::PartialFailure | S::Imbalanced | S::Unresolved, Manual(ManualEvent::RequestClose)) => {
            Some(S::Closing)
        }
        (
            S::PartialFailure | S::Imbalanced | S::Unresolved,
            Manual(ManualEvent::ConfirmClosed { verified_flat: true, pnl }),
        ) if pnl.is_satisfied() => Some(S::Finalized),
        _ => None,
    };
    to.ok_or(IllegalTransition { from: state, event })
}

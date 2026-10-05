//! `Command` (what the UI and the scheduler ask for), `Event` (results sent back to the actor by
//! spawned I/O tasks) and `Snapshot` (read-only copy for the UI). Design D2, D5; task 1.1.

use serde_json::Value;
use tong_funding_core::pair::PairState;
use tong_funding_core::risk::{ExecutionMode, TriggerMode};
use tong_funding_core::types::{Decimal, Exchange};

use super::ports::{FreshQuote, Leg, OrderAction, OrderRules, OrderSide, QueryOutcome, SubmitOutcome};

/// Engine-internal id of a pair (`pairs.internal_uuid`).
pub type PairUuid = String;

/// A new PREPARED pair as chosen by the user or the scanner.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPreparedPair {
    pub internal_uuid: PairUuid,
    pub pair_id: String,
    pub symbol: String,
    pub long_exchange: Exchange,
    pub short_exchange: Exchange,
    /// Settlement time `T` (earlier `next_funding_time` of the two legs), fixed at creation (D10).
    pub settlement_ms: i64,
    /// Scan-time snapshot kept in `pairs.entry_json` (prices, Net Edge, notional, leverage).
    pub entry: Value,
}

/// A manual order from the manual order page; goes through the same `Executor` (one order path).
#[derive(Debug, Clone, PartialEq)]
pub struct ManualOrder {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: Decimal,
    pub reduce_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Scheduler heartbeat (internal sender). Never opens exposure by itself.
    Tick,
    AddPrepared(NewPreparedPair),
    /// Scheduler-internal entry trigger for a PREPARED pair whose entry time has come.
    EntryTrigger { pair: PairUuid },
    /// User pressed "enter now" for a PREPARED pair.
    ManualEnter { pair: PairUuid },
    ManualOrder(ManualOrder),
    /// Scheduler-internal exit at `T + exit_delay_ms`.
    AutoExit { pair: PairUuid },
    ManualExit { pair: PairUuid },
    /// Manual close of a pair in PARTIAL_FAILURE / IMBALANCED / UNRESOLVED.
    ManualClose { pair: PairUuid },
    /// User confirms both legs are flat (`verified_flat` = they checked positions and orders).
    ConfirmClosed { pair: PairUuid, verified_flat: bool },
    CancelPrepared { pair: PairUuid, reason: String },
    SetTriggerMode(TriggerMode),
    SetExecutionMode(ExecutionMode),
    /// New global risk config / overrides as JSON (validated by `core::risk` before use).
    UpdateConfig { key: String, value: Value },
    SetKillSwitch { on: bool },
}

impl Command {
    /// Whether this command can open or increase exposure (design D5). The kill switch, an
    /// unfinished reconciliation and a halted store refuse exactly the commands for which this is
    /// true. Exhaustive on purpose: no wildcard arm (guarded by a source-scan test below).
    pub fn opens_exposure(&self) -> bool {
        match self {
            Command::EntryTrigger { .. } | Command::ManualEnter { .. } => true,
            Command::ManualOrder(o) => !o.reduce_only,
            Command::Tick
            | Command::AddPrepared(_)
            | Command::AutoExit { .. }
            | Command::ManualExit { .. }
            | Command::ManualClose { .. }
            | Command::ConfirmClosed { .. }
            | Command::CancelPrepared { .. }
            | Command::SetTriggerMode(_)
            | Command::SetExecutionMode(_)
            | Command::UpdateConfig { .. }
            | Command::SetKillSwitch { .. } => false,
        }
    }
}

/// Results sent back to the actor by spawned tasks. Only the actor changes state when it
/// handles one of these (engine-core spec: "結果回送後才改變狀態").
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    BaselineFetched { pair: PairUuid, long: Result<FreshQuote, String>, short: Result<FreshQuote, String> },
    PretradeFetched { pair: PairUuid, long: Result<FreshQuote, String>, short: Result<FreshQuote, String> },
    MarginFetched { pair: PairUuid, long: Result<Decimal, String>, short: Result<Decimal, String> },
    Submitted { pair: PairUuid, leg: Leg, action: OrderAction, client_order_id: String, outcome: SubmitOutcome },
    Queried { pair: Option<PairUuid>, client_order_id: String, outcome: QueryOutcome },
    /// Manual order (not part of a pair) finished.
    ManualSubmitted { client_order_id: String, outcome: SubmitOutcome },
    /// Node 0 / Node 1 context fetched with the pre-trade prices: order rules per leg and whether
    /// the leg's symbol already carries a position or open order that is not this pair's
    /// (`Err` = could not be read; treated as foreign exposure, fail closed).
    EntryContextFetched {
        pair: PairUuid,
        long_rules: Result<OrderRules, String>,
        short_rules: Result<OrderRules, String>,
        long_foreign: Result<bool, String>,
        short_foreign: Result<bool, String>,
    },
    /// Periodic re-fetch for the PREPARED auto-cancel evaluation (AUTO only).
    RecheckFetched { pair: PairUuid, long: Result<FreshQuote, String>, short: Result<FreshQuote, String> },
    /// Read before closing: signed positions (exchange order unit) of both legs' symbol, and the
    /// pair's recorded fill per leg (sum of its open orders' filled quantity, looked up by
    /// `client_order_id`; `Err` = not known). Close quantity = min(recorded, |position|).
    /// `*_reference`: a fresh single-symbol refetch per leg made right before the reduce-only
    /// closes are sent: the close reference price (funding-pnl gap 2). `Err` never holds the close
    /// back; the close then has no reference (its slippage stays "無參考價").
    ClosePositionsFetched {
        pair: PairUuid,
        long: Result<Decimal, String>,
        short: Result<Decimal, String>,
        long_recorded: Result<Decimal, String>,
        short_recorded: Result<Decimal, String>,
        long_reference: Result<FreshQuote, String>,
        short_reference: Result<FreshQuote, String>,
    },
    /// Closed-confirmation read: `Ok(true)` = both legs' positions are 0 and no open order is
    /// left on the symbol (complete lists only).
    FlatChecked { pair: PairUuid, flat: Result<bool, String> },
    /// The startup reconciler finished (`ports::StartupReconciler`).
    ReconciliationDone { result: Result<(), String> },
    /// Fill timeout: an own, not completely filled order was cancelled (`cancel` = what the cancel
    /// call said) and then looked up again (`after`): the final fill decides (fill-confirmation spec).
    CancelChecked { pair: PairUuid, client_order_id: String, cancel: QueryOutcome, after: QueryOutcome },
    /// "Confirm closed" re-query of both legs (partial-failure-alerting spec): positions and
    /// open orders on the pair's symbol, read again from the account (never the user's word).
    ConfirmChecked { pair: PairUuid, result: Result<FlatReport, String> },
}

/// Both legs' signed positions and open-order counts on the pair's symbol (complete lists only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatReport {
    pub positions: [Decimal; 2],
    pub open_orders: [usize; 2],
}

impl FlatReport {
    pub fn is_flat(&self) -> bool {
        self.positions.iter().all(|p| p.is_zero()) && self.open_orders.iter().all(|n| *n == 0)
    }
}

/// Banner data for a pair in PARTIAL_FAILURE / IMBALANCED / UNRESOLVED, derived from its state
/// (partial-failure-alerting spec). Present until a manual command moves the pair on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub pair: PairUuid,
    pub pair_id: String,
    pub symbol: String,
    pub state: PairState,
    pub simulated: bool,
    /// `AlertReason::as_str` of the current entry, once its `PAIR_ALERT` was written.
    pub reason: Option<String>,
}

/// Read-only view of one pair for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct PairView {
    pub internal_uuid: PairUuid,
    pub pair_id: String,
    pub symbol: String,
    pub long_exchange: Exchange,
    pub short_exchange: Exchange,
    pub state: PairState,
    pub settlement_ms: i64,
    pub simulated: bool,
}

/// Why the engine refuses exposure-opening commands right now (shown in the UI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    KillSwitch,
    /// Kill switch could not be read: treated as halted.
    KillSwitchUnreadable(String),
    StoreHalted(String),
    /// Startup reconciliation of unfinished intents has not completed (crash-recovery spec).
    ReconciliationPending(String),
}

/// Something the user must be told about (shown as a banner until it no longer applies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Machine-readable kind; equals the event type written for it (e.g. `EXECUTION_MODE_FALLBACK`).
    pub code: String,
    /// Human-readable text for the user.
    pub message: String,
}

/// A read-only copy; contains nothing that can change engine state.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub now_ms: i64,
    pub trigger_mode: TriggerMode,
    pub execution_mode: ExecutionMode,
    pub pairs: Vec<PairView>,
    pub blockers: Vec<Blocker>,
    /// Latest known prices by (exchange, symbol), merged from the market `watch` channel.
    pub prices: Vec<(Exchange, String, Decimal)>,
    /// User notices (additive), e.g. the startup fallback from EXCHANGE_DEMO to SIMULATION.
    pub notices: Vec<Notice>,
    /// One per pair in an alert state (derived from the pairs; survives restarts).
    pub alerts: Vec<Alert>,
}

/// Reply to a command (sent on the command's oneshot, if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReply {
    Accepted,
    /// Same symbol already has a PREPARED pair (atomic add).
    AlreadyPending,
    Rejected(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual(reduce_only: bool) -> Command {
        Command::ManualOrder(ManualOrder {
            exchange: Exchange::Binance,
            symbol: "BTCUSDT".into(),
            side: OrderSide::Sell,
            quantity: Decimal::new(1, 3),
            reduce_only,
        })
    }

    #[test]
    fn manual_order_is_classified_by_reduce_only() {
        assert!(!manual(true).opens_exposure(), "reduce-only can only shrink a position");
        assert!(manual(false).opens_exposure(), "a plain order may open or add");
    }

    #[test]
    fn every_variant_has_the_design_d5_classification() {
        let p = || "u1".to_string();
        let opens = [Command::EntryTrigger { pair: p() }, Command::ManualEnter { pair: p() }];
        let not = [
            Command::Tick,
            Command::AddPrepared(NewPreparedPair {
                internal_uuid: p(),
                pair_id: "p".into(),
                symbol: "BTCUSDT".into(),
                long_exchange: Exchange::Binance,
                short_exchange: Exchange::Bybit,
                settlement_ms: 0,
                entry: serde_json::json!({}),
            }),
            Command::AutoExit { pair: p() },
            Command::ManualExit { pair: p() },
            Command::ManualClose { pair: p() },
            Command::ConfirmClosed { pair: p(), verified_flat: true },
            Command::CancelPrepared { pair: p(), reason: "r".into() },
            Command::SetTriggerMode(TriggerMode::Auto),
            Command::SetExecutionMode(ExecutionMode::ExchangeDemo),
            Command::UpdateConfig { key: "risk".into(), value: serde_json::json!({}) },
            Command::SetKillSwitch { on: false },
        ];
        for c in &opens {
            assert!(c.opens_exposure(), "{c:?} must open exposure");
        }
        for c in &not {
            assert!(!c.opens_exposure(), "{c:?} must not open exposure");
        }
    }

    /// The compile-time guarantee ("a new variant must be classified") comes from rustc's
    /// exhaustiveness check, which only holds while `opens_exposure` has no wildcard arm. This
    /// test keeps it that way. Chosen over trybuild (needs a library crate; `app` is a binary)
    /// and `clippy::wildcard_enum_match_arm` (only runs under clippy).
    #[test]
    fn opens_exposure_has_no_wildcard_arm() {
        let src = include_str!("command.rs");
        let start = src.find("pub fn opens_exposure").expect("function present");
        let body_start = start + src[start..].find('{').unwrap();
        let mut depth = 0usize;
        let mut end = body_start;
        for (i, ch) in src[body_start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[body_start..end];
        assert!(body.contains("match self"), "must be an explicit match:\n{body}");
        let compact: String = body.split_whitespace().collect();
        assert!(!compact.contains("_=>") && !compact.contains("|_|"), "wildcard arm found:\n{body}");
        assert!(!compact.contains("..=>"), "rest pattern hides variants:\n{body}");
    }
}

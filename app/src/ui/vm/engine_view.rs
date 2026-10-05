//! Engine `Snapshot` → page data (ui-trading-pages, composition root). Pure: the composition root
//! forwards every new engine snapshot as [`SourceUpdate::Engine`]; the pages never touch the
//! engine except through [`super::bridge::CommandSink`].

use super::bridge::{EngineState, PairInfo};
use crate::engine::command::{Blocker, Snapshot};
use crate::engine::gate;
use tong_funding_core::pair::PairState;

/// A copy of the engine snapshot (prices are not needed: the pages read the market feed).
pub fn engine_state(s: &Snapshot) -> EngineState {
    EngineState {
        now_ms: s.now_ms,
        trigger_mode: s.trigger_mode,
        execution_mode: s.execution_mode,
        pairs: s.pairs.clone(),
        blockers: s.blockers.clone(),
        notices: s.notices.clone(),
        alerts: s.alerts.clone(),
    }
}

/// The engine's open pairs as the read-only pages know them (closed ones are not shown, like the
/// store mapping `live::pair_infos`).
pub fn pair_infos(e: &EngineState) -> Vec<PairInfo> {
    e.pairs
        .iter()
        .filter(|p| match p.state {
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
        })
        .map(|p| PairInfo { pair_id: p.pair_id.clone(), symbol: p.symbol.clone(), long_exchange: p.long_exchange, short_exchange: p.short_exchange, state: Ok(p.state) })
        .collect()
}

/// The blockers that refuse exposure in the engine's current mode: the engine's own gate rule
/// (`gate::refusing`), so the pages disable exactly what the engine would refuse.
pub fn refusing_blockers(e: &EngineState) -> Vec<Blocker> {
    gate::refusing(&e.blockers, e.execution_mode)
}

/// User text for a blocker.
pub fn blocker_text(b: &Blocker) -> String {
    match b {
        Blocker::KillSwitch => "緊急停止中".into(),
        Blocker::KillSwitchUnreadable(r) => format!("系統停機中（kill switch 無法讀取：{r}）"),
        Blocker::StoreHalted(r) => format!("系統停機中（{r}）"),
        Blocker::ReconciliationPending(r) => format!("重啟對帳未完成（{r}）"),
    }
}

#[cfg(test)]
use super::bridge::{apply_update, CommandOutcome, SourceUpdate, UiSnapshot, MAX_REPLIES};

#[cfg(test)]
#[path = "engine_view_tests.rs"]
mod tests;

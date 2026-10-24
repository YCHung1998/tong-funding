//! What a page may say about an exchange the risk settings left unticked (change ui-plain-language,
//! review fix). ONE rule for every page, so none of them can fold real exposure into a quiet line:
//!
//! - `Quiet`: the latest read succeeded, found no position, the position list is complete and no
//!   unfinished pair has a leg there. Only then is the exchange one muted line.
//! - `Unconfirmed`: the read failed, is missing or still loading, and no unfinished pair needs the
//!   exchange. One line in the warning tone: "無法確認持倉", never "沒有持倉".
//! - `Exposed`: it holds positions, its position list is incomplete, or an unfinished pair has a leg
//!   there. The full card, list or row stays, with the exposure note.
//!
//! "Unfinished" is every pair state but `FINALIZED`, `CANCELLED` and `BLOCKED` (the terminal ones); a
//! state that cannot be read counts as unfinished (unknown is an anomaly). Pure: no clock, no GPUI.

use std::collections::BTreeSet;
use std::str::FromStr;

use tong_funding_core::pair::PairState;
use tong_funding_core::types::Exchange;

use super::bridge::{AccountState, UiSnapshot};
use super::labels::plain_error;

/// How much of an error message the one visible line carries (the rest stays behind the line).
const REASON_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledStatus {
    /// Latest read succeeded and shows nothing: the quiet muted line.
    Quiet,
    /// Positions cannot be confirmed; `reason` is the short cause for the warning line.
    Unconfirmed { reason: String },
    /// Positions, an incomplete list or an unfinished pair: nothing is folded away.
    Exposed,
}

/// Symbols of the unfinished pairs (engine snapshot and stored records) with a leg on `e`, in order,
/// without repeats. A stored record that names no exchange could be on any of them and counts.
pub fn unfinished_pair_symbols(snap: &UiSnapshot, e: Exchange) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut add = |id: &str, symbol: &str| {
        if seen.insert(id.to_string()) {
            out.push(symbol.to_string());
        }
    };
    for p in &snap.pairs {
        let open = p.state.as_ref().map_or(true, |s| !s.is_terminal());
        if open && (p.long_exchange == e || p.short_exchange == e) {
            add(&p.pair_id, &p.symbol);
        }
    }
    for r in snap.pair_records.values() {
        let open = PairState::from_str(&r.status).map_or(true, |s| !s.is_terminal());
        if open && (r.long_exchange.is_none_or(|x| x == e) || r.short_exchange.is_none_or(|x| x == e)) {
            add(&r.pair_id, &r.symbol);
        }
    }
    out
}

/// The error of the latest poll, when it is newer than the data it failed to refresh.
fn latest_error(fetched_at: i64, error: &Option<(String, i64)>) -> Option<&str> {
    error.as_ref().filter(|(_, at)| *at >= fetched_at).map(|(e, _)| e.as_str())
}

/// Whether `e` holds positions, has an incomplete position list or is on a leg of an unfinished pair,
/// whatever the state of its latest read.
pub fn holds_exposure(snap: &UiSnapshot, e: Exchange) -> bool {
    let on_account = match snap.account(e) {
        AccountState::Loaded { data, .. } => !data.positions.is_empty() || data.positions_incomplete.is_some(),
        _ => false,
    };
    on_account || !unfinished_pair_symbols(snap, e).is_empty()
}

fn short(raw: &str) -> String {
    let plain = plain_error(raw);
    if plain.chars().count() <= REASON_CHARS { plain } else { format!("{}…", plain.chars().take(REASON_CHARS).collect::<String>()) }
}

/// The verdict for `e`, assuming it is not in `allowed_exchanges` (a ticked exchange has no verdict).
pub fn status(snap: &UiSnapshot, e: Exchange) -> DisabledStatus {
    if holds_exposure(snap, e) {
        return DisabledStatus::Exposed;
    }
    let reason = match snap.account(e) {
        AccountState::Loaded { fetched_at, error, .. } => match latest_error(fetched_at, &error) {
            Some(err) => format!("最新讀取失敗：{}", short(err)),
            None => return DisabledStatus::Quiet,
        },
        AccountState::NotConnected { reason } => format!("未連線：{}", short(&reason)),
        AccountState::Failed { error, .. } => short(&error),
        AccountState::Loading => "載入中".to_string(),
    };
    DisabledStatus::Unconfirmed { reason }
}

#[cfg(test)]
#[path = "disabled_tests.rs"]
mod tests;

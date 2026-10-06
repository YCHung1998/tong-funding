//! searchable-symbol-inputs: candidate lists and filtering for the symbol / coin inputs (pure).

use super::bridge::UiSnapshot;
use super::format::base_coin;
use super::manual_order::{open_orders, PickState};
use std::collections::BTreeSet;
use tong_funding_core::types::Exchange;

/// One row of a symbol dropdown. `free` = the typed text itself ("use NEWUSDT"), not a known symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOption {
    pub value: String,
    pub free: bool,
}

fn sorted(set: BTreeSet<String>) -> Vec<String> {
    set.into_iter().collect()
}

/// Symbols in one exchange's market snapshot (sorted, deduplicated; empty until the market loads).
pub fn symbol_options(snap: &UiSnapshot, exchange: Exchange) -> Vec<String> {
    sorted(snap.market.get(&exchange).into_iter().flat_map(|f| f.observations.iter().map(|o| o.symbol.clone())).collect())
}

/// Symbols of every exchange's snapshot.
pub fn all_symbol_options(snap: &UiSnapshot) -> Vec<String> {
    sorted(snap.market.values().flat_map(|f| f.observations.iter().map(|o| o.symbol.clone())).collect())
}

/// Base coins of every symbol in every snapshot.
pub fn coin_options(snap: &UiSnapshot) -> Vec<String> {
    sorted(snap.market.values().flat_map(|f| f.observations.iter().map(|o| base_coin(&o.symbol).to_string())).collect())
}

/// Symbols with an open order on `exchange` (the account of the current execution mode).
pub fn open_order_symbols(snap: &UiSnapshot, exchange: Exchange) -> Vec<String> {
    let lists = open_orders(snap);
    let Some(list) = lists.iter().find(|l| l.exchange == exchange) else { return Vec::new() };
    match &list.state {
        PickState::Rows { rows, .. } => sorted(rows.iter().map(|r| r.symbol.clone()).collect()),
        _ => Vec::new(),
    }
}

fn norm(s: &str) -> String {
    s.trim().to_ascii_uppercase()
}

/// Rows for a typed `query`: case-insensitive substring matches in list order, preceded by the
/// typed text itself (uppercased, trimmed) when it is not already an option. Empty query = all.
pub fn options_with_query(query: &str, options: &[String]) -> Vec<SymbolOption> {
    let q = norm(query);
    let mut rows = Vec::new();
    if !q.is_empty() && !options.iter().any(|o| norm(o) == q) {
        rows.push(SymbolOption { value: q.clone(), free: true });
    }
    rows.extend(options.iter().filter(|o| norm(o).contains(&q)).map(|o| SymbolOption { value: o.clone(), free: false }));
    rows
}

/// Whether `value` (trimmed, any case) is one of `options`.
pub fn is_known(value: &str, options: &[String]) -> bool {
    let v = norm(value);
    options.iter().any(|o| norm(o) == v)
}

/// `allowed_coins` text -> coins (uppercase, deduplicated, first-seen order).
pub fn parse_coins(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in text.split(',').map(norm).filter(|c| !c.is_empty()) {
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// Coins -> the stored `allowed_coins` text format (`BTC, ETH`).
pub fn join_coins(coins: &[String]) -> String {
    coins.join(", ")
}

#[cfg(test)]
#[path = "symbol_options_tests.rs"]
mod tests;

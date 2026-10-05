//! Pure grouping of position rows into hedge pairs (spec: position-grouping).

use crate::types::Exchange;

/// A position row as shown in the positions table; `payload` carries caller-defined display data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionRow<T = ()> {
    pub exchange: Exchange,
    pub symbol: String,
    pub payload: T,
}

/// The identity of a `RECONCILED` pair: which symbol, and which exchange holds each leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledPair {
    pub symbol: String,
    pub long_exchange: Exchange,
    pub short_exchange: Exchange,
}

/// One matched pair: indices into the input `pairs` and `rows` slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairGroup {
    pub pair_index: usize,
    pub long_row: usize,
    pub short_row: usize,
}

/// Result of [`group_positions`]; all values are indices into the inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grouping {
    /// Matched groups, in input pair order.
    pub groups: Vec<PairGroup>,
    /// Row indices not used by any group, in input row order.
    pub ungrouped: Vec<usize>,
}

/// Matches each pair to unused rows by symbol and exchange; each row joins at most one group.
pub fn group_positions<T>(rows: &[PositionRow<T>], pairs: &[ReconciledPair]) -> Grouping {
    let mut used = vec![false; rows.len()];
    let mut groups = Vec::new();
    for (pair_index, p) in pairs.iter().enumerate() {
        let long = find_unused(rows, &used, p.long_exchange, &p.symbol);
        let short = find_unused(rows, &used, p.short_exchange, &p.symbol);
        if let (Some(long_row), Some(short_row)) = (long, short) {
            if long_row != short_row {
                used[long_row] = true;
                used[short_row] = true;
                groups.push(PairGroup { pair_index, long_row, short_row });
            }
        }
    }
    let ungrouped = (0..rows.len()).filter(|&i| !used[i]).collect();
    Grouping { groups, ungrouped }
}

fn find_unused<T>(rows: &[PositionRow<T>], used: &[bool], exchange: Exchange, symbol: &str) -> Option<usize> {
    rows.iter()
        .enumerate()
        .position(|(i, r)| !used[i] && r.exchange == exchange && r.symbol == symbol)
}

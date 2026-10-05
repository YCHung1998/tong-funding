use tong_funding_core::grouping::{group_positions, PairGroup, PositionRow, ReconciledPair};
use tong_funding_core::types::Exchange::{Binance, Bybit, Okx};
use tong_funding_core::types::Exchange;

fn row(ex: Exchange, sym: &str) -> PositionRow<u32> {
    PositionRow { exchange: ex, symbol: sym.to_string(), payload: 0 }
}
fn pair(sym: &str, long: Exchange, short: Exchange) -> ReconciledPair {
    ReconciledPair { symbol: sym.to_string(), long_exchange: long, short_exchange: short }
}

#[test]
fn normal_pair_groups_two_rows() {
    let rows = [row(Binance, "BTCUSDT"), row(Bybit, "BTCUSDT")];
    let g = group_positions(&rows, &[pair("BTCUSDT", Binance, Bybit)]);
    assert_eq!(g.groups, vec![PairGroup { pair_index: 0, long_row: 0, short_row: 1 }]);
    assert!(g.ungrouped.is_empty());
}

#[test]
fn long_and_short_rows_follow_pair_not_row_order() {
    let rows = [row(Bybit, "BTCUSDT"), row(Binance, "BTCUSDT")];
    let g = group_positions(&rows, &[pair("BTCUSDT", Binance, Bybit)]);
    assert_eq!(g.groups, vec![PairGroup { pair_index: 0, long_row: 1, short_row: 0 }]);
}

#[test]
fn duplicate_pair_does_not_consume_rows_twice() {
    let rows = [row(Binance, "BTCUSDT"), row(Bybit, "BTCUSDT")];
    let p = pair("BTCUSDT", Binance, Bybit);
    let g = group_positions(&rows, &[p.clone(), p]);
    assert_eq!(g.groups.len(), 1);
    assert_eq!(g.groups[0].pair_index, 0);
    assert!(g.ungrouped.is_empty());
}

#[test]
fn missing_leg_leaves_other_row_ungrouped() {
    let rows = [row(Binance, "BTCUSDT")];
    let g = group_positions(&rows, &[pair("BTCUSDT", Binance, Bybit)]);
    assert!(g.groups.is_empty());
    assert_eq!(g.ungrouped, vec![0]);
}

#[test]
fn partially_matched_rows_are_not_consumed_by_failed_pair() {
    // First pair cannot match (no Okx row) and must not burn the Binance row for the second pair.
    let rows = [row(Binance, "ETHUSDT"), row(Bybit, "ETHUSDT")];
    let g = group_positions(
        &rows,
        &[pair("ETHUSDT", Binance, Okx), pair("ETHUSDT", Binance, Bybit)],
    );
    assert_eq!(g.groups, vec![PairGroup { pair_index: 1, long_row: 0, short_row: 1 }]);
}

#[test]
fn two_identical_positions_serve_two_identical_pairs() {
    let rows = [
        row(Binance, "BTCUSDT"),
        row(Bybit, "BTCUSDT"),
        row(Binance, "BTCUSDT"),
        row(Bybit, "BTCUSDT"),
    ];
    let p = pair("BTCUSDT", Binance, Bybit);
    let g = group_positions(&rows, &[p.clone(), p]);
    assert_eq!(
        g.groups,
        vec![
            PairGroup { pair_index: 0, long_row: 0, short_row: 1 },
            PairGroup { pair_index: 1, long_row: 2, short_row: 3 },
        ]
    );
}

#[test]
fn symbol_must_match_and_unmatched_rows_keep_input_order() {
    let rows = [
        row(Okx, "SOLUSDT"),
        row(Binance, "BTCUSDT"),
        row(Bybit, "ETHUSDT"),
        row(Bybit, "BTCUSDT"),
    ];
    let g = group_positions(&rows, &[pair("BTCUSDT", Binance, Bybit)]);
    assert_eq!(g.groups, vec![PairGroup { pair_index: 0, long_row: 1, short_row: 3 }]);
    assert_eq!(g.ungrouped, vec![0, 2]);
}

#[test]
fn same_exchange_on_both_legs_never_groups_one_row_twice() {
    let rows = [row(Binance, "BTCUSDT")];
    let g = group_positions(&rows, &[pair("BTCUSDT", Binance, Binance)]);
    assert!(g.groups.is_empty());
    assert_eq!(g.ungrouped, vec![0]);
}

#[test]
fn no_pairs_returns_all_rows_ungrouped_and_empty_inputs_ok() {
    let rows = [row(Binance, "A"), row(Okx, "B")];
    let g = group_positions(&rows, &[]);
    assert!(g.groups.is_empty());
    assert_eq!(g.ungrouped, vec![0, 1]);
    let g = group_positions::<u32>(&[], &[pair("A", Binance, Bybit)]);
    assert!(g.groups.is_empty() && g.ungrouped.is_empty());
}

#[test]
fn input_is_not_modified() {
    let rows = vec![row(Binance, "BTCUSDT"), row(Bybit, "BTCUSDT")];
    let before = rows.clone();
    let _ = group_positions(&rows, &[pair("BTCUSDT", Binance, Bybit)]);
    assert_eq!(rows, before);
}

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

// ---- 2.2: what one closed pair is worth (net when known, else price PnL minus fees) ----------

mod pair_amount {
    use super::*;
    use tong_funding_core::pnl::{
        compute_pnl, FillAction, FillRecord, FundingFetchState, FundingLedgerEntry, LegInput, PairInput,
    };
    use tong_funding_core::types::{Exchange, Side};

    fn fill(id: &str, action: FillAction, qty: &str, avg: Option<&str>, fee: Option<&str>, reference: Option<&str>) -> FillRecord {
        FillRecord {
            id: id.into(),
            action,
            quantity: d(qty),
            expected_price: reference.map(d),
            actual_price: avg.map(d),
            fee: fee.map(d),
            fee_asset: Some("USDT".into()),
            filled_at_ms: 0,
            contract_value_missing: false,
        }
    }

    fn leg(exchange: Exchange, side: Side, fills: Vec<FillRecord>, funding: Vec<FundingLedgerEntry>, fetch: FundingFetchState) -> LegInput {
        LegInput { exchange, symbol: "BTCUSDT".into(), side, fills, funding, expected_settlements: Some(1), funding_fetch: fetch }
    }

    /// Long closes 2.0 below its open (price PnL -2.0 on qty 1), short flat; fees 1.9 in total.
    fn losing_pair(funding_fetch: FundingFetchState, funding: Vec<FundingLedgerEntry>) -> PairInput {
        let long = leg(
            Exchange::Binance,
            Side::Long,
            vec![
                fill("lo", FillAction::Open, "1", Some("100"), Some("0.5"), Some("100")),
                fill("lc", FillAction::Close, "1", Some("98"), Some("0.4"), Some("98")),
            ],
            vec![],
            funding_fetch.clone(),
        );
        let short = leg(
            Exchange::Bybit,
            Side::Short,
            vec![
                fill("so", FillAction::Open, "1", Some("100"), Some("0.5"), Some("100")),
                fill("sc", FillAction::Close, "1", Some("100"), Some("0.5"), Some("100")),
            ],
            funding,
            funding_fetch,
        );
        PairInput { legs: vec![long, short], ambiguous_attribution: false, reconciliation_mismatch: false }
    }

    #[test]
    fn funding_not_in_yet_uses_price_pnl_minus_both_fees() {
        // spec: price PnL -2.0, fees 1.9 (open 1.0 + close 0.9), funding not fetched -> -3.9
        let b = compute_pnl(&losing_pair(FundingFetchState::NotFetched, vec![]));
        assert_eq!(amount_of(&b), PairAmount::Provisional(d("-3.9")));
        assert_eq!(amount_of(&b).value(), Some(d("-3.9")));
    }

    #[test]
    fn a_known_net_is_used_as_it_is() {
        // spec: later the net is +0.5 -> that value; -2.0 - 1.9 + funding 4.4 = +0.5
        let entry = FundingLedgerEntry::new(Exchange::Bybit, "BTCUSDT", d("4.4"), "USDT", 1, "k1", "SETTLEMENT", serde_json::json!({}));
        let long_funding = FundingLedgerEntry::new(Exchange::Binance, "BTCUSDT", d("0"), "USDT", 1, "k0", "FUNDING_FEE", serde_json::json!({}));
        let mut input = losing_pair(FundingFetchState::Fetched, vec![entry]);
        input.legs[0].funding = vec![long_funding];
        let b = compute_pnl(&input);
        assert_eq!(amount_of(&b), PairAmount::Net(d("0.5")));
    }

    #[test]
    fn a_missing_fill_price_or_fee_is_unknown_never_zero() {
        let mut no_price = losing_pair(FundingFetchState::NotFetched, vec![]);
        no_price.legs[0].fills[1].actual_price = None;
        assert_eq!(amount_of(&compute_pnl(&no_price)), PairAmount::Unknown);
        let mut no_fee = losing_pair(FundingFetchState::NotFetched, vec![]);
        no_fee.legs[1].fills[0].fee = None;
        assert_eq!(amount_of(&compute_pnl(&no_fee)), PairAmount::Unknown);
        assert_eq!(PairAmount::Unknown.value(), None);
    }

    #[test]
    fn no_fills_at_all_is_unknown() {
        let empty = PairInput { legs: vec![leg(Exchange::Binance, Side::Long, vec![], vec![], FundingFetchState::NotFetched)], ambiguous_attribution: false, reconciliation_mismatch: false };
        assert_eq!(amount_of(&compute_pnl(&empty)), PairAmount::Unknown);
    }

    #[test]
    fn open_and_close_quantities_that_differ_are_unknown() {
        let mut input = losing_pair(FundingFetchState::NotFetched, vec![]);
        input.legs[0].fills[1].quantity = d("0.5");
        assert_eq!(amount_of(&compute_pnl(&input)), PairAmount::Unknown);
    }

    #[test]
    fn a_missing_reference_price_does_not_make_the_amount_unknown() {
        // the reference price only feeds slippage; price PnL and fees are what the breaker uses
        let mut input = losing_pair(FundingFetchState::NotFetched, vec![]);
        input.legs[0].fills[1].expected_price = None;
        assert_eq!(amount_of(&compute_pnl(&input)), PairAmount::Provisional(d("-3.9")));
    }
}

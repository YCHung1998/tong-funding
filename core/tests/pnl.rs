//! funding-pnl tasks 2.1–2.5: pure PnL computation (spec pnl-accounting). Hand-computed cases.
//! Run: `cargo test -p tong-funding-core --test pnl` (filters: pnl_breakdown, pnl_fill, pnl_slippage,
//! pnl_attribution, pnl_status, pnl_reconcile, pnl_expected).

use std::str::FromStr;

use serde_json::json;
use tong_funding_core::pnl::{
    attribute, compare_expected, compute_pnl, expected_settlements, fill_ratio, reconcile, slippage_pct, slippage_summary, Attribution,
    CompareItem, Comparison, Component, ExpectedSnapshot, FillAction, FillRecord, FundingFetchState, FundingLedgerEntry, IncompleteReason,
    LegInput, LegWindow, PairInput, PnlStatus, Reconciliation, TradeDirection,
};
use tong_funding_core::types::{Decimal, Exchange, Side};

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn fill(id: &str, action: FillAction, qty: &str, expected: Option<&str>, actual: &str, fee: &str) -> FillRecord {
    FillRecord {
        id: id.into(),
        action,
        quantity: d(qty),
        expected_price: expected.map(d),
        actual_price: Some(d(actual)),
        fee: Some(d(fee)),
        fee_asset: Some("USDT".into()),
        filled_at_ms: 0,
        contract_value_missing: false,
    }
}

fn entry(exchange: Exchange, id: &str, amount: &str, at: i64) -> FundingLedgerEntry {
    FundingLedgerEntry::new(exchange, "BTCUSDT", d(amount), "USDT", at, id, if exchange == Exchange::Binance { "FUNDING_FEE" } else { "SETTLEMENT" }, json!({}))
}

fn leg(exchange: Exchange, side: Side, fills: Vec<FillRecord>, funding: Vec<FundingLedgerEntry>, expected: usize) -> LegInput {
    LegInput {
        exchange,
        symbol: "BTCUSDT".into(),
        side,
        fills,
        funding,
        expected_settlements: Some(expected),
        funding_fetch: FundingFetchState::Fetched,
    }
}

/// The spec's −0.82 case: funding +0.24 (−0.12 + 0.36), reference price PnL 0, fees 0.48 + 0.48,
/// slippage 0.10 (actual price PnL −0.10), other 0.
fn spec_case() -> PairInput {
    // Long Binance 0.02 BTC: expected 60000 → 60000, actual open 60002.5 (2.5 × 0.02 = 0.05 adverse),
    // close 59997.5 (0.05 adverse). Short Bybit 0.02: reference 60000 → 60000, filled exactly.
    let long = leg(
        Exchange::Binance,
        Side::Long,
        vec![
            fill("l-open", FillAction::Open, "0.02", Some("60000"), "60002.5", "0.24"),
            fill("l-close", FillAction::Close, "0.02", Some("60000"), "59997.5", "0.24"),
        ],
        vec![entry(Exchange::Binance, "9689322392", "-0.12", 10)],
        1,
    );
    let short = leg(
        Exchange::Bybit,
        Side::Short,
        vec![
            fill("s-open", FillAction::Open, "0.02", Some("60000"), "60000", "0.24"),
            fill("s-close", FillAction::Close, "0.02", Some("60000"), "60000", "0.24"),
        ],
        vec![entry(Exchange::Bybit, "b-1", "0.36", 10)],
        1,
    );
    PairInput { legs: vec![long, short], ambiguous_attribution: false, reconciliation_mismatch: false }
}

// ---- 2.1 breakdown ------------------------------------------------------------------------

#[test]
fn pnl_breakdown_hand_computed_case_is_minus_0_82() {
    let b = compute_pnl(&spec_case());
    let t = &b.total;
    assert_eq!(t.funding, d("0.24"));
    assert_eq!(t.price_ref, d("0"));
    assert_eq!(t.opening_fee, d("0.48"));
    assert_eq!(t.closing_fee, d("0.48"));
    assert_eq!(t.slippage, d("0.10"));
    assert_eq!(t.other_cost, d("0"));
    assert_eq!(t.net, d("-0.82"));
    assert_eq!(b.status, PnlStatus::Complete);
    assert!(b.missing.is_empty());
    assert!(!b.other_cost_included, "other cost has no data source: shown as 0 and 'not included'");
}

#[test]
fn pnl_breakdown_actual_basis_gives_the_same_net() {
    let b = compute_pnl(&spec_case());
    let t = &b.total;
    assert_eq!(t.price_actual, d("-0.10"));
    // Funding + price (actual) − fees − other, no second deduction of slippage.
    assert_eq!(t.funding + t.price_actual - t.opening_fee - t.closing_fee - t.other_cost, t.net);
    assert_eq!(t.price_ref - t.price_actual, t.slippage);
}

#[test]
fn pnl_breakdown_pair_total_is_the_sum_of_the_legs() {
    let b = compute_pnl(&spec_case());
    assert_eq!(b.legs.len(), 2);
    let sum = |f: fn(&tong_funding_core::pnl::Components) -> Decimal| b.legs.iter().map(|l| f(&l.components)).sum::<Decimal>();
    assert_eq!(sum(|c| c.funding), b.total.funding);
    assert_eq!(sum(|c| c.net), b.total.net);
    assert_eq!(b.legs[0].components.funding, d("-0.12"));
    assert_eq!(b.legs[1].components.funding, d("0.36"));
}

#[test]
fn pnl_breakdown_short_leg_price_direction() {
    let short = leg(
        Exchange::Bybit,
        Side::Short,
        vec![
            fill("o", FillAction::Open, "0.02", Some("60000"), "60000", "0"),
            fill("c", FillAction::Close, "0.02", Some("60200"), "60200", "0"),
        ],
        vec![],
        0,
    );
    let b = compute_pnl(&PairInput { legs: vec![short], ambiguous_attribution: false, reconciliation_mismatch: false });
    assert_eq!(b.legs[0].components.price_ref, d("-4.00"));
}

#[test]
fn pnl_breakdown_single_leg_pair_is_still_broken_down() {
    // PARTIAL_FAILURE then a manual close: only the long leg ever filled.
    let long = leg(
        Exchange::Binance,
        Side::Long,
        vec![fill("o", FillAction::Open, "1", Some("100"), "100", "0.05"), fill("c", FillAction::Close, "1", Some("101"), "101", "0.05")],
        vec![],
        0,
    );
    let short = leg(Exchange::Bybit, Side::Short, vec![], vec![], 0);
    let b = compute_pnl(&PairInput { legs: vec![long, short], ambiguous_attribution: false, reconciliation_mismatch: false });
    assert_eq!(b.status, PnlStatus::Complete);
    assert_eq!(b.total.price_ref, d("1"));
    assert_eq!(b.total.net, d("0.9"));
}

#[test]
fn pnl_breakdown_fee_in_another_asset_is_incomplete_not_zero() {
    let mut p = spec_case();
    p.legs[0].fills[0].fee_asset = Some("BNB".into());
    let b = compute_pnl(&p);
    match &b.status {
        PnlStatus::Incomplete(r) => assert!(
            r.iter().any(|x| matches!(x, IncompleteReason::FeeNotConvertible { asset, .. } if asset == "BNB")),
            "{r:?}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(b.missing.contains(&Component::OpeningFee));
    assert!(b.missing.contains(&Component::Net));
    assert!(!b.missing.contains(&Component::ClosingFee));
}

// ---- 2.2 fill ratio, slippage, attribution -----------------------------------------------

#[test]
fn pnl_fill_ratio_997_42_of_1000() {
    assert_eq!(fill_ratio(d("1000"), d("997.42")), Some(d("0.99742")));
    assert_eq!(fill_ratio(d("0"), d("1")), None, "no requested notional: no ratio, never a division by zero");
}

#[test]
fn pnl_slippage_long_buy_higher_is_adverse() {
    assert_eq!(slippage_pct(TradeDirection::Buy, d("60000"), d("60006")), Some(d("0.01")));
    assert_eq!(TradeDirection::of(Side::Long, FillAction::Open), TradeDirection::Buy);
}

#[test]
fn pnl_slippage_short_sell_lower_is_adverse() {
    assert_eq!(slippage_pct(TradeDirection::Sell, d("60000"), d("59994")), Some(d("0.01")));
    assert_eq!(TradeDirection::of(Side::Short, FillAction::Open), TradeDirection::Sell);
    assert_eq!(TradeDirection::of(Side::Long, FillAction::Close), TradeDirection::Sell);
    assert_eq!(TradeDirection::of(Side::Short, FillAction::Close), TradeDirection::Buy);
    // Favourable fills are negative.
    assert_eq!(slippage_pct(TradeDirection::Sell, d("60000"), d("60006")), Some(d("-0.01")));
}

#[test]
fn pnl_slippage_summary_per_side() {
    let s = slippage_summary(&compute_pnl(&spec_case()));
    assert_eq!(s.long, Some(d("0.10")));
    assert_eq!(s.short, Some(d("0")));
    assert_eq!(s.total, Some(d("0.10")));
}

#[test]
fn pnl_slippage_missing_reference_price_is_incomplete_not_zero() {
    let mut p = spec_case();
    p.legs[1].fills[0].expected_price = None;
    let b = compute_pnl(&p);
    match &b.status {
        PnlStatus::Incomplete(r) => assert!(r.iter().any(|x| matches!(x, IncompleteReason::MissingReferencePrice { id, .. } if id == "s-open")), "{r:?}"),
        other => panic!("{other:?}"),
    }
    assert!(b.missing.contains(&Component::Slippage));
    assert!(b.missing.contains(&Component::PriceRef));
    assert!(!b.missing.contains(&Component::PriceActual));
    assert_eq!(slippage_summary(&b).short, None);
}

#[test]
fn pnl_a_fill_without_its_contract_value_is_incomplete_with_a_named_reason_and_no_price_math() {
    let mut p = spec_case();
    p.legs[1].fills[0].contract_value_missing = true;
    let with = compute_pnl(&p);
    match &with.status {
        PnlStatus::Incomplete(r) => {
            let hit = r.iter().find(|x| matches!(x, IncompleteReason::MissingContractValue { id, .. } if id == "s-open"));
            assert!(hit.is_some(), "{r:?}");
            assert!(hit.unwrap().label().contains("合約面值"), "{}", hit.unwrap().label());
        }
        other => panic!("{other:?}"),
    }
    for c in [Component::PriceRef, Component::PriceActual, Component::Slippage] {
        assert!(with.missing.contains(&c), "{c:?}");
    }
    // the fill's price x quantity is not added in contracts-as-coins: totals equal the case without that fill's prices
    let mut q = spec_case();
    q.legs[1].fills[0].contract_value_missing = false;
    assert_ne!(with.legs[1].components.price_actual, compute_pnl(&q).legs[1].components.price_actual);
    // a false flag changes nothing
    assert_eq!(compute_pnl(&spec_case()), compute_pnl(&q));
}

#[test]
fn pnl_an_okx_close_without_its_contract_value_is_incomplete_without_a_quantity_mismatch() {
    // S1: open recorded with ct_val (10 coins); after a restart the close (1000 contracts) has none.
    // The raw contract count must not be added to `closed`: no "open 10, closed 1000" text.
    let mut p = spec_case();
    p.legs[1].fills[0].quantity = d("10");
    p.legs[1].fills[1].quantity = d("1000");
    p.legs[1].fills[1].contract_value_missing = true;
    let b = compute_pnl(&p);
    match &b.status {
        PnlStatus::Incomplete(r) => {
            assert!(r.iter().any(|x| matches!(x, IncompleteReason::MissingContractValue { id, .. } if id == "s-close")), "{r:?}");
            assert!(!r.iter().any(|x| matches!(x, IncompleteReason::OpenCloseQuantityMismatch { .. })), "no bogus quantity mismatch: {r:?}");
            for x in r {
                assert!(!x.label().contains("1000"), "{}", x.label());
            }
        }
        other => panic!("{other:?}"),
    }
    // and the symmetric case: the open lost its contract value
    let mut q = spec_case();
    q.legs[1].fills[0].quantity = d("1000");
    q.legs[1].fills[0].contract_value_missing = true;
    q.legs[1].fills[1].quantity = d("10");
    let r = match compute_pnl(&q).status {
        PnlStatus::Incomplete(r) => r,
        other => panic!("{other:?}"),
    };
    assert!(!r.iter().any(|x| matches!(x, IncompleteReason::OpenCloseQuantityMismatch { .. })), "{r:?}");
}

fn window(exchange: Exchange, opened: i64, closed: Option<i64>) -> LegWindow {
    LegWindow { exchange, symbol: "BTCUSDT".into(), opened_at_ms: opened, closed_at_ms: closed }
}

#[test]
fn pnl_attribution_settlement_before_the_open_fill_is_not_counted() {
    let w = [window(Exchange::Binance, 1_000, Some(9_000))];
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 999), &w, 20_000), Attribution::Unattributed);
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 1_000), &w, 20_000), Attribution::Unattributed, "strictly after the open fill");
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 1_001), &w, 20_000), Attribution::Leg(0));
}

#[test]
fn pnl_attribution_settlement_exactly_at_the_close_fill_is_counted() {
    let w = [window(Exchange::Binance, 1_000, Some(9_000))];
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 9_000), &w, 20_000), Attribution::Leg(0));
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 9_001), &w, 20_000), Attribution::Unattributed);
}

#[test]
fn pnl_attribution_open_leg_counts_until_now_and_other_exchange_or_symbol_never() {
    let w = [window(Exchange::Binance, 1_000, None)];
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 5_000), &w, 5_000), Attribution::Leg(0));
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 5_001), &w, 5_000), Attribution::Unattributed);
    assert_eq!(attribute(&entry(Exchange::Bybit, "1", "0.1", 2_000), &w, 5_000), Attribution::Unattributed);
    let mut e = entry(Exchange::Binance, "1", "0.1", 2_000);
    e.symbol = "ETHUSDT".into();
    assert_eq!(attribute(&e, &w, 5_000), Attribution::Unattributed);
}

#[test]
fn pnl_attribution_two_pairs_on_the_same_exchange_and_symbol_is_ambiguous() {
    let w = [window(Exchange::Binance, 1_000, Some(9_000)), window(Exchange::Binance, 2_000, None)];
    assert_eq!(attribute(&entry(Exchange::Binance, "1", "0.1", 5_000), &w, 20_000), Attribution::Ambiguous(vec![0, 1]));
}

// ---- 2.3 status and expected settlements ---------------------------------------------------

#[test]
fn pnl_status_expected_settlements_inside_the_window() {
    const H8: i64 = 8 * 3600;
    let first = 28_800_000; // 08:00 UTC
    // Opened 07:59:50, closed 08:00:15: one settlement.
    assert_eq!(expected_settlements(first, H8, first - 10_000, first + 15_000), vec![first]);
    // Closed exactly at the settlement: counted (same rule as attribution).
    assert_eq!(expected_settlements(first, H8, first - 10_000, first), vec![first]);
    // Opened exactly at it: not counted.
    assert_eq!(expected_settlements(first, H8, first, first + 1), Vec::<i64>::new());
    // Held 17 hours from 07:00: 08:00, 16:00.
    assert_eq!(expected_settlements(first, H8, first - 3_600_000, first + 16 * 3_600_000 - 1), vec![first, first + H8 * 1000]);
    // A first settlement already in the past at entry is stepped forward.
    assert_eq!(expected_settlements(first - H8 * 1000, H8, first - 1, first + 1), vec![first]);
    assert!(expected_settlements(first, 0, 0, i64::MAX).is_empty(), "no interval: nothing derivable");
}

#[test]
fn pnl_status_one_missing_settlement_is_incomplete_not_zero() {
    let mut p = spec_case();
    p.legs[0].funding.clear();
    let b = compute_pnl(&p);
    match &b.status {
        PnlStatus::Incomplete(r) => assert!(
            r.contains(&IncompleteReason::MissingSettlement { exchange: Exchange::Binance, expected: 1, received: 0 }),
            "{r:?}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(b.missing.contains(&Component::Funding), "funding is not shown as a complete 0");
    assert!(b.missing.contains(&Component::Net));
    assert_eq!(b.legs[0].settlements_received, 0);
}

#[test]
fn pnl_status_everything_present_is_complete() {
    assert_eq!(compute_pnl(&spec_case()).status, PnlStatus::Complete);
}

#[test]
fn pnl_status_fetch_failure_retention_and_ambiguity_are_incomplete() {
    for state in [FundingFetchState::Failed("timeout".into()), FundingFetchState::BeyondRetention, FundingFetchState::NotFetched] {
        let mut p = spec_case();
        p.legs[1].funding_fetch = state.clone();
        let b = compute_pnl(&p);
        assert!(matches!(b.status, PnlStatus::Incomplete(_)), "{state:?}");
        assert!(b.missing.contains(&Component::Funding), "{state:?}");
    }
    let mut p = spec_case();
    p.ambiguous_attribution = true;
    assert!(matches!(&compute_pnl(&p).status, PnlStatus::Incomplete(r) if r.contains(&IncompleteReason::AmbiguousAttribution)));
    let mut p = spec_case();
    p.legs[0].expected_settlements = None;
    assert!(
        matches!(&compute_pnl(&p).status, PnlStatus::Incomplete(r) if r.contains(&IncompleteReason::SettlementsNotDerivable { exchange: Exchange::Binance }))
    );
}

#[test]
fn pnl_status_missing_fill_details_and_unbalanced_quantities_are_incomplete() {
    let mut p = spec_case();
    p.legs[0].fills[1].actual_price = None;
    let b = compute_pnl(&p);
    assert!(b.missing.contains(&Component::PriceActual) && b.missing.contains(&Component::Slippage));
    assert!(matches!(&b.status, PnlStatus::Incomplete(r) if r.iter().any(|x| matches!(x, IncompleteReason::MissingFillDetail { id, .. } if id == "l-close"))));

    let mut p = spec_case();
    p.legs[0].fills[1].quantity = d("0.01");
    let b = compute_pnl(&p);
    assert!(b.missing.contains(&Component::PriceRef));
    assert!(matches!(&b.status, PnlStatus::Incomplete(r) if r.iter().any(|x| matches!(x, IncompleteReason::OpenCloseQuantityMismatch { .. }))));
}

#[test]
fn pnl_status_a_pair_without_any_recorded_fill_is_incomplete_not_a_complete_zero() {
    let p = PairInput {
        legs: vec![leg(Exchange::Binance, Side::Long, vec![], vec![], 0), leg(Exchange::Bybit, Side::Short, vec![], vec![], 0)],
        ambiguous_attribution: false,
        reconciliation_mismatch: false,
    };
    let b = compute_pnl(&p);
    assert!(matches!(&b.status, PnlStatus::Incomplete(r) if r.contains(&IncompleteReason::NoFillsRecorded)), "{:?}", b.status);
    assert!(b.missing.contains(&Component::Net));
}

#[test]
fn pnl_status_reasons_have_a_label_and_serialize() {
    let r = IncompleteReason::MissingSettlement { exchange: Exchange::Bybit, expected: 2, received: 1 };
    assert!(r.label().contains("缺少結算流水"), "{}", r.label());
    let b = compute_pnl(&spec_case());
    let v = serde_json::to_value(&b).unwrap();
    assert_eq!(v["total"]["net"], json!("-0.82"));
    assert_eq!(v["status"], json!("Complete"));
}

#[test]
fn pnl_status_dedupe_keys() {
    assert_eq!(entry(Exchange::Binance, "9689322392", "1", 0).dedupe_key, "binance:FUNDING_FEE:9689322392");
    assert_eq!(entry(Exchange::Bybit, "abc", "1", 0).dedupe_key, "bybit:abc");
}

// ---- 2.4 reconciliation --------------------------------------------------------------------

fn keyed(v: &[(&str, &str)]) -> Vec<(String, Decimal)> {
    v.iter().map(|(k, a)| (k.to_string(), d(a))).collect()
}

#[test]
fn pnl_reconcile_identical_is_ok() {
    let r = reconcile(&keyed(&[("a", "-0.12")]), &keyed(&[("a", "-0.12")]));
    assert_eq!(r, Reconciliation::Ok { count: 1, sum: d("-0.12") });
}

#[test]
fn pnl_reconcile_different_amount_is_a_mismatch_with_the_difference() {
    let r = reconcile(&keyed(&[("a", "-0.12")]), &keyed(&[("a", "-0.15")]));
    match r {
        Reconciliation::Mismatch { local_sum, remote_sum, diff, local_count, remote_count, .. } => {
            assert_eq!((local_sum, remote_sum, diff), (d("-0.12"), d("-0.15"), d("-0.03")));
            assert_eq!((local_count, remote_count), (1, 1));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn pnl_reconcile_one_more_on_the_exchange_or_locally_is_a_mismatch() {
    match reconcile(&keyed(&[("a", "0.1")]), &keyed(&[("a", "0.1"), ("b", "0")])) {
        Reconciliation::Mismatch { local_count, remote_count, missing_locally, .. } => {
            assert_eq!((local_count, remote_count), (1, 2), "a zero-amount extra row still counts");
            assert_eq!(missing_locally, vec!["b".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    match reconcile(&keyed(&[("a", "0.1"), ("c", "0.2")]), &keyed(&[("a", "0.1")])) {
        Reconciliation::Mismatch { missing_remotely, .. } => assert_eq!(missing_remotely, vec!["c".to_string()]),
        other => panic!("{other:?}"),
    }
}

// ---- 2.5 expected vs actual ----------------------------------------------------------------

fn snapshot() -> ExpectedSnapshot {
    ExpectedSnapshot {
        funding_income: d("0.40"),
        fee: d("0.96"),
        slippage: d("0.05"),
        safety_margin: d("0.02"),
        net_edge: d("-0.63"),
        assumed_settlements: 1,
    }
}

#[test]
fn pnl_expected_line_by_line_difference() {
    let b = compute_pnl(&spec_case());
    let Comparison::Lines { lines, safety_margin, settlement_note } = compare_expected(Some(&snapshot()), &b, 1) else { panic!() };
    let line = |i: CompareItem| lines.iter().find(|l| l.item == i).unwrap().clone();
    let f = line(CompareItem::Funding);
    assert_eq!((f.expected, f.actual, f.diff, f.diff_pct), (d("0.40"), Some(d("0.24")), Some(d("-0.16")), Some(d("-40"))));
    let fee = line(CompareItem::Fee);
    assert_eq!((fee.actual, fee.diff), (Some(d("0.96")), Some(d("0"))));
    let s = line(CompareItem::Slippage);
    assert_eq!((s.actual, s.diff, s.diff_pct), (Some(d("0.10")), Some(d("0.05")), Some(d("100"))));
    let n = line(CompareItem::Net);
    assert_eq!((n.actual, n.diff), (Some(d("-0.82")), Some(d("-0.19"))));
    assert_eq!(safety_margin, d("0.02"), "safety margin has no actual counterpart: expected side only");
    assert_eq!(settlement_note, None);
}

#[test]
fn pnl_expected_more_settlements_than_assumed_is_flagged() {
    let b = compute_pnl(&spec_case());
    let Comparison::Lines { settlement_note, .. } = compare_expected(Some(&snapshot()), &b, 2) else { panic!() };
    assert_eq!(settlement_note, Some((2, 1)));
}

#[test]
fn pnl_expected_without_snapshot_is_no_snapshot_never_recomputed() {
    assert_eq!(compare_expected(None, &compute_pnl(&spec_case()), 1), Comparison::NoSnapshot);
}

#[test]
fn pnl_expected_missing_actual_gives_no_difference() {
    let mut p = spec_case();
    p.legs[0].funding.clear();
    let Comparison::Lines { lines, .. } = compare_expected(Some(&snapshot()), &compute_pnl(&p), 1) else { panic!() };
    let f = lines.iter().find(|l| l.item == CompareItem::Funding).unwrap();
    assert_eq!((f.actual, f.diff, f.diff_pct), (None, None, None));
}

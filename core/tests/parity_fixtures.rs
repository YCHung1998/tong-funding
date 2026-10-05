//! Parity with the Python reference (spec: parity-fixtures). Loads every fixture exported by
//! `tools/dump_fixtures.py` and compares the Rust implementation case by case.
//! Fixtures are never edited by hand; an intentional difference must be listed in
//! `INTENTIONAL_DIFFERENCES` with a reason (and in design.md's difference table).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;

use rust_decimal::Decimal;
use serde_json::Value;
use tong_funding_core::grouping::{group_positions, PositionRow, ReconciledPair};
use tong_funding_core::pretrade::{evaluate_pretrade, Check, LegInput, PretradeInput, PretradeLimits};
use tong_funding_core::quantity::{LotSize, Quantity, QuantityError};
use tong_funding_core::types::Exchange;

/// (fixture file, case index, reason). Empty = Rust matches Python on every exported case.
const INTENTIONAL_DIFFERENCES: &[(&str, usize, &str)] = &[];

fn dec(v: &Value) -> Decimal {
    Decimal::from_str(v.as_str().unwrap_or_else(|| panic!("expected string, got {v}"))).unwrap()
}

fn load(name: &str) -> (Value, Vec<Value>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))).unwrap();
    let cases = doc["cases"].as_array().unwrap().clone();
    assert_eq!(doc["case_count"].as_u64().unwrap() as usize, cases.len(), "{name}: case_count mismatch");
    assert!(!cases.is_empty(), "{name}: no cases");
    (doc["header"].clone(), cases)
}

fn skipped(file: &str, idx: usize) -> bool {
    INTENTIONAL_DIFFERENCES.iter().any(|(f, i, _)| *f == file && *i == idx)
}

fn exchange(s: &str) -> Exchange {
    match s {
        "Binance" => Exchange::Binance,
        "Bybit" => Exchange::Bybit,
        "OKX" => Exchange::Okx,
        other => panic!("unknown exchange {other}"),
    }
}

#[test]
fn every_intentional_difference_has_a_reason() {
    for (file, idx, reason) in INTENTIONAL_DIFFERENCES {
        assert!(!reason.trim().is_empty(), "{file}[{idx}] is excluded without a reason");
    }
}

#[test]
fn fixture_headers_record_their_source() {
    for f in ["quantity.json", "quantity_okx.json", "pretrade.json", "grouping.json"] {
        let (h, _) = load(f);
        assert_eq!(h["source"]["git_commit"].as_str().unwrap().len(), 40, "{f}: commit hash");
        assert!(h["source"]["files_sha256"].as_object().is_some_and(|m| !m.is_empty()), "{f}: file hashes");
        assert!(h["source"]["functions"].as_array().is_some_and(|m| !m.is_empty()), "{f}: functions");
        assert!(h["exported_at"].as_str().is_some_and(|s| s.ends_with('Z')), "{f}: exported_at");
    }
}

#[test]
fn quantity_matches_python() {
    let (_, cases) = load("quantity.json");
    let mut checked = 0;
    for (i, c) in cases.iter().enumerate() {
        if skipped("quantity.json", i) {
            continue;
        }
        let lot = LotSize { step_size: dec(&c["input"]["step_size"]), min_qty: dec(&c["input"]["min_qty"]) };
        let got = Quantity::round_down(dec(&c["input"]["qty"]), &lot);
        let exp = &c["expected"];
        if exp["below_min"].as_bool().unwrap() {
            assert!(matches!(got, Err(QuantityError::BelowMinimum { .. })), "case {i} {}: expected below-min, got {got:?}", c["input"]);
        } else {
            let q = got.unwrap_or_else(|e| panic!("case {i} {}: {e:?}", c["input"]));
            assert_eq!(q.value(), dec(&exp["qty"]), "case {i} {}: value", c["input"]);
            assert_eq!(q.to_order_string(&lot), exp["formatted"].as_str().unwrap(), "case {i} {}: formatted", c["input"]);
        }
        checked += 1;
    }
    println!("quantity.json: compared {checked} cases");
}

#[test]
fn okx_contract_conversion_matches_python() {
    let (_, cases) = load("quantity_okx.json");
    let mut checked = 0;
    for (i, c) in cases.iter().enumerate() {
        if skipped("quantity_okx.json", i) {
            continue;
        }
        let i_ = &c["input"];
        let lot = LotSize { step_size: dec(&i_["lot_sz"]), min_qty: dec(&i_["min_sz"]) };
        let got = Quantity::okx_contracts(dec(&i_["base_qty"]), dec(&i_["ct_val"]), &lot);
        if c["expected"]["below_min"].as_bool().unwrap() {
            assert!(matches!(got, Err(QuantityError::BelowMinimum { .. })), "case {i} {i_}: expected below-min, got {got:?}");
        } else {
            let q = got.unwrap_or_else(|e| panic!("case {i} {i_}: {e:?}"));
            assert_eq!(q.value(), dec(&c["expected"]["contracts"]), "case {i} {i_}");
        }
        checked += 1;
    }
    println!("quantity_okx.json: compared {checked} cases");
}

fn leg(ex: Exchange, baseline: Option<Decimal>, scan: Decimal, latest: Decimal, margin: Decimal) -> LegInput {
    LegInput {
        exchange: ex,
        baseline_price: baseline,
        scan_price: scan,
        latest_price: latest,
        price_observed_at_ms: 0,
        funding_observed_at_ms: 0,
        available_margin: margin,
        listed: true,
        exchange_allowed: true,
        volume_24h_quote: Some(Decimal::from(1_000_000_000u64)),
        has_foreign_exposure: false,
    }
}

#[test]
fn pretrade_drift_margin_leverage_match_python() {
    let (_, cases) = load("pretrade.json");
    let in_scope: BTreeSet<&str> = ["PriceDrift", "Margin", "Leverage"].into();
    let mut checked = 0;
    for (i, c) in cases.iter().enumerate() {
        if skipped("pretrade.json", i) {
            continue;
        }
        let x = &c["input"];
        let (lb, sb) = (dec(&x["long_baseline"]), dec(&x["short_baseline"]));
        let from_pretrade = x["baseline_source"].as_str().unwrap() == "pretrade";
        let mk = |ex, base: Decimal, latest: &Value, margin: &Value| {
            if from_pretrade {
                // Python also had a stale scan-time price on the entry; it must be ignored.
                leg(ex, Some(base), base * Decimal::from_str("1.5").unwrap(), dec(latest), dec(margin))
            } else {
                leg(ex, None, base, dec(latest), dec(margin))
            }
        };
        let input = PretradeInput {
            now_ms: 0,
            net_edge_qualified: true,
            long: mk(Exchange::Binance, lb, &x["long_latest"], &x["available_margin_long"]),
            short: mk(Exchange::Bybit, sb, &x["short_latest"], &x["available_margin_short"]),
            margin_needed: dec(&x["margin_needed"]),
            leverage: dec(&x["leverage"]),
            open_pair_count: 0,
        };
        let limits = PretradeLimits {
            max_price_drift_pct: dec(&x["max_price_drift_pct"]),
            stale_data_threshold_ms: 1_000_000,
            max_leverage: dec(&x["max_leverage"]),
            max_concurrent_pairs: 1000,
            min_24h_volume_usdt: Decimal::ZERO,
        };
        let verdict = evaluate_pretrade(&input, &limits);
        let names: Vec<String> = verdict.failed().iter().map(|ch: &Check| format!("{ch:?}")).collect();
        let got: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        assert!(got.is_subset(&in_scope), "case {i}: a check outside the exported scope failed: {names:?}");
        let want: BTreeSet<&str> = c["expected"]["failed_checks"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(got, want, "case {i} {x}");
        assert_eq!(c["expected"]["pass"].as_bool().unwrap(), verdict.failed().is_empty(), "case {i}: pass flag");
        checked += 1;
    }
    println!("pretrade.json: compared {checked} cases");
}

#[test]
fn grouping_matches_python() {
    let (_, cases) = load("grouping.json");
    let mut checked = 0;
    for (i, c) in cases.iter().enumerate() {
        if skipped("grouping.json", i) {
            continue;
        }
        let rows: Vec<PositionRow> = c["input"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| PositionRow { exchange: exchange(r["exchange"].as_str().unwrap()), symbol: r["symbol"].as_str().unwrap().to_string(), payload: () })
            .collect();
        let pairs: Vec<ReconciledPair> = c["input"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| ReconciledPair {
                symbol: e["symbol"].as_str().unwrap().to_string(),
                long_exchange: exchange(e["long_exchange"].as_str().unwrap()),
                short_exchange: exchange(e["short_exchange"].as_str().unwrap()),
            })
            .collect();
        let got = group_positions(&rows, &pairs);
        let got_groups: Vec<[usize; 3]> = got.groups.iter().map(|g| [g.pair_index, g.long_row, g.short_row]).collect();
        let want_groups: Vec<[usize; 3]> = c["expected"]["grouped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| {
                let a = g.as_array().unwrap();
                [a[0].as_u64().unwrap() as usize, a[1].as_u64().unwrap() as usize, a[2].as_u64().unwrap() as usize]
            })
            .collect();
        let want_ungrouped: Vec<usize> = c["expected"]["ungrouped"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        assert_eq!(got_groups, want_groups, "case {i} {}", c["input"]);
        assert_eq!(got.ungrouped, want_ungrouped, "case {i} {}", c["input"]);
        checked += 1;
    }
    println!("grouping.json: compared {checked} cases");
}

//! symbol-leverage-cap: parsers of the per-symbol leverage cap (Binance `leverageBracket`, Bybit
//! `instruments-info`). The Bybit body is a real response of the demo host; the Binance body has the
//! documented shape (verified against the demo host by the live probe).
#![cfg(test)]

use serde_json::json;
use tong_funding_core::types::Decimal;

use super::binance::leverage_cap_from_brackets;
use super::bybit::max_leverage_from_instruments;
use crate::exchange::error::AdapterError;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn brackets(symbol: &str) -> serde_json::Value {
    json!([{ "symbol": symbol, "notionalCoef": 1.0, "brackets": [
        { "bracket": 1, "initialLeverage": 125, "notionalCap": 50000, "notionalFloor": 0, "maintMarginRatio": 0.004, "cum": 0.0 },
        { "bracket": 2, "initialLeverage": 100, "notionalCap": 250000, "notionalFloor": 50000, "maintMarginRatio": 0.005, "cum": 50.0 },
        { "bracket": 3, "initialLeverage": 50, "notionalCap": 1000000, "notionalFloor": 250000, "maintMarginRatio": 0.01, "cum": 1300.0 },
    ]}])
}

#[test]
fn binance_cap_is_the_bracket_that_contains_the_notional() {
    let b = brackets("BTCUSDT");
    assert_eq!(leverage_cap_from_brackets(&b, "BTCUSDT", d("1000")).unwrap(), d("125"));
    assert_eq!(leverage_cap_from_brackets(&b, "BTCUSDT", d("100000")).unwrap(), d("100"));
    assert_eq!(leverage_cap_from_brackets(&b, "BTCUSDT", d("250001")).unwrap(), d("50"));
    assert_eq!(leverage_cap_from_brackets(&b, "BTCUSDT", d("1000000")).unwrap(), d("50"), "cap is inclusive");
}

#[test]
fn binance_notional_above_every_bracket_or_wrong_symbol_is_an_error() {
    let b = brackets("BTCUSDT");
    assert!(matches!(leverage_cap_from_brackets(&b, "BTCUSDT", d("1000001")), Err(AdapterError::Parse(_))));
    assert!(matches!(leverage_cap_from_brackets(&b, "ETHUSDT", d("1000")), Err(AdapterError::Parse(_))));
    assert!(matches!(leverage_cap_from_brackets(&json!([]), "BTCUSDT", d("1")), Err(AdapterError::Parse(_))));
    let no_brackets = json!([{ "symbol": "BTCUSDT" }]);
    assert!(matches!(leverage_cap_from_brackets(&no_brackets, "BTCUSDT", d("1")), Err(AdapterError::Parse(_))));
}

#[test]
fn binance_accepts_the_single_object_shape() {
    let one = brackets("BTCUSDT")[0].clone();
    assert_eq!(leverage_cap_from_brackets(&one, "BTCUSDT", d("100000")).unwrap(), d("100"));
}

#[test]
fn bybit_cap_is_read_from_a_real_demo_response() {
    let body: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/bybit/instruments_info_nmrusdt_demo.json")).unwrap();
    assert_eq!(max_leverage_from_instruments(&body, "NMRUSDT").unwrap(), d("50"));
}

#[test]
fn bybit_missing_symbol_non_trading_or_missing_field_is_an_error() {
    let body: serde_json::Value = serde_json::from_str(include_str!("../../../tests/fixtures/bybit/instruments_info_nmrusdt_demo.json")).unwrap();
    assert!(max_leverage_from_instruments(&body, "OGNUSDT").is_err());
    let mut halted = body.clone();
    halted["result"]["list"][0]["status"] = json!("Closed");
    assert!(max_leverage_from_instruments(&halted, "NMRUSDT").is_err());
    let mut nofield = body;
    nofield["result"]["list"][0]["leverageFilter"] = json!({});
    assert!(max_leverage_from_instruments(&nofield, "NMRUSDT").is_err());
}

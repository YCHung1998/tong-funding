//! Per-settlement funding-rate history (change funding-slot-tracing, design D10): which rate applied
//! at a settlement time, from each exchange's public history, so that "no ledger row" can be told
//! apart from "the rate was exactly 0". Parsers only; the clients' GET methods call them.
//!
//! - Binance `GET /fapi/v1/fundingRate` → `[{"symbol","fundingTime":ms,"fundingRate":"0.0001"}]`
//! - Bybit `GET /v5/market/funding/history` → `result.list[{"symbol","fundingRate","fundingRateTimestamp":"ms"}]`
//! - OKX `GET /api/v5/public/funding-rate-history` → `data[{"instId","fundingRate","realizedRate","fundingTime":"ms"}]`

use serde_json::Value;
use tong_funding_core::pnl::SLOT_MATCH_TOLERANCE_MS;
use tong_funding_core::types::Decimal;

use super::models::{dec_opt, dec_req};
use crate::exchange::error::AdapterError;

fn millis(row: &Value, field: &str) -> Result<i64, AdapterError> {
    match row.get(field) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| AdapterError::parse(format!("funding rate row without {field}")))
}

/// The row stamped closest to `settle_ms` within the settlement tolerance, `None` when no row is.
fn nearest<'a>(rows: &'a [Value], time_field: &str, settle_ms: i64) -> Result<Option<&'a Value>, AdapterError> {
    let mut best: Option<(&Value, i64)> = None;
    for r in rows {
        let gap = (millis(r, time_field)? - settle_ms).abs();
        if gap <= SLOT_MATCH_TOLERANCE_MS && best.is_none_or(|(_, g)| gap < g) {
            best = Some((r, gap));
        }
    }
    Ok(best.map(|(r, _)| r))
}

/// Binance: the body is the array itself.
pub fn binance_rate_at(body: &Value, settle_ms: i64) -> Result<Option<Decimal>, AdapterError> {
    let rows = body.as_array().ok_or_else(|| AdapterError::parse("expected a JSON array"))?;
    nearest(rows, "fundingTime", settle_ms)?.map(|r| dec_req(r, "fundingRate")).transpose()
}

/// Bybit: `result.list`.
pub fn bybit_rate_at(body: &Value, settle_ms: i64) -> Result<Option<Decimal>, AdapterError> {
    let rows = body.pointer("/result/list").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing result.list"))?;
    nearest(rows, "fundingRateTimestamp", settle_ms)?.map(|r| dec_req(r, "fundingRate")).transpose()
}

/// OKX: `data`; the rate that was actually applied (`realizedRate`) wins over the announced one.
pub fn okx_rate_at(body: &Value, settle_ms: i64) -> Result<Option<Decimal>, AdapterError> {
    let rows = body.get("data").and_then(Value::as_array).ok_or_else(|| AdapterError::parse("missing data"))?;
    let Some(r) = nearest(rows, "fundingTime", settle_ms)? else { return Ok(None) };
    match dec_opt(r, "realizedRate")? {
        Some(x) => Ok(Some(x)),
        None => dec_req(r, "fundingRate").map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T: i64 = 1_791_302_400_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn funding_rate_binance_picks_the_row_at_the_settlement_time() {
        let body = json!([
            { "symbol": "NMRUSDT", "fundingTime": T - 28_800_000, "fundingRate": "0.00010000", "markPrice": "16.7" },
            { "symbol": "NMRUSDT", "fundingTime": T + 3, "fundingRate": "0.01000000", "markPrice": "16.7" },
        ]);
        assert_eq!(binance_rate_at(&body, T).unwrap(), Some(d("0.01")));
        assert_eq!(binance_rate_at(&json!([]), T).unwrap(), None, "no row for that time");
    }

    #[test]
    fn funding_rate_zero_is_a_value_not_a_missing_row() {
        let body = json!([{ "symbol": "X", "fundingTime": T, "fundingRate": "0.00000000" }]);
        let r = binance_rate_at(&body, T).unwrap();
        assert_eq!(r, Some(Decimal::ZERO));
        assert!(r.unwrap().is_zero());
    }

    #[test]
    fn funding_rate_a_row_outside_the_tolerance_is_not_the_settlement() {
        let body = json!([{ "symbol": "X", "fundingTime": T + 3_600_000, "fundingRate": "0.5" }]);
        assert_eq!(binance_rate_at(&body, T).unwrap(), None);
    }

    #[test]
    fn funding_rate_bybit_reads_result_list_with_string_timestamps() {
        let body = json!({ "retCode": 0, "retMsg": "OK", "result": { "category": "linear", "list": [
            { "symbol": "NMRUSDT", "fundingRate": "0.0000", "fundingRateTimestamp": (T).to_string() },
            { "symbol": "NMRUSDT", "fundingRate": "0.0001", "fundingRateTimestamp": (T - 3_600_000).to_string() },
        ]}});
        assert_eq!(bybit_rate_at(&body, T).unwrap(), Some(Decimal::ZERO));
        assert!(matches!(bybit_rate_at(&json!({ "retCode": 0 }), T), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn funding_rate_okx_prefers_the_realized_rate() {
        let body = json!({ "code": "0", "data": [
            { "instId": "NMR-USDT-SWAP", "fundingRate": "0.0002", "realizedRate": "0.0001", "fundingTime": T.to_string() },
        ]});
        assert_eq!(okx_rate_at(&body, T).unwrap(), Some(d("0.0001")));
        let announced = json!({ "code": "0", "data": [{ "instId": "X", "fundingRate": "0.0003", "realizedRate": "", "fundingTime": T.to_string() }]});
        assert_eq!(okx_rate_at(&announced, T).unwrap(), Some(d("0.0003")));
    }

    #[test]
    fn funding_rate_a_row_without_a_time_or_rate_is_an_error_never_zero() {
        assert!(binance_rate_at(&json!([{ "fundingRate": "0" }]), T).is_err());
        assert!(binance_rate_at(&json!([{ "fundingTime": T }]), T).is_err());
    }
}

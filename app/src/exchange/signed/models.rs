//! Normalised results of the signed queries. Exchange-specific field names and units stay inside
//! `binance.rs` / `bybit.rs`; everything here is Decimal (never floating point).

use serde_json::Value;
use tong_funding_core::types::{Decimal, Exchange, Side};

use crate::exchange::error::AdapterError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// one-way / hedge position mode; `Unknown` when the response does not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionMode {
    OneWay,
    Hedge,
    Unknown,
}

/// One currency of an account. `usdt_value` is `None` when the exchange gave no valuation; a
/// non-USDT amount is never reinterpreted as USDT.
#[derive(Debug, Clone, PartialEq)]
pub struct Balance {
    pub exchange: Exchange,
    pub asset: String,
    /// Wallet balance in units of `asset`.
    pub amount: Decimal,
    pub available: Option<Decimal>,
    pub usdt_value: Option<Decimal>,
    /// Local clock (unix ms, uncalibrated) when the response was received.
    pub fetched_at: i64,
}

/// A non-zero position. `quantity` is in base-coin units and signed: long positive, short negative
/// (`side` is derived from the sign and kept for convenience).
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub exchange: Exchange,
    pub symbol: String,
    pub side: Side,
    pub quantity: Decimal,
    pub entry_price: Option<Decimal>,
    pub mark_price: Option<Decimal>,
    pub leverage: Option<Decimal>,
    pub unrealized_pnl: Option<Decimal>,
    /// Margin when the exchange provides it (Binance: isolated margin only; Bybit: initial margin).
    pub margin: Option<Decimal>,
    pub notional: Option<Decimal>,
    pub mode: PositionMode,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenOrder {
    pub exchange: Exchange,
    pub symbol: String,
    pub order_id: String,
    pub side: OrderSide,
    pub order_type: String,
    pub price: Option<Decimal>,
    pub quantity: Decimal,
    pub filled_quantity: Decimal,
    pub reduce_only: bool,
    pub status: String,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    Complete,
    /// Some pages could not be fetched: the items are a subset and must not be used as a full list.
    Incomplete { reason: String },
}

/// A list result that always says whether it is complete (spec: signed-read-access, pagination).
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub completeness: Completeness,
}

impl<T> Listing<T> {
    pub fn is_complete(&self) -> bool {
        self.completeness == Completeness::Complete
    }
}

/// Returned by account queries on an exchange without a signed client (OKX). Dedicated type so
/// callers can show "price comparison only" instead of treating it as empty data or a failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{exchange:?}: account queries not supported ({reason})")]
pub struct Unsupported {
    pub exchange: Exchange,
    pub reason: &'static str,
}

/// OKX has public market data only (design D8): its account methods never send a request
/// (this type holds no transport and no credentials).
#[derive(Debug, Clone, Copy, Default)]
pub struct OkxAccount;

impl OkxAccount {
    const REASON: &'static str = "public market data only";
    fn unsupported() -> Unsupported {
        Unsupported { exchange: Exchange::Okx, reason: Self::REASON }
    }
    pub async fn get_balances(&self) -> Result<Vec<Balance>, Unsupported> {
        Err(Self::unsupported())
    }
    pub async fn get_positions(&self) -> Result<Listing<Position>, Unsupported> {
        Err(Self::unsupported())
    }
    pub async fn get_open_orders(&self) -> Result<Listing<OpenOrder>, Unsupported> {
        Err(Self::unsupported())
    }
}

// ---- JSON field helpers shared by the two parsers ----

fn text_of(field: &str, v: &Value) -> Result<Option<String>, AdapterError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        // Only integers are accepted as numbers; a float would already have lost precision.
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Some(n.to_string())),
        other => Err(AdapterError::parse(format!("field {field}: unexpected JSON value {other}"))),
    }
}

fn to_decimal(field: &str, s: &str) -> Result<Decimal, AdapterError> {
    s.parse::<Decimal>()
        .or_else(|_| Decimal::from_scientific(s))
        .map_err(|_| AdapterError::parse(format!("field {field}: not a decimal: {s}")))
}

/// Required decimal: absent, null or empty is a parse error.
pub(super) fn dec_req(obj: &Value, field: &str) -> Result<Decimal, AdapterError> {
    match text_of(field, obj.get(field).unwrap_or(&Value::Null))? {
        Some(s) => to_decimal(field, &s),
        None => Err(AdapterError::parse(format!("missing field {field}"))),
    }
}

/// Optional decimal: absent, null or empty is `None`; malformed is a parse error.
pub(super) fn dec_opt(obj: &Value, field: &str) -> Result<Option<Decimal>, AdapterError> {
    match text_of(field, obj.get(field).unwrap_or(&Value::Null))? {
        Some(s) => to_decimal(field, &s).map(Some),
        None => Ok(None),
    }
}

pub(super) fn str_req(obj: &Value, field: &str) -> Result<String, AdapterError> {
    text_of(field, obj.get(field).unwrap_or(&Value::Null))?
        .ok_or_else(|| AdapterError::parse(format!("missing field {field}")))
}

pub(super) fn str_opt(obj: &Value, field: &str) -> Result<Option<String>, AdapterError> {
    text_of(field, obj.get(field).unwrap_or(&Value::Null))
}

pub(super) fn bool_opt(obj: &Value, field: &str) -> bool {
    obj.get(field).and_then(Value::as_bool).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn decimals_keep_full_precision_from_strings() {
        let o = json!({"positionAmt": "0.019934", "tiny": "0.00000001"});
        assert_eq!(dec_req(&o, "positionAmt").unwrap(), d("0.019934"));
        assert_eq!(dec_req(&o, "positionAmt").unwrap().to_string(), "0.019934");
        assert_eq!(dec_req(&o, "tiny").unwrap(), d("0.00000001"));
    }

    #[test]
    fn required_and_optional_decimals_treat_missing_and_garbage_differently() {
        let o = json!({"a": "", "b": "x1", "c": 1.5});
        assert!(matches!(dec_req(&o, "a"), Err(AdapterError::Parse(_))));
        assert!(matches!(dec_req(&o, "missing"), Err(AdapterError::Parse(_))));
        assert_eq!(dec_opt(&o, "a").unwrap(), None);
        assert_eq!(dec_opt(&o, "missing").unwrap(), None);
        assert!(matches!(dec_opt(&o, "b"), Err(AdapterError::Parse(_))));
        // a JSON float must never be accepted: it has already gone through binary floating point
        assert!(matches!(dec_opt(&o, "c"), Err(AdapterError::Parse(_))));
    }

    #[test]
    fn okx_account_queries_are_unsupported_not_empty() {
        let okx = OkxAccount;
        let e1 = block_on(okx.get_balances()).unwrap_err();
        assert_eq!(e1.exchange, Exchange::Okx);
        assert!(e1.to_string().contains("public market data only"));
        assert!(block_on(okx.get_positions()).is_err());
        assert!(block_on(okx.get_open_orders()).is_err());
    }
}

//! Result classification of order requests (design D4; signed-order-execution spec "送單結果必須
//! 分類"). Four classes for a submit: accepted, rejected (the exchange clearly refused: no order
//! exists), rate limited, unknown. A timeout, a connection reset, an unparsable reply, a 5xx and
//! the exchanges' own "status unknown" codes are UNKNOWN, never rejected: the engine then looks the
//! order up under the same `client_order_id` and never resubmits.
//!
//! "Rate limited" is reported to the engine as `SubmitOutcome::Unknown` (whether a 429 guarantees
//! that nothing was processed is UNVERIFIED, Open Question 9), so it is confirmed by a lookup
//! before the intent may become FAILED. The shared `SubmitOutcome` contract keeps its three
//! variants; the fourth class lives here and in the latency event (`result = "rate_limited"`).

use serde_json::Value;
use tong_funding_core::redact::redact_secrets;

use crate::engine::ports::{OrderState, OrderStatus, QueryOutcome, SubmitOutcome};
use crate::exchange::error::AdapterError;
use crate::exchange::health::ratelimit::parse_retry_after_ms;
use crate::exchange::transport::HttpResponse;

/// Binance codes that say "the request may or may not have been executed" (UNVERIFIED list from
/// the public error-code page as I remember it): -1000 unknown, -1001 disconnected, -1006
/// unexpected response, -1007 timeout waiting for the backend ("execution status unknown").
pub const BINANCE_UNKNOWN_CODES: [i64; 4] = [-1000, -1001, -1006, -1007];
/// Binance "order does not exist" (query) / "unknown order sent" (cancel).
pub const BINANCE_NOT_FOUND_CODES: [i64; 2] = [-2013, -2011];
/// Binance rate-limit code inside a body.
pub const BINANCE_RATE_LIMIT_CODE: i64 = -1003;
/// Bybit "server timeout" / "internal error" (UNVERIFIED): outcome unknown.
pub const BYBIT_UNKNOWN_CODES: [i64; 2] = [10000, 10016];
/// Bybit rate-limit retCodes (too many visits / IP rate limit).
pub const BYBIT_RATE_LIMIT_CODES: [i64; 2] = [10006, 10018];
/// Bybit "order not exists or too late to cancel" (UNVERIFIED).
pub const BYBIT_NOT_FOUND_CODES: [i64; 1] = [110001];

/// Bybit "leverage not modified" (set-leverage to the value the symbol already has): success.
pub const BYBIT_LEVERAGE_UNCHANGED_CODES: [i64; 1] = [110043];

/// Result of a set-leverage request. Anything but `Set` means the order after it is NOT sent.
#[derive(Debug, Clone, PartialEq)]
pub enum LeverageOutcome {
    Set,
    Failed { reason: String },
    RateLimited { retry_after_ms: Option<u64> },
}

/// A set-leverage reply; a refusal whose code is in `unchanged` ("already that leverage") is `Set`.
pub fn leverage_outcome(reply: Reply, unchanged: &[i64]) -> LeverageOutcome {
    match reply {
        Reply::Ok(_) => LeverageOutcome::Set,
        Reply::Refused { code, .. } if unchanged.contains(&code) => LeverageOutcome::Set,
        Reply::Refused { code, message } => LeverageOutcome::Failed { reason: redact_secrets(&format!("{code}: {message}")) },
        Reply::RateLimited { retry_after_ms } => LeverageOutcome::RateLimited { retry_after_ms },
        Reply::Unknown { reason } => LeverageOutcome::Failed { reason },
    }
}

/// The four submit classes.
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitClass {
    Accepted(OrderStatus),
    Rejected { code: String, message: String },
    RateLimited { retry_after_ms: Option<u64> },
    Unknown { reason: String },
}

impl SubmitClass {
    /// Label used in latency / alert events.
    pub fn label(&self) -> &'static str {
        match self {
            SubmitClass::Accepted(_) => "accepted",
            SubmitClass::Rejected { .. } => "rejected",
            SubmitClass::RateLimited { .. } => "rate_limited",
            SubmitClass::Unknown { .. } => "unknown",
        }
    }

    /// The engine contract: rate limited and unknown both mean "look it up by id first".
    pub fn into_outcome(self) -> SubmitOutcome {
        match self {
            SubmitClass::Accepted(status) => SubmitOutcome::Accepted(status),
            SubmitClass::Rejected { code, message } => SubmitOutcome::Rejected { reason: redact_secrets(&format!("{code}: {message}")) },
            SubmitClass::RateLimited { retry_after_ms } => {
                SubmitOutcome::Unknown { reason: format!("rate limited (retry after {retry_after_ms:?} ms); confirm by lookup") }
            }
            SubmitClass::Unknown { reason } => SubmitOutcome::Unknown { reason: redact_secrets(&reason) },
        }
    }
}

/// What a non-2xx / error body said, independent of the exchange.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Parsed JSON body of a 2xx response that the exchange did not mark as an error.
    Ok(Value),
    /// The exchange refused with this code (HTTP 4xx with a code, or Bybit retCode != 0).
    Refused { code: i64, message: String },
    RateLimited { retry_after_ms: Option<u64> },
    /// Timeout, connection failure, 5xx, unparsable body, a 4xx without a code, or a code that
    /// means "status unknown".
    Unknown { reason: String },
}

fn retry_after(resp: &HttpResponse) -> Option<u64> {
    resp.header_value("retry-after").and_then(parse_retry_after_ms)
}

fn transport_failure(e: &AdapterError) -> Reply {
    match e {
        AdapterError::RateLimited { retry_after_ms } => Reply::RateLimited { retry_after_ms: *retry_after_ms },
        AdapterError::Timeout
        | AdapterError::Network(_)
        | AdapterError::Http { .. }
        | AdapterError::Exchange { .. }
        | AdapterError::Parse(_)
        | AdapterError::Incomplete(_)
        | AdapterError::NotConnected => Reply::Unknown { reason: redact_secrets(&e.to_string()) },
    }
}

/// Binance: errors are `{"code": <negative>, "msg": "..."}` with HTTP 4xx (sometimes 200).
pub fn binance_reply(result: Result<HttpResponse, AdapterError>) -> Reply {
    let resp = match result {
        Ok(r) => r,
        Err(e) => return transport_failure(&e),
    };
    if matches!(resp.status, 429 | 418) {
        return Reply::RateLimited { retry_after_ms: retry_after(&resp) };
    }
    let body: Option<Value> = serde_json::from_str(&resp.body).ok();
    let code = body.as_ref().and_then(|b| b.get("code")).and_then(Value::as_i64).filter(|c| *c < 0);
    if let Some(code) = code {
        let message = body.as_ref().and_then(|b| b.get("msg")).and_then(Value::as_str).unwrap_or("").to_string();
        if code == BINANCE_RATE_LIMIT_CODE {
            return Reply::RateLimited { retry_after_ms: retry_after(&resp) };
        }
        if BINANCE_UNKNOWN_CODES.contains(&code) || resp.status >= 500 {
            return Reply::Unknown { reason: redact_secrets(&format!("binance {code}: {message} (status unknown)")) };
        }
        return Reply::Refused { code, message: redact_secrets(&message) };
    }
    match (resp.status, body) {
        (200..=299, Some(b)) => Reply::Ok(b),
        (200..=299, None) => Reply::Unknown { reason: "unparsable 2xx reply".into() },
        (s, _) => Reply::Unknown { reason: format!("HTTP {s} without an exchange error code") },
    }
}

/// Bybit: HTTP 200 with `retCode`; non-zero is a refusal (except rate-limit / unknown codes).
pub fn bybit_reply(result: Result<HttpResponse, AdapterError>) -> Reply {
    let resp = match result {
        Ok(r) => r,
        Err(e) => return transport_failure(&e),
    };
    if matches!(resp.status, 429 | 418) {
        return Reply::RateLimited { retry_after_ms: retry_after(&resp) };
    }
    if !(200..=299).contains(&resp.status) {
        return Reply::Unknown { reason: format!("HTTP {}", resp.status) };
    }
    let Ok(body) = serde_json::from_str::<Value>(&resp.body) else {
        return Reply::Unknown { reason: "unparsable 2xx reply".into() };
    };
    let Some(code) = body.get("retCode").and_then(Value::as_i64) else {
        return Reply::Unknown { reason: "reply without retCode".into() };
    };
    let message = body.get("retMsg").and_then(Value::as_str).unwrap_or("").to_string();
    if code == 0 {
        Reply::Ok(body)
    } else if BYBIT_RATE_LIMIT_CODES.contains(&code) {
        Reply::RateLimited { retry_after_ms: retry_after(&resp) }
    } else if BYBIT_UNKNOWN_CODES.contains(&code) {
        Reply::Unknown { reason: redact_secrets(&format!("bybit {code}: {message} (status unknown)")) }
    } else {
        Reply::Refused { code, message: redact_secrets(&message) }
    }
}

/// A submit reply turned into a class; `parse_ack` reads the accepted body.
pub fn submit_class(reply: Reply, parse_ack: impl FnOnce(&Value) -> Result<OrderStatus, AdapterError>) -> SubmitClass {
    match reply {
        Reply::Ok(body) => match parse_ack(&body) {
            Ok(status) => SubmitClass::Accepted(status),
            Err(e) => SubmitClass::Unknown { reason: format!("accepted reply could not be read: {e}") },
        },
        Reply::Refused { code, message } => SubmitClass::Rejected { code: code.to_string(), message },
        Reply::RateLimited { retry_after_ms } => SubmitClass::RateLimited { retry_after_ms },
        Reply::Unknown { reason } => SubmitClass::Unknown { reason },
    }
}

/// A query / cancel reply turned into a `QueryOutcome`; `not_found` lists the exchange's
/// "no such order" codes, `parse` reads the body (`Ok(None)` = the order is not in it).
pub fn query_outcome(reply: Reply, not_found: &[i64], parse: impl FnOnce(&Value) -> Result<Option<OrderStatus>, AdapterError>) -> QueryOutcome {
    match reply {
        Reply::Ok(body) => match parse(&body) {
            Ok(Some(s)) => QueryOutcome::Found(s),
            Ok(None) => QueryOutcome::NotFound,
            Err(e) => QueryOutcome::Failed { reason: format!("reply could not be read: {e}") },
        },
        Reply::Refused { code, .. } if not_found.contains(&code) => QueryOutcome::NotFound,
        Reply::Refused { code, message } => QueryOutcome::Failed { reason: redact_secrets(&format!("{code}: {message}")) },
        Reply::RateLimited { retry_after_ms } => QueryOutcome::Failed { reason: format!("rate limited (retry after {retry_after_ms:?} ms)") },
        Reply::Unknown { reason } => QueryOutcome::Failed { reason },
    }
}

/// Binance order status → engine state. An unknown status is an error, never a guess.
pub fn binance_state(s: &str) -> Result<OrderState, AdapterError> {
    match s {
        "NEW" | "PARTIALLY_FILLED" => Ok(OrderState::Open),
        "FILLED" => Ok(OrderState::Filled),
        "CANCELED" | "EXPIRED" | "EXPIRED_IN_MATCH" => Ok(OrderState::Cancelled),
        "REJECTED" => Ok(OrderState::Rejected),
        other => Err(AdapterError::parse(format!("unknown Binance order status {other}"))),
    }
}

/// Bybit order status → engine state (UNVERIFIED list for linear market orders).
pub fn bybit_state(s: &str) -> Result<OrderState, AdapterError> {
    match s {
        "New" | "PartiallyFilled" | "Untriggered" | "Created" => Ok(OrderState::Open),
        "Filled" => Ok(OrderState::Filled),
        "Cancelled" | "PartiallyFilledCanceled" | "Deactivated" => Ok(OrderState::Cancelled),
        "Rejected" => Ok(OrderState::Rejected),
        other => Err(AdapterError::parse(format!("unknown Bybit order status {other}"))),
    }
}

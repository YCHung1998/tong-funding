//! The closed error type of every adapter method (spec: exchange-adapter). Messages are redacted
//! when the error is constructed, so a secret can never leave the adapter inside an error string.

use tong_funding_core::redact::redact_secrets;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    #[error("request timed out")]
    Timeout,
    /// Connection, DNS or TLS failure.
    #[error("network error: {0}")]
    Network(String),
    #[error("HTTP status {status}")]
    Http { status: u16 },
    #[error("rate limited (retry after {retry_after_ms:?} ms)")]
    RateLimited { retry_after_ms: Option<u64> },
    /// HTTP 200 but the body says it failed (Bybit `retCode != 0`, OKX `code != "0"`, Binance `{code, msg}`).
    #[error("exchange error {code}: {message}")]
    Exchange { code: String, message: String },
    #[error("parse error: {0}")]
    Parse(String),
    /// Paged data could not be fetched completely; never read as "does not exist".
    #[error("incomplete data: {0}")]
    Incomplete(String),
    #[error("not connected")]
    NotConnected,
}

impl AdapterError {
    pub fn network(msg: impl AsRef<str>) -> Self {
        AdapterError::Network(redact_secrets(msg.as_ref()))
    }
    pub fn parse(msg: impl AsRef<str>) -> Self {
        AdapterError::Parse(redact_secrets(msg.as_ref()))
    }
    pub fn incomplete(msg: impl AsRef<str>) -> Self {
        AdapterError::Incomplete(redact_secrets(msg.as_ref()))
    }
    pub fn exchange(code: impl Into<String>, message: impl AsRef<str>) -> Self {
        AdapterError::Exchange { code: code.into(), message: redact_secrets(message.as_ref()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_redact_secrets_from_messages() {
        let e = AdapterError::network("error sending request for url (https://h/x?timestamp=1&signature=deadbeef)");
        assert!(!e.to_string().contains("deadbeef"));
        assert!(e.to_string().contains("timestamp=1"));
        assert!(!AdapterError::parse("X-MBX-APIKEY: realkey").to_string().contains("realkey"));
        assert!(!AdapterError::exchange("-1", "bad signature=abc123").to_string().contains("abc123"));
        assert!(!AdapterError::incomplete("page 2 failed ?apiKey=zzz").to_string().contains("zzz"));
    }
}

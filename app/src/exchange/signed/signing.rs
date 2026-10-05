//! Signing and request plumbing shared by the Binance and Bybit signed clients.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;
use tong_funding_core::types::Exchange;

use crate::exchange::error::AdapterError;
use crate::exchange::transport::HttpResponse;
use crate::ports::{Clock, SecretName, SecretProvider};

/// `recvWindow` in milliseconds (same as the Python reference).
pub const RECV_WINDOW_MS: u64 = 5000;
/// Timeout of every signed request (design D10 proposal, UNVERIFIED).
pub const SIGNED_TIMEOUT: Duration = Duration::from_secs(5);

/// Source of the "exchange time minus local time" offset (feed-health clock sync). `None` means
/// the exchange has never been successfully calibrated, in which case nothing may be signed.
pub trait ClockOffsetSource: Send + Sync {
    fn offset_ms(&self) -> Option<i64>;
}

impl<F: Fn() -> Option<i64> + Send + Sync> ClockOffsetSource for F {
    fn offset_ms(&self) -> Option<i64> {
        self()
    }
}

/// API credentials. `Debug` never prints the values.
pub struct Credentials {
    pub(in crate::exchange) api_key: String,
    pub(in crate::exchange) api_secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

/// HMAC-SHA256 as lowercase hex.
pub fn hmac_sha256_hex(secret: &str, message: &str) -> Result<String, AdapterError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| AdapterError::parse("invalid HMAC key"))?;
    mac.update(message.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Binance: `HMAC-SHA256(secret, queryString)`.
pub fn binance_signature(secret: &str, query: &str) -> Result<String, AdapterError> {
    hmac_sha256_hex(secret, query)
}

/// Bybit v5: `HMAC-SHA256(secret, timestamp + apiKey + recvWindow + queryString)`.
pub fn bybit_signature(secret: &str, timestamp: i64, api_key: &str, recv_window: u64, query: &str) -> Result<String, AdapterError> {
    hmac_sha256_hex(secret, &format!("{timestamp}{api_key}{recv_window}{query}"))
}

/// RFC 3986 percent-encoding (unreserved characters kept).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `k1=v1&k2=v2` in the given order, values percent-encoded. The same string is signed and sent.
pub fn encode_query(params: &[(&str, String)]) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", percent_encode(v))).collect::<Vec<_>>().join("&")
}

/// Why a signed method answered `NotConnected` (the error type itself has no reason field).
/// Contains no secret content, so a UI may show it as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotConnectedReason {
    NoKey,
    NoSecret,
    /// OKX only; never produced by the Binance/Bybit clients.
    NoPassphrase,
    ClockUnsynced,
    SecretStoreError,
}

impl From<NotConnectedReason> for AdapterError {
    fn from(_: NotConnectedReason) -> Self {
        AdapterError::NotConnected
    }
}

/// Re-synchronises the exchange clock offset (feed-health `clock_sync`). Called once when an
/// exchange rejects a timestamp. Boxed future so the trait can be used as `Arc<dyn Resync>`.
pub trait Resync: Send + Sync {
    fn resync(&self) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>>;
}

/// The exchange refused the request's timestamp: Binance code -1021, Bybit retCode 10002.
pub fn is_timestamp_rejected(e: &AdapterError) -> bool {
    matches!(e, AdapterError::Exchange { code, .. } if code == "-1021" || code == "10002")
}

/// Bybit pagination cursors are percent-encoded once more when placed in the query string (same as
/// the Python reference; whether the exchange wants that is UNVERIFIED). The same string is signed.
pub fn encode_cursor(cursor: &str) -> String {
    percent_encode(cursor)
}

/// Key and secret must exist and be non-empty (and the passphrase too when `require_passphrase`);
/// a missing value, an empty one or a store error means "not connected" (no default value, no
/// empty-string signing). Checked in the order key, secret, passphrase.
pub fn load_credentials(secrets: &dyn SecretProvider, exchange: Exchange, require_passphrase: bool) -> Result<Credentials, NotConnectedReason> {
    let get = |name, missing| match secrets.get(exchange, name) {
        Ok(Some(v)) if !v.is_empty() => Ok(v),
        Ok(_) => Err(missing),
        Err(_) => Err(NotConnectedReason::SecretStoreError),
    };
    let api_key = get(SecretName::ApiKey, NotConnectedReason::NoKey)?;
    let api_secret = get(SecretName::ApiSecret, NotConnectedReason::NoSecret)?;
    if require_passphrase {
        get(SecretName::Passphrase, NotConnectedReason::NoPassphrase)?;
    }
    Ok(Credentials { api_key, api_secret })
}

/// The calibrated offset, or `ClockUnsynced` when the exchange was never calibrated.
pub fn require_offset(offset: &dyn ClockOffsetSource) -> Result<i64, NotConnectedReason> {
    offset.offset_ms().ok_or(NotConnectedReason::ClockUnsynced)
}

/// Local clock plus the calibrated offset (exchange time): the signing timestamp and `fetched_at`.
pub fn signed_timestamp(clock: &dyn Clock, offset: &dyn ClockOffsetSource) -> Result<i64, NotConnectedReason> {
    Ok(clock.now_ms().saturating_add(require_offset(offset)?))
}

/// Re-applies redaction to any error that crosses the client boundary, even if a transport built
/// the variant without going through the redacting constructors.
pub fn sanitize_error(e: AdapterError) -> AdapterError {
    match e {
        AdapterError::Network(m) => AdapterError::network(m),
        AdapterError::Parse(m) => AdapterError::parse(m),
        AdapterError::Incomplete(m) => AdapterError::incomplete(m),
        AdapterError::Exchange { code, message } => AdapterError::exchange(code, message),
        other => other,
    }
}

/// 429 and Binance's 418 (IP ban) become `RateLimited`; every other non-2xx becomes `Http`.
pub fn check_status(resp: HttpResponse) -> Result<HttpResponse, AdapterError> {
    match resp.status {
        200..=299 => Ok(resp),
        429 | 418 => {
            let retry_after_ms = resp.header_value("retry-after").and_then(|v| v.trim().parse::<u64>().ok()).map(|s| s.saturating_mul(1000));
            Err(AdapterError::RateLimited { retry_after_ms })
        }
        status => Err(AdapterError::Http { status }),
    }
}

/// serde's error text names only a position, never the body, so nothing sensitive is echoed.
pub fn parse_json(body: &str) -> Result<Value, AdapterError> {
    serde_json::from_str(body).map_err(|e| AdapterError::parse(format!("invalid JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ManualClock, MemorySecrets, SecretError};

    // Expected values below were computed independently with Python's hmac/hashlib:
    //   hmac.new(secret.encode(), message.encode(), hashlib.sha256).hexdigest()
    const SECRET: &str = "TEST_SECRET_NOT_REAL";
    const KEY: &str = "TEST_KEY_NOT_REAL";

    #[test]
    fn hmac_matches_binance_documentation_vector() {
        // Example from Binance's public API documentation (its own demo secret).
        let sig = hmac_sha256_hex(
            "NhqPtmdSJYdKjVHjA7PZj4Mge3R5YNiP1e3UZjInClVN65XAbvqqM6A7H5fATj0j",
            "symbol=LTCBTC&side=BUY&type=LIMIT&timeInForce=GTC&quantity=1&price=0.1&recvWindow=5000&timestamp=1499827319559",
        )
        .unwrap();
        assert_eq!(sig, "c8db56825ae71d6d79447849e617115f4a920fa2acdcab2b053c4b2838bd6b71");
    }

    #[test]
    fn binance_signature_is_hmac_of_the_query_string() {
        let sig = binance_signature(SECRET, "timestamp=1700000001200&recvWindow=5000").unwrap();
        assert_eq!(sig, "5ad0e24bfec9202bf18cbc664ac9a72192a4d5363344bf19227160d8d9179401");
    }

    #[test]
    fn bybit_signature_prehash_is_timestamp_key_recvwindow_query() {
        let sig = bybit_signature(SECRET, 1_700_000_001_200, KEY, 5000, "category=linear&settleCoin=USDT&limit=200").unwrap();
        assert_eq!(sig, "429d142587bebbfdd9e78d247e8340e26b434d9bf329584048ace1c7ac1985b9");
    }

    #[test]
    fn percent_encoding_keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(percent_encode("a b&c=d%"), "a%20b%26c%3Dd%25");
        assert_eq!(encode_query(&[("category", "linear".into()), ("cursor", "x%3D".into())]), "category=linear&cursor=x%253D");
        assert_eq!(encode_query(&[]), "");
    }

    struct FailingSecrets;
    impl SecretProvider for FailingSecrets {
        fn get(&self, _e: Exchange, _n: SecretName) -> Result<Option<String>, SecretError> {
            Err(SecretError::Unavailable("keychain locked".into()))
        }
    }

    #[test]
    fn credentials_require_both_values_and_never_default() {
        let both = MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY).with(Exchange::Bybit, SecretName::ApiSecret, SECRET);
        let c = load_credentials(&both, Exchange::Bybit, false).unwrap();
        assert_eq!((c.api_key.as_str(), c.api_secret.as_str()), (KEY, SECRET));

        let only_key = MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY);
        assert_eq!(load_credentials(&only_key, Exchange::Bybit, false).unwrap_err(), NotConnectedReason::NoSecret);
        let only_secret = MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiSecret, SECRET);
        assert_eq!(load_credentials(&only_secret, Exchange::Bybit, false).unwrap_err(), NotConnectedReason::NoKey);
        let none = MemorySecrets::default();
        assert_eq!(load_credentials(&none, Exchange::Bybit, false).unwrap_err(), NotConnectedReason::NoKey);
        assert_eq!(load_credentials(&FailingSecrets, Exchange::Bybit, false).unwrap_err(), NotConnectedReason::SecretStoreError);
        let empty_key = MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, "").with(Exchange::Bybit, SecretName::ApiSecret, SECRET);
        assert_eq!(load_credentials(&empty_key, Exchange::Bybit, false).unwrap_err(), NotConnectedReason::NoKey);
        let empty_secret = MemorySecrets::default().with(Exchange::Bybit, SecretName::ApiKey, KEY).with(Exchange::Bybit, SecretName::ApiSecret, "");
        assert_eq!(load_credentials(&empty_secret, Exchange::Bybit, false).unwrap_err(), NotConnectedReason::NoSecret);
        // another exchange's secrets are never used
        assert_eq!(load_credentials(&both, Exchange::Binance, false).unwrap_err(), NotConnectedReason::NoKey);
    }

    #[test]
    fn passphrase_is_checked_last_and_only_when_required() {
        let no_pass = MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, KEY).with(Exchange::Okx, SecretName::ApiSecret, SECRET);
        assert!(load_credentials(&no_pass, Exchange::Okx, false).is_ok());
        assert_eq!(load_credentials(&no_pass, Exchange::Okx, true).unwrap_err(), NotConnectedReason::NoPassphrase);
        let with_pass = MemorySecrets::default().with(Exchange::Okx, SecretName::ApiKey, KEY).with(Exchange::Okx, SecretName::ApiSecret, SECRET).with(Exchange::Okx, SecretName::Passphrase, "TEST_PASS_NOT_REAL");
        assert!(load_credentials(&with_pass, Exchange::Okx, true).is_ok());
    }

    #[test]
    fn every_reason_maps_to_not_connected_and_the_variant_names_hold_no_secret() {
        for r in [NotConnectedReason::NoKey, NotConnectedReason::NoSecret, NotConnectedReason::NoPassphrase, NotConnectedReason::ClockUnsynced, NotConnectedReason::SecretStoreError] {
            assert_eq!(AdapterError::from(r), AdapterError::NotConnected);
        }
    }

    #[test]
    fn timestamp_rejection_is_recognised_by_code() {
        let ex = |c: &str| AdapterError::Exchange { code: c.into(), message: "m".into() };
        assert!(is_timestamp_rejected(&ex("-1021")));
        assert!(is_timestamp_rejected(&ex("10002")));
        assert!(!is_timestamp_rejected(&ex("-2015")));
        assert!(!is_timestamp_rejected(&ex("10003")));
        assert!(!is_timestamp_rejected(&AdapterError::Timeout));
        assert!(!is_timestamp_rejected(&AdapterError::Http { status: 400 }));
    }

    #[test]
    fn cursor_encoding_rule_is_percent_encoding_of_every_reserved_character() {
        assert_eq!(encode_cursor("page_token%3D1%26"), "page_token%253D1%2526");
        assert_eq!(encode_cursor("a=b&c"), "a%3Db%26c");
        assert_eq!(encode_cursor("100%"), "100%25");
        assert_eq!(encode_cursor("AZaz09-._~"), "AZaz09-._~");
        // must agree with what encode_query does to a value
        assert_eq!(format!("cursor={}", encode_cursor("x=%&")), encode_query(&[("cursor", "x=%&".to_string())]));
    }

    #[test]
    fn credentials_debug_does_not_print_values() {
        let c = Credentials { api_key: KEY.into(), api_secret: SECRET.into() };
        let s = format!("{c:?}");
        assert!(!s.contains(KEY) && !s.contains(SECRET));
    }

    #[test]
    fn timestamp_is_local_clock_plus_offset_and_requires_calibration() {
        let clock = ManualClock::new(1_700_000_000_000);
        assert_eq!(signed_timestamp(&clock, &|| Some(1200)).unwrap(), 1_700_000_001_200);
        assert_eq!(signed_timestamp(&clock, &|| Some(-300)).unwrap(), 1_699_999_999_700);
        assert_eq!(signed_timestamp(&clock, &|| None).unwrap_err(), NotConnectedReason::ClockUnsynced);
    }

    #[test]
    fn sanitize_redacts_every_message_carrying_variant() {
        let url = "https://h/x?timestamp=1&signature=abcdef0123456789";
        for e in [
            AdapterError::Network(format!("error sending request for url ({url})")),
            AdapterError::Parse(format!("bad body at {url}")),
            AdapterError::Incomplete(format!("page 2 failed {url}")),
            AdapterError::Exchange { code: "1".into(), message: format!("rejected {url}") },
        ] {
            let text = sanitize_error(e).to_string();
            assert!(!text.contains("abcdef0123456789"), "leaked: {text}");
            assert!(text.contains("timestamp=1"));
        }
        assert_eq!(sanitize_error(AdapterError::Timeout), AdapterError::Timeout);
        assert_eq!(sanitize_error(AdapterError::NotConnected), AdapterError::NotConnected);
    }

    #[test]
    fn status_mapping() {
        assert!(check_status(HttpResponse::ok("{}")).is_ok());
        let mut r = HttpResponse::with_status(429, "slow down");
        r.headers.push(("Retry-After".into(), "7".into()));
        assert_eq!(check_status(r).unwrap_err(), AdapterError::RateLimited { retry_after_ms: Some(7000) });
        assert_eq!(check_status(HttpResponse::with_status(429, "")).unwrap_err(), AdapterError::RateLimited { retry_after_ms: None });
        assert_eq!(check_status(HttpResponse::with_status(418, "")).unwrap_err(), AdapterError::RateLimited { retry_after_ms: None });
        assert_eq!(check_status(HttpResponse::with_status(500, "x")).unwrap_err(), AdapterError::Http { status: 500 });
        assert_eq!(check_status(HttpResponse::with_status(401, "x")).unwrap_err(), AdapterError::Http { status: 401 });
    }

    #[test]
    fn invalid_json_is_a_parse_error_without_secrets() {
        assert!(parse_json("{\"a\":1}").is_ok());
        let e = parse_json("not json signature=abc123").unwrap_err();
        assert!(matches!(e, AdapterError::Parse(_)));
        assert!(!e.to_string().contains("abc123"));
    }
}

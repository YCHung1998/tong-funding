//! API-key storage in the macOS Keychain, plus redaction of event payloads
//! (spec: secret-storage; tasks 4.1, 4.2).
//!
//! Calls into `keyring` sit behind the small [`KeyStore`] seam so unit tests never touch the real
//! Keychain. Callers must treat any `Err` and any `Ok(None)` from [`SecretProvider::get`] as
//! "not connected" and send no request.
#![allow(dead_code)]

use serde_json::Value;
use tong_funding_core::redact::{PLACEHOLDER, is_sensitive_name, redact_secrets};
use tong_funding_core::types::Exchange;

use crate::ports::{SecretError, SecretName, SecretProvider};

/// Keychain service name under which every account is stored.
pub const SERVICE: &str = "tong-funding";

/// Account name for one secret: `{Exchange}:{name}`, e.g. `Binance:api_key`, `OKX:passphrase`.
pub fn account_name(exchange: Exchange, name: SecretName) -> String {
    let n = match name {
        SecretName::ApiKey => "api_key",
        SecretName::ApiSecret => "api_secret",
        SecretName::Passphrase => "passphrase",
    };
    format!("{}:{n}", exchange.name())
}

fn unavailable(e: keyring::Error) -> SecretError {
    SecretError::Unavailable(redact_secrets(&e.to_string()))
}

/// Error messages never carry secret material, whatever the store put in them.
fn scrub(e: SecretError) -> SecretError {
    match e {
        SecretError::Unavailable(m) => SecretError::Unavailable(redact_secrets(&m)),
    }
}

/// Minimal key/value seam over the OS secret store, keyed by account name.
pub trait KeyStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError>;
    /// Deleting a missing account succeeds (idempotent).
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// The real macOS Keychain (via `keyring` 3.x, service [`SERVICE`]).
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    pub fn new() -> Self {
        KeychainStore { service: SERVICE.to_string() }
    }

    /// Same store under another service name (used by the manual test so it never touches
    /// real credentials).
    pub fn with_service(service: &str) -> Self {
        KeychainStore { service: service.to_string() }
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(&self.service, account).map_err(unavailable)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }
    fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&self.service, account).map_err(unavailable)?;
        entry.set_password(value).map_err(unavailable)
    }
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&self.service, account).map_err(unavailable)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(unavailable(e)),
        }
    }
}

/// `SecretProvider` + store/delete on top of any [`KeyStore`].
pub struct KeychainSecrets<S: KeyStore = KeychainStore> {
    store: S,
}

impl KeychainSecrets<KeychainStore> {
    pub fn system() -> Self {
        KeychainSecrets { store: KeychainStore::new() }
    }
}

impl<S: KeyStore> KeychainSecrets<S> {
    pub fn new(store: S) -> Self {
        KeychainSecrets { store }
    }

    /// Stores (or overwrites) a secret. Empty values are rejected.
    pub fn set_secret(&self, exchange: Exchange, name: SecretName, value: &str) -> Result<(), SecretError> {
        if value.is_empty() {
            return Err(SecretError::Unavailable("refusing to store an empty secret".into()));
        }
        self.store.set(&account_name(exchange, name), value).map_err(|e| match scrub(e) {
            // a store may echo the value it failed to write; drop it verbatim too
            SecretError::Unavailable(m) => SecretError::Unavailable(m.replace(value, PLACEHOLDER)),
        })
    }

    /// Removes a secret; removing a missing one is not an error.
    pub fn delete_secret(&self, exchange: Exchange, name: SecretName) -> Result<(), SecretError> {
        self.store.delete(&account_name(exchange, name)).map_err(scrub)
    }
}

impl<S: KeyStore> SecretProvider for KeychainSecrets<S> {
    fn get(&self, exchange: Exchange, name: SecretName) -> Result<Option<String>, SecretError> {
        // An empty stored value is never usable: report it as missing, not as a credential.
        Ok(self.store.get(&account_name(exchange, name)).map_err(scrub)?.filter(|v| !v.is_empty()))
    }
}

/// Returns `value` made safe to store in `events.payload`, recursively:
/// - an object entry whose KEY is a sensitive name (case/whitespace-insensitive) has its whole
///   value (string, number, array or object) replaced by [`PLACEHOLDER`];
/// - object keys and every string leaf go through `redact_secrets` (a key may be a URL);
/// - a `[name, value]` pair array with a sensitive name has its value replaced.
///
/// Structure and non-sensitive non-string values are kept. Idempotent. If two keys collapse to the
/// same text after redaction the later one wins (never reached for ordinary payloads).
pub fn safe_event_payload(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_secrets(&s)),
        Value::Array(a) => {
            let is_pair = a.len() == 2 && matches!(&a[0], Value::String(n) if is_sensitive_name(n));
            let mut it = a.into_iter();
            if is_pair {
                let name = it.next().map(safe_event_payload).unwrap_or(Value::Null);
                Value::Array(vec![name, Value::String(PLACEHOLDER.to_string())])
            } else {
                Value::Array(it.map(safe_event_payload).collect())
            }
        }
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| {
                    let masked = if is_sensitive_name(&k) { Value::String(PLACEHOLDER.to_string()) } else { safe_event_payload(v) };
                    (redact_secrets(&k), masked)
                })
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::store::schema::SCHEMA_V1;

    /// In-memory KeyStore; `fail` makes every call error like a locked/denied Keychain.
    #[derive(Default)]
    struct MemStore {
        map: Mutex<HashMap<String, String>>,
        fail: bool,
    }

    impl MemStore {
        fn failing() -> Self {
            MemStore { map: Mutex::default(), fail: true }
        }
        fn err() -> SecretError {
            SecretError::Unavailable("denied".into())
        }
    }

    impl KeyStore for MemStore {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            if self.fail {
                return Err(Self::err());
            }
            Ok(self.map.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
            if self.fail {
                return Err(Self::err());
            }
            self.map.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            if self.fail {
                return Err(Self::err());
            }
            self.map.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[test]
    fn account_names_are_exchange_colon_name() {
        assert_eq!(account_name(Exchange::Binance, SecretName::ApiKey), "Binance:api_key");
        assert_eq!(account_name(Exchange::Bybit, SecretName::ApiSecret), "Bybit:api_secret");
        assert_eq!(account_name(Exchange::Okx, SecretName::Passphrase), "OKX:passphrase");
    }

    #[test]
    fn set_then_get_round_trips_per_exchange_and_name() {
        let s = KeychainSecrets::new(MemStore::default());
        s.set_secret(Exchange::Binance, SecretName::ApiKey, "bk").unwrap();
        s.set_secret(Exchange::Bybit, SecretName::ApiKey, "yk").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("bk"));
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiKey).unwrap().as_deref(), Some("yk"));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap(), None);
    }

    #[test]
    fn set_overwrites() {
        let s = KeychainSecrets::new(MemStore::default());
        s.set_secret(Exchange::Okx, SecretName::Passphrase, "old").unwrap();
        s.set_secret(Exchange::Okx, SecretName::Passphrase, "new").unwrap();
        assert_eq!(s.get(Exchange::Okx, SecretName::Passphrase).unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn delete_removes_and_is_idempotent() {
        let s = KeychainSecrets::new(MemStore::default());
        s.set_secret(Exchange::Binance, SecretName::ApiSecret, "x").unwrap();
        s.delete_secret(Exchange::Binance, SecretName::ApiSecret).unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap(), None);
        s.delete_secret(Exchange::Binance, SecretName::ApiSecret).unwrap();
    }

    #[test]
    fn missing_item_is_none_not_error() {
        let s = KeychainSecrets::new(MemStore::default());
        assert_eq!(s.get(Exchange::Okx, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn read_failure_is_an_error_never_a_default() {
        let s = KeychainSecrets::new(MemStore::failing());
        assert!(matches!(s.get(Exchange::Binance, SecretName::ApiKey), Err(SecretError::Unavailable(_))));
    }

    #[test]
    fn write_and_delete_failures_surface() {
        let s = KeychainSecrets::new(MemStore::failing());
        assert!(s.set_secret(Exchange::Binance, SecretName::ApiKey, "k").is_err());
        assert!(s.delete_secret(Exchange::Binance, SecretName::ApiKey).is_err());
    }

    #[test]
    fn empty_value_is_rejected_on_set_and_treated_as_missing_on_get() {
        let s = KeychainSecrets::new(MemStore::default());
        assert!(s.set_secret(Exchange::Binance, SecretName::ApiKey, "").is_err());
        // an empty string already sitting in the store must not be returned as a usable secret
        s.store.set("Binance:api_key", "").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn works_through_the_secret_provider_trait_object() {
        let s = KeychainSecrets::new(MemStore::default());
        s.set_secret(Exchange::Binance, SecretName::ApiKey, "k").unwrap();
        let p: &dyn SecretProvider = &s;
        assert_eq!(p.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("k"));
    }

    // ---- 4.2 redaction integration ----

    #[test]
    fn safe_event_payload_redacts_nested_strings_and_keeps_the_rest() {
        let v = json!({
            "error": "timeout GET https://x/api?symbol=BTCUSDT&timestamp=1&signature=abcdef",
            "n": 5, "f": 1.5, "b": true, "nil": null,
            "nested": { "hdr": "X-MBX-APIKEY: realkey", "list": ["ok", "api_key=zzz", 3] }
        });
        let out = safe_event_payload(v);
        let s = out.to_string();
        assert!(!s.contains("abcdef") && !s.contains("realkey") && !s.contains("zzz"), "{s}");
        assert!(s.contains("symbol=BTCUSDT") && s.contains("timestamp=1"), "{s}");
        assert_eq!(out["n"], 5);
        assert_eq!(out["f"], 1.5);
        assert_eq!(out["b"], true);
        assert!(out["nil"].is_null());
        assert_eq!(out["nested"]["list"][0], "ok");
        assert_eq!(out["nested"]["list"][2], 3);
    }

    #[test]
    fn safe_event_payload_masks_values_under_sensitive_field_names() {
        let out = safe_event_payload(json!({
            "apiKey": "k1", "Signature": "s1", "OK-ACCESS-SIGN": "s2", "symbol": "BTCUSDT",
            "inner": [{ "api_key": "k2" }]
        }));
        let s = out.to_string();
        for leaked in ["k1", "s1", "s2", "k2"] {
            assert!(!s.contains(&format!("\"{leaked}\"")), "{leaked} leaked: {s}");
        }
        assert_eq!(out["symbol"], "BTCUSDT");
        assert_eq!(out["apiKey"], PLACEHOLDER);
    }

    #[test]
    fn safe_event_payload_is_idempotent() {
        let v = json!({"e": "a?signature=abc&symbol=X"});
        let once = safe_event_payload(v);
        assert_eq!(safe_event_payload(once.clone()), once);
    }

    // ---- redteam cases for safe_event_payload ----

    fn assert_no_leak(v: Value, secret: &str) -> Value {
        let out = safe_event_payload(v);
        let s = out.to_string();
        assert!(!s.contains(secret), "{secret} leaked: {s}");
        assert_eq!(safe_event_payload(out.clone()), out, "not idempotent");
        out
    }

    #[test]
    fn sensitive_key_masks_the_whole_subtree_whatever_its_shape() {
        assert_no_leak(json!({"apiKey": ["S3CR3T"]}), "S3CR3T");
        assert_no_leak(json!({"X-MBX-APIKEY": {"v": "S3CR3T"}}), "S3CR3T");
        assert_no_leak(json!({"api_key": 123456789}), "123456789");
        assert_no_leak(json!({"signature": {"a": [{"b": "S3CR3T"}]}}), "S3CR3T");
        let out = safe_event_payload(json!({"apiKey": ["S3CR3T"], "symbol": "BTCUSDT", "n": 1}));
        assert_eq!(out["apiKey"], PLACEHOLDER);
        assert_eq!(out["symbol"], "BTCUSDT");
        assert_eq!(out["n"], 1);
    }

    #[test]
    fn every_listed_sensitive_name_is_masked_in_any_case_with_padding() {
        for name in [
            "apiKey", "api_key", "api_secret", "secretKey", "secret", "passphrase", "signature",
            "X-MBX-APIKEY", "X-BAPI-API-KEY", "X-BAPI-SIGN", "OK-ACCESS-KEY", "OK-ACCESS-SIGN", "OK-ACCESS-PASSPHRASE",
        ] {
            for k in [name.to_string(), name.to_uppercase(), format!("  {name}\t"), format!("{name} ")] {
                let out = safe_event_payload(json!({ k.clone(): "S3CR3T" }));
                assert!(!out.to_string().contains("S3CR3T"), "key {k:?}: {out}");
            }
        }
    }

    #[test]
    fn name_value_pair_arrays_mask_the_value() {
        let out = assert_no_leak(json!([["X-MBX-APIKEY", "S3CR3T"], ["symbol", "BTCUSDT"]]), "S3CR3T");
        assert_eq!(out[1][1], "BTCUSDT");
        assert_no_leak(json!({"headers": [["OK-ACCESS-SIGN", {"x": "S3CR3T"}]]}), "S3CR3T");
        assert_no_leak(json!([["  Signature ", 987654321]]), "987654321");
    }

    #[test]
    fn keys_are_redacted_too() {
        assert_no_leak(json!({"https://x/api?symbol=B&signature=S3CR3T": 1}), "S3CR3T");
        assert_no_leak(json!({"https://x/api?symbol=B&signature%3DS3CR3T": 1}), "S3CR3T");
        assert_no_leak(json!({"outer": {"GET /x?api_key=S3CR3T": ["a"]}}), "S3CR3T");
    }

    #[test]
    fn leaf_formats_tab_newline_single_quote_escaped_json_and_okx_passphrase() {
        assert_no_leak(json!("signature=\tS3CR3T"), "S3CR3T");
        assert_no_leak(json!("signature:\nS3CR3T"), "S3CR3T");
        assert_no_leak(json!("{'api_secret': 'S3CR3T'}"), "S3CR3T");
        assert_no_leak(json!("x signature%3DS3CR3T&y=1"), "S3CR3T");
        assert_no_leak(json!({"body": "{\"secretKey\": \"S3CR3T\"}"}), "S3CR3T");
        let two = json!({"inner": json!({"msg": "{\"apiKey\":\"S3CR3T\"}"}).to_string()});
        assert_no_leak(two, "S3CR3T");
        assert_no_leak(json!("OK-ACCESS-PASSPHRASE: my pass S3CR3T phrase"), "S3CR3T");
        assert_no_leak(json!({"e": "passphrase=my pass S3CR3T"}), "S3CR3T");
    }

    #[test]
    fn structure_and_non_sensitive_values_survive() {
        let v = json!({"a": [1, 2.5, true, null, {"b": "x"}], "pair": ["k", "v"], "s": "plain"});
        assert_eq!(safe_event_payload(v.clone()), v);
    }

    // ---- 4.1 contract: Ok(None) / Ok(None) / Err(Unavailable) with redacted message ----

    #[test]
    fn get_contract_empty_missing_and_error() {
        let s = KeychainSecrets::new(MemStore::default());
        s.store.set("Binance:api_key", "").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey), Ok(None), "stored empty");
        assert_eq!(s.get(Exchange::Okx, SecretName::ApiKey), Ok(None), "missing");
        let f = KeychainSecrets::new(MemStore::failing());
        assert!(matches!(f.get(Exchange::Binance, SecretName::ApiKey), Err(SecretError::Unavailable(_))), "error");
    }

    struct LeakyStore;
    impl KeyStore for LeakyStore {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            Err(SecretError::Unavailable("denied: api_key=S3CR3T signature=SIG123".into()))
        }
        fn set(&self, a: &str, v: &str) -> Result<(), SecretError> {
            self.get(a).map(|_| ()).map_err(|_| SecretError::Unavailable(format!("cannot set {v}: api_key=S3CR3T")))
        }
        fn delete(&self, a: &str) -> Result<(), SecretError> {
            self.get(a).map(|_| ())
        }
    }

    #[test]
    fn error_messages_from_any_store_are_redacted() {
        let s = KeychainSecrets::new(LeakyStore);
        let Err(SecretError::Unavailable(m)) = s.get(Exchange::Binance, SecretName::ApiKey) else { panic!() };
        assert!(!m.contains("S3CR3T") && !m.contains("SIG123"), "{m}");
        let Err(SecretError::Unavailable(m)) = s.set_secret(Exchange::Binance, SecretName::ApiKey, "VALUE-XYZ") else { panic!() };
        assert!(!m.contains("S3CR3T") && !m.contains("VALUE-XYZ"), "{m}");
        let Err(SecretError::Unavailable(m)) = s.delete_secret(Exchange::Binance, SecretName::ApiKey) else { panic!() };
        assert!(!m.contains("S3CR3T"), "{m}");
    }

    #[test]
    fn keyring_errors_are_redacted_by_the_real_store_adapter() {
        let e = keyring::Error::PlatformFailure(Box::new(std::io::Error::other("boom api_key=S3CR3T")));
        let SecretError::Unavailable(m) = unavailable(e);
        assert!(!m.contains("S3CR3T"), "{m}");
        assert!(m.contains("boom"), "{m}");
    }

    fn mem_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA recursive_triggers = ON;").unwrap();
        c.execute_batch(SCHEMA_V1).unwrap();
        c
    }

    /// Every cell of every table, rendered as text (stand-in for scanning the db file).
    fn dump_all(c: &Connection) -> String {
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut out = String::new();
        for t in tables {
            let mut st = c.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
            let n = st.column_count();
            let mut rows = st.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for i in 0..n {
                    match row.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Text(b) | rusqlite::types::ValueRef::Blob(b) => {
                            out.push_str(&String::from_utf8_lossy(b))
                        }
                        other => out.push_str(&format!("{other:?}")),
                    }
                    out.push('|');
                }
                out.push('\n');
            }
        }
        out
    }

    const KEY: &str = "AKIA-SUPER-SECRET-KEY-123";
    const SIG: &str = "deadbeefcafe0123";

    fn leaky_payload() -> Value {
        json!({
            "error": format!("connect timeout https://fapi/x?symbol=BTCUSDT&signature={SIG}"),
            "headers": [format!("X-MBX-APIKEY: {KEY}"), format!("OK-ACCESS-PASSPHRASE={KEY}")],
            "extra": { "apiKey": KEY, "raw": format!("?x=1&api_key={KEY}&y=2") }
        })
    }

    #[test]
    fn database_scan_finds_no_secret_after_safe_payload_is_stored() {
        let c = mem_db();
        let payload = safe_event_payload(leaky_payload()).to_string();
        c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'FETCH_ERROR',?1)", [payload]).unwrap();
        let dump = dump_all(&c);
        assert!(dump.contains("FETCH_ERROR") && dump.contains("symbol=BTCUSDT"), "{dump}");
        assert!(!dump.contains(KEY), "key leaked: {dump}");
        assert!(!dump.contains(SIG), "signature leaked: {dump}");
    }

    #[test]
    fn database_scan_detects_a_leak_when_redaction_is_skipped() {
        // negative control: proves the scanner above can actually see secrets
        let c = mem_db();
        c.execute("INSERT INTO events (ts_ms, event_type, payload) VALUES (1,'X',?1)", [leaky_payload().to_string()]).unwrap();
        let dump = dump_all(&c);
        assert!(dump.contains(KEY) && dump.contains(SIG));
    }

    /// Real Keychain round trip. Pops a macOS authorization dialog and writes to the user's real
    /// Keychain, so it is ignored by default. Run by hand:
    /// `cargo test -p tong-funding store::secrets::tests::real_keychain -- --ignored --nocapture`
    #[test]
    #[ignore = "touches the real macOS Keychain"]
    fn real_keychain_round_trip() {
        // dedicated service name: never touches the user's real tong-funding credentials
        let s = KeychainSecrets::new(KeychainStore::with_service("tong-funding-test"));
        let (ex, nm) = (Exchange::Binance, SecretName::ApiKey);
        s.delete_secret(ex, nm).unwrap();
        assert_eq!(s.get(ex, nm).unwrap(), None);
        s.set_secret(ex, nm, "tong-funding-test-value").unwrap();
        assert_eq!(s.get(ex, nm).unwrap().as_deref(), Some("tong-funding-test-value"));
        s.delete_secret(ex, nm).unwrap();
        assert_eq!(s.get(ex, nm).unwrap(), None);
    }
}

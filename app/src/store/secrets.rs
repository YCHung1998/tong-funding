//! API-key storage in the macOS Keychain, plus redaction of event payloads
//! (spec: secret-storage; tasks 4.1, 4.2).
//!
//! Calls into `keyring` sit behind the small [`KeyStore`] seam so unit tests never touch the real
//! Keychain. Callers must treat any `Err` and any `Ok(None)` from [`SecretProvider::get`] as
//! "not connected" and send no request.
#![allow(dead_code)]

use serde_json::Value;
use tong_funding_core::redact::{PLACEHOLDER, redact_secrets};
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
    SecretError::Unavailable(e.to_string())
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
        self.store.set(&account_name(exchange, name), value)
    }

    /// Removes a secret; removing a missing one is not an error.
    pub fn delete_secret(&self, exchange: Exchange, name: SecretName) -> Result<(), SecretError> {
        self.store.delete(&account_name(exchange, name))
    }
}

impl<S: KeyStore> SecretProvider for KeychainSecrets<S> {
    fn get(&self, exchange: Exchange, name: SecretName) -> Result<Option<String>, SecretError> {
        // An empty stored value is never usable: report it as missing, not as a credential.
        Ok(self.store.get(&account_name(exchange, name))?.filter(|v| !v.is_empty()))
    }
}

/// Field names whose string value is a secret (the spec's query params and headers).
const SENSITIVE_KEYS: [&str; 9] = [
    "signature",
    "api_key",
    "apikey",
    "x-mbx-apikey",
    "x-bapi-api-key",
    "x-bapi-sign",
    "ok-access-key",
    "ok-access-sign",
    "ok-access-passphrase",
];

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k))
}

/// Returns `value` with every string leaf passed through `redact_secrets`; structure, keys and
/// non-string values are untouched. Use before anything goes into `events.payload`.
pub fn safe_event_payload(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_secrets(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(safe_event_payload).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| match v {
                    // `redact_secrets` only sees leaf text, so `{"apiKey": "secret"}` needs the key name.
                    Value::String(_) if is_sensitive_key(&k) => (k, Value::String(PLACEHOLDER.to_string())),
                    v => (k, safe_event_payload(v)),
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

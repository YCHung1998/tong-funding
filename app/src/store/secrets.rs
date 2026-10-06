//! API-key storage in the macOS Keychain (one bundle item, cached per process), plus redaction of event payloads
//! (spec: secret-storage; tasks 4.1, 4.2).
//!
//! Calls into `keyring` sit behind the small [`KeyStore`] seam so unit tests never touch the real
//! Keychain. Callers must treat any `Err` and any `Ok(None)` from [`SecretProvider::get`] as
//! "not connected" and send no request.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::Value;
use tong_funding_core::redact::{PLACEHOLDER, is_sensitive_name, redact_secrets, register_secret};
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

/// Account of the single Keychain item that holds every credential (a JSON map
/// `"<Exchange>:<name>" -> value`), so macOS asks for authorisation once, not once per secret.
pub const BUNDLE_ACCOUNT: &str = "credentials";

type Bundle = BTreeMap<String, String>;

/// Which secrets each exchange needs (OKX also needs a passphrase).
pub fn needed(exchange: Exchange) -> &'static [SecretName] {
    match exchange {
        Exchange::Okx => &[SecretName::ApiKey, SecretName::ApiSecret, SecretName::Passphrase],
        Exchange::Binance | Exchange::Bybit => &[SecretName::ApiKey, SecretName::ApiSecret],
    }
}

/// `SecretProvider` + store/delete on top of any [`KeyStore`]. The bundle item is read once per
/// process and cached, a failed read included (no retry, so a denied prompt does not come back
/// every poll); writes go through [`BundleSecrets::update`] and refresh the cache.
pub struct BundleSecrets<S: KeyStore = KeychainStore> {
    store: S,
    cache: Mutex<Option<Result<Bundle, SecretError>>>,
}

impl BundleSecrets<KeychainStore> {
    pub fn system() -> Self {
        BundleSecrets::new(KeychainStore::new())
    }
}

impl<S: KeyStore> BundleSecrets<S> {
    pub fn new(store: S) -> Self {
        BundleSecrets { store, cache: Mutex::new(None) }
    }

    /// Reads the bundle; when the item does not exist yet, migrates the legacy per-account items
    /// (kept in place) into it. A corrupt bundle is a failure: falling back to legacy items could
    /// silently use stale credentials.
    fn load(&self) -> Result<Bundle, SecretError> {
        match self.store.get(BUNDLE_ACCOUNT).map_err(scrub)? {
            Some(json) => serde_json::from_str::<Bundle>(&json).map_err(|_| {
                SecretError::Unavailable("credentials item is not valid JSON; rewrite it with `secrets import-env`".into())
            }),
            None => {
                let mut bundle = Bundle::new();
                for ex in Exchange::ALL {
                    for &name in needed(ex) {
                        let account = account_name(ex, name);
                        if let Some(v) = self.store.get(&account).map_err(scrub)?.filter(|v| !v.is_empty()) {
                            bundle.insert(account, v);
                        }
                    }
                }
                if !bundle.is_empty() {
                    let json = serde_json::to_string(&bundle).expect("string map serialises");
                    if let Err(e) = self.store.set(BUNDLE_ACCOUNT, &json) {
                        eprintln!("credentials migration: bundle not written, will retry next start: {}", scrub(e));
                    }
                }
                Ok(bundle)
            }
        }
    }

    /// Runs `f` on the cached bundle, loading it first if needed.
    fn with_cache<R>(&self, f: impl FnOnce(&mut Option<Result<Bundle, SecretError>>) -> R) -> R {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.is_none() {
            let loaded = self.load();
            if let Ok(b) = &loaded {
                b.values().for_each(|v| register_secret(v));
            }
            *cache = Some(loaded);
        }
        f(&mut cache)
    }

    /// Read-modify-write of the bundle: one load (cached), one store write, only if `f` changed
    /// something. Fails without writing when the bundle could not be read.
    fn update(&self, f: impl FnOnce(&mut Bundle)) -> Result<(), SecretError> {
        self.with_cache(|cache| {
            let mut next = cache.clone().expect("loaded")?;
            let before = next.clone();
            f(&mut next);
            if next == before {
                return Ok(());
            }
            let json = serde_json::to_string(&next).expect("string map serialises");
            // a store may echo the value it failed to write; drop every value verbatim
            self.store.set(BUNDLE_ACCOUNT, &json).map_err(|e| match scrub(e) {
                SecretError::Unavailable(m) => SecretError::Unavailable(next.values().fold(m, |m, v| m.replace(v.as_str(), PLACEHOLDER))),
            })?;
            *cache = Some(Ok(next));
            Ok(())
        })
    }

    /// Stores (or overwrites) a secret. Empty values are rejected.
    pub fn set_secret(&self, exchange: Exchange, name: SecretName, value: &str) -> Result<(), SecretError> {
        self.set_many(&[(exchange, name, value)])
    }

    /// Stores several secrets with a single bundle write. Empty values are rejected.
    pub fn set_many(&self, items: &[(Exchange, SecretName, &str)]) -> Result<(), SecretError> {
        if items.iter().any(|(_, _, v)| v.is_empty()) {
            return Err(SecretError::Unavailable("refusing to store an empty secret".into()));
        }
        items.iter().for_each(|(_, _, v)| register_secret(v));
        self.update(|b| {
            for (e, n, v) in items {
                b.insert(account_name(*e, *n), v.to_string());
            }
        })
    }

    /// Removes a secret; removing a missing one is not an error.
    pub fn delete_secret(&self, exchange: Exchange, name: SecretName) -> Result<(), SecretError> {
        self.update(|b| {
            b.remove(&account_name(exchange, name));
        })
    }
}

impl<S: KeyStore> SecretProvider for BundleSecrets<S> {
    fn get(&self, exchange: Exchange, name: SecretName) -> Result<Option<String>, SecretError> {
        // An empty stored value is never usable: report it as missing, not as a credential.
        self.with_cache(|cache| match cache.as_ref().expect("loaded") {
            Ok(b) => Ok(b.get(&account_name(exchange, name)).filter(|v| !v.is_empty()).cloned()),
            Err(e) => Err(e.clone()),
        })
    }
}

/// Deepest nesting kept; anything below is replaced by [`PLACEHOLDER`] (and torn down iteratively,
/// because dropping a very deep `Value` recursively would overflow the stack).
pub const MAX_PAYLOAD_DEPTH: usize = 64;

/// Object keys that name a field (`{"name": "X-MBX-APIKEY", "value": ...}`).
const NAME_FIELDS: [&str; 6] = ["name", "key", "header", "field", "param", "parameter"];
/// Object keys that carry the value of a name/value pair.
const VALUE_FIELDS: [&str; 6] = ["value", "val", "data", "v", "content", "contents"];

fn placeholder() -> Value {
    Value::String(PLACEHOLDER.to_string())
}

/// A string that IS a sensitive name (not text that merely mentions one, like `api_key=abc`, which
/// the leaf redaction already handles).
fn is_sensitive_str(v: &Value) -> bool {
    matches!(v, Value::String(s) if !s.contains(['=', ':', '&', '?', '/']) && is_sensitive_name(s))
}

/// Drops a possibly very deep value without recursion.
fn drop_iteratively(v: Value) {
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Array(a) => stack.extend(a),
            Value::Object(o) => stack.extend(o.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

/// Returns `value` made safe to store in `events.payload`, recursively:
/// - an object entry whose KEY is a sensitive name (same normalised rule as the text redactor:
///   case, punctuation, whitespace, zero-width and full-width forms are ignored) has its whole value
///   (string, number, array or object) replaced by [`PLACEHOLDER`];
/// - an object that names a field in one entry and carries its value in another
///   (`{"name": "X-MBX-APIKEY", "value": ...}`) has the value entry replaced;
/// - in an array, an element that is a sensitive name masks the element after it
///   (`["X-MBX-APIKEY", "S", "other", "v"]`); when the array has 2 or 3 elements and starts with
///   such a name, everything after the name is masked;
/// - object keys and every string leaf go through `redact_secrets` (a key may be a URL; registered
///   secrets are masked by exact value);
/// - nesting deeper than [`MAX_PAYLOAD_DEPTH`] is replaced by [`PLACEHOLDER`].
///
/// Structure and non-sensitive non-string values are kept. Idempotent. If two keys collapse to the
/// same text after redaction the later one wins (never reached for ordinary payloads).
pub fn safe_event_payload(value: Value) -> Value {
    safe_at(value, 0)
}

fn safe_at(value: Value, depth: usize) -> Value {
    if depth > MAX_PAYLOAD_DEPTH {
        drop_iteratively(value);
        return placeholder();
    }
    match value {
        Value::String(s) => Value::String(redact_secrets(&s)),
        Value::Array(a) => {
            let short_pair = (2..=3).contains(&a.len()) && is_sensitive_str(&a[0]);
            let mut out = Vec::with_capacity(a.len());
            let mut mask_next = false;
            for (i, el) in a.into_iter().enumerate() {
                if short_pair && i > 0 || mask_next {
                    drop_iteratively(el);
                    out.push(placeholder());
                    mask_next = false;
                } else {
                    mask_next = is_sensitive_str(&el);
                    out.push(safe_at(el, depth + 1));
                }
            }
            Value::Array(out)
        }
        Value::Object(o) => {
            // `{"name": "<sensitive>", "value": ...}`: the value entry is the secret
            let names_a_secret = o.iter().any(|(k, v)| NAME_FIELDS.iter().any(|f| k.trim().eq_ignore_ascii_case(f)) && is_sensitive_str(v));
            Value::Object(
                o.into_iter()
                    .map(|(k, v)| {
                        let sensitive = is_sensitive_name(&k) || (names_a_secret && VALUE_FIELDS.iter().any(|f| k.trim().eq_ignore_ascii_case(f)));
                        let masked = if sensitive {
                            drop_iteratively(v);
                            placeholder()
                        } else {
                            safe_at(v, depth + 1)
                        };
                        (redact_secrets(&k), masked)
                    })
                    .collect(),
            )
        }
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
        let s = BundleSecrets::new(MemStore::default());
        s.set_secret(Exchange::Binance, SecretName::ApiKey, "bk").unwrap();
        s.set_secret(Exchange::Bybit, SecretName::ApiKey, "yk").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("bk"));
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiKey).unwrap().as_deref(), Some("yk"));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap(), None);
    }

    #[test]
    fn set_overwrites() {
        let s = BundleSecrets::new(MemStore::default());
        s.set_secret(Exchange::Okx, SecretName::Passphrase, "old").unwrap();
        s.set_secret(Exchange::Okx, SecretName::Passphrase, "new").unwrap();
        assert_eq!(s.get(Exchange::Okx, SecretName::Passphrase).unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn delete_removes_and_is_idempotent() {
        let s = BundleSecrets::new(MemStore::default());
        s.set_secret(Exchange::Binance, SecretName::ApiSecret, "x").unwrap();
        s.delete_secret(Exchange::Binance, SecretName::ApiSecret).unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap(), None);
        s.delete_secret(Exchange::Binance, SecretName::ApiSecret).unwrap();
    }

    #[test]
    fn missing_item_is_none_not_error() {
        let s = BundleSecrets::new(MemStore::default());
        assert_eq!(s.get(Exchange::Okx, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn read_failure_is_an_error_never_a_default() {
        let s = BundleSecrets::new(MemStore::failing());
        assert!(matches!(s.get(Exchange::Binance, SecretName::ApiKey), Err(SecretError::Unavailable(_))));
    }

    #[test]
    fn write_and_delete_failures_surface() {
        let s = BundleSecrets::new(MemStore::failing());
        assert!(s.set_secret(Exchange::Binance, SecretName::ApiKey, "k").is_err());
        assert!(s.delete_secret(Exchange::Binance, SecretName::ApiKey).is_err());
    }

    #[test]
    fn empty_value_is_rejected_on_set_and_treated_as_missing_on_get() {
        let s = BundleSecrets::new(MemStore::default());
        assert!(s.set_secret(Exchange::Binance, SecretName::ApiKey, "").is_err());
        // an empty string already sitting in the store must not be returned as a usable secret
        s.store.set("Binance:api_key", "").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn works_through_the_secret_provider_trait_object() {
        let s = BundleSecrets::new(MemStore::default());
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
        let s = BundleSecrets::new(MemStore::default());
        s.store.set("Binance:api_key", "").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey), Ok(None), "stored empty");
        assert_eq!(s.get(Exchange::Okx, SecretName::ApiKey), Ok(None), "missing");
        let f = BundleSecrets::new(MemStore::failing());
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
        let s = BundleSecrets::new(LeakyStore);
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

    // ---- second round ----

    #[test]
    fn secrets_read_from_the_keychain_are_tracked_by_exact_value() {
        let s = BundleSecrets::new(MemStore::default());
        s.store.set("Binance:api_secret", "kc-get-Zx81-unique").unwrap();
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap().as_deref(), Some("kc-get-Zx81-unique"));
        for text in ["x kc-get-Zx81-unique y", "weird_field\t=kc-get-Zx81-unique", "kc%2Dget%2DZx81%2Dunique"] {
            assert!(!redact_secrets(text).contains("Zx81"), "{text}");
        }
        // and therefore in any payload field, under any name
        let out = safe_event_payload(json!({"harmless_name": "see kc-get-Zx81-unique here", "n": ["kc-get-Zx81-unique"]}));
        assert!(!out.to_string().contains("Zx81"), "{out}");
    }

    #[test]
    fn secrets_written_to_the_keychain_are_tracked_too() {
        let s = BundleSecrets::new(MemStore::default());
        s.set_secret(Exchange::Okx, SecretName::Passphrase, "kc-set-Qp44-unique pass").unwrap();
        assert!(!redact_secrets("a kc-set-Qp44-unique pass b").contains("Qp44"));
    }

    #[test]
    fn name_and_value_in_separate_fields_is_masked() {
        let out = assert_no_leak(json!({"name": "X-MBX-APIKEY", "value": "S3CR3T"}), "S3CR3T");
        assert_eq!(out["name"], "X-MBX-APIKEY");
        assert_no_leak(json!({"header": "OK-ACCESS-SIGN", "val": {"deep": ["S3CR3T"]}}), "S3CR3T");
        assert_no_leak(json!({"key": "binanceApiKey", "data": 123456789}), "123456789");
        assert_no_leak(json!([{"name": "Authorization", "value": "Bearer S3CR3T"}]), "S3CR3T");
        let ok = safe_event_payload(json!({"name": "symbol", "value": "BTCUSDT"}));
        assert_eq!(ok["value"], "BTCUSDT");
    }

    #[test]
    fn name_followed_by_values_in_arrays_is_masked() {
        let out = assert_no_leak(json!(["X-MBX-APIKEY", "S3CR3T", "other", "visible"]), "S3CR3T");
        assert_eq!(out[3], "visible");
        assert_no_leak(json!(["signature", "S3CR3T", "x"]), "S3CR3T");
        assert_no_leak(json!(["a", "b", "OK-ACCESS-KEY", "S3CR3T"]), "S3CR3T");
        assert_no_leak(json!(["X-BAPI-SIGN", {"k": "S3CR3T"}]), "S3CR3T");
        assert_no_leak(json!(["Authorization", "S3CR3T", "x", "y"]), "S3CR3T");
    }

    #[test]
    fn normalised_sensitive_key_names_are_masked() {
        for k in ["binanceApiKey", "Authorization", "clientSecret", "access_token", "X-BAPI-SIGN", "api-key", "sign", "PASSWORD", "sig\u{200B}nature"] {
            let out = safe_event_payload(json!({ k: {"x": "S3CR3T"} }));
            assert!(!out.to_string().contains("S3CR3T"), "key {k:?}: {out}");
        }
    }

    #[test]
    fn deep_nesting_is_cut_without_overflowing_the_stack() {
        let mut v = json!("S3CR3T-deep");
        for _ in 0..50_000 {
            v = Value::Array(vec![v]);
        }
        let out = safe_event_payload(v);
        let s = out.to_string();
        assert!(!s.contains("S3CR3T-deep") && s.contains(PLACEHOLDER), "deep leaf survived");
        let mut o = json!({"k": "S3CR3T-deep-obj"});
        for _ in 0..50_000 {
            let mut m = serde_json::Map::new();
            m.insert("n".to_string(), o);
            o = Value::Object(m);
        }
        assert!(!safe_event_payload(o).to_string().contains("S3CR3T-deep-obj"));
        // moderate depth is kept intact
        let mut ok = json!("leaf");
        for _ in 0..40 {
            ok = Value::Array(vec![ok]);
        }
        assert_eq!(safe_event_payload(ok.clone()), ok);
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

    // ---- keychain-single-item: one bundle item, one read per process ----

    /// Counts every underlying store access, so tests can assert how many Keychain prompts a
    /// process would trigger. `fail_account` makes only that account error (e.g. a denied bundle).
    struct CountStore {
        inner: MemStore,
        gets: Mutex<Vec<String>>,
        sets: Mutex<Vec<String>>,
        fail_set: bool,
    }

    impl CountStore {
        fn new() -> Self {
            CountStore { inner: MemStore::default(), gets: Mutex::default(), sets: Mutex::default(), fail_set: false }
        }
        fn with(self, account: &str, value: &str) -> Self {
            self.inner.set(account, value).unwrap();
            self
        }
        fn gets(&self) -> Vec<String> {
            self.gets.lock().unwrap().clone()
        }
        fn sets(&self) -> Vec<String> {
            self.sets.lock().unwrap().clone()
        }
    }

    impl KeyStore for CountStore {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            self.gets.lock().unwrap().push(account.into());
            self.inner.get(account)
        }
        fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
            self.sets.lock().unwrap().push(account.into());
            if self.fail_set {
                return Err(MemStore::err());
            }
            self.inner.set(account, value)
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.inner.delete(account)
        }
    }

    const BUNDLE: &str = "credentials";

    fn bundle_json(s: &BundleSecrets<CountStore>) -> serde_json::Value {
        serde_json::from_str(&s.store.inner.get(BUNDLE).unwrap().expect("bundle item")).unwrap()
    }

    #[test]
    fn many_reads_touch_the_store_exactly_once() {
        let s = BundleSecrets::new(CountStore::new().with(BUNDLE, r#"{"Binance:api_key":"bk","Binance:api_secret":"bs","Bybit:api_key":"yk","Bybit:api_secret":"ys"}"#));
        for _ in 0..3 {
            for (ex, nm) in [(Exchange::Binance, SecretName::ApiKey), (Exchange::Binance, SecretName::ApiSecret), (Exchange::Bybit, SecretName::ApiKey), (Exchange::Bybit, SecretName::ApiSecret), (Exchange::Okx, SecretName::Passphrase)] {
                let _ = s.get(ex, nm);
            }
        }
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiSecret).unwrap().as_deref(), Some("ys"));
        assert_eq!(s.get(Exchange::Okx, SecretName::Passphrase).unwrap(), None);
        assert_eq!(s.store.gets(), vec![BUNDLE.to_string()], "only the bundle, once");
        assert!(s.store.sets().is_empty());
    }

    #[test]
    fn a_failed_load_is_cached_and_never_retried() {
        let s = BundleSecrets::new(MemStore::failing());
        let counted = BundleSecrets::new(CountingFail::default());
        for _ in 0..4 {
            assert!(matches!(counted.get(Exchange::Binance, SecretName::ApiKey), Err(SecretError::Unavailable(_))));
            assert!(matches!(counted.get(Exchange::Bybit, SecretName::ApiSecret), Err(SecretError::Unavailable(_))));
        }
        assert_eq!(*counted.store.0.lock().unwrap(), 1, "denied once, no re-prompt");
        assert!(s.get(Exchange::Okx, SecretName::ApiKey).is_err());
    }

    #[derive(Default)]
    struct CountingFail(Mutex<u32>);
    impl KeyStore for CountingFail {
        fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
            *self.0.lock().unwrap() += 1;
            Err(MemStore::err())
        }
        fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
            Err(MemStore::err())
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            Err(MemStore::err())
        }
    }

    #[test]
    fn set_writes_into_the_bundle_and_keeps_other_values() {
        let s = BundleSecrets::new(CountStore::new().with(BUNDLE, r#"{"Binance:api_key":"bk","Binance:api_secret":"bs"}"#));
        s.set_secret(Exchange::Bybit, SecretName::ApiKey, "yk").unwrap();
        assert_eq!(bundle_json(&s), json!({"Binance:api_key":"bk","Binance:api_secret":"bs","Bybit:api_key":"yk"}));
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiKey).unwrap().as_deref(), Some("yk"), "cache updated");
        assert_eq!(s.store.sets(), vec![BUNDLE.to_string()]);
        s.delete_secret(Exchange::Binance, SecretName::ApiKey).unwrap();
        assert_eq!(bundle_json(&s), json!({"Binance:api_secret":"bs","Bybit:api_key":"yk"}));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn set_many_writes_the_bundle_once() {
        let s = BundleSecrets::new(CountStore::new());
        s.set_many(&[(Exchange::Binance, SecretName::ApiKey, "a"), (Exchange::Binance, SecretName::ApiSecret, "b"), (Exchange::Okx, SecretName::Passphrase, "c")]).unwrap();
        assert_eq!(s.store.sets().len(), 1);
        assert_eq!(bundle_json(&s), json!({"Binance:api_key":"a","Binance:api_secret":"b","OKX:passphrase":"c"}));
    }

    #[test]
    fn empty_bundle_values_are_treated_as_unset() {
        let s = BundleSecrets::new(CountStore::new().with(BUNDLE, r#"{"Binance:api_key":""}"#));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap(), None);
    }

    #[test]
    fn legacy_items_migrate_into_the_bundle_once_and_are_kept() {
        let legacy = CountStore::new().with("Binance:api_key", "bk").with("Binance:api_secret", "bs").with("Bybit:api_key", "yk").with("Bybit:api_secret", "ys");
        let s = BundleSecrets::new(legacy);
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("bk"));
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiSecret).unwrap().as_deref(), Some("ys"));
        assert_eq!(bundle_json(&s), json!({"Binance:api_key":"bk","Binance:api_secret":"bs","Bybit:api_key":"yk","Bybit:api_secret":"ys"}));
        assert_eq!(s.store.inner.get("Binance:api_key").unwrap().as_deref(), Some("bk"), "legacy item kept");
        assert_eq!(s.store.sets(), vec![BUNDLE.to_string()]);
        // the next process sees the bundle and never reads a legacy account
        let next = BundleSecrets::new(CountStore { inner: MemStore { map: Mutex::new(s.store.inner.map.lock().unwrap().clone()), fail: false }, ..CountStore::new() });
        assert_eq!(next.get(Exchange::Bybit, SecretName::ApiKey).unwrap().as_deref(), Some("yk"));
        assert_eq!(next.store.gets(), vec![BUNDLE.to_string()]);
    }

    #[test]
    fn nothing_anywhere_means_unset_and_no_empty_bundle() {
        let s = BundleSecrets::new(CountStore::new());
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap(), None);
        assert!(s.store.sets().is_empty());
        assert_eq!(s.store.inner.get(BUNDLE).unwrap(), None);
    }

    #[test]
    fn migration_still_works_when_the_bundle_write_fails() {
        let mut store = CountStore::new().with("Binance:api_key", "bk-migr-fail");
        store.fail_set = true;
        let s = BundleSecrets::new(store);
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("bk-migr-fail"));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("bk-migr-fail"));
        assert_eq!(s.store.sets().len(), 1, "tried once; retried next process");
    }

    #[test]
    fn corrupt_bundle_is_a_failure_without_legacy_fallback_or_leak() {
        let s = BundleSecrets::new(CountStore::new().with(BUNDLE, "{not json S3CR3T-corrupt").with("Binance:api_key", "stale"));
        let Err(SecretError::Unavailable(m)) = s.get(Exchange::Binance, SecretName::ApiKey) else { panic!("expected failure") };
        assert!(!m.contains("S3CR3T-corrupt"), "{m}");
        assert_eq!(s.store.gets(), vec![BUNDLE.to_string()], "no legacy fallback");
        assert!(s.set_secret(Exchange::Binance, SecretName::ApiKey, "new").is_err(), "must not overwrite a bundle it cannot read");
        assert!(s.store.sets().is_empty());
    }

    /// Real Keychain round trip. Pops a macOS authorization dialog and writes to the user's real
    /// Keychain, so it is ignored by default. Run by hand:
    /// `cargo test -p tong-funding store::secrets::tests::real_keychain -- --ignored --nocapture`
    #[test]
    #[ignore = "touches the real macOS Keychain"]
    fn real_keychain_round_trip() {
        // dedicated service name: never touches the user's real tong-funding credentials
        let s = BundleSecrets::new(KeychainStore::with_service("tong-funding-test"));
        let (ex, nm) = (Exchange::Binance, SecretName::ApiKey);
        s.delete_secret(ex, nm).unwrap();
        assert_eq!(s.get(ex, nm).unwrap(), None);
        s.set_secret(ex, nm, "tong-funding-test-value").unwrap();
        assert_eq!(s.get(ex, nm).unwrap().as_deref(), Some("tong-funding-test-value"));
        s.delete_secret(ex, nm).unwrap();
        assert_eq!(s.get(ex, nm).unwrap(), None);
    }
}

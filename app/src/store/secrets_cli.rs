//! `tong-funding secrets` subcommand: put exchange API credentials into the macOS Keychain
//! without `.env` files, shell history or the repository (spec: secret-storage).
//!
//! ```text
//! tong-funding secrets set <binance|bybit|okx> <api-key|api-secret|passphrase>   (value on stdin)
//! tong-funding secrets import-env <path/to/.env>   (the Python version's .env format)
//! tong-funding secrets status
//! tong-funding secrets delete <binance|bybit|okx> <api-key|api-secret|passphrase>
//! ```
//!
//! Everything lives in one Keychain item (see `store::secrets::BundleSecrets`); each subcommand
//! reads it once and writes it back at most once.
//!
//! The value is read from stdin (one line), never from the command line, and is never printed.
//! Exit codes: 0 ok, 1 store error, 2 usage error.

use std::io::{BufRead, Write};

use tong_funding_core::redact::redact_secrets;
use tong_funding_core::types::Exchange;

use crate::ports::{SecretName, SecretProvider};
use crate::store::secrets::{BundleSecrets, KeyStore, needed};

pub const SUBCOMMAND: &str = "secrets";

const USAGE: &str = "usage:\n  tong-funding secrets set <binance|bybit|okx> <api-key|api-secret|passphrase>   (value on stdin)\n  tong-funding secrets import-env <path/to/.env>\n  tong-funding secrets status\n  tong-funding secrets delete <binance|bybit|okx> <api-key|api-secret|passphrase>";

fn parse_exchange(s: &str) -> Option<Exchange> {
    match s.to_ascii_lowercase().as_str() {
        "binance" => Some(Exchange::Binance),
        "bybit" => Some(Exchange::Bybit),
        "okx" => Some(Exchange::Okx),
        _ => None,
    }
}

fn parse_name(s: &str) -> Option<SecretName> {
    match s.to_ascii_lowercase().replace('_', "-").as_str() {
        "api-key" => Some(SecretName::ApiKey),
        "api-secret" => Some(SecretName::ApiSecret),
        "passphrase" => Some(SecretName::Passphrase),
        _ => None,
    }
}

fn name_str(n: SecretName) -> &'static str {
    match n {
        SecretName::ApiKey => "api-key",
        SecretName::ApiSecret => "api-secret",
        SecretName::Passphrase => "passphrase",
    }
}

fn target(args: &[String]) -> Result<(Exchange, SecretName), String> {
    match args {
        [e, n] => {
            let ex = parse_exchange(e).ok_or_else(|| format!("unknown exchange {e}"))?;
            let name = parse_name(n).ok_or_else(|| format!("unknown secret name {n}"))?;
            if !needed(ex).contains(&name) {
                return Err(format!("{} has no {}", ex.name(), name_str(name)));
            }
            Ok((ex, name))
        }
        _ => Err("expected <exchange> <name>".into()),
    }
}

/// Runs the subcommand on the real Keychain; returns the process exit code.
pub fn run(args: &[String], stdin: &mut dyn BufRead, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    run_with(&BundleSecrets::system(), args, stdin, out, err)
}

fn run_with<S: KeyStore>(secrets: &BundleSecrets<S>, args: &[String], stdin: &mut dyn BufRead, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let usage = |err: &mut dyn Write, msg: &str| {
        let _ = writeln!(err, "{msg}\n{USAGE}");
        2
    };
    match args.split_first() {
        Some((cmd, rest)) if cmd == "set" => {
            let (ex, name) = match target(rest) {
                Ok(t) => t,
                Err(e) => return usage(err, &e),
            };
            let mut line = String::new();
            if let Err(e) = stdin.read_line(&mut line) {
                let _ = writeln!(err, "cannot read the value from stdin: {}", redact_secrets(&e.to_string()));
                return 1;
            }
            let value = line.trim_end_matches(['\n', '\r']);
            if value.trim().is_empty() {
                let _ = writeln!(err, "empty value; nothing stored");
                return 1;
            }
            if value.trim() != value {
                let _ = writeln!(err, "the value has leading or trailing spaces; nothing stored (check the paste)");
                return 1;
            }
            match secrets.set_secret(ex, name, value) {
                Ok(()) => {
                    let _ = writeln!(out, "stored {} {} (a running app picks it up after a restart)", ex.name(), name_str(name));
                    0
                }
                Err(e) => {
                    let _ = writeln!(err, "{e}");
                    1
                }
            }
        }
        Some((cmd, rest)) if cmd == "delete" => {
            let (ex, name) = match target(rest) {
                Ok(t) => t,
                Err(e) => return usage(err, &e),
            };
            match secrets.delete_secret(ex, name) {
                Ok(()) => {
                    let _ = writeln!(out, "deleted {} {}", ex.name(), name_str(name));
                    0
                }
                Err(e) => {
                    let _ = writeln!(err, "{e}");
                    1
                }
            }
        }
        Some((cmd, rest)) if cmd == "import-env" => match rest {
            [path] => import_env(secrets, path, out, err),
            _ => usage(err, "expected <path/to/.env>"),
        },
        Some((cmd, rest)) if cmd == "status" && rest.is_empty() => {
            let mut code = 0;
            for ex in Exchange::ALL {
                for &name in needed(ex) {
                    let state = match secrets.get(ex, name) {
                        Ok(Some(_)) => "present",
                        Ok(None) => "missing",
                        Err(_) => {
                            code = 1;
                            "unreadable"
                        }
                    };
                    let _ = writeln!(out, "{:<8} {:<11} {state}", ex.name(), name_str(name));
                }
            }
            code
        }
        _ => usage(err, "unknown or missing secrets command"),
    }
}

/// `.env` variable names of the Python version and the secret each one holds.
const ENV_KEYS: [(&str, Exchange, SecretName); 7] = [
    ("BINANCE_API_KEY", Exchange::Binance, SecretName::ApiKey),
    ("BINANCE_API_SECRET", Exchange::Binance, SecretName::ApiSecret),
    ("BYBIT_API_KEY", Exchange::Bybit, SecretName::ApiKey),
    ("BYBIT_API_SECRET", Exchange::Bybit, SecretName::ApiSecret),
    ("OKX_DEMO_API_KEY", Exchange::Okx, SecretName::ApiKey),
    ("OKX_DEMO_API_SECRET", Exchange::Okx, SecretName::ApiSecret),
    ("OKX_DEMO_PASSPHRASE", Exchange::Okx, SecretName::Passphrase),
];

/// Stores every known credential of a Python-style `.env` file. Values are never printed: the
/// report names only variables and secrets. Other variables (base URLs, account type) are ignored.
fn import_env<S: KeyStore>(secrets: &BundleSecrets<S>, path: &str, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(err, "cannot read {path}: {}", redact_secrets(&e.to_string()));
            return 1;
        }
    };
    let mut found: Vec<(Exchange, SecretName, &str)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, raw)) = line.split_once('=') else { continue };
        let name = name.trim().trim_start_matches("export ").trim();
        let raw = raw.trim();
        let value = match (raw.chars().next(), raw.chars().last()) {
            (Some('"'), Some('"')) | (Some('\''), Some('\'')) if raw.len() >= 2 => &raw[1..raw.len() - 1],
            _ => raw,
        };
        let Some(&(_, ex, secret)) = ENV_KEYS.iter().find(|(k, _, _)| *k == name) else {
            let _ = writeln!(out, "ignored {name}");
            continue;
        };
        if value.is_empty() {
            let _ = writeln!(out, "skipped {} {} (empty)", ex.name(), name_str(secret));
            continue;
        }
        found.push((ex, secret, value));
    }
    // one bundle write for everything
    match secrets.set_many(&found) {
        Ok(()) => {
            for (ex, secret, _) in &found {
                let _ = writeln!(out, "stored {} {}", ex.name(), name_str(*secret));
            }
            0
        }
        Err(e) => {
            let _ = writeln!(err, "nothing stored: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::SecretError;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Mem(Mutex<HashMap<String, String>>);

    impl KeyStore for Mem {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().insert(account.to_string(), value.to_string());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    /// Counts bundle writes (every write is the one `credentials` item).
    #[derive(Default)]
    struct CountingMem(Mem, Mutex<u32>);

    impl KeyStore for CountingMem {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            self.0.get(account)
        }
        fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
            *self.1.lock().unwrap() += 1;
            self.0.set(account, value)
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.0.delete(account)
        }
    }

    struct Shared(std::sync::Arc<CountingMem>);

    impl KeyStore for Shared {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            self.0.get(account)
        }
        fn set(&self, account: &str, value: &str) -> Result<(), SecretError> {
            self.0.set(account, value)
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.0.delete(account)
        }
    }

    fn call(s: &BundleSecrets<Mem>, args: &[&str], stdin: &str) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(s, &args, &mut stdin.as_bytes(), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn set_reads_stdin_stores_and_never_prints_the_value() {
        let s = BundleSecrets::new(Mem::default());
        let (code, out, err) = call(&s, &["set", "binance", "api-secret"], "Sup3rSecretValue\n");
        assert_eq!(code, 0, "{err}");
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiSecret).unwrap().as_deref(), Some("Sup3rSecretValue"));
        assert!(!out.contains("Sup3r") && !err.contains("Sup3r"), "{out}{err}");
        let (_, out, _) = call(&s, &["status"], "");
        assert!(out.contains("Binance  api-secret  present") && out.contains("Binance  api-key     missing"), "{out}");
        assert!(out.contains("OKX      passphrase  missing"), "{out}");
        assert!(!out.contains("Sup3r"));
    }

    #[test]
    fn crlf_is_stripped_but_empty_or_padded_values_are_refused() {
        let s = BundleSecrets::new(Mem::default());
        assert_eq!(call(&s, &["set", "bybit", "api_key"], "abcd1234\r\n").0, 0);
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiKey).unwrap().as_deref(), Some("abcd1234"));
        for bad in ["\n", "", "   \n", " abcd1234\n", "abcd1234 \n"] {
            let (code, _, err) = call(&s, &["set", "okx", "passphrase"], bad);
            assert_eq!(code, 1, "{bad:?}: {err}");
        }
        assert_eq!(s.get(Exchange::Okx, SecretName::Passphrase).unwrap(), None);
    }

    #[test]
    fn delete_and_usage_errors() {
        let s = BundleSecrets::new(Mem::default());
        call(&s, &["set", "okx", "api-key"], "k-123456\n");
        assert_eq!(call(&s, &["delete", "okx", "api-key"], "").0, 0);
        assert_eq!(s.get(Exchange::Okx, SecretName::ApiKey).unwrap(), None);
        for bad in [&["set"][..], &["set", "kraken", "api-key"], &["set", "binance", "passphrase"], &["nope"], &[], &["status", "x"]] {
            let (code, _, err) = call(&s, bad, "v\n");
            assert_eq!(code, 2, "{bad:?}");
            assert!(err.contains("usage:"), "{err}");
        }
    }

    #[test]
    fn import_env_reads_the_python_env_format_and_never_prints_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(
            &p,
            "# comment\nBINANCE_API_KEY=bk-AAAA1111\nBINANCE_API_SECRET=\"bs-BBBB2222\"\nBINANCE_BASE_URL=https://testnet.binancefuture.com\n\nBYBIT_API_KEY=\nBYBIT_API_SECRET=ys-CCCC3333\r\nBYBIT_ACCOUNT_TYPE=UNIFIED\nOKX_DEMO_API_KEY='ok-DDDD4444'\nOKX_DEMO_API_SECRET=os-EEEE5555\nOKX_DEMO_PASSPHRASE=my pass phrase\nSOMETHING_ELSE=zzz\n",
        )
        .unwrap();
        let s = BundleSecrets::new(Mem::default());
        let (code, out, err) = call(&s, &["import-env", p.to_str().unwrap()], "");
        assert_eq!(code, 0, "{out}{err}");
        let got = |e, n| s.get(e, n).unwrap();
        assert_eq!(got(Exchange::Binance, SecretName::ApiKey).as_deref(), Some("bk-AAAA1111"));
        assert_eq!(got(Exchange::Binance, SecretName::ApiSecret).as_deref(), Some("bs-BBBB2222"), "double quotes stripped");
        assert_eq!(got(Exchange::Bybit, SecretName::ApiKey), None, "empty values are skipped, not stored");
        assert_eq!(got(Exchange::Bybit, SecretName::ApiSecret).as_deref(), Some("ys-CCCC3333"), "CRLF stripped");
        assert_eq!(got(Exchange::Okx, SecretName::ApiKey).as_deref(), Some("ok-DDDD4444"), "single quotes stripped");
        assert_eq!(got(Exchange::Okx, SecretName::Passphrase).as_deref(), Some("my pass phrase"), "inner spaces kept");
        for v in ["AAAA1111", "BBBB2222", "CCCC3333", "DDDD4444", "EEEE5555", "pass phrase", "zzz"] {
            assert!(!out.contains(v) && !err.contains(v), "value {v} printed:\n{out}{err}");
        }
        assert!(out.contains("stored Binance api-key") && out.contains("skipped Bybit api-key (empty)"), "{out}");
        assert!(out.contains("ignored BINANCE_BASE_URL") && out.contains("ignored SOMETHING_ELSE"), "{out}");
    }

    #[test]
    fn import_env_with_a_missing_file_fails_without_echoing_anything() {
        let s = BundleSecrets::new(Mem::default());
        let (code, _, err) = call(&s, &["import-env", "/nonexistent/.env"], "");
        assert_eq!(code, 1, "{err}");
        assert_eq!(call(&s, &["import-env"], "").0, 2);
    }

    #[test]
    fn import_env_writes_the_bundle_exactly_once_and_set_keeps_other_values() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "BINANCE_API_KEY=a1\nBINANCE_API_SECRET=a2\nBYBIT_API_KEY=a3\nBYBIT_API_SECRET=a4\nOKX_DEMO_API_KEY=a5\nOKX_DEMO_API_SECRET=a6\nOKX_DEMO_PASSPHRASE=a7\n").unwrap();
        let raw = std::sync::Arc::new(CountingMem::default());
        let s = BundleSecrets::new(Shared(raw.clone()));
        let args = ["import-env".to_string(), p.to_str().unwrap().to_string()];
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_with(&s, &args, &mut "".as_bytes(), &mut out, &mut err), 0);
        assert_eq!(*raw.1.lock().unwrap(), 1, "one keychain write for seven values");
        let bundle: serde_json::Value = serde_json::from_str(&raw.0.get("credentials").unwrap().unwrap()).unwrap();
        assert_eq!(bundle.as_object().unwrap().len(), 7);
        let set = ["set".to_string(), "bybit".to_string(), "api-key".to_string()];
        assert_eq!(run_with(&s, &set, &mut "new-key\n".as_bytes(), &mut out, &mut err), 0);
        assert_eq!(s.get(Exchange::Bybit, SecretName::ApiKey).unwrap().as_deref(), Some("new-key"));
        assert_eq!(s.get(Exchange::Binance, SecretName::ApiKey).unwrap().as_deref(), Some("a1"));
        assert_eq!(*raw.1.lock().unwrap(), 2);
    }
}

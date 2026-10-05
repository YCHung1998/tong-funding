//! `tong-funding secrets` subcommand: put exchange API credentials into the macOS Keychain
//! without `.env` files, shell history or the repository (spec: secret-storage).
//!
//! ```text
//! tong-funding secrets set <binance|bybit|okx> <api-key|api-secret|passphrase>   (value on stdin)
//! tong-funding secrets status
//! tong-funding secrets delete <binance|bybit|okx> <api-key|api-secret|passphrase>
//! ```
//!
//! The value is read from stdin (one line), never from the command line, and is never printed.
//! Exit codes: 0 ok, 1 store error, 2 usage error.

use std::io::{BufRead, Write};

use tong_funding_core::redact::redact_secrets;
use tong_funding_core::types::Exchange;

use crate::ports::{SecretName, SecretProvider};
use crate::store::secrets::{KeyStore, KeychainSecrets};

pub const SUBCOMMAND: &str = "secrets";

const USAGE: &str = "usage:\n  tong-funding secrets set <binance|bybit|okx> <api-key|api-secret|passphrase>   (value on stdin)\n  tong-funding secrets status\n  tong-funding secrets delete <binance|bybit|okx> <api-key|api-secret|passphrase>";

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

/// Which secrets each exchange needs (OKX also needs a passphrase).
fn needed(exchange: Exchange) -> &'static [SecretName] {
    match exchange {
        Exchange::Okx => &[SecretName::ApiKey, SecretName::ApiSecret, SecretName::Passphrase],
        Exchange::Binance | Exchange::Bybit => &[SecretName::ApiKey, SecretName::ApiSecret],
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
    run_with(&KeychainSecrets::system(), args, stdin, out, err)
}

fn run_with<S: KeyStore>(secrets: &KeychainSecrets<S>, args: &[String], stdin: &mut dyn BufRead, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
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
                    let _ = writeln!(out, "stored {} {}", ex.name(), name_str(name));
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

    fn call(s: &KeychainSecrets<Mem>, args: &[&str], stdin: &str) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(s, &args, &mut stdin.as_bytes(), &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn set_reads_stdin_stores_and_never_prints_the_value() {
        let s = KeychainSecrets::new(Mem::default());
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
        let s = KeychainSecrets::new(Mem::default());
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
        let s = KeychainSecrets::new(Mem::default());
        call(&s, &["set", "okx", "api-key"], "k-123456\n");
        assert_eq!(call(&s, &["delete", "okx", "api-key"], "").0, 0);
        assert_eq!(s.get(Exchange::Okx, SecretName::ApiKey).unwrap(), None);
        for bad in [&["set"][..], &["set", "kraken", "api-key"], &["set", "binance", "passphrase"], &["nope"], &[], &["status", "x"]] {
            let (code, _, err) = call(&s, bad, "v\n");
            assert_eq!(code, 2, "{bad:?}");
            assert!(err.contains("usage:"), "{err}");
        }
    }
}

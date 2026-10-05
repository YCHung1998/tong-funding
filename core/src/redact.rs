//! Secret redaction for anything that may be logged or stored (spec: secret-storage).
//!
//! Masks the VALUE of: URL query params `signature`, `api_key`, `apiKey`, and the headers
//! `X-MBX-APIKEY`, `X-BAPI-API-KEY`, `X-BAPI-SIGN`, `OK-ACCESS-KEY`, `OK-ACCESS-SIGN`,
//! `OK-ACCESS-PASSPHRASE` (case-insensitive; `name=value`, `name: value` and JSON `"name":"value"`).
//! Everything else (symbol, timestamp, ...) is kept for debugging. Idempotent.

pub const PLACEHOLDER: &str = "[REDACTED]";

const QUERY_KEYS: [&str; 3] = ["signature", "api_key", "apikey"];
const HEADER_KEYS: [&str; 6] = [
    "x-mbx-apikey",
    "x-bapi-api-key",
    "x-bapi-sign",
    "ok-access-key",
    "ok-access-sign",
    "ok-access-passphrase",
];

/// Returns `text` with every secret value replaced by [`PLACEHOLDER`].
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for key in QUERY_KEYS {
        out = redact_key(&out, key, &['=']);
    }
    for key in HEADER_KEYS {
        out = redact_key(&out, key, &[':', '=']);
    }
    out
}

fn is_value_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ')' | ']' | ',' | ';' | '}')
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn redact_key(text: &str, key: &str, separators: &[char]) -> String {
    // ASCII lowercasing keeps byte offsets identical to `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find(key) {
        let start = pos + rel;
        let key_end = start + key.len();
        let boundary_ok = text[..start].chars().next_back().is_none_or(|c| !is_key_char(c));
        // optional closing quote of the key, optional spaces, then a separator
        let mut i = key_end;
        let rest = |i: usize| text[i..].chars().next();
        if rest(i) == Some('"') {
            i += 1;
        }
        while rest(i) == Some(' ') {
            i += 1;
        }
        let has_sep = boundary_ok && rest(i).is_some_and(|c| separators.contains(&c));
        if !has_sep {
            out.push_str(&text[pos..key_end]);
            pos = key_end;
            continue;
        }
        i += 1;
        while rest(i) == Some(' ') {
            i += 1;
        }
        if rest(i) == Some('"') {
            i += 1;
        }
        let value_start = i;
        let value_end = if text[value_start..].starts_with(PLACEHOLDER) {
            value_start + PLACEHOLDER.len() // already masked: ']' would otherwise end the value early
        } else {
            text[value_start..].find(is_value_end).map_or(text.len(), |n| value_start + n)
        };
        out.push_str(&text[pos..value_start]);
        if value_start < value_end && &text[value_start..value_end] != PLACEHOLDER {
            out.push_str(PLACEHOLDER);
        } else {
            out.push_str(&text[value_start..value_end]);
        }
        pos = value_end;
    }
    out.push_str(&text[pos..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_signature_is_masked_other_params_kept() {
        let got = redact_secrets("GET /fapi/v1/account?symbol=BTCUSDT&timestamp=1791187200000&signature=abcdef0123");
        assert_eq!(got, "GET /fapi/v1/account?symbol=BTCUSDT&timestamp=1791187200000&signature=[REDACTED]");
    }

    #[test]
    fn signature_in_the_middle_of_a_query_stops_at_the_ampersand() {
        let got = redact_secrets("?signature=abc123&recvWindow=5000");
        assert_eq!(got, "?signature=[REDACTED]&recvWindow=5000");
    }

    #[test]
    fn api_key_query_variants_are_masked() {
        assert_eq!(redact_secrets("?apiKey=KEY123&x=1"), "?apiKey=[REDACTED]&x=1");
        assert_eq!(redact_secrets("?api_key=KEY123"), "?api_key=[REDACTED]");
    }

    #[test]
    fn header_values_are_masked_case_insensitively() {
        assert_eq!(redact_secrets("X-MBX-APIKEY: realkey"), "X-MBX-APIKEY: [REDACTED]");
        assert_eq!(redact_secrets("x-bapi-sign: zzz999"), "x-bapi-sign: [REDACTED]");
        assert_eq!(redact_secrets("OK-ACCESS-PASSPHRASE: pp"), "OK-ACCESS-PASSPHRASE: [REDACTED]");
    }

    #[test]
    fn all_listed_headers_are_covered() {
        for h in ["X-MBX-APIKEY", "X-BAPI-API-KEY", "X-BAPI-SIGN", "OK-ACCESS-KEY", "OK-ACCESS-SIGN", "OK-ACCESS-PASSPHRASE"] {
            let got = redact_secrets(&format!("{h}: secretvalue"));
            assert!(!got.contains("secretvalue"), "{h} not redacted: {got}");
        }
    }

    #[test]
    fn json_style_header_is_masked() {
        let got = redact_secrets(r#"{"OK-ACCESS-KEY":"abcd","note":"keep"}"#);
        assert_eq!(got, r#"{"OK-ACCESS-KEY":"[REDACTED]","note":"keep"}"#);
    }

    #[test]
    fn connection_error_text_with_a_full_url_is_masked() {
        let got = redact_secrets("error sending request for url (https://demo.example/fapi/v2/account?timestamp=1&signature=deadbeef): timed out");
        assert!(!got.contains("deadbeef"));
        assert!(got.contains("timestamp=1"));
        assert!(got.ends_with("timed out"));
    }

    #[test]
    fn multiple_occurrences_are_all_masked() {
        let got = redact_secrets("a?signature=111 and b?signature=222");
        assert!(!got.contains("111") && !got.contains("222"));
    }

    #[test]
    fn text_without_secrets_is_unchanged() {
        let s = "symbol=BTCUSDT&timestamp=1 status 200 OK";
        assert_eq!(redact_secrets(s), s);
    }

    #[test]
    fn words_that_merely_contain_a_key_name_are_not_touched() {
        assert_eq!(redact_secrets("mysignature=abc"), "mysignature=abc");
    }

    #[test]
    fn already_redacted_text_is_stable() {
        let once = redact_secrets("?signature=abc");
        assert_eq!(redact_secrets(&once), once);
    }
}

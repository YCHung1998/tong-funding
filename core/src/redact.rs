//! Secret redaction for anything that may be logged or stored (spec: secret-storage).
//!
//! Masks the VALUE that follows a sensitive name, in any of these shapes: URL query
//! (`name=value`), plain text / header (`name: value`), JSON (`"name":"value"`, also escaped
//! inside a JSON string), Python repr (`'name': 'value'`) and URL-encoded (`name%3Dvalue`).
//! Whitespace of any kind around the separator is tolerated. The OKX passphrase may contain
//! spaces, so an unquoted one is masked to the end of the line. Everything else (symbol,
//! timestamp, ...) is kept for debugging. Idempotent.

pub const PLACEHOLDER: &str = "[REDACTED]";

const QUERY_KEYS: [&str; 8] = [
    "signature",
    "api_key",
    "apikey",
    "api_secret",
    "apisecret",
    "secret_key",
    "secretkey",
    "secret",
];
const HEADER_KEYS: [&str; 5] = ["x-mbx-apikey", "x-bapi-api-key", "x-bapi-sign", "ok-access-key", "ok-access-sign"];
/// Values may contain spaces: when unquoted, mask to the end of the line.
const TO_EOL_KEYS: [&str; 2] = ["ok-access-passphrase", "passphrase"];

/// Returns `text` with every secret value replaced by [`PLACEHOLDER`].
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for key in QUERY_KEYS.iter().chain(HEADER_KEYS.iter()) {
        out = redact_key(&out, key, false);
    }
    for key in TO_EOL_KEYS {
        out = redact_key(&out, key, true);
    }
    out
}

/// Names whose VALUE must never be stored, for callers that walk structured data (JSON).
pub fn is_sensitive_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    QUERY_KEYS.iter().chain(HEADER_KEYS.iter()).chain(TO_EOL_KEYS.iter()).any(|k| *k == n)
}

fn is_value_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '&' | '"' | '\'' | ')' | ']' | ',' | ';' | '}' | '\\')
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn ends_with_ci(text: &str, suffix: &str) -> bool {
    text.len() >= suffix.len() && text.as_bytes()[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// Skips one quote (plain `"`, single `'`, or escaped `\"`); returns the new index and the quote.
fn skip_quote(text: &str, i: usize) -> (usize, bool) {
    if text[i..].starts_with("\\\"") {
        (i + 2, true)
    } else if text[i..].starts_with('"') || text[i..].starts_with('\'') {
        (i + 1, true)
    } else {
        (i, false)
    }
}

fn skip_while(text: &str, mut i: usize, f: impl Fn(char) -> bool) -> usize {
    while let Some(c) = text[i..].chars().next() {
        if !f(c) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

fn redact_key(text: &str, key: &str, to_eol: bool) -> String {
    // ASCII lowercasing keeps byte offsets identical to `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find(key) {
        let start = pos + rel;
        let key_end = start + key.len();
        let before = &text[..start];
        let boundary_ok = before.chars().next_back().is_none_or(|c| !is_key_char(c))
            || ends_with_ci(before, "%26")
            || ends_with_ci(before, "%3f");
        // key, optional spaces, optional closing quote, optional spaces, then the separator
        let mut i = skip_while(text, key_end, |c| c == ' ');
        i = skip_quote(text, i).0;
        i = skip_while(text, i, |c| c == ' ');
        let (sep_len, encoded) = match text[i..].chars().next() {
            Some('=') | Some(':') => (1, false),
            _ if starts_with_ci(&text[i..], "%3d") || starts_with_ci(&text[i..], "%3a") => (3, true),
            _ => (0, false),
        };
        if !boundary_ok || sep_len == 0 {
            out.push_str(&text[pos..key_end]);
            pos = key_end;
            continue;
        }
        i += sep_len;
        i = skip_while(text, i, char::is_whitespace);
        let (value_start, quoted) = {
            let (j, q) = skip_quote(text, i);
            (j, q)
        };
        let end_of = |from: usize| -> usize {
            let mut k = from;
            while let Some(c) = text[k..].chars().next() {
                let stop = if quoted {
                    matches!(c, '"' | '\'' | '\\')
                } else if to_eol {
                    matches!(c, '\n' | '\r')
                } else {
                    is_value_end(c) || (encoded && starts_with_ci(&text[k..], "%26"))
                };
                if stop || (!quoted && !to_eol && starts_with_ci(&text[k..], "%26")) {
                    break;
                }
                k += c.len_utf8();
            }
            k
        };
        let value_end = if text[value_start..].starts_with(PLACEHOLDER) {
            // Already masked only if the placeholder is the whole value; glued text is a leak.
            end_of(value_start + PLACEHOLDER.len())
        } else {
            end_of(value_start)
        };
        out.push_str(&text[pos..value_start]);
        let value = &text[value_start..value_end];
        if value_start < value_end && value != PLACEHOLDER {
            out.push_str(PLACEHOLDER);
        } else {
            out.push_str(value);
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

    #[test]
    fn json_style_query_keys_are_masked() {
        assert_eq!(redact_secrets(r#"{"api_key":"K1","x":"keep"}"#), r#"{"api_key":"[REDACTED]","x":"keep"}"#);
        assert_eq!(redact_secrets(r#"{"apiKey":"K2"}"#), r#"{"apiKey":"[REDACTED]"}"#);
        assert_eq!(redact_secrets(r#"{"signature":"abc123","symbol":"BTCUSDT"}"#), r#"{"signature":"[REDACTED]","symbol":"BTCUSDT"}"#);
    }

    #[test]
    fn json_nested_inside_a_json_string_is_masked() {
        // escaped quotes: the secret hides inside a string value
        let got = redact_secrets(r#"{"raw":"{\"api_key\":\"KSECRET\",\"n\":1}"}"#);
        assert!(!got.contains("KSECRET"), "{got}");
        assert!(got.contains("\\\"n\\\":1") || got.contains("n"), "structure should survive: {got}");
    }

    #[test]
    fn colon_style_query_keys_are_masked_in_plain_text() {
        assert_eq!(redact_secrets("signature: abc123 end"), "signature: [REDACTED] end");
    }

    #[test]
    fn any_whitespace_after_the_separator_is_skipped() {
        assert_eq!(redact_secrets("X-MBX-APIKEY:\tSECRET"), "X-MBX-APIKEY:\t[REDACTED]");
        assert_eq!(redact_secrets("X-MBX-APIKEY:  SECRET"), "X-MBX-APIKEY:  [REDACTED]");
        assert_eq!(redact_secrets("X-BAPI-SIGN:\nSECRET"), "X-BAPI-SIGN:\n[REDACTED]");
    }

    #[test]
    fn passphrase_is_masked_to_the_end_of_the_line_because_it_may_contain_spaces() {
        let got = redact_secrets("OK-ACCESS-PASSPHRASE: my secret pass phrase\nnext: line");
        assert_eq!(got, "OK-ACCESS-PASSPHRASE: [REDACTED]\nnext: line");
    }

    #[test]
    fn quoted_passphrase_with_spaces_ends_at_the_closing_quote() {
        let got = redact_secrets(r#"{"OK-ACCESS-PASSPHRASE":"my secret","keep":"yes"}"#);
        assert_eq!(got, r#"{"OK-ACCESS-PASSPHRASE":"[REDACTED]","keep":"yes"}"#);
    }

    #[test]
    fn single_quoted_python_repr_is_masked() {
        let got = redact_secrets("{'X-BAPI-API-KEY': 'SECRET', 'a': 1}");
        assert!(!got.contains("SECRET"), "{got}");
    }

    #[test]
    fn text_glued_to_a_placeholder_is_not_trusted_as_redacted() {
        let got = redact_secrets("signature=[REDACTED]SECRET");
        assert!(!got.contains("SECRET"), "{got}");
    }

    #[test]
    fn url_encoded_separators_are_masked() {
        let got = redact_secrets("redirect=%3Fsymbol%3DBTC%26signature%3DSECRET%26x%3D1");
        assert!(!got.contains("SECRET"), "{got}");
        assert!(got.contains("symbol"), "{got}");
    }

    #[test]
    fn spaces_inside_the_quoted_key_name_are_tolerated() {
        let got = redact_secrets(r#"{" apiKey ":"SECRET"}"#);
        assert!(!got.contains("SECRET"), "{got}");
    }
}

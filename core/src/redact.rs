//! Secret redaction for anything that may be logged or stored (spec: secret-storage).
//!
//! Two layers, applied by [`redact_secrets`]:
//!
//! 1. **Exact values (primary defence).** Every secret that is read from the Keychain (or written
//!    to it) is passed to [`register_secret`]. Any occurrence of a registered value is replaced by
//!    [`PLACEHOLDER`] wherever it appears, whatever field name or format surrounds it: verbatim,
//!    percent-encoded (any hex case, repeatedly), JSON-escaped (`\"`, `\\`, `\uXXXX`, also nested
//!    inside another JSON string), with zero-width characters inserted, or written in full-width
//!    forms. Matching works on a canonical form of the text (escapes decoded, zero-width removed,
//!    full-width folded to ASCII) and the replacement is mapped back onto the original bytes.
//! 2. **Name patterns (second layer).** For text that holds a secret we never saw: a tokenizer
//!    looks for a separator (`=`, `:`, `%3D`, `%253D`, full-width `＝`), takes the name token right
//!    before it and, if the normalised name (lower-case, non-alphanumerics removed) contains
//!    `apikey`, `apisecret`, `secretkey`, `accesskey`, `passphrase`, `signature`, `authorization`
//!    or `password`, ends with `secret` or `token`, or is/ends in `sign` or equals `bearer`, masks
//!    the value that follows. `authorization` and `passphrase` values run to the end of the line
//!    (they may contain spaces); a quoted value ends at its matching quote (the other quote kind may
//!    appear inside); `{...}`/`[...]` values are masked whole. A bare `Bearer <token>` is masked too.
//!
//! Idempotent. `core` does no I/O: the registry lives in process memory only.
//!
//! **Known limit:** a secret printed as a Debug byte array (`[82, 65, ...]` from `{:?}` on bytes)
//! cannot be recognised by either layer, exact-value registration included. Callers must never
//! format a secret with `{:?}`/`{:#?}` into anything that is logged or stored.

use std::sync::RwLock;

pub const PLACEHOLDER: &str = "[REDACTED]";

/// Shortest secret that is tracked; shorter values would mask ordinary text.
const MIN_SECRET_LEN: usize = 4;

/// Canonical forms of every registered secret, longest first, no duplicates.
static REGISTRY: RwLock<Vec<Vec<char>>> = RwLock::new(Vec::new());

/// One character of canonical text together with the byte range it came from in the original.
#[derive(Clone, Copy)]
struct Ch {
    c: char,
    start: usize,
    end: usize,
}

fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200D}' | '\u{FEFF}' | '\u{2060}' | '\u{00AD}')
}

fn fold_fullwidth(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        _ => c,
    }
}

fn hex_val(c: char) -> Option<u32> {
    c.to_digit(16)
}

/// Decodes one pass over `input`; `json_escapes` also decodes `\"`, `\\`, `\/`, `\n`, ...
fn canon_pass(input: &[Ch], json_escapes: bool) -> Vec<Ch> {
    let mut out: Vec<Ch> = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let ch = input[i];
        let c = ch.c;
        if is_zero_width(c) {
            i += 1;
            continue;
        }
        let folded = fold_fullwidth(c);
        if folded != c {
            out.push(Ch { c: folded, ..ch });
            i += 1;
            continue;
        }
        // percent-encoding: a run of %XX groups decoded as UTF-8
        if c == '%' {
            let mut bytes: Vec<(u8, usize)> = Vec::new(); // (byte, index of its '%')
            let mut j = i;
            while j + 2 < input.len() && input[j].c == '%' {
                match (hex_val(input[j + 1].c), hex_val(input[j + 2].c)) {
                    (Some(h), Some(l)) => bytes.push((((h << 4) | l) as u8, j)),
                    _ => break,
                }
                j += 3;
            }
            if !bytes.is_empty() {
                let raw: Vec<u8> = bytes.iter().map(|b| b.0).collect();
                let valid = match std::str::from_utf8(&raw) {
                    Ok(_) => raw.len(),
                    Err(e) => e.valid_up_to(),
                };
                if valid > 0 {
                    let text = std::str::from_utf8(&raw[..valid]).unwrap_or("");
                    let mut b = 0;
                    for dc in text.chars() {
                        let n = dc.len_utf8();
                        let first = input[bytes[b].1];
                        let last = input[bytes[b + n - 1].1 + 2];
                        out.push(Ch { c: dc, start: first.start, end: last.end });
                        b += n;
                    }
                    i = bytes[valid - 1].1 + 3;
                    continue;
                }
            }
        }
        if c == '\\' && i + 1 < input.len() {
            let n = input[i + 1].c;
            if n == 'u' && i + 5 < input.len() {
                let digits: Option<Vec<u32>> = (2..6).map(|k| hex_val(input[i + k].c)).collect();
                if let Some(d) = digits {
                    let unit = d.iter().fold(0u32, |a, x| (a << 4) | x);
                    let mut consumed = 6;
                    let mut ch_out = char::from_u32(unit);
                    // surrogate pair 😀
                    if (0xD800..0xDC00).contains(&unit)
                        && i + 11 < input.len()
                        && input[i + 6].c == '\\'
                        && input[i + 7].c == 'u'
                    {
                        let low: Option<Vec<u32>> = (8..12).map(|k| hex_val(input[i + k].c)).collect();
                        if let Some(l) = low {
                            let lu = l.iter().fold(0u32, |a, x| (a << 4) | x);
                            if (0xDC00..0xE000).contains(&lu) {
                                ch_out = char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (lu - 0xDC00));
                                consumed = 12;
                            }
                        }
                    }
                    if let Some(dc) = ch_out {
                        out.push(Ch { c: dc, start: ch.start, end: input[i + consumed - 1].end });
                        i += consumed;
                        continue;
                    }
                }
            }
            if json_escapes {
                let dec = match n {
                    '"' => Some('"'),
                    '\\' => Some('\\'),
                    '/' => Some('/'),
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    'b' => Some('\u{8}'),
                    'f' => Some('\u{c}'),
                    _ => None,
                };
                if let Some(dc) = dec {
                    out.push(Ch { c: dc, start: ch.start, end: input[i + 1].end });
                    i += 2;
                    continue;
                }
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// Canonical form of `text`, decoded repeatedly (double encodings) until nothing changes.
fn canonicalize(text: &str, json_escapes: bool) -> Vec<Ch> {
    let mut cur: Vec<Ch> = text.char_indices().map(|(i, c)| Ch { c, start: i, end: i + c.len_utf8() }).collect();
    for _ in 0..6 {
        let next = canon_pass(&cur, json_escapes);
        let same = next.len() == cur.len() && next.iter().zip(&cur).all(|(a, b)| a.c == b.c);
        cur = next;
        if same {
            break;
        }
    }
    cur
}

fn canon_chars(text: &str) -> Vec<char> {
    canonicalize(text, true).into_iter().map(|c| c.c).collect()
}

fn lock_read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Starts tracking `value` as a secret: every later [`redact_secrets`] call masks it in any format.
/// Values shorter than 4 characters are ignored (they would mask ordinary text), as are values
/// that are part of [`PLACEHOLDER`]. Registering the same value again is a no-op.
pub fn register_secret(value: &str) {
    if value.chars().count() < MIN_SECRET_LEN {
        return;
    }
    let canon = canon_chars(value);
    if canon.len() < MIN_SECRET_LEN {
        return;
    }
    let as_text: String = canon.iter().collect();
    if PLACEHOLDER.contains(as_text.as_str()) {
        return;
    }
    {
        if lock_read(&REGISTRY).iter().any(|c| *c == canon) {
            return;
        }
    }
    let mut w = REGISTRY.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if w.iter().any(|c| *c == canon) {
        return;
    }
    w.push(canon);
    w.sort_by_key(|c| std::cmp::Reverse(c.len())); // longest first
}

/// Merges byte ranges and replaces each with the placeholder.
fn apply_ranges(text: &str, mut ranges: Vec<(usize, usize)>) -> String {
    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (s, e) in merged {
        out.push_str(&text[pos..s]);
        out.push_str(PLACEHOLDER);
        pos = e;
    }
    out.push_str(&text[pos..]);
    out
}

/// Layer 1: replaces every occurrence of a registered secret.
fn redact_registered(text: &str) -> String {
    let secrets = lock_read(&REGISTRY);
    if secrets.is_empty() {
        return text.to_string();
    }
    let canon = canonicalize(text, true);
    let hay: Vec<char> = canon.iter().map(|c| c.c).collect();
    let mut ranges = Vec::new();
    for s in secrets.iter() {
        if s.len() > hay.len() {
            continue;
        }
        let mut i = 0;
        while i + s.len() <= hay.len() {
            if hay[i..i + s.len()] == s[..] {
                ranges.push((canon[i].start, canon[i + s.len() - 1].end));
                i += s.len();
            } else {
                i += 1;
            }
        }
    }
    drop(secrets);
    apply_ranges(text, ranges)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Value ends at a delimiter.
    Normal,
    /// Value may contain spaces: runs to the end of the line.
    ToEol,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Classifies a name token by the normalised rule; `None` = not sensitive.
fn classify_name(token: &str) -> Option<Kind> {
    let norm: String = canonicalize(token, true)
        .iter()
        .map(|c| c.c)
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if norm.is_empty() {
        return None;
    }
    if norm.contains("authorization") || norm.contains("passphrase") {
        return Some(Kind::ToEol);
    }
    let last_segment: String = {
        let canon: String = canonicalize(token, true).iter().map(|c| c.c).collect();
        canon.rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_lowercase()
    };
    let hit = ["apikey", "apisecret", "secretkey", "accesskey", "signature", "password"].iter().any(|k| norm.contains(k))
        || norm.ends_with("secret")
        || norm.ends_with("token")
        || norm == "sign"
        || norm == "bearer"
        || last_segment == "sign";
    hit.then_some(Kind::Normal)
}

/// Names whose VALUE must never be stored, for callers that walk structured data (JSON). Same
/// normalised rule as the text tokenizer (case, whitespace, punctuation, zero-width and full-width
/// forms are ignored).
pub fn is_sensitive_name(name: &str) -> bool {
    classify_name(name.trim()).is_some()
}

fn is_value_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '&' | '"' | '\'' | '`' | ')' | ']' | ',' | ';' | '}' | '\\')
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

fn starts_with_placeholder(chars: &[char], at: usize) -> bool {
    PLACEHOLDER.chars().enumerate().all(|(k, p)| chars.get(at + k) == Some(&p))
}

/// The sensitive name token directly before the separator at `sep`, if any.
fn sensitive_before(chars: &[char], sep: usize) -> Option<Kind> {
    let mut j = sep;
    while j > 0 && ((chars[j - 1].is_whitespace() && !matches!(chars[j - 1], '\n' | '\r')) || is_quote(chars[j - 1]) || chars[j - 1] == '\\') {
        j -= 1;
    }
    let end = j;
    while j > 0 && is_name_char(chars[j - 1]) {
        j -= 1;
    }
    if j == end {
        return None;
    }
    let token: String = chars[j..end].iter().collect();
    classify_name(&token)
}

/// Index of the bracket that closes the one at `open` (or the end of the text).
fn balanced_end(chars: &[char], open: usize) -> usize {
    let (o, c) = if chars[open] == '{' { ('{', '}') } else { ('[', ']') };
    let mut depth = 0usize;
    for (k, ch) in chars.iter().enumerate().skip(open) {
        if *ch == o {
            depth += 1;
        } else if *ch == c {
            depth -= 1;
            if depth == 0 {
                return k + 1;
            }
        }
    }
    chars.len()
}

fn eol(chars: &[char], from: usize) -> usize {
    chars[from..].iter().position(|c| matches!(c, '\n' | '\r')).map_or(chars.len(), |p| from + p)
}

/// The range `[start, end)` of the value that follows a separator whose next char is `from`.
fn value_range(chars: &[char], from: usize, kind: Kind) -> Option<(usize, usize)> {
    let mut i = from;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    // opening quote, possibly escaped (`\"`)
    let mut k = i;
    while k < chars.len() && chars[k] == '\\' {
        k += 1;
    }
    if k < chars.len() && is_quote(chars[k]) {
        let q = chars[k];
        let escaped = k > i;
        let vs = k + 1;
        let mut e = vs;
        let close = loop {
            if e >= chars.len() {
                break None;
            }
            if escaped {
                if chars[e] == '\\' && chars.get(e + 1) == Some(&q) {
                    break Some(e);
                }
            } else if chars[e] == q {
                break Some(e);
            } else if chars[e] == '\\' {
                e += 1; // skip the escaped char
            }
            e += 1;
        };
        let ve = close.unwrap_or_else(|| eol(chars, vs));
        return Some((vs, ve));
    }
    if matches!(chars[i], '{' | '[') && !starts_with_placeholder(chars, i) {
        return Some((i, balanced_end(chars, i)));
    }
    let end_of = |from: usize| {
        let mut e = from;
        while e < chars.len() {
            let stop = match kind {
                Kind::ToEol => matches!(chars[e], '\n' | '\r'),
                Kind::Normal => is_value_end(chars[e]),
            };
            if stop {
                break;
            }
            e += 1;
        }
        e
    };
    // text glued to a placeholder is not trusted as redacted
    let ve = if starts_with_placeholder(chars, i) { end_of(i + PLACEHOLDER.chars().count()) } else { end_of(i) };
    Some((i, ve))
}

/// Layer 2: the tokenizer.
fn redact_patterns(text: &str) -> String {
    let canon = canonicalize(text, false);
    let chars: Vec<char> = canon.iter().map(|c| c.c).collect();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let is_placeholder = |s: usize, e: usize| chars[s..e].iter().copied().eq(PLACEHOLDER.chars());
    let mut p = 0;
    while p < chars.len() {
        let c = chars[p];
        if c == '=' || c == ':' {
            if let Some(kind) = sensitive_before(&chars, p) {
                if let Some((vs, ve)) = value_range(&chars, p + 1, kind) {
                    if vs < ve && !is_placeholder(vs, ve) {
                        ranges.push((canon[vs].start, canon[ve - 1].end));
                    }
                    p = ve.max(p + 1);
                    continue;
                }
            }
        } else if (c == 'b' || c == 'B') && (p == 0 || !is_name_char(chars[p - 1])) {
            // a bare `Bearer <token>`
            let word: String = chars[p..(p + 6).min(chars.len())].iter().collect();
            if word.eq_ignore_ascii_case("bearer") && chars.get(p + 6).is_some_and(|c| c.is_whitespace() && !matches!(c, '\n' | '\r')) {
                let mut vs = p + 6;
                while vs < chars.len() && chars[vs].is_whitespace() && !matches!(chars[vs], '\n' | '\r') {
                    vs += 1;
                }
                let mut ve = vs;
                while ve < chars.len() && !is_value_end(chars[ve]) {
                    ve += 1;
                }
                if starts_with_placeholder(&chars, vs) {
                    ve = ve.max(vs + PLACEHOLDER.chars().count());
                }
                if vs < ve && !is_placeholder(vs, ve) {
                    ranges.push((canon[vs].start, canon[ve - 1].end));
                }
                p = ve.max(p + 1);
                continue;
            }
        }
        p += 1;
    }
    apply_ranges(text, ranges)
}

/// Returns `text` with every secret value replaced by [`PLACEHOLDER`] (see the module docs).
pub fn redact_secrets(text: &str) -> String {
    redact_patterns(&redact_registered(text))
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
    fn words_that_merely_resemble_a_key_name_are_not_touched() {
        // second-round rule: a name that CONTAINS `signature` is sensitive (`mysignature=` is masked),
        // but unrelated words such as `design` are not
        assert_eq!(redact_secrets("design=abc&max_tokens=5"), "design=abc&max_tokens=5");
        assert_eq!(redact_secrets("mysignature=abc"), "mysignature=[REDACTED]");
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

    // ===== second round: exact-value registry (layer 1) =====

    fn fw(s: &str) -> String {
        s.chars().map(|c| if c.is_ascii_graphic() { char::from_u32(c as u32 + 0xFEE0).unwrap() } else { c }).collect()
    }
    fn pct(s: &str, upper: bool) -> String {
        s.bytes().map(|b| if upper { format!("%{b:02X}") } else { format!("%{b:02x}") }).collect()
    }
    fn json_inner(s: &str) -> String {
        let j = serde_json::to_string(s).unwrap();
        j[1..j.len() - 1].to_string()
    }
    fn zw(s: &str) -> String {
        s.chars().map(|c| format!("{c}\u{200B}\u{FEFF}")).collect()
    }

    #[test]
    fn registered_value_is_masked_in_every_format_and_any_field_name() {
        let s = "Zq7!pLm9#Xv2";
        register_secret(s);
        let forms = [
            s.to_string(),
            format!("random_field\t{s}\tend"),
            format!("note: {s}"),
            fw(s),
            zw(s),
            pct(s, true),
            pct(s, false),
            pct(&pct(s, true), false),
            format!("a=1&innocent={}&b=2", pct(s, true)),
            json_inner(s),
            format!("{{\"anything\":\"{}\"}}", json_inner(s)),
            s.chars().map(|c| format!("\\u{:04x}", c as u32)).collect::<String>(),
            s.chars().map(|c| format!("\\u{:04X}", c as u32)).collect::<String>(),
        ];
        for f in forms {
            let got = redact_secrets(&format!("pre {f} post"));
            assert!(got.contains(PLACEHOLDER), "not masked: {f:?} -> {got:?}");
            assert!(got.starts_with("pre ") && got.ends_with(" post"), "context lost: {got:?}");
            for frag in ["Zq7", "pLm9", "Xv2", "%5A", "\\u005a"] {
                assert!(!got.to_lowercase().contains(&frag.to_lowercase()), "fragment {frag} left in {got:?} (from {f:?})");
            }
            assert_eq!(redact_secrets(&got), got, "not idempotent");
        }
    }

    #[test]
    fn registered_value_with_quote_and_backslash_survives_json_escaping() {
        let s = "ab\"c\\d9Xk";
        register_secret(s);
        let got = redact_secrets(&format!("{{\"v\":\"{}\"}}", json_inner(s)));
        assert!(!got.contains("d9Xk") && !got.contains("ab\\\"c"), "{got}");
    }

    #[test]
    fn registered_value_inside_double_json_encoding() {
        let s = "Qw3rTy-Uio9";
        register_secret(s);
        let once = serde_json::json!({"m": format!("boom {s} boom")}).to_string();
        let twice = serde_json::json!({"outer": once}).to_string();
        assert!(!redact_secrets(&twice).contains("Uio9"));
    }

    #[test]
    fn too_short_registrations_are_ignored() {
        register_secret("xyz");
        register_secret("");
        assert_eq!(redact_secrets("xyz and more xyz"), "xyz and more xyz");
    }

    #[test]
    fn longer_registered_value_wins_over_its_prefix() {
        register_secret("PFXAAAA1111");
        register_secret("PFXAAAA1111BBBB2222");
        let got = redact_secrets("t PFXAAAA1111BBBB2222 t");
        assert_eq!(got, "t [REDACTED] t");
    }

    #[test]
    fn registering_a_piece_of_the_placeholder_is_ignored_and_output_stays_stable() {
        register_secret("REDACTED");
        let got = redact_secrets("x=[REDACTED] y");
        assert_eq!(got, "x=[REDACTED] y");
    }

    #[test]
    fn registration_is_deduplicated_and_thread_safe() {
        let handles: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..200 {
                        let secret = format!("conc-secret-{}-{}", t % 3, i % 5);
                        register_secret(&secret);
                        assert!(!redact_secrets(&format!("v {secret} v")).contains(&secret));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    // ===== second round: tokenizer patterns (layer 2) =====

    fn masked(text: &str, secret: &str) {
        let got = redact_secrets(text);
        assert!(!got.contains(secret), "{text:?} -> {got:?}");
        assert!(got.contains(PLACEHOLDER), "{text:?} -> {got:?}");
        assert_eq!(redact_secrets(&got), got, "not idempotent for {text:?}");
    }

    #[test]
    fn red_team_round_two_pattern_cases() {
        masked("signature\t=S3CR3T", "S3CR3T");
        masked("a=1&signature%253DS3CR3T&b=2", "S3CR3T");
        masked("a=1&signature%3dS3CR3T&b=2", "S3CR3T");
        masked("OK-ACCESS-PASSPHRASE: \"it's S3CR3T\"", "S3CR3T");
        masked("OK-ACCESS-PASSPHRASE: \"it's S3CR3T\" next", "S3CR3T");
        masked("BINANCE_API_SECRET=S3CR3T", "S3CR3T");
        masked("binanceApiKey=S3CR3T", "S3CR3T");
        masked("?sign=S3CR3T&x=1", "S3CR3T");
        masked("api-key: S3CR3T", "S3CR3T");
        masked("accessKey=S3CR3T", "S3CR3T");
        masked("Authorization: Bearer S3CR3T", "S3CR3T");
        masked("Authorization: Basic abc S3CR3T def", "S3CR3T");
        masked("signature\u{FF1D}S3CR3T", "S3CR3T");
        masked("sig\u{200B}nature=S3CR3T", "S3CR3T");
        masked("password=S3CR3T", "S3CR3T");
        masked("{\"access_token\": \"S3CR3T\"}", "S3CR3T");
        masked("Bearer S3CR3T", "S3CR3T");
        masked("signature = {\"a\": \"S3CR3T\"} tail", "S3CR3T");
        masked("{\"x\": \"{\\\"clientSecret\\\": \\\"S3CR3T\\\"}\"}", "S3CR3T");
        masked("X-BAPI-SIGN:S3CR3T", "S3CR3T");
        masked("{'signature': \"a'b S3CR3T\"}", "S3CR3T");
    }

    #[test]
    fn ordinary_text_is_left_alone_by_the_tokenizer() {
        for t in [
            "symbol=BTCUSDT&timestamp=1791187200000&recvWindow=5000",
            "status: 200 OK at 12:34:56",
            "design: flat, max_tokens: 5",
            "https://demo.example/fapi/v2/account?timestamp=1",
        ] {
            assert_eq!(redact_secrets(t), t);
        }
    }

    #[test]
    fn sensitive_names_use_the_normalised_contains_rule() {
        for n in [
            "apiKey", "API_KEY", "api-key", " X-MBX-APIKEY ", "binanceApiKey", "BINANCE_API_SECRET", "accessKey",
            "OK-ACCESS-KEY", "OK-ACCESS-SIGN", "X-BAPI-SIGN", "sign", "signature", "Authorization", "passphrase",
            "OK-ACCESS-PASSPHRASE", "password", "clientSecret", "secret", "token", "access_token", "bearer", "sig\u{200B}nature",
        ] {
            assert!(is_sensitive_name(n), "{n:?} should be sensitive");
        }
        for n in ["symbol", "design", "timestamp", "max_tokens", "name", "value", ""] {
            assert!(!is_sensitive_name(n), "{n:?} should not be sensitive");
        }
    }
}

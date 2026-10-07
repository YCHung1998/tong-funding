//! Static source checks for the exchange module (spec: exchange-adapter, "公開行情與簽名請求使用
//! 結構上分離的 HTTP 客戶端"). They read the `.rs` files under `src/exchange` at test time, so they
//! also cover code other people write into `signed/` later.
//!
//! What is scanned is *production code*: comments are ignored (a comment can mention a host
//! without being able to connect to it) and so are `#[cfg(test)]` modules (tests may assert that a
//! production host is NOT used). String literals are scanned.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The OKX REST host. OKX demo and production share it (demo = the `x-simulated-trading: 1`
/// header), so it counts as a production host everywhere except its single home, `signed/endpoints.rs`.
pub const OKX_SIGNED_HOST: &str = "openapi.okx.com";

/// Production hosts that only the public (unsigned) clients may name (and, for the OKX signed
/// host, only `signed/endpoints.rs`).
pub const PRODUCTION_HOSTS: [&str; 4] = ["fapi.binance.com", "api.bybit.com", "www.okx.com", OKX_SIGNED_HOST];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    ProductionHost(String),
    /// A public fn / field through which a caller could choose the host.
    ExternalBaseUrl(String),
    /// Host or credentials read from the environment or a config file.
    EnvOrConfigRead(String),
    /// POST / PUT / DELETE / PATCH in a read-only client.
    NonGetMethod(String),
    /// A `cfg` attribute that is not exactly `#[cfg(test)] mod name { .. }` / `mod name;`, or an
    /// inner `#![cfg(..)]` that is not at the head of the file: it could hide code from the scan.
    CfgAttribute(String),
    /// `concat!`, `include_str!`, `env!`, ...: ways to build or load a host the scan cannot see.
    ForbiddenMacro(String),
    /// `\x..`, `\u{..}` or a line-continuation backslash inside a string literal.
    EscapedString(String),
    /// The signed client names the `public` module (it must not share the public clients' hosts).
    ImportsPublic,
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn blank(v: &mut [u8], from: usize, to: usize) {
    let to = to.min(v.len());
    if from >= to {
        return;
    }
    for x in &mut v[from..to] {
        if *x != b'\n' {
            *x = b' ';
        }
    }
}

/// Start of a raw string literal at `i` (`r"`, `r#"`, `br"`): returns (hash count, content start).
fn raw_string_start(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let n = b.len();
    if b[i] != b'r' {
        return None;
    }
    let prefixed_ok = i == 0 || !is_ident(b[i - 1]) || (b[i - 1] == b'b' && (i == 1 || !is_ident(b[i - 2])));
    if !prefixed_ok {
        return None;
    }
    let mut k = i + 1;
    while k < n && b[k] == b'#' {
        k += 1;
    }
    if k < n && b[k] == b'"' {
        Some((k - i - 1, k + 1))
    } else {
        None
    }
}

/// One string literal's content range and whether it is a raw string.
#[derive(Debug, Clone, Copy)]
struct Lit {
    start: usize,
    end: usize,
    raw: bool,
}

/// Returns (code, structure, literals): both buffers have comments blanked; `structure` additionally
/// has string and char literal contents blanked, so brace matching and keyword search cannot be
/// fooled by them. `literals` lists the string literals found (ranges into `code`).
fn mask(src: &str) -> (Vec<u8>, Vec<u8>, Vec<Lit>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut code = b.to_vec();
    let mut structure = b.to_vec();
    let mut lits = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            let end = b[i..].iter().position(|&x| x == b'\n').map_or(n, |p| i + p);
            blank(&mut code, i, end);
            blank(&mut structure, i, end);
            i = end;
        } else if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 1;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if b[j] == b'/' && j + 1 < n && b[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && j + 1 < n && b[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut code, i, j);
            blank(&mut structure, i, j);
            i = j;
        } else if let Some((hashes, start)) = raw_string_start(b, i) {
            let mut closer = vec![b'"'];
            closer.extend(std::iter::repeat_n(b'#', hashes));
            let end = find_from(b, &closer, start).unwrap_or(n);
            lits.push(Lit { start, end: end.min(n), raw: true });
            blank(&mut structure, start, end);
            i = (end + closer.len()).min(n);
        } else if c == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(n);
            lits.push(Lit { start, end, raw: false });
            blank(&mut structure, start, end);
            i = (end + 1).min(n);
        } else if c == b'\'' {
            // char literal ('x', '\n', '"', multi-byte) or a lifetime ('a)
            let close = if i + 1 < n && b[i + 1] == b'\\' {
                b[i + 2..].iter().take(10).position(|&x| x == b'\'').map(|p| i + 2 + p)
            } else if i + 2 < n && b[i + 2] == b'\'' {
                Some(i + 2)
            } else if i + 1 < n && b[i + 1] >= 0x80 {
                b[i + 2..].iter().take(5).position(|&x| x == b'\'').map(|p| i + 2 + p)
            } else {
                None
            };
            match close {
                Some(e) => {
                    blank(&mut structure, i + 1, e);
                    i = e + 1;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    (code, structure, lits)
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// Index just past the `}` matching the `{` at `open`.
fn matching_brace(structure: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < structure.len() {
        match structure[i] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    structure.len()
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn starts_with_at(b: &[u8], i: usize, pat: &[u8]) -> bool {
    b.len() >= i + pat.len() && &b[i..i + pat.len()] == pat
}

/// The part of a file that counts as production code.
struct Production {
    /// Comments blanked, test-only modules blanked, string literals kept.
    code: String,
    /// Same, with string and char literal contents blanked too.
    structure: String,
    /// `cfg` attributes that are not the one allowed shape.
    violations: Vec<Violation>,
}

/// After `#[cfg(test)]` at `after_attr`, accepts exactly `[pub[(..)]] mod name { ... }` or
/// `mod name;` and returns the end of that item.
fn complete_test_mod(structure: &[u8], after_attr: usize) -> Option<usize> {
    let mut i = skip_ws(structure, after_attr);
    if starts_with_at(structure, i, b"pub") && !structure.get(i + 3).is_some_and(|c| is_ident(*c)) {
        i = skip_ws(structure, i + 3);
        if structure.get(i) == Some(&b'(') {
            i = structure[i..].iter().position(|&c| c == b')').map(|p| i + p + 1)?;
            i = skip_ws(structure, i);
        }
    }
    if !starts_with_at(structure, i, b"mod") || structure.get(i + 3).is_some_and(|c| is_ident(*c)) {
        return None;
    }
    i = skip_ws(structure, i + 3);
    let name_start = i;
    while i < structure.len() && is_ident(structure[i]) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    i = skip_ws(structure, i);
    match structure.get(i) {
        Some(b';') => Some(i + 1),
        Some(b'{') => Some(matching_brace(structure, i)),
        _ => None,
    }
}

fn production(src: &str) -> Production {
    let (mut code, mut structure, _) = mask(src);
    let mut violations = Vec::new();
    let finish = |code: Vec<u8>, structure: Vec<u8>, violations| Production {
        code: String::from_utf8_lossy(&code).into_owned(),
        structure: String::from_utf8_lossy(&structure).into_owned(),
        violations,
    };

    // Inner attributes: only a `#![cfg(test)]` among the attributes at the very head of the file
    // makes the whole file test-only; any other inner cfg is a violation.
    let mut head = skip_ws(&structure, 0);
    let mut head_inner_end = head;
    while starts_with_at(&structure, head, b"#![") {
        let close = structure[head..].iter().position(|&c| c == b']').map_or(structure.len(), |p| head + p + 1);
        if &structure[head..close] == b"#![cfg(test)]" {
            let len = code.len();
            blank(&mut code, 0, len);
            blank(&mut structure, 0, len);
            return finish(code, structure, violations);
        }
        head = skip_ws(&structure, close);
        head_inner_end = head;
    }
    let _ = head_inner_end;

    let mut from = 0;
    while let Some(at) = find_from(&structure, b"#", from) {
        from = at + 1;
        let inner = starts_with_at(&structure, at + 1, b"!");
        let bracket = skip_ws(&structure, at + 1 + usize::from(inner));
        if structure.get(bracket) != Some(&b'[') {
            continue;
        }
        let name = skip_ws(&structure, bracket + 1);
        if !starts_with_at(&structure, name, b"cfg") {
            continue;
        }
        let attr_end = structure[name..].iter().position(|&c| c == b']').map_or(structure.len(), |p| name + p + 1);
        let attr = String::from_utf8_lossy(&structure[at..attr_end]).into_owned();
        if !inner && &structure[at..attr_end] == b"#[cfg(test)]" {
            if let Some(end) = complete_test_mod(&structure, attr_end) {
                blank(&mut code, at, end);
                blank(&mut structure, at, end);
                from = end;
                continue;
            }
        }
        violations.push(Violation::CfgAttribute(attr.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    finish(code, structure, violations)
}

/// Source with comments blanked out and `#[cfg(test)] mod` items removed; string literals kept.
/// (Blanked ranges become spaces, newlines are kept, so lines are preserved.)
pub fn production_code(src: &str) -> String {
    production(src).code
}

fn norm(ident: &str) -> String {
    ident.to_ascii_lowercase().replace('_', "")
}

/// A parameter or field name that smells like "where to connect".
fn is_host_like_name(ident: &str) -> bool {
    let n = norm(ident);
    ["baseurl", "baseuri", "host", "endpoint", "url", "root", "uri"].iter().any(|w| n.contains(w))
}

/// Splits `text` at top-level commas (not inside `<>`, `()`, `[]`, `{}`).
fn split_top_level(text: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, 0usize, Vec::new());
    let bytes = text.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' if i > 0 && bytes[i - 1] == b'-' => {} // `->`
            b'>' | b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// Public fns with a host-like PARAMETER NAME, and public fields with a host-like name.
/// Names only: types and the fn's own name are not looked at.
fn external_base_url(structure: &str) -> Vec<Violation> {
    let b = structure.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = find_from(b, b"pub", from) {
        from = at + 3;
        if (at > 0 && is_ident(b[at - 1])) || from >= b.len() || is_ident(b[from]) {
            continue; // part of a longer identifier
        }
        let mut i = skip_ws(b, from);
        if b.get(i) == Some(&b'(') {
            i = b[i..].iter().position(|&c| c == b')').map_or(b.len(), |p| i + p + 1);
            i = skip_ws(b, i);
        }
        let rest = &structure[i.min(structure.len())..];
        let rb = rest.as_bytes();
        // qualifiers, then `fn`
        let mut j = 0;
        let word_end = |j: &mut usize| {
            let s = skip_ws(rb, *j);
            let mut e = s;
            while e < rb.len() && is_ident(rb[e]) {
                e += 1;
            }
            *j = e;
            &rest[s..e]
        };
        let mut w = word_end(&mut j);
        let mut had_qualifier = false;
        while matches!(w, "async" | "const" | "unsafe" | "extern") {
            had_qualifier = true;
            w = word_end(&mut j);
        }
        if w == "fn" {
            let _name = word_end(&mut j);
            // optional generics, then the parameter list
            let mut k = skip_ws(rb, j);
            if rb.get(k) == Some(&b'<') {
                let mut depth = 0i32;
                while k < rb.len() {
                    match rb[k] {
                        b'<' => depth += 1,
                        b'>' if k > 0 && rb[k - 1] != b'-' => {
                            depth -= 1;
                            if depth == 0 {
                                k += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                k = skip_ws(rb, k);
            }
            if rb.get(k) != Some(&b'(') {
                continue;
            }
            let mut depth = 0i32;
            let mut close = rb.len();
            for (m, &c) in rb.iter().enumerate().skip(k) {
                match c {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = m;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            for param in split_top_level(&rest[k + 1..close.min(rest.len())]) {
                let name_part = param.split(':').next().unwrap_or("");
                if idents(name_part).iter().any(|t| !matches!(*t, "mut" | "self" | "ref") && is_host_like_name(t)) {
                    out.push(Violation::ExternalBaseUrl(format!("pub fn parameter `{}`", name_part.trim())));
                }
            }
        } else if !had_qualifier && !w.is_empty() && !matches!(w, "struct" | "enum" | "mod" | "use" | "const" | "static" | "trait" | "type" | "union") {
            // `pub name: Type` field
            let after = skip_ws(rb, j);
            if rb.get(after) == Some(&b':') && rb.get(after + 1) != Some(&b':') && is_host_like_name(w) {
                out.push(Violation::ExternalBaseUrl(format!("pub field {w}")));
            }
        }
    }
    out
}

fn idents(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|t| !t.is_empty()).collect()
}

const FORBIDDEN_MACROS: [&str; 7] = ["concat", "include_str", "include_bytes", "include", "env", "option_env", "stringify"];

/// Macros that can build or load a host the scan cannot read.
fn forbidden_macros(structure: &str) -> Vec<Violation> {
    let b = structure.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if is_ident(b[i]) {
            let s = i;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            let word = &structure[s..i];
            let bang = skip_ws(b, i);
            if b.get(bang) == Some(&b'!') && b.get(bang + 1) != Some(&b'=') && FORBIDDEN_MACROS.contains(&word) {
                out.push(Violation::ForbiddenMacro(format!("{word}!")));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// `\x..`, `\u{..}` and line-continuation backslashes inside (non-raw) string literals.
fn escaped_strings(code: &str, lits: &[Lit]) -> Vec<Violation> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for lit in lits.iter().filter(|l| !l.raw) {
        let mut i = lit.start;
        while i < lit.end.min(b.len()) {
            if b[i] == b'\\' {
                match b.get(i + 1) {
                    Some(b'x') => out.push(Violation::EscapedString("\\x".into())),
                    Some(b'u') => out.push(Violation::EscapedString("\\u".into())),
                    Some(b'\n') | Some(b'\r') => out.push(Violation::EscapedString("line continuation".into())),
                    _ => {}
                }
                i += 2;
            } else {
                i += 1;
            }
        }
    }
    out.dedup();
    out
}

/// Domain fragments that must not appear in any string literal of the signed client, even split
/// across literals (`"fapi."` + `"binance.com"`). The allowed demo hosts are removed first.
fn host_fragments(code: &str, lits: &[Lit]) -> Vec<Violation> {
    let mut out = Vec::new();
    for lit in lits {
        let text = code.get(lit.start..lit.end).unwrap_or("").to_ascii_lowercase();
        let mut rest = text.clone();
        for allowed in crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS {
            rest = rest.replace(allowed, " ");
        }
        if !scan_production_hosts_in(&rest).is_empty() {
            continue; // a whole production host: already reported by the host scan
        }
        for frag in ["binance.com", "bybit.com", "okx.com"] {
            if rest.contains(frag) {
                out.push(Violation::ProductionHost(frag.to_string()));
            }
        }
    }
    out
}

/// Scans one signed-client source file's text.
pub fn scan_signed_source(src: &str) -> Vec<Violation> {
    let prod = production(src);
    let (_, _, lits) = mask(&prod.code);
    let mut out = prod.violations.clone();
    out.extend(scan_production_hosts_in(&prod.code));
    out.extend(host_fragments(&prod.code, &lits));
    out.extend(external_base_url(&prod.structure));
    out.extend(forbidden_macros(&prod.structure));
    out.extend(escaped_strings(&prod.code, &lits));
    if idents(&prod.structure).contains(&"public") {
        out.push(Violation::ImportsPublic);
    }
    for pat in ["env::var", "env::vars", "std::env", "dotenv", "read_to_string", "File::open"] {
        if prod.structure.contains(pat) {
            out.push(Violation::EnvOrConfigRead(pat.to_string()));
        }
    }
    for pat in [".post(", ".put(", ".delete(", ".patch(", "Method::POST", "Method::PUT", "Method::DELETE", "Method::PATCH"] {
        if prod.structure.contains(pat) {
            out.push(Violation::NonGetMethod(pat.to_string()));
        }
    }
    for pat in ["\"POST\"", "\"PUT\"", "\"DELETE\"", "\"PATCH\""] {
        if prod.code.contains(pat) {
            out.push(Violation::NonGetMethod(pat.to_string()));
        }
    }
    out.dedup();
    out
}

fn scan_production_hosts_in(code: &str) -> Vec<Violation> {
    let lower = code.to_ascii_lowercase();
    PRODUCTION_HOSTS.iter().filter(|h| names_host(&lower, h)).map(|h| Violation::ProductionHost((*h).to_string())).collect()
}

/// True if `host` occurs as a whole host name. A longer name that merely ends with it, such as the
/// demo host `demo-fapi.binance.com` (ends with `fapi.binance.com`), does not count; a real
/// subdomain (`x.fapi.binance.com`, preceded by `.`) does. `code` must already be lower-case.
fn names_host(code: &str, host: &str) -> bool {
    let b = code.as_bytes();
    let mut from = 0;
    while let Some(at) = find_from(b, host.as_bytes(), from) {
        from = at + 1;
        if at == 0 || !(is_ident(b[at - 1]) || b[at - 1] == b'-') {
            return true;
        }
    }
    false
}

/// Production hosts named anywhere in `src`'s production code.
pub fn scan_production_hosts(src: &str) -> Vec<Violation> {
    scan_production_hosts_in(&production_code(src))
}

/// Lower-cased text of every string literal in production code (comments and test modules excluded).
pub fn production_literals(src: &str) -> Vec<String> {
    let prod = production(src);
    let (_, _, lits) = mask(&prod.code);
    lits.iter().map(|l| prod.code.get(l.start..l.end).unwrap_or("").to_ascii_lowercase()).collect()
}

/// Literals that spell out the OKX boundary, and the only files (relative to `src/`) allowed to
/// hold them. This rule is separate from the host-fragment scan, which deliberately exempts the
/// whitelisted signed hosts everywhere: here even the whitelisted OKX host is confined to its home.
const LITERAL_RULES: [(&str, &[&str]); 3] = [
    ("x-simulated-trading", &["exchange/signed/endpoints.rs"]),
    ("ok-access", &["exchange/signed/endpoints.rs", "exchange/signed/signing.rs"]),
    ("okx.com", &["exchange/signed/endpoints.rs", "exchange/public/endpoints.rs"]),
];

/// Protected literals found in `src`'s production code when `rel_path` (relative to `src/`, `/`
/// separated) is not one of their home files.
pub fn literal_rule_violations(rel_path: &str, src: &str) -> Vec<String> {
    let lits = production_literals(src);
    let mut out = Vec::new();
    for (needle, homes) in LITERAL_RULES {
        if !homes.contains(&rel_path) && lits.iter().any(|l| l.contains(needle)) {
            out.push(format!("literal containing `{needle}` outside {homes:?}"));
        }
    }
    out
}

/// Files that may name the `reqwest` crate: the two real transports.
const REQWEST_HOMES: [&str; 2] = ["exchange/reqwest_transport.rs", "exchange/execution/http.rs"];

/// The identifier `reqwest` in production code outside the two transports: any other HTTP path
/// would bypass `HostPolicy::allows` (the one rule that decides host and OKX header).
pub fn reqwest_rule_violations(rel_path: &str, src: &str) -> Vec<String> {
    if REQWEST_HOMES.contains(&rel_path) {
        return Vec::new();
    }
    if idents(&production(src).structure).contains(&"reqwest") {
        vec![format!("identifier `reqwest` outside {REQWEST_HOMES:?}")]
    } else {
        Vec::new()
    }
}

/// Names of the `pub` fns in every `impl OkxHost { .. }` block. The only allowed one is `target`:
/// anything else could hand out an OKX host or URL without the `x-simulated-trading` header.
pub fn okx_host_pub_fns(src: &str) -> Vec<String> {
    let structure = production(src).structure;
    let b = structure.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = find_from(b, b"impl OkxHost", from) {
        let Some(open) = find_from(b, b"{", at) else { break };
        let close = matching_brace(b, open);
        let body = &structure[open..close];
        let mut depth = 0usize;
        let mut i = 0;
        let bb = body.as_bytes();
        while i < bb.len() {
            match bb[i] {
                b'{' => depth += 1,
                b'}' => depth = depth.saturating_sub(1),
                _ if depth == 1 && starts_with_at(bb, i, b"pub") && (i == 0 || !is_ident(bb[i - 1])) => {
                    let rest = &body[i..];
                    let head: String = rest.chars().take_while(|c| *c != '{' && *c != ';').collect();
                    if let Some(pos) = head.find("fn ") {
                        let name: String = head[pos + 3..].trim_start().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                        out.push(name);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        from = close.max(at + 1);
    }
    out
}

const ORDER_KEYWORDS: [&[&str]; 13] = [
    &["place"],
    &["cancel"],
    &["amend"],
    &["withdraw"],
    &["set", "leverage"],
    &["change", "leverage"],
    &["set", "margin"],
    &["new", "order"],
    &["create", "order"],
    &["submit", "order"],
    &["send", "order"],
    &["close", "position"],
    &["modify", "order"],
];

/// Names of fns whose name contains an order-mutating keyword (place / cancel / amend /
/// set_leverage / ...), taken from production code. Matching is by `_`-separated segment.
pub fn scan_order_keywords(src: &str) -> Vec<String> {
    let code = production_code(src);
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = find_from(b, b"fn ", from) {
        from = at + 3;
        if at > 0 && is_ident(b[at - 1]) {
            continue;
        }
        let name: String = code[from..].trim_start().chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let segs: Vec<&str> = name.split('_').filter(|s| !s.is_empty()).collect();
        if ORDER_KEYWORDS.iter().any(|kw| segs.windows(kw.len()).any(|w| w == *kw)) {
            out.push(name);
        }
    }
    out
}

/// All `.rs` files under `dir`, sorted.
pub fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(dir, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn exchange_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("exchange")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(src: &str) -> Vec<Violation> {
        scan_signed_source(src).into_iter().filter(|v| matches!(v, Violation::ProductionHost(_))).collect()
    }

    // ------------------------------------------------ scanner self-tests (must catch violations)

    #[test]
    fn each_production_host_in_code_is_flagged() {
        for h in PRODUCTION_HOSTS {
            let src = format!("const BASE: &str = \"https://{h}/fapi\";");
            assert_eq!(hosts(&src), vec![Violation::ProductionHost(h.to_string())], "{h}");
        }
    }

    #[test]
    fn demo_hosts_are_not_flagged() {
        let src = r#"const A: &str = "https://testnet.binancefuture.com"; const B: &str = "https://api-demo.bybit.com"; const C: &str = "https://demo-fapi.binance.com";"#;
        assert!(hosts(src).is_empty());
    }

    #[test]
    fn a_subdomain_of_a_production_host_is_still_flagged() {
        assert_eq!(hosts("const A: &str = \"https://x.fapi.binance.com\";").len(), 1);
        assert_eq!(hosts("const A: &str = \"fapi.binance.com\";").len(), 1, "host at the very start of a string");
    }

    #[test]
    fn hosts_in_comments_and_test_modules_are_ignored_but_not_after_them() {
        let src = r#"
// never use fapi.binance.com here
/// doc: api.bybit.com is public-only
/* block www.okx.com */
fn ok() {}
#[cfg(test)]
mod tests {
    #[test]
    fn t() { assert!(!"x".contains("fapi.binance.com")); let _ = "{"; }
}
const LATE: &str = "api.bybit.com";
"#;
        assert_eq!(hosts(src), vec![Violation::ProductionHost("api.bybit.com".into())]);
    }

    #[test]
    fn a_url_in_a_string_is_not_mistaken_for_a_line_comment() {
        let src = "let u = \"https://fapi.binance.com/x\"; // trailing comment";
        assert_eq!(hosts(src).len(), 1);
    }

    #[test]
    fn a_char_literal_quote_does_not_hide_a_later_host() {
        // A scanner that treats the '"' as opening a string would then see `//fapi...` as a comment.
        let src = "let q = '\"'; let s = \"a\"; let h = \"https://fapi.binance.com\";";
        assert_eq!(hosts(src).len(), 1);
    }

    #[test]
    fn raw_strings_are_scanned() {
        let src = "let h = r#\"api.bybit.com\"#;";
        assert_eq!(hosts(src).len(), 1);
    }

    #[test]
    fn pub_fn_taking_a_base_url_or_host_is_flagged_even_across_lines() {
        for src in [
            "pub fn new(base_url: &str) -> Self { todo!() }",
            "pub fn with_host(host: String) -> Self { todo!() }",
            "pub(crate) fn from_url(\n    key: &str,\n    base_url: impl Into<String>,\n) -> Self { todo!() }",
            "pub fn build<T>(t: T, baseUrl: &str) -> Self where T: Clone { todo!() }",
            "pub struct C { pub base_url: String }",
        ] {
            let v = scan_signed_source(src);
            assert!(v.iter().any(|x| matches!(x, Violation::ExternalBaseUrl(_))), "not flagged: {src}");
        }
    }

    #[test]
    fn private_fns_and_hostless_constructors_are_not_flagged() {
        let src = "fn helper(base_url: &str) {}\npub fn new(key: &str) -> Self { todo!() }\npub fn ghost_town() {}";
        assert!(scan_signed_source(src).is_empty(), "{:?}", scan_signed_source(src));
    }

    #[test]
    fn reading_host_or_config_from_the_environment_is_flagged() {
        for src in [
            "let h = std::env::var(\"BINANCE_BASE_URL\");",
            "let h = env::var(\"X\");",
            "dotenv::dotenv().ok();",
            "let s = std::fs::read_to_string(\".env\");",
        ] {
            assert!(scan_signed_source(src).iter().any(|v| matches!(v, Violation::EnvOrConfigRead(_))), "not flagged: {src}");
        }
    }

    #[test]
    fn write_methods_are_flagged_and_get_is_not() {
        for src in [
            "client.post(url).send()",
            "client.delete(url)",
            "client.put(url)",
            "client.patch(url)",
            "Method::POST",
            "let m = \"DELETE\";",
            "let m = \"PUT\";",
        ] {
            assert!(scan_signed_source(src).iter().any(|v| matches!(v, Violation::NonGetMethod(_))), "not flagged: {src}");
        }
        assert!(scan_signed_source("transport.get(req).await; let positions = 1; let output = 2;").is_empty());
    }

    #[test]
    fn order_keyword_fn_names_are_found_by_segment_not_by_substring() {
        let src = "fn place_order() {} pub async fn cancel_all() {} fn amend() {} fn set_leverage(x: u8) {} fn replace_cache() {} fn open_orders() {} fn is_cancelled() {} fn new_orders_page() {}";
        let mut found = scan_order_keywords(src);
        found.sort();
        assert_eq!(found, vec!["amend", "cancel_all", "place_order", "set_leverage"]);
    }

    #[test]
    fn order_keywords_inside_comments_and_test_modules_are_ignored() {
        let src = "// fn place_order() {}\n#[cfg(test)]\nmod tests { fn rejects_place_order() {} }\nfn fetch_balance() {}";
        assert!(scan_order_keywords(src).is_empty());
    }

    #[test]
    fn scanning_a_directory_picks_up_a_violating_file_written_later() {
        let dir = std::env::temp_dir().join(format!("tong_static_check_{}", std::process::id()));
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("clean.rs"), "pub fn new(key: &str) {}").unwrap();
        std::fs::write(nested.join("bad.rs"), "const H: &str = \"www.okx.com\"; pub fn new(host: &str) {}").unwrap();
        std::fs::write(nested.join("note.txt"), "fapi.binance.com").unwrap();
        let files = rust_files(&dir);
        assert_eq!(files.len(), 2, "{files:?}");
        let mut all = Vec::new();
        for f in &files {
            all.extend(scan_signed_source(&std::fs::read_to_string(f).unwrap()));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(all.contains(&Violation::ProductionHost("www.okx.com".into())), "{all:?}");
        assert!(all.iter().any(|v| matches!(v, Violation::ExternalBaseUrl(_))), "{all:?}");
    }

    // ------------------------------------------------ the real checks over the repository

    #[test]
    fn signed_client_sources_have_no_production_host_no_base_url_ctor_and_only_get() {
        let files = rust_files(&exchange_dir().join("signed"));
        assert!(!files.is_empty(), "signed/ must exist and be scanned");
        let mut violations = Vec::new();
        for f in &files {
            let src = std::fs::read_to_string(f).unwrap();
            for v in scan_signed_source(&src) {
                // `signed/endpoints.rs` is the one file that names the OKX signed host (it is also a
                // production host: demo and production share it, see `OkxHost`).
                if f == &okx_home() && v == Violation::ProductionHost(OKX_SIGNED_HOST.to_string()) {
                    continue;
                }
                violations.push(format!("{}: {v:?}", f.display()));
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
    }

    fn okx_home() -> PathBuf {
        exchange_dir().join("signed").join("endpoints.rs")
    }

    #[test]
    fn production_hosts_appear_only_under_exchange_public() {
        let files: Vec<_> = rust_files(&exchange_dir())
            .into_iter()
            .filter(|p| !p.starts_with(exchange_dir().join("public")) && p.file_name().is_some_and(|n| n != "static_checks.rs"))
            .collect();
        assert!(files.len() >= 5, "scanned only {files:?}");
        let mut violations = Vec::new();
        for f in &files {
            for v in scan_production_hosts(&std::fs::read_to_string(f).unwrap()) {
                if f == &okx_home() && v == Violation::ProductionHost(OKX_SIGNED_HOST.to_string()) {
                    continue;
                }
                violations.push(format!("{}: {v:?}", f.display()));
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
    }

    /// Order-mutating fns (place / cancel / ...) may exist only in `execution/` (change
    /// exchange-demo-execution evolves the earlier "GET only" rule deliberately: orders in ONE
    /// module, demo/testnet hosts only; see `execution_sources_pass_every_signed_rule_except_get_only`).
    #[test]
    fn no_order_mutating_fns_outside_public_health_and_execution() {
        let files: Vec<_> = rust_files(&exchange_dir())
            .into_iter()
            .filter(|p| {
                !p.starts_with(exchange_dir().join("public"))
                    && !p.starts_with(exchange_dir().join("health"))
                    && !p.starts_with(exchange_dir().join("execution"))
                    && p.file_name().is_some_and(|n| n != "static_checks.rs")
            })
            .collect();
        assert!(!files.is_empty());
        let mut violations = Vec::new();
        for f in &files {
            for name in scan_order_keywords(&std::fs::read_to_string(f).unwrap()) {
                violations.push(format!("{}: fn {name}", f.display()));
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
    }

    fn execution_dir() -> PathBuf {
        exchange_dir().join("execution")
    }

    /// The order module obeys every rule of the signed clients (no production host, no host
    /// parameter or public host field, no env / config read, no host-building macro or escape,
    /// no `public` import, only the allowed `cfg(test)` shape) except "GET only".
    #[test]
    fn execution_sources_pass_every_signed_rule_except_get_only() {
        let files = rust_files(&execution_dir());
        assert!(files.len() >= 8, "execution/ must exist and be scanned: {files:?}");
        let mut violations = Vec::new();
        let mut methods_seen = false;
        for f in &files {
            for v in scan_signed_source(&std::fs::read_to_string(f).unwrap()) {
                match v {
                    Violation::NonGetMethod(_) => methods_seen = true,
                    other => violations.push(format!("{}: {other:?}", f.display())),
                }
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
        assert!(methods_seen, "the scan must actually see the order methods in execution/ (or it scans nothing)");
    }

    /// POST / PUT / DELETE / PATCH requests exist only under `exchange/execution/`: every other
    /// exchange file is scanned with the full signed-client method list, the rest of the crate
    /// with the HTTP-method patterns (a plain `.delete(` there is e.g. a Keychain call).
    #[test]
    fn non_get_methods_only_in_execution() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations = Vec::new();
        for f in rust_files(&src) {
            if f.starts_with(execution_dir()) || f.file_name().is_some_and(|n| n == "static_checks.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            if f.starts_with(exchange_dir()) {
                for v in scan_signed_source(&text) {
                    if let Violation::NonGetMethod(m) = v {
                        violations.push(format!("{}: {m}", f.display()));
                    }
                }
            } else {
                let prod = production(&text);
                for pat in ["Method::POST", "Method::PUT", "Method::DELETE", "Method::PATCH", "reqwest::Method"] {
                    if prod.structure.contains(pat) {
                        violations.push(format!("{}: {pat}", f.display()));
                    }
                }
                for pat in ["\"POST\"", "\"PUT\"", "\"DELETE\"", "\"PATCH\""] {
                    if prod.code.contains(pat) {
                        violations.push(format!("{}: {pat}", f.display()));
                    }
                }
            }
        }
        assert!(violations.is_empty(), "non-GET outside exchange/execution:\n{}", violations.join("\n"));
    }

    /// The order module names no host at all (hosts come only from `signed::endpoints` through
    /// the `DemoEnv` enum) and has no OKX request: no `okx` identifier other than the
    /// `Exchange::Okx` variant, no OKX path or domain in any literal.
    #[test]
    fn execution_names_no_host_literal_and_has_no_okx_request() {
        let mut violations = Vec::new();
        for f in rust_files(&execution_dir()) {
            let prod = production(&std::fs::read_to_string(&f).unwrap());
            let (_, _, lits) = mask(&prod.code);
            for lit in &lits {
                let text = prod.code.get(lit.start..lit.end).unwrap_or("").to_ascii_lowercase();
                for bad in ["http://", "https://", "okx.com", "/api/v5/", ".com"] {
                    if text.contains(bad) {
                        violations.push(format!("{}: literal {text:?} contains {bad}", f.display()));
                    }
                }
            }
            for ident in idents(&prod.structure) {
                // `account.rs` is the read-only AccountView: it may name the OKX signed GET client
                // (okx-signed-read). Order code (okx-demo-execution) lives in other files.
                let account_view = f.file_name().is_some_and(|n| n == "account.rs");
                if !account_view && ident.to_ascii_lowercase().contains("okx") && ident != "Okx" && ident != "OKX_UNSUPPORTED" && ident != "OKX_NOT_WIRED" {
                    violations.push(format!("{}: identifier {ident}", f.display()));
                }
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
    }


    // ------------------------------------------------ OKX demo boundary (okx-signed-read)

    #[test]
    fn the_okx_signed_host_is_a_production_host_for_every_file_except_its_home() {
        assert!(PRODUCTION_HOSTS.contains(&OKX_SIGNED_HOST));
        let src = format!("const H: &str = \"https://{OKX_SIGNED_HOST}\";");
        assert!(has(&src, prod_v), "a signed client naming the OKX host must be flagged");
        assert!(has("const H: &str = \"OPENAPI.OKX.COM\";", prod_v), "any case");
        assert!(has("let a = \"openapi.\"; let b = \"okx.com\"; let h = format!(\"{a}{b}\");", prod_v), "split literals");
    }

    #[test]
    fn okx_host_exposes_target_and_no_other_public_method() {
        // counter-examples: any other pub fn on OkxHost could hand out a host or URL without the flag
        for bad in [
            "impl OkxHost { pub fn target(self) -> T { todo!() } pub fn host(self) -> &'static str { \"x\" } }",
            "impl OkxHost {\n    pub fn target(self) -> T { todo!() }\n    pub(crate) fn base_url(self) -> &'static str { \"x\" }\n}",
            "impl OkxHost { pub fn url(&self) -> String { todo!() } }",
            "impl OkxHost { pub const fn target(self) -> T { todo!() } pub async fn get(self) {} }",
        ] {
            let extra: Vec<String> = okx_host_pub_fns(bad).into_iter().filter(|n| n != "target").collect();
            assert!(!extra.is_empty(), "not caught: {bad}");
        }
        assert_eq!(okx_host_pub_fns("impl OkxHost { pub fn target(self) -> T { todo!() } fn private(self) {} }"), vec!["target".to_string()]);
        // the real file
        let src = std::fs::read_to_string(okx_home()).unwrap();
        assert_eq!(okx_host_pub_fns(&src), vec!["target".to_string()], "OkxHost::target is the only way to an OKX URL");
    }

    fn rel(p: &str) -> &str {
        p
    }

    #[test]
    fn literal_rules_catch_each_protected_literal_outside_its_home_files() {
        let flag = "const H: &str = \"x-simulated-trading\";";
        let flag_upper = "const H: &str = \"X-Simulated-Trading\";";
        let auth = "const H: &str = \"OK-ACCESS-KEY\";";
        let host = "const H: &str = \"https://openapi.okx.com\";";
        let host_piece = "const H: &str = \"okx.com\";";
        let elsewhere = rel("exchange/signed/okx.rs");
        for (what, src) in [("flag", flag), ("flag upper-case", flag_upper), ("OK-ACCESS", auth), ("host", host), ("host piece", host_piece)] {
            assert!(!literal_rule_violations(elsewhere, src).is_empty(), "{what} not caught in {elsewhere}");
            assert!(!literal_rule_violations("ui/live.rs", src).is_empty(), "{what} not caught in ui/live.rs");
            assert!(!literal_rule_violations("exchange/execution/okx.rs", src).is_empty(), "{what} not caught in execution");
        }
        // home files
        assert!(literal_rule_violations("exchange/signed/endpoints.rs", flag).is_empty());
        assert!(literal_rule_violations("exchange/signed/endpoints.rs", auth).is_empty());
        assert!(literal_rule_violations("exchange/signed/endpoints.rs", host).is_empty());
        assert!(literal_rule_violations("exchange/signed/signing.rs", auth).is_empty(), "the signing module builds the OK-ACCESS-* names");
        assert!(!literal_rule_violations("exchange/signed/signing.rs", flag).is_empty(), "but not the flag");
        assert!(!literal_rule_violations("exchange/signed/signing.rs", host).is_empty(), "nor the host");
        // comments and test modules do not count
        assert!(literal_rule_violations(elsewhere, "// x-simulated-trading\n#[cfg(test)]\nmod tests { const H: &str = \"OK-ACCESS-KEY\"; }").is_empty());
    }

    #[test]
    fn the_reqwest_identifier_is_allowed_only_in_the_two_transports() {
        let src = "fn f() { let c = reqwest::Client::new(); }";
        assert!(!reqwest_rule_violations("exchange/signed/okx.rs", src).is_empty());
        assert!(!reqwest_rule_violations("ui/live.rs", "use reqwest::Url;").is_empty());
        assert!(!reqwest_rule_violations("main.rs", "use reqwest::Client;").is_empty());
        assert!(!reqwest_rule_violations("exchange/execution/okx.rs", "fn f() { reqwest::get(\"x\"); }").is_empty());
        assert!(reqwest_rule_violations("exchange/reqwest_transport.rs", src).is_empty());
        assert!(reqwest_rule_violations("exchange/execution/http.rs", src).is_empty());
        assert!(reqwest_rule_violations("exchange/signed/okx.rs", "use crate::exchange::reqwest_transport::ReqwestTransport;").is_empty(), "a different identifier");
        assert!(reqwest_rule_violations("exchange/signed/okx.rs", "// reqwest is mentioned here\n#[cfg(test)]\nmod t { fn f() { reqwest::Url::parse(\"x\"); } }").is_empty());
    }

    /// The whole crate (src, tests, binaries), not just `exchange/signed`: the shared transport is
    /// built in `ui/live.rs`, and a stray `x-simulated-trading` or `reqwest` call anywhere else could
    /// build an OKX request the boundary does not know about.
    #[test]
    fn okx_literals_and_the_reqwest_identifier_stay_in_their_home_files_across_the_crate() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = rust_files(&app.join("src"));
        files.extend(rust_files(&app.join("tests")));
        files.retain(|f| f.file_name().is_some_and(|n| n != "static_checks.rs"));
        assert!(files.iter().any(|f| f.ends_with("ui/live.rs")), "ui/live.rs (the shared signed transport) must be scanned");
        assert!(files.iter().any(|f| f.ends_with("main.rs")));
        assert!(files.len() > 30, "scanned only {} files", files.len());
        let mut violations = Vec::new();
        for f in &files {
            let rel_path = f.strip_prefix(app).unwrap().to_string_lossy().replace('\\', "/");
            let rel_path = rel_path.strip_prefix("src/").unwrap_or(&rel_path).to_string();
            let text = std::fs::read_to_string(f).unwrap();
            violations.extend(literal_rule_violations(&rel_path, &text).into_iter().map(|v| format!("{rel_path}: {v}")));
            violations.extend(reqwest_rule_violations(&rel_path, &text).into_iter().map(|v| format!("{rel_path}: {v}")));
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
    }

    #[test]
    fn ui_live_names_no_production_host_literal() {
        let live = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("ui").join("live.rs");
        let v = scan_production_hosts(&std::fs::read_to_string(live).unwrap());
        assert!(v.is_empty(), "{v:?}");
    }

    // ------------------------------------------------ round 2: every known bypass must be caught

    fn has(src: &str, pred: impl Fn(&Violation) -> bool) -> bool {
        scan_signed_source(src).iter().any(pred)
    }
    fn cfg_v(v: &Violation) -> bool {
        matches!(v, Violation::CfgAttribute(_))
    }
    fn macro_v(v: &Violation) -> bool {
        matches!(v, Violation::ForbiddenMacro(_))
    }
    fn esc_v(v: &Violation) -> bool {
        matches!(v, Violation::EscapedString(_))
    }
    fn prod_v(v: &Violation) -> bool {
        matches!(v, Violation::ProductionHost(_))
    }

    #[test]
    fn bypass_cfg_test_on_a_non_mod_item_is_a_violation_and_does_not_hide_its_content() {
        for src in [
            "#[cfg(test)]\nconst X: &str = \"https://fapi.binance.com\";",
            "enum Host { Demo, #[cfg(test)] Prod }",
            "struct S { #[cfg(test)] pub field: String }",
            "fn f(x: u8) { match x { #[cfg(test)] 0 => {}, _ => {} } }",
            "#[cfg(test)]\nfn evil() { client.post(url); }",
            "#[cfg(test)] impl Foo { fn host(&self) {} }",
        ] {
            assert!(has(src, cfg_v), "cfg not flagged: {src}");
        }
        // and what the attribute would have hidden is still scanned
        assert!(has("#[cfg(test)]\nconst X: &str = \"https://fapi.binance.com\";", prod_v));
        assert!(has("#[cfg(test)]\nfn evil() { client.post(url); }", |v| matches!(v, Violation::NonGetMethod(_))));
    }

    #[test]
    fn bypass_other_cfg_forms_are_violations() {
        for src in [
            "#[cfg(not(test))]\nfn f() {}",
            "#[cfg(feature = \"x\")]\nfn f() {}",
            "#[cfg(unix)] const H: &str = \"x\";",
            "#[cfg_attr(test, derive(Debug))]\nstruct S;",
            "#[ cfg(test) ]\nconst X: u8 = 1;",
            "#[cfg(test)]\n#[allow(dead_code)]\nmod tests { }",
            "#![cfg(unix)]\nfn f() {}",
        ] {
            assert!(has(src, cfg_v), "cfg not flagged: {src}");
        }
    }

    #[test]
    fn the_one_allowed_cfg_shape_is_a_complete_test_mod() {
        for src in [
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    const X: &str = \"fapi.binance.com\";\n}\n",
            "#[cfg(test)]\nmod tests;\nfn a() {}",
            "mod outer {\n    #[cfg(test)]\n    mod tests { fn t() { client.post(1); } }\n}",
            "#[cfg(test)] pub(crate) mod helpers { const X: &str = \"www.okx.com\"; }",
        ] {
            assert!(scan_signed_source(src).is_empty(), "wrongly flagged: {src}: {:?}", scan_signed_source(src));
        }
    }

    #[test]
    fn bypass_inner_cfg_test_in_the_middle_of_a_file_is_a_violation_and_hides_nothing() {
        let src = "fn a() {}\n#![cfg(test)]\nconst H: &str = \"https://fapi.binance.com\";";
        assert!(has(src, cfg_v));
        assert!(has(src, prod_v), "the text after a mid-file #![cfg(test)] must still be scanned");
    }

    #[test]
    fn inner_cfg_test_at_the_head_of_the_file_makes_it_a_test_only_file() {
        let src = "//! doc\n#![allow(dead_code)]\n#![cfg(test)]\nconst H: &str = \"www.okx.com\";";
        assert!(scan_signed_source(src).is_empty(), "{:?}", scan_signed_source(src));
    }

    #[test]
    fn bypass_forbidden_macros() {
        for src in [
            "const H: &str = concat!(\"fapi.\", \"binance.com\");",
            "const H: &str = include_str!(\"host.txt\");",
            "const H: &[u8] = include_bytes!(\"host.bin\");",
            "const H: &str = env!(\"HOST\");",
            "const H: Option<&str> = option_env!(\"HOST\");",
            "const H: &str = stringify!(fapi);",
            "include!(\"gen.rs\");",
            "let h = concat !(\"a\", \"b\");",
        ] {
            assert!(has(src, macro_v), "macro not flagged: {src}");
        }
        assert!(!has("let a = 1; if a != 2 { println!(\"x\"); } let v = vec![1]; format!(\"{}\", a);", macro_v), "ordinary macros and != are fine");
    }

    #[test]
    fn bypass_escaped_strings_are_violations() {
        for src in [
            "const H: &str = \"fapi\\x2ebinance.com\";",
            "const H: &str = \"fapi\\u{2e}binance\\u{2e}com\";",
            "const H: &[u8] = b\"\\x66api\";",
            "const H: &str = \"fapi.\\\n    binance.com\";",
        ] {
            assert!(has(src, esc_v), "escape not flagged: {src}");
        }
        for ok in ["let s = \"line\\n\";", "let s = \"quote\\\"\";", "let s = \"back\\\\x41\";", "let s = r\"raw \\x41\";", "let s = \"tab\\t\";"] {
            assert!(!has(ok, esc_v), "wrongly flagged: {ok}");
        }
    }

    #[test]
    fn bypass_host_pieces_in_separate_literals_are_caught() {
        assert!(has("let a = \"fapi.\"; let b = \"binance.com\"; let h = format!(\"{a}{b}\");", prod_v));
        assert!(has("let b = \"bybit.com\";", prod_v));
        assert!(has("let b = \"okx.com\";", prod_v));
        assert!(!has("let a = \"demo-fapi.binance.com\"; let b = \"api-demo.bybit.com\"; let c = \"testnet.binancefuture.com\";", prod_v));
    }

    #[test]
    fn bypass_upper_and_mixed_case_hosts_are_caught() {
        assert!(has("const H: &str = \"https://FAPI.BINANCE.COM\";", prod_v));
        assert!(has("const H: &str = \"Api.Bybit.Com\";", prod_v));
        assert!(has("const H: &str = \"WWW.OKX.COM\";", prod_v));
        assert!(!has("const H: &str = \"DEMO-FAPI.BINANCE.COM\";", prod_v), "demo host in any case is fine");
    }

    #[test]
    fn bypass_importing_the_public_module_is_a_violation() {
        for src in [
            "use crate::exchange::public::endpoints::BINANCE_HOST;",
            "use super::super::public::endpoints;",
            "fn f() -> String { crate::exchange::public::endpoints::binance_url(\"/x\") }",
            "use crate::exchange::{public, signed};",
            "use crate::exchange::{error, public as p};",
        ] {
            assert!(has(src, |v| matches!(v, Violation::ImportsPublic)), "not flagged: {src}");
        }
        for ok in ["const REASON: &str = \"public market data only\";", "fn is_public() {}", "let publicize = 1;", "// use crate::exchange::public;"] {
            assert!(!has(ok, |v| matches!(v, Violation::ImportsPublic)), "wrongly flagged: {ok}");
        }
    }

    #[test]
    fn bypass_pub_fn_parameter_names_containing_host_like_words_are_violations() {
        for src in [
            "pub fn new(endpoint: &str) -> Self { todo!() }",
            "pub fn with(url: String) -> Self { todo!() }",
            "pub fn at(root: &str) -> Self { todo!() }",
            "pub fn at(uri: Uri) -> Self { todo!() }",
            "pub fn at(api_host: &str) -> Self { todo!() }",
            "pub fn at(hostName: &str) -> Self { todo!() }",
            "pub fn at(my_base_url: &str) -> Self { todo!() }",
            "pub fn at<T: Clone>(t: T, mut root_url: String) -> Self { todo!() }",
            "pub async fn at(&self, key: &str, endpoint_override: Option<String>) {}",
            "pub struct C { pub endpoint: String }",
            "pub struct C { pub(crate) root_url: String }",
        ] {
            assert!(has(src, |v| matches!(v, Violation::ExternalBaseUrl(_))), "not flagged: {src}");
        }
    }

    #[test]
    fn parameter_scan_looks_at_names_not_types_or_return_types() {
        for ok in [
            "pub fn new(transport: Arc<T>, demo_env: BinanceHost) -> Self { todo!() }",
            "pub fn host(self) -> &'static str { \"x\" }",
            "pub fn base_url(self) -> &'static str { \"x\" }",
            "pub fn get(&self, key: &str) -> Result<Url, E> { todo!() }",
            "pub fn at(offset: Arc<dyn Fn(Url) -> Host>) {}",
            "fn private(base_url: &str) {}",
        ] {
            assert!(!has(ok, |v| matches!(v, Violation::ExternalBaseUrl(_))), "wrongly flagged: {ok}");
        }
    }
}

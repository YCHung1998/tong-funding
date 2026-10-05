//! Static source checks for the exchange module (spec: exchange-adapter, "公開行情與簽名請求使用
//! 結構上分離的 HTTP 客戶端"). They read the `.rs` files under `src/exchange` at test time, so they
//! also cover code other people write into `signed/` later.
//!
//! What is scanned is *production code*: comments are ignored (a comment can mention a host
//! without being able to connect to it) and so are `#[cfg(test)]` modules (tests may assert that a
//! production host is NOT used). String literals are scanned.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Production hosts that only the public (unsigned) clients may name.
pub const PRODUCTION_HOSTS: [&str; 3] = ["fapi.binance.com", "api.bybit.com", "www.okx.com"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    ProductionHost(String),
    /// A public fn / field through which a caller could choose the host.
    ExternalBaseUrl(String),
    /// Host or credentials read from the environment or a config file.
    EnvOrConfigRead(String),
    /// POST / PUT / DELETE / PATCH in a read-only client.
    NonGetMethod(String),
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

/// Returns (code, structure): both have comments blanked; `structure` additionally has string and
/// char literal contents blanked, so brace matching and keyword search cannot be fooled by them.
fn mask(src: &str) -> (Vec<u8>, Vec<u8>) {
    let b = src.as_bytes();
    let n = b.len();
    let mut code = b.to_vec();
    let mut structure = b.to_vec();
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
            blank(&mut structure, start, end);
            i = (end + closer.len()).min(n);
        } else if c == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = j.min(n);
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
    (code, structure)
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// Index just past the item that starts at `from` (up to a `;` or a balanced `{ ... }`).
fn item_end(structure: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < structure.len() {
        match structure[i] {
            b';' => return i + 1,
            b'{' => {
                let mut depth = 0usize;
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
                return structure.len();
            }
            _ => i += 1,
        }
    }
    structure.len()
}

/// Source with comments blanked out and `#[cfg(test)]` items removed; string literals kept.
/// (Blanked ranges become spaces, newlines are kept, so lines are preserved.)
pub fn production_code(src: &str) -> String {
    let (mut code, structure) = mask(src);
    if find_from(&structure, b"#![cfg(test)]", 0).is_some() {
        let len = code.len();
        blank(&mut code, 0, len);
        return String::from_utf8_lossy(&code).into_owned();
    }
    let mut from = 0;
    while let Some(at) = find_from(&structure, b"#[cfg(test)]", from) {
        let end = item_end(&structure, at + b"#[cfg(test)]".len());
        blank(&mut code, at, end);
        from = end;
    }
    String::from_utf8_lossy(&code).into_owned()
}

fn norm(ident: &str) -> String {
    ident.to_ascii_lowercase().replace('_', "")
}

fn is_host_ident(ident: &str) -> bool {
    matches!(norm(ident).as_str(), "baseurl" | "baseuri" | "host" | "hostname")
}

fn idents(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|t| !t.is_empty()).collect()
}

/// Public fns that take a host / base URL parameter, and public fields of that name.
fn external_base_url(code: &str) -> Vec<Violation> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = find_from(b, b"pub", from) {
        from = at + 3;
        if (at > 0 && is_ident(b[at - 1])) || from >= b.len() || is_ident(b[from]) {
            continue; // part of a longer identifier
        }
        let mut i = from;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b'(' {
            // pub(crate) / pub(super) / pub(in path)
            while i < b.len() && b[i] != b')' {
                i += 1;
            }
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        let rest = &code[i.min(code.len())..];
        let head_end = rest.find(['(', '{', ';', ':', '<']).unwrap_or(rest.len());
        let head = idents(&rest[..head_end]);
        let is_fn = head.contains(&"fn") && head.iter().all(|w| matches!(*w, "async" | "const" | "unsafe" | "extern" | "fn") || head.last() == Some(w));
        if is_fn {
            let sig = &rest[..rest.find(['{', ';']).unwrap_or(rest.len())];
            let toks = idents(sig);
            let fn_pos = toks.iter().position(|t| *t == "fn").unwrap_or(0);
            // skip `fn` and the fn's own name
            if toks.iter().skip(fn_pos + 2).any(|t| is_host_ident(t)) {
                out.push(Violation::ExternalBaseUrl(sig.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        } else if let Some(field) = head.first() {
            if is_host_ident(field) && rest[head_end..].starts_with(':') {
                out.push(Violation::ExternalBaseUrl(format!("pub field {field}")));
            }
        }
    }
    out
}

/// Scans one signed-client source file's text.
pub fn scan_signed_source(src: &str) -> Vec<Violation> {
    let code = production_code(src);
    let mut out = scan_production_hosts_in(&code);
    out.extend(external_base_url(&code));
    for pat in ["env::var", "env::vars", "std::env", "dotenv", "read_to_string", "File::open", "env!("] {
        if code.contains(pat) {
            out.push(Violation::EnvOrConfigRead(pat.to_string()));
        }
    }
    for pat in [
        ".post(", ".put(", ".delete(", ".patch(", "Method::POST", "Method::PUT", "Method::DELETE", "Method::PATCH", "\"POST\"", "\"PUT\"",
        "\"DELETE\"", "\"PATCH\"",
    ] {
        if code.contains(pat) {
            out.push(Violation::NonGetMethod(pat.to_string()));
        }
    }
    out
}

fn scan_production_hosts_in(code: &str) -> Vec<Violation> {
    PRODUCTION_HOSTS.iter().filter(|h| names_host(code, h)).map(|h| Violation::ProductionHost((*h).to_string())).collect()
}

/// True if `host` occurs as a whole host name. A longer name that merely ends with it, such as the
/// demo host `demo-fapi.binance.com` (ends with `fapi.binance.com`), does not count; a real
/// subdomain (`x.fapi.binance.com`, preceded by `.`) does.
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
                violations.push(format!("{}: {v:?}", f.display()));
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
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
                violations.push(format!("{}: {v:?}", f.display()));
            }
        }
        assert!(violations.is_empty(), "violations:\n{}", violations.join("\n"));
    }

    #[test]
    fn no_order_mutating_fns_outside_public_and_health() {
        let files: Vec<_> = rust_files(&exchange_dir())
            .into_iter()
            .filter(|p| {
                !p.starts_with(exchange_dir().join("public"))
                    && !p.starts_with(exchange_dir().join("health"))
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
}

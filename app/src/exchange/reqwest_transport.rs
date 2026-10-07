//! The real `HttpTransport` (GET only) on `reqwest` + rustls.
//! Transport failures become `AdapterError` (timeout / network); HTTP status codes are returned
//! untouched, because mapping 429 / 4xx / 5xx is the caller's job.
//!
//! Second line of defence for "no signed request ever reaches a production host": a transport is
//! built with a [`HostPolicy`] and refuses (zero connections) any URL whose host the policy does
//! not list. Signed clients are meant to use [`ReqwestTransport::signed_demo`], public market data
//! [`ReqwestTransport::public_production`].
#![allow(dead_code)]

use super::error::AdapterError;
use super::transport::{HttpRequest, HttpResponse, HttpTransport};

/// Largest response body accepted (32 MiB). Bigger is `Incomplete`, never silently truncated.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPolicy {
    /// Only the public production market-data hosts (https, default port).
    PublicProduction,
    /// Only the demo / testnet hosts that may receive signed requests (https, default port).
    SignedDemo,
    /// 127.0.0.1 over http: local fake servers in tests.
    #[cfg(test)]
    LocalTest,
    /// Like `LocalTest`, but 127.0.0.1 stands in for the OKX host: the OKX header rule applies.
    #[cfg(test)]
    LocalOkxTest,
}

fn host_of_base(base_url: &str) -> &str {
    base_url.strip_prefix("https://").unwrap_or(base_url)
}

impl HostPolicy {
    /// The ONE admission rule of every real transport (GET and order transport alike): the host
    /// must be on the policy's list AND the headers must satisfy the OKX demo rule.
    ///
    /// OKX demo and production share a host, so the host alone proves nothing: on the signed
    /// policy an OKX host (any `*.okx.com`) needs exactly one `x-simulated-trading` header whose
    /// value is exactly `1`, and so does any request carrying an `OK-ACCESS-*` header, whatever
    /// its host. The public policy is unsigned: it refuses both header families.
    pub fn allows(self, url: &reqwest::Url, headers: &[(String, String)]) -> bool {
        use crate::exchange::public::endpoints::{BINANCE_HOST, BYBIT_HOST, OKX_HOST};
        use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, carries_okx_auth, is_okx_host, is_protected_okx_header, okx_headers_valid};
        let okx_rule = || okx_headers_valid(headers);
        match self {
            HostPolicy::PublicProduction => {
                https_host_in(url, &[host_of_base(BINANCE_HOST), host_of_base(BYBIT_HOST), host_of_base(OKX_HOST)]) && !headers.iter().any(|(n, _)| is_protected_okx_header(n))
            }
            HostPolicy::SignedDemo => {
                https_host_in(url, &ALLOWED_SIGNED_HOSTS) && (!(url.host_str().is_some_and(is_okx_host) || carries_okx_auth(headers)) || okx_rule())
            }
            #[cfg(test)]
            HostPolicy::LocalTest => url.scheme() == "http" && url.host_str() == Some("127.0.0.1") && (!carries_okx_auth(headers) || okx_rule()),
            #[cfg(test)]
            HostPolicy::LocalOkxTest => url.scheme() == "http" && url.host_str() == Some("127.0.0.1") && okx_rule(),
        }
    }
}

/// https, no credentials in the URL, default port, and the (already lower-cased, dot-preserving)
/// host is exactly one of `hosts`. `https://allowed@evil/` parses with host `evil`, so it fails.
fn https_host_in(url: &reqwest::Url, hosts: &[&str]) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(|h| hosts.contains(&h))
}

pub struct ReqwestTransport {
    client: reqwest::Client,
    policy: HostPolicy,
    max_body_bytes: usize,
}

impl ReqwestTransport {
    fn build(policy: HostPolicy) -> Result<Self, AdapterError> {
        // No redirects, no environment proxy (a proxy would see signed requests and could rewrite
        // headers). reqwest's retry policy is off by default and stays off: a signed request is
        // sent once, retries are decided by the callers.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| AdapterError::network(e.to_string()))?;
        Ok(Self { client, policy, max_body_bytes: MAX_BODY_BYTES })
    }

    /// For public (unsigned) market data: only the production market-data hosts.
    pub fn public_production() -> Result<Self, AdapterError> {
        Self::build(HostPolicy::PublicProduction)
    }

    /// For signed read-only requests: only demo / testnet hosts.
    pub fn signed_demo() -> Result<Self, AdapterError> {
        Self::build(HostPolicy::SignedDemo)
    }

    #[cfg(test)]
    fn local_for_tests() -> Result<Self, AdapterError> {
        Self::build(HostPolicy::LocalTest)
    }

    #[cfg(test)]
    fn local_okx_for_tests() -> Result<Self, AdapterError> {
        Self::build(HostPolicy::LocalOkxTest)
    }

    #[cfg(test)]
    fn with_max_body_bytes(mut self, n: usize) -> Self {
        self.max_body_bytes = n;
        self
    }

    pub fn policy(&self) -> HostPolicy {
        self.policy
    }
}

fn map_error(e: reqwest::Error) -> AdapterError {
    if e.is_timeout() {
        AdapterError::Timeout
    } else {
        // `AdapterError::network` redacts signatures / keys that may sit in the URL.
        AdapterError::network(e.to_string())
    }
}

impl HttpTransport for ReqwestTransport {
    async fn get(&self, req: HttpRequest) -> Result<HttpResponse, AdapterError> {
        let url = reqwest::Url::parse(&req.url).map_err(|_| AdapterError::network("invalid url"))?;
        if req.misuses_protected_header() || !self.policy.allows(&url, &req.headers) {
            return Err(AdapterError::network("host not allowed"));
        }
        let mut builder = self.client.get(url).timeout(req.timeout);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let mut resp = builder.send().await.map_err(map_error)?;
        let too_big = || AdapterError::incomplete(format!("response body exceeds {} bytes", self.max_body_bytes));
        if resp.content_length().is_some_and(|n| n > self.max_body_bytes as u64) {
            return Err(too_big());
        }
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        // Stream the body so a hostile or broken server cannot make us buffer without bound.
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(map_error)? {
            if buf.len().saturating_add(chunk.len()) > self.max_body_bytes {
                return Err(too_big());
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(HttpResponse { status, headers, body: String::from_utf8_lossy(&buf).into_owned() })
    }
}

/// Serialises the tests that set `HTTP_PROXY` (process-wide state), across both transports.
#[cfg(test)]
pub(crate) static PROXY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    /// Minimal server on 127.0.0.1, one request per connection. `handler` gets the raw request
    /// text and returns the raw response (None = read the request, then stall for 3 s).
    fn serve(handler: impl Fn(&str) -> Option<String> + Send + 'static) -> String {
        serve_counting(handler).0
    }

    /// Same, also returning a counter of accepted connections.
    fn serve_counting(handler: impl Fn(&str) -> Option<String> + Send + 'static) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let mut req = String::new();
                while !req.contains("\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.push_str(&String::from_utf8_lossy(&buf[..n])),
                    }
                }
                match handler(&req) {
                    Some(resp) => {
                        let _ = stream.write_all(resp.as_bytes());
                    }
                    None => std::thread::sleep(Duration::from_secs(3)),
                }
            }
        });
        (format!("http://{addr}"), count)
    }

    fn response(status: &str, extra: &str, body: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[test]
    fn returns_200_body_and_headers_and_sends_custom_headers() {
        let base = serve(|req| {
            assert!(req.starts_with("GET /fapi/v1/time?x=1 "), "request line: {req}");
            assert!(req.to_lowercase().contains("x-test: hello"));
            Some(response("200 OK", "X-Mbx-Used-Weight-1m: 42\r\n", r#"{"serverTime":1}"#))
        });
        let t = ReqwestTransport::local_for_tests().unwrap();
        let resp = block_on(t.get(HttpRequest::get(format!("{base}/fapi/v1/time?x=1"), Duration::from_secs(2)).header("X-Test", "hello"))).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, r#"{"serverTime":1}"#);
        assert_eq!(resp.header_value("x-mbx-used-weight-1m"), Some("42"));
    }

    #[test]
    fn status_codes_are_returned_as_is_not_turned_into_errors() {
        let base = serve(|req| {
            if req.contains("/limited") {
                Some(response("429 Too Many Requests", "Retry-After: 5\r\n", "slow down"))
            } else if req.contains("/teapot") {
                Some(response("418 I'm a teapot", "", "banned"))
            } else {
                Some(response("500 Internal Server Error", "", "boom"))
            }
        });
        let t = ReqwestTransport::local_for_tests().unwrap();
        let get = |p: &str| block_on(t.get(HttpRequest::get(format!("{base}{p}"), Duration::from_secs(2)))).unwrap();
        let r = get("/limited");
        assert_eq!((r.status, r.header_value("retry-after"), r.body.as_str()), (429, Some("5"), "slow down"));
        assert_eq!(get("/teapot").status, 418);
        assert_eq!(get("/x").status, 500);
    }

    #[test]
    fn unresponsive_server_yields_timeout_after_about_the_request_timeout() {
        let base = serve(|_| None);
        let t = ReqwestTransport::local_for_tests().unwrap();
        let started = Instant::now();
        let r = block_on(t.get(HttpRequest::get(format!("{base}/hang"), Duration::from_millis(300))));
        assert_eq!(r, Err(AdapterError::Timeout));
        assert!(started.elapsed() < Duration::from_millis(2500), "took {:?}", started.elapsed());
    }

    #[test]
    fn refused_connection_is_a_redacted_network_error() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        }; // listener dropped: nothing listens here
        let t = ReqwestTransport::local_for_tests().unwrap();
        let r = block_on(t.get(HttpRequest::get(format!("http://127.0.0.1:{port}/p?timestamp=1&signature=deadbeef"), Duration::from_secs(2))));
        match r {
            Err(AdapterError::Network(msg)) => {
                assert!(!msg.contains("deadbeef"), "signature leaked: {msg}");
                assert!(!msg.contains("stub"), "stub error: {msg}");
            }
            other => panic!("expected Network error, got {other:?}"),
        }
    }

    #[test]
    fn malformed_url_is_a_network_error_not_a_panic() {
        let t = ReqwestTransport::local_for_tests().unwrap();
        let r = block_on(t.get(HttpRequest::get("not a url", Duration::from_secs(1))));
        assert!(matches!(&r, Err(AdapterError::Network(m)) if !m.contains("stub")), "{r:?}");
    }

    // ------------------------------------------------ round 2: host policy and body limit

    use crate::exchange::public::endpoints::{BINANCE_HOST, BYBIT_HOST, OKX_HOST};
    use crate::exchange::signed::endpoints::{ALLOWED_SIGNED_HOSTS, OKX_DEMO_HOST, is_okx_host};

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn public_production_policy_allows_exactly_the_three_public_hosts_over_https() {
        let p = HostPolicy::PublicProduction;
        for base in [BINANCE_HOST, BYBIT_HOST, OKX_HOST] {
            assert!(p.allows(&url(&format!("{base}/x?symbol=A")), &[]), "{base}");
            assert!(p.allows(&url(&format!("{base}:443/x")), &[]), "explicit default port");
            assert!(!p.allows(&url(&base.replace("https://", "http://")), &[]), "plain http");
            assert!(!p.allows(&url(&format!("{base}:8443/x")), &[]), "other port");
            assert!(!p.allows(&url(&format!("{base}.evil.example/x")), &[]), "suffix lookalike");
            assert!(!p.allows(&url(&format!("{}evil.example/x", base.replace("https://", "https://x"))), &[]), "prefix lookalike");
            assert!(!p.allows(&url(&format!("{}@evil.example/x", base)), &[]), "userinfo trick: real host is evil.example");
            assert!(!p.allows(&url(&format!("{}:pw@{}/x", "https://user", base.replace("https://", ""))), &[]), "credentials in URL");
        }
        for h in ALLOWED_SIGNED_HOSTS {
            assert!(!p.allows(&url(&format!("https://{h}/x")), &[]), "demo host {h} is not a public host");
        }
        assert!(!p.allows(&url("https://example.com/"), &[]));
    }

    #[test]
    fn signed_demo_policy_allows_exactly_the_demo_hosts_and_never_a_production_host() {
        let p = HostPolicy::SignedDemo;
        let sim = hdrs(&[("x-simulated-trading", "1")]);
        for h in ALLOWED_SIGNED_HOSTS {
            let headers = if is_okx_host(h) { &sim[..] } else { &[][..] };
            assert!(p.allows(&url(&format!("https://{h}/fapi/v2/balance?timestamp=1&signature=x")), headers), "{h}");
            assert!(!p.allows(&url(&format!("http://{h}/x")), headers), "plain http {h}");
            assert!(!p.allows(&url(&format!("https://{h}.evil.example/x")), headers), "{h}");
        }
        for base in [BINANCE_HOST, BYBIT_HOST, OKX_HOST] {
            assert!(!p.allows(&url(&format!("{base}/fapi/v2/balance")), &sim), "production host {base} must never get a signed request");
        }
        assert!(!p.allows(&url("https://example.com/"), &sim));
    }

    #[test]
    fn a_trailing_dot_or_uppercase_does_not_slip_past_the_policy() {
        let p = HostPolicy::PublicProduction;
        let base = BINANCE_HOST.replace("https://", "");
        assert!(p.allows(&url(&format!("https://{}/", base.to_uppercase())), &[]), "host names are case-insensitive");
        assert!(!p.allows(&url(&format!("https://{base}./")), &[]), "FQDN dot is not in the whitelist: refuse (fail closed)");
    }

    // ------------------------------------------------ OKX: one policy function decides host AND header

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
    }

    #[test]
    fn signed_demo_requires_exactly_one_simulated_header_with_value_one_for_the_okx_host() {
        let p = HostPolicy::SignedDemo;
        let u = url(&format!("https://{OKX_DEMO_HOST}/api/v5/account/balance"));
        assert!(p.allows(&u, &hdrs(&[("x-simulated-trading", "1")])));
        assert!(p.allows(&u, &hdrs(&[("OK-ACCESS-KEY", "k"), ("X-SIMULATED-TRADING", "1")])));
        for (what, h) in [
            ("none", hdrs(&[])),
            ("value 0", hdrs(&[("x-simulated-trading", "0")])),
            ("empty", hdrs(&[("x-simulated-trading", "")])),
            ("other value", hdrs(&[("x-simulated-trading", "true")])),
            ("duplicate 1", hdrs(&[("x-simulated-trading", "1"), ("x-simulated-trading", "1")])),
            ("mixed-case duplicate", hdrs(&[("x-simulated-trading", "1"), ("X-Simulated-Trading", "1")])),
            ("duplicate 0 then 1", hdrs(&[("x-simulated-trading", "0"), ("x-simulated-trading", "1")])),
            ("only auth headers", hdrs(&[("OK-ACCESS-KEY", "k")])),
        ] {
            assert!(!p.allows(&u, &h), "{what} must be refused");
        }
    }

    #[test]
    fn an_ok_access_header_needs_the_simulated_header_on_any_host_and_never_goes_to_public_hosts() {
        let sim = ("x-simulated-trading", "1");
        // (b): OK-ACCESS-* present => the flag is required even on a non-OKX signed host
        let bybit = url("https://api-demo.bybit.com/x");
        assert!(!HostPolicy::SignedDemo.allows(&bybit, &hdrs(&[("OK-ACCESS-KEY", "k")])));
        assert!(HostPolicy::SignedDemo.allows(&bybit, &hdrs(&[])), "ordinary Bybit requests are unchanged");
        assert!(HostPolicy::SignedDemo.allows(&bybit, &hdrs(&[("X-BAPI-API-KEY", "k")])));
        // public clients are unsigned: neither the flag nor OK-ACCESS-* may appear, and the public OKX host stays usable
        let public_okx = url(&format!("{OKX_HOST}/api/v5/public/time"));
        assert!(HostPolicy::PublicProduction.allows(&public_okx, &[]));
        assert!(!HostPolicy::PublicProduction.allows(&public_okx, &hdrs(&[sim])));
        assert!(!HostPolicy::PublicProduction.allows(&public_okx, &hdrs(&[("OK-ACCESS-KEY", "k")])));
        // the signed OKX host is never reachable by the public policy, with or without the flag
        let signed_okx = url(&format!("https://{OKX_DEMO_HOST}/x"));
        assert!(!HostPolicy::PublicProduction.allows(&signed_okx, &hdrs(&[sim])));
    }

    /// Local fake server + `LocalOkxTest` policy (127.0.0.1 counts as an OKX host): every refusal
    /// below must leave the connection counter at zero.
    #[test]
    fn local_okx_requests_without_a_valid_flag_are_refused_with_zero_connections() {
        let (base, connections) = serve_counting(|_| Some(response("200 OK", "", "{}")));
        let t = ReqwestTransport::local_okx_for_tests().unwrap();
        let get = |headers: &[(&str, &str)]| {
            let mut r = HttpRequest::get(format!("{base}/api/v5/account/balance"), Duration::from_secs(2));
            r.headers = hdrs(headers); // bypass header(): the transport itself must not trust its input
            block_on(t.get(r))
        };
        for (what, h) in [
            ("no flag", vec![("OK-ACCESS-KEY", "k")]),
            ("flag 0", vec![("x-simulated-trading", "0")]),
            ("duplicate 0 and 1", vec![("x-simulated-trading", "0"), ("x-simulated-trading", "1")]),
            ("mixed-case duplicate", vec![("x-simulated-trading", "1"), ("X-Simulated-Trading", "1")]),
            ("no headers at all", vec![]),
        ] {
            assert_eq!(get(&h), Err(AdapterError::network("host not allowed")), "{what}");
        }
        // an attempt through the generic header() API is refused too, even though the flag is "set"
        let via_header = HttpRequest::get(format!("{base}/api/v5/account/balance"), Duration::from_secs(2)).header("x-simulated-trading", "1").header("OK-ACCESS-KEY", "k");
        assert_eq!(block_on(t.get(via_header)), Err(AdapterError::network("host not allowed")), "header() attempt");
        assert_eq!(connections.load(Ordering::SeqCst), 0, "zero connections for every refusal");
    }

    #[test]
    fn a_request_built_by_the_okx_constructor_is_sent_with_the_flag_exactly_once() {
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_in = seen.clone();
        let (base, connections) = serve_counting(move |req| {
            *seen_in.lock().unwrap() = req.to_lowercase();
            Some(response("200 OK", "", "{}"))
        });
        let t = ReqwestTransport::local_okx_for_tests().unwrap();
        // the constructor targets openapi.okx.com; only its parts are reused so it can hit the local server
        let target = crate::exchange::signed::endpoints::OkxHost::Demo.target();
        let built = HttpRequest::okx_signed_get(&target, "/p", vec![("OK-ACCESS-KEY".into(), "k".into())], Duration::from_secs(2));
        let mut local = HttpRequest::get(format!("{base}/p"), Duration::from_secs(2));
        local.headers = built.headers.clone();
        assert!(block_on(t.get(local)).is_ok());
        assert_eq!(connections.load(Ordering::SeqCst), 1);
        let raw = seen.lock().unwrap().clone();
        assert_eq!(raw.matches("x-simulated-trading: 1").count(), 1, "{raw}");
        assert!(raw.contains("ok-access-key: k"));
    }

    // ------------------------------------------------ no proxy, no redirect, no retry

    #[test]
    fn the_real_clients_ignore_proxy_environment_variables() {
        // A listener plays the "proxy". The control proves the environment variable really diverts a
        // default reqwest client (otherwise this test would pass vacuously).
        let _guard = PROXY_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (proxy, proxied) = serve_counting(|_| Some(response("200 OK", "", "via proxy")));
        let (origin, direct) = serve_counting(|_| Some(response("200 OK", "", "direct")));
        // SAFETY: no other test in this crate reads proxy variables; every client built here is built before the variable is removed.
        unsafe {
            std::env::set_var("HTTP_PROXY", &proxy);
            std::env::set_var("http_proxy", &proxy);
        }
        let control = reqwest::Client::builder().build().unwrap();
        let control_body = block_on(async { control.get(format!("{origin}/c")).send().await.unwrap().text().await.unwrap() });
        let t = ReqwestTransport::local_for_tests().unwrap();
        let r = block_on(t.get(HttpRequest::get(format!("{origin}/p"), Duration::from_secs(2))));
        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
        }
        assert_eq!(control_body, "via proxy", "control: the environment proxy must divert a default client");
        let before = proxied.load(Ordering::SeqCst);
        assert_eq!(before, 1, "only the control used the proxy");
        assert_eq!(r.unwrap().body, "direct", "the transport must connect to the origin itself");
        assert_eq!(direct.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_host_outside_the_policy_is_refused_without_any_connection() {
        let (base, connections) = serve_counting(|_| Some(response("200 OK", "", "should never be fetched")));
        for t in [ReqwestTransport::public_production().unwrap(), ReqwestTransport::signed_demo().unwrap()] {
            let r = block_on(t.get(HttpRequest::get(format!("{base}/fapi/v1/time?timestamp=1&signature=deadbeef"), Duration::from_secs(2))));
            assert_eq!(r, Err(AdapterError::network("host not allowed")));
        }
        assert_eq!(connections.load(Ordering::SeqCst), 0, "zero connections");
    }

    #[test]
    fn refusal_happens_before_dns_for_names_that_do_not_exist() {
        let t = ReqwestTransport::signed_demo().unwrap();
        let started = Instant::now();
        let r = block_on(t.get(HttpRequest::get("https://definitely-not-allowed.invalid/x", Duration::from_secs(5))));
        assert_eq!(r, Err(AdapterError::network("host not allowed")), "not a DNS error");
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn constructors_carry_their_policy() {
        assert_eq!(ReqwestTransport::public_production().unwrap().policy(), HostPolicy::PublicProduction);
        assert_eq!(ReqwestTransport::signed_demo().unwrap().policy(), HostPolicy::SignedDemo);
    }

    #[test]
    fn body_limit_is_32_mib() {
        assert_eq!(MAX_BODY_BYTES, 33_554_432);
    }

    #[test]
    fn a_body_over_the_limit_is_incomplete_not_truncated_with_content_length() {
        let big = "x".repeat(5_000);
        let base = serve(move |_| Some(response("200 OK", "", &big)));
        let t = ReqwestTransport::local_for_tests().unwrap().with_max_body_bytes(1_024);
        let r = block_on(t.get(HttpRequest::get(format!("{base}/big"), Duration::from_secs(2))));
        assert!(matches!(r, Err(AdapterError::Incomplete(_))), "{r:?}");
    }

    #[test]
    fn a_body_over_the_limit_is_incomplete_when_the_server_gives_no_content_length() {
        let big = "y".repeat(5_000);
        let base = serve(move |_| Some(format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{big}")));
        let t = ReqwestTransport::local_for_tests().unwrap().with_max_body_bytes(1_024);
        let r = block_on(t.get(HttpRequest::get(format!("{base}/big"), Duration::from_secs(2))));
        assert!(matches!(r, Err(AdapterError::Incomplete(_))), "{r:?}");
    }

    #[test]
    fn a_body_exactly_at_the_limit_is_accepted() {
        let body = "z".repeat(1_024);
        let expected = body.clone();
        let base = serve(move |_| Some(response("200 OK", "", &body)));
        let t = ReqwestTransport::local_for_tests().unwrap().with_max_body_bytes(1_024);
        let r = block_on(t.get(HttpRequest::get(format!("{base}/ok"), Duration::from_secs(2)))).unwrap();
        assert_eq!(r.body, expected);
    }
}

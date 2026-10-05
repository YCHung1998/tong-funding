//! The order-capable HTTP path (change: exchange-demo-execution, design D1, D2). This is the ONLY
//! place in the crate that may send POST / DELETE (static check
//! `non_get_methods_only_in_execution`). A request can only be built for one of the compile-time
//! demo/testnet hosts ([`DemoEnv`]): there is no constructor taking a URL or a host name, and the
//! real transport refuses (zero connections) any host outside `ALLOWED_SIGNED_HOSTS` as a second
//! line of defence.

use std::future::Future;
use std::time::Duration;

use tong_funding_core::redact::redact_secrets;

use crate::exchange::error::AdapterError;
use crate::exchange::reqwest_transport::{HostPolicy, MAX_BODY_BYTES};
use crate::exchange::signed::endpoints::{BinanceHost, BybitHost};
use crate::exchange::transport::HttpResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Delete => "DELETE",
        }
    }
}

/// Which compile-time demo/testnet host a request goes to. The only knob a caller has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoEnv {
    Binance(BinanceHost),
    Bybit(BybitHost),
}

impl DemoEnv {
    fn base(self) -> &'static str {
        match self {
            DemoEnv::Binance(h) => h.base_url(),
            DemoEnv::Bybit(h) => h.base_url(),
        }
    }
}

/// One signed order request. Fields are private: it is built only by [`OrderHttpRequest::to_demo`].
#[derive(Clone, PartialEq, Eq)]
pub struct OrderHttpRequest {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    timeout: Duration,
}

impl OrderHttpRequest {
    /// `path_and_query` starts with `/` and is appended to the demo host's base URL.
    pub fn to_demo(method: Method, demo_env: DemoEnv, path_and_query: &str, timeout: Duration) -> Self {
        OrderHttpRequest { method, url: format!("{}{}", demo_env.base(), path_and_query), headers: Vec::new(), body: None, timeout }
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
    pub fn json_body(mut self, body: String) -> Self {
        self.headers.push(("Content-Type".into(), "application/json".into()));
        self.body = Some(body);
        self
    }
    pub fn method(&self) -> Method {
        self.method
    }
    pub fn full_url(&self) -> &str {
        &self.url
    }
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Header values carry the API key and signature: never printed.
impl std::fmt::Debug for OrderHttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<(&str, &str)> = self.headers.iter().map(|(n, _)| (n.as_str(), "[REDACTED]")).collect();
        f.debug_struct("OrderHttpRequest")
            .field("method", &self.method)
            .field("url", &redact_secrets(&self.url))
            .field("headers", &names)
            .field("body", &self.body.as_deref().map(redact_secrets))
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Order transport: transport-level failures (timeout, connection reset, TLS) are `AdapterError`;
/// HTTP status codes are returned untouched (classification is `classify`'s job).
pub trait OrderTransport: Send + Sync {
    fn send(&self, req: OrderHttpRequest) -> impl Future<Output = Result<HttpResponse, AdapterError>> + Send;
}

/// The real transport on `reqwest` + rustls, restricted to the signed demo/testnet hosts.
pub struct ReqwestOrderTransport {
    client: reqwest::Client,
}

impl ReqwestOrderTransport {
    pub fn signed_demo() -> Result<Self, AdapterError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AdapterError::network(e.to_string()))?;
        Ok(ReqwestOrderTransport { client })
    }
}

fn map_error(e: reqwest::Error) -> AdapterError {
    if e.is_timeout() { AdapterError::Timeout } else { AdapterError::network(e.to_string()) }
}

impl OrderTransport for ReqwestOrderTransport {
    async fn send(&self, req: OrderHttpRequest) -> Result<HttpResponse, AdapterError> {
        let parsed = reqwest::Url::parse(&req.url).map_err(|_| AdapterError::network("invalid url"))?;
        if !HostPolicy::SignedDemo.allows(&parsed) {
            return Err(AdapterError::network("host not allowed"));
        }
        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self.client.request(method, parsed).timeout(req.timeout);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = req.body {
            builder = builder.body(body);
        }
        let mut resp = builder.send().await.map_err(map_error)?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(map_error)? {
            if buf.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err(AdapterError::incomplete("response body too large"));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(HttpResponse { status, headers, body: String::from_utf8_lossy(&buf).into_owned() })
    }
}

#[cfg(test)]
pub mod fake {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// One scripted reply: a response or a transport error, optionally after a delay on tokio's
    /// (pausable) clock.
    #[derive(Clone, Debug)]
    pub struct Reply {
        pub result: Result<HttpResponse, AdapterError>,
        pub delay_ms: u64,
    }

    impl Reply {
        pub fn ok(body: &str) -> Reply {
            Reply { result: Ok(HttpResponse::ok(body)), delay_ms: 0 }
        }
        pub fn status(status: u16, body: &str) -> Reply {
            Reply { result: Ok(HttpResponse::with_status(status, body)), delay_ms: 0 }
        }
        pub fn err(e: AdapterError) -> Reply {
            Reply { result: Err(e), delay_ms: 0 }
        }
        pub fn after(mut self, ms: u64) -> Reply {
            self.delay_ms = ms;
            self
        }
        pub fn with_header(mut self, name: &str, value: &str) -> Reply {
            if let Ok(r) = &mut self.result {
                r.headers.push((name.to_string(), value.to_string()));
            }
            self
        }
    }

    /// Replays scripted replies keyed by `(method, text the URL contains)`; replies for the same key
    /// are consumed in order and the last one repeats. Records every request (and the tokio
    /// instant it arrived / was answered) so tests can check hosts, parameters and parallelism.
    #[derive(Default, Clone)]
    pub struct FakeOrderTransport {
        inner: Arc<Inner>,
    }

    /// `(method, text the URL contains)`.
    type Key = (Method, String);

    #[derive(Default)]
    struct Inner {
        script: Mutex<Vec<(Key, VecDeque<Reply>)>>,
        seen: Mutex<Vec<OrderHttpRequest>>,
        log: Mutex<Vec<(String, tokio::time::Instant)>>,
    }

    impl FakeOrderTransport {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn on(&self, method: Method, url_contains: &str, reply: Reply) -> &Self {
            let mut s = self.inner.script.lock().unwrap();
            match s.iter_mut().find(|(k, _)| k.0 == method && k.1 == url_contains) {
                Some((_, q)) => q.push_back(reply),
                None => s.push(((method, url_contains.to_string()), VecDeque::from([reply]))),
            }
            self
        }
        /// Drop every scripted reply for `(method, url_contains)` and use `reply` from now on.
        pub fn replace(&self, method: Method, url_contains: &str, reply: Reply) -> &Self {
            self.inner.script.lock().unwrap().retain(|(k, _)| !(k.0 == method && k.1 == url_contains));
            self.on(method, url_contains, reply)
        }
        pub fn requests(&self) -> Vec<OrderHttpRequest> {
            self.inner.seen.lock().unwrap().clone()
        }
        pub fn count(&self, method: Method, url_contains: &str) -> usize {
            self.requests().iter().filter(|r| r.method() == method && r.full_url().contains(url_contains)).count()
        }
        /// `("sent:<url>" | "done:<url>", instant)` in the order they happened.
        pub fn log(&self) -> Vec<(String, tokio::time::Instant)> {
            self.inner.log.lock().unwrap().clone()
        }
    }

    impl OrderTransport for FakeOrderTransport {
        fn send(&self, req: OrderHttpRequest) -> impl Future<Output = Result<HttpResponse, AdapterError>> + Send {
            let reply = {
                let mut script = self.inner.script.lock().unwrap();
                // Longest matching key wins, so a specific script beats a generic one.
                let found = script
                    .iter_mut()
                    .filter(|(k, _)| k.0 == req.method() && req.full_url().contains(k.1.as_str()))
                    .max_by_key(|(k, _)| k.1.len());
                match found {
                    Some((_, q)) if q.len() > 1 => q.pop_front().unwrap(),
                    Some((_, q)) => q.front().cloned().unwrap(),
                    None => Reply::err(AdapterError::network(format!("no fake reply scripted for {} {}", req.method().as_str(), req.full_url()))),
                }
            };
            let url = req.full_url().to_string();
            self.inner.log.lock().unwrap().push((format!("sent:{url}"), tokio::time::Instant::now()));
            self.inner.seen.lock().unwrap().push(req);
            let inner = self.inner.clone();
            async move {
                if reply.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(reply.delay_ms)).await;
                }
                inner.log.lock().unwrap().push((format!("done:{url}"), tokio::time::Instant::now()));
                reply.result
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::signed::endpoints::ALLOWED_SIGNED_HOSTS;

    #[test]
    fn every_demo_env_builds_a_url_on_an_allowed_host() {
        for env in [DemoEnv::Binance(BinanceHost::Testnet), DemoEnv::Binance(BinanceHost::Demo), DemoEnv::Bybit(BybitHost::Demo)] {
            let r = OrderHttpRequest::to_demo(Method::Post, env, "/x?a=1", Duration::from_secs(1));
            let parsed = reqwest::Url::parse(r.full_url()).unwrap();
            assert!(ALLOWED_SIGNED_HOSTS.contains(&parsed.host_str().unwrap()), "{}", r.full_url());
            assert!(HostPolicy::SignedDemo.allows(&parsed));
        }
    }

    #[test]
    fn a_path_cannot_smuggle_another_host() {
        // `path_and_query` is appended to `https://<demo host>`; a crafted path still parses to the demo host.
        let r = OrderHttpRequest::to_demo(Method::Post, DemoEnv::Bybit(BybitHost::Demo), "@evil.example/x", Duration::from_secs(1));
        let parsed = reqwest::Url::parse(r.full_url()).unwrap();
        // `https://api-demo.bybit.com@evil.example/x` would carry credentials: the policy refuses it.
        assert!(!HostPolicy::SignedDemo.allows(&parsed) || parsed.host_str() == Some("api-demo.bybit.com"));
    }

    #[test]
    fn debug_never_prints_header_values_or_a_url_signature() {
        let r = OrderHttpRequest::to_demo(Method::Delete, DemoEnv::Binance(BinanceHost::Testnet), "/fapi/v1/order?timestamp=1&signature=cafebabe", Duration::from_secs(1))
            .header("X-MBX-APIKEY", "SUPERSECRETKEY");
        let out = format!("{r:?}");
        assert!(!out.contains("SUPERSECRETKEY") && !out.contains("cafebabe"), "{out}");
        assert!(out.contains("X-MBX-APIKEY") && out.contains("Delete"));
    }

    #[tokio::test]
    async fn the_real_transport_refuses_a_host_outside_the_allow_list_without_connecting() {
        // Build a request by hand for a non-demo host (only possible inside this module).
        let t = ReqwestOrderTransport::signed_demo().unwrap();
        let mut r = OrderHttpRequest::to_demo(Method::Post, DemoEnv::Bybit(BybitHost::Demo), "/v5/order/create", Duration::from_millis(50));
        r.url = "https://example.invalid/v5/order/create".into();
        assert_eq!(t.send(r).await, Err(AdapterError::network("host not allowed")));
    }
}

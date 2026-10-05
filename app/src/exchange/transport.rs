//! GET-only HTTP abstraction so adapters can be tested with recorded responses.
//! The real implementation (`reqwest`) lives in `exchange::reqwest_transport` (owned by the
//! feed/signed work); tests use [`FakeTransport`].

use std::future::Future;
use std::time::Duration;

use super::error::AdapterError;

#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// Full URL including the query string.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub timeout: Duration,
}

const REDACTED: &str = "[REDACTED]";

/// Header names are useful in logs, header values are where API keys and signatures live: never print them.
fn redacted_headers(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers.iter().map(|(n, _)| (n.as_str(), REDACTED)).collect()
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &tong_funding_core::redact::redact_secrets(&self.url))
            .field("headers", &redacted_headers(&self.headers))
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &redacted_headers(&self.headers))
            .field("body", &self.body)
            .finish()
    }
}

impl HttpRequest {
    pub fn get(url: impl Into<String>, timeout: Duration) -> Self {
        HttpRequest { url: url.into(), headers: Vec::new(), timeout }
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn ok(body: impl Into<String>) -> Self {
        HttpResponse { status: 200, headers: Vec::new(), body: body.into() }
    }
    pub fn with_status(status: u16, body: impl Into<String>) -> Self {
        HttpResponse { status, headers: Vec::new(), body: body.into() }
    }
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Transport-level failures (timeout, connection, TLS) are returned as `AdapterError`, never panics.
/// HTTP status handling (429 → `RateLimited`, 4xx/5xx → `Http`) is the caller's job.
pub trait HttpTransport: Send + Sync {
    fn get(&self, req: HttpRequest) -> impl Future<Output = Result<HttpResponse, AdapterError>> + Send;
}

#[cfg(test)]
#[allow(unused_imports)]
pub use fake::FakeTransport;

#[cfg(test)]
mod fake {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// Replays scripted responses. A script entry matches a request whose URL contains the given
    /// text; entries for the same matcher are consumed in order (the last one repeats).
    #[derive(Default)]
    pub struct FakeTransport {
        script: Mutex<Vec<(String, VecDeque<Result<HttpResponse, AdapterError>>)>>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl FakeTransport {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn on(self, url_contains: &str, response: Result<HttpResponse, AdapterError>) -> Self {
            {
                let mut s = self.script.lock().unwrap();
                match s.iter_mut().find(|(m, _)| m == url_contains) {
                    Some((_, q)) => q.push_back(response),
                    None => s.push((url_contains.to_string(), VecDeque::from([response]))),
                }
            }
            self
        }
        pub fn requests(&self) -> Vec<HttpRequest> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl HttpTransport for FakeTransport {
        fn get(&self, req: HttpRequest) -> impl Future<Output = Result<HttpResponse, AdapterError>> + Send {
            let result = {
                let mut script = self.script.lock().unwrap();
                match script.iter_mut().find(|(m, _)| req.url.contains(m.as_str())) {
                    Some((_, q)) if q.len() > 1 => q.pop_front().unwrap(),
                    Some((_, q)) => q.front().cloned().unwrap(),
                    None => Err(AdapterError::network(format!("no fake response scripted for {}", req.url))),
                }
            };
            self.seen.lock().unwrap().push(req);
            std::future::ready(result)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn block_on<F: Future>(f: F) -> F::Output {
            tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
        }

        #[test]
        fn replays_in_order_then_repeats_the_last_and_records_requests() {
            let t = FakeTransport::new()
                .on("/a", Ok(HttpResponse::ok("one")))
                .on("/a", Ok(HttpResponse::ok("two")))
                .on("/b", Err(AdapterError::Timeout));
            let get = |u: &str| block_on(t.get(HttpRequest::get(u, Duration::from_secs(1))));
            assert_eq!(get("https://h/a").unwrap().body, "one");
            assert_eq!(get("https://h/a").unwrap().body, "two");
            assert_eq!(get("https://h/a").unwrap().body, "two");
            assert_eq!(get("https://h/b"), Err(AdapterError::Timeout));
            assert!(matches!(get("https://h/zzz"), Err(AdapterError::Network(_))));
            assert_eq!(t.requests().len(), 5);
        }
    }
}

#[cfg(test)]
mod debug_tests {
    use super::*;

    #[test]
    fn request_debug_never_prints_header_values_but_keeps_names_url_and_timeout() {
        let req = HttpRequest::get("https://h/x?symbol=BTC", Duration::from_secs(2))
            .header("X-MBX-APIKEY", "SUPERSECRETKEY")
            .header("X-BAPI-SIGN", "deadbeefsig")
            .header("Authorization", "Bearer tok123");
        let out = format!("{req:?} / {req:#?}");
        for secret in ["SUPERSECRETKEY", "deadbeefsig", "tok123"] {
            assert!(!out.contains(secret), "{secret} leaked: {out}");
        }
        assert!(out.contains("X-MBX-APIKEY") && out.contains("X-BAPI-SIGN") && out.contains("[REDACTED]"));
        assert!(out.contains("https://h/x?symbol=BTC") && out.contains("symbol=BTC"));
    }

    #[test]
    fn request_debug_redacts_a_signature_in_the_url_too() {
        let req = HttpRequest::get("https://h/x?timestamp=1&signature=cafebabe", Duration::from_secs(1));
        let out = format!("{req:?}");
        assert!(!out.contains("cafebabe"), "{out}");
        assert!(out.contains("timestamp=1"));
    }

    #[test]
    fn response_debug_hides_header_values_but_shows_status_and_body() {
        let mut r = HttpResponse::with_status(429, "slow down");
        r.headers.push(("Set-Cookie".into(), "session=SECRETCOOKIE".into()));
        r.headers.push(("Retry-After".into(), "5".into()));
        let out = format!("{r:?}");
        assert!(!out.contains("SECRETCOOKIE"), "{out}");
        assert!(out.contains("Set-Cookie") && out.contains("Retry-After") && out.contains("429") && out.contains("slow down"));
    }

    #[test]
    fn request_equality_and_clone_still_work() {
        let a = HttpRequest::get("https://h/", Duration::from_secs(1)).header("A", "1");
        assert_eq!(a.clone(), a);
        assert_ne!(a, HttpRequest::get("https://h/", Duration::from_secs(1)).header("A", "2"));
    }
}

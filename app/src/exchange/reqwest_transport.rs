//! The real `HttpTransport` (GET only) on `reqwest` + rustls.
//! Transport failures become `AdapterError` (timeout / network); HTTP status codes are returned
//! untouched, because mapping 429 / 4xx / 5xx is the caller's job.
#![allow(dead_code)]

use super::error::AdapterError;
use super::transport::{HttpRequest, HttpResponse, HttpTransport};

pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, AdapterError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AdapterError::network(e.to_string()))?;
        Ok(Self { client })
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
        let mut builder = self.client.get(&req.url).timeout(req.timeout);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let resp = builder.send().await.map_err(map_error)?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = resp.text().await.map_err(map_error)?;
        Ok(HttpResponse { status, headers, body })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    /// Minimal server on 127.0.0.1, one request per connection. `handler` gets the raw request
    /// text and returns the raw response (None = read the request, then stall for 3 s).
    fn serve(handler: impl Fn(&str) -> Option<String> + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
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
        format!("http://{addr}")
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
        let t = ReqwestTransport::new().unwrap();
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
        let t = ReqwestTransport::new().unwrap();
        let get = |p: &str| block_on(t.get(HttpRequest::get(format!("{base}{p}"), Duration::from_secs(2)))).unwrap();
        let r = get("/limited");
        assert_eq!((r.status, r.header_value("retry-after"), r.body.as_str()), (429, Some("5"), "slow down"));
        assert_eq!(get("/teapot").status, 418);
        assert_eq!(get("/x").status, 500);
    }

    #[test]
    fn unresponsive_server_yields_timeout_after_about_the_request_timeout() {
        let base = serve(|_| None);
        let t = ReqwestTransport::new().unwrap();
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
        let t = ReqwestTransport::new().unwrap();
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
        let t = ReqwestTransport::new().unwrap();
        let r = block_on(t.get(HttpRequest::get("not a url", Duration::from_secs(1))));
        assert!(matches!(&r, Err(AdapterError::Network(m)) if !m.contains("stub")), "{r:?}");
    }
}

//! The HTTP seam of the store. [`Fetcher`] performs exactly one GET and never follows
//! redirects (the store validates them itself); [`UreqFetcher`] is the default
//! implementation (ureq + rustls, HTTPS only).

use std::io::Read;
use std::time::Duration;

/// One GET request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Deadline for the whole exchange, including the body.
    pub timeout: Duration,
}

/// A response whose body has not been read yet.
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn Read + Send>,
}

impl HttpResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// 2xx
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish()
    }
}

/// Why no response arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    Timeout(String),
    Network(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Timeout(m) | FetchError::Network(m) => f.write_str(m),
        }
    }
}

/// Performs one GET; must not follow redirects.
pub trait Fetcher {
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, FetchError>;
}

impl<F> Fetcher for F
where
    F: Fn(&HttpRequest) -> Result<HttpResponse, FetchError>,
{
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, FetchError> {
        self(request)
    }
}

/// A deadline hit anywhere (connect, TLS handshake, read) surfaces as an I/O error of kind
/// `TimedOut` or `WouldBlock` somewhere in the source chain.
fn is_timeout(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
                return true;
            }
        }
        current = e.source();
    }
    false
}

/// The default fetcher: ureq with rustls, redirects disabled, HTTPS only.
#[derive(Debug, Clone, Copy, Default)]
pub struct UreqFetcher;

impl Fetcher for UreqFetcher {
    fn fetch(&self, request: &HttpRequest) -> Result<HttpResponse, FetchError> {
        if !request.url.starts_with("https://") {
            return Err(FetchError::Network(format!("refusing non-HTTPS URL {}", request.url)));
        }
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(request.timeout)
            .https_only(true)
            .build();
        let mut req = agent.get(&request.url);
        for (k, v) in &request.headers {
            req = req.set(k, v);
        }
        let response = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => {
                let message = t.to_string();
                let timed_out = is_timeout(&t) || message.to_ascii_lowercase().contains("timed out");
                return Err(if timed_out {
                    FetchError::Timeout(message)
                } else {
                    FetchError::Network(message)
                });
            }
        };
        let headers = response
            .headers_names()
            .into_iter()
            .filter_map(|name| response.header(&name).map(|v| (name.clone(), v.to_string())))
            .collect();
        Ok(HttpResponse {
            status: response.status(),
            status_text: response.status_text().to_string(),
            headers,
            body: Box::new(response.into_reader()),
        })
    }
}

//! Sending one webhook request (design SHAA-2725 §5.3, §5.4): to the vetted
//! addresses only, through the configured proxy, over rustls with the public,
//! OS and corporate roots, without following redirects, within the
//! endpoint's timeout, reading at most 64 KiB of the answer.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::Webhooks;
use super::ssrf::{self, Refusal};
use crate::config::WebhookProxy;

/// Connecting (TCP, proxy, TLS) may take this long; the whole attempt the endpoint's `timeoutMs`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The most of a response body read before it is dropped.
const MAX_BODY_READ: usize = 64 * 1024;
/// The most of a response body kept to explain a failure.
const MAX_BODY_KEPT: usize = 1024;

/// What one attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// The receiver answered.
    Answered {
        status: u16,
        /// `Location` of a 3xx; `Retry-After` of a 429/503, in seconds.
        location: Option<String>,
        retry_after: Option<Duration>,
        /// The start of the body (lossy UTF-8), for a failure's diagnosis.
        excerpt: String,
    },
    /// Nothing was sent: the destination is refused (permanent).
    Refused { reason: String, message: String },
    /// No answer: DNS, connection, TLS or timeout (transient).
    Failed { message: String },
}

/// A webhook request: the URL as checked, the body and the headers to send.
pub struct Request<'a> {
    pub url: &'a url::Url,
    pub body: Vec<u8>,
    pub headers: HeaderMap,
    pub timeout: Duration,
}

fn describe(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

/// `Retry-After`: seconds or an HTTP date.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let at = chrono::DateTime::parse_from_rfc2822(v).ok()?;
    let wait = at.with_timezone(&chrono::Utc) - chrono::Utc::now();
    Some(wait.to_std().unwrap_or(Duration::ZERO))
}

impl Webhooks {
    /// The proxy for `url`, if a request to it goes through one.
    fn proxy_for(&self, url: &url::Url) -> Result<Option<reqwest::Proxy>, String> {
        let (proxy_url, no_proxy) = match &self.cfg.proxy {
            WebhookProxy::Direct => return Ok(None),
            WebhookProxy::Explicit(u) => (u, None),
            WebhookProxy::FromEnv { url, no_proxy } => (url, no_proxy.as_deref()),
        };
        if let (Some(list), Some(host)) = (no_proxy, url.host_str()) {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let exempt = list.split(',').map(str::trim).filter(|e| !e.is_empty()).any(|e| {
                let e = e.trim_start_matches('.').to_ascii_lowercase();
                e == "*" || host == e || host.ends_with(&format!(".{e}"))
            });
            if exempt {
                return Ok(None);
            }
        }
        let mut bare = proxy_url.clone();
        let _ = bare.set_username("");
        let _ = bare.set_password(None);
        let mut proxy = reqwest::Proxy::all(bare.as_str()).map_err(|e| format!("proxy {bare}: {e}"))?;
        let user = percent_encoding::percent_decode_str(proxy_url.username()).decode_utf8_lossy().into_owned();
        let password = match &self.proxy_password {
            Some(p) => Some(p.clone()),
            None => {
                proxy_url.password().map(|p| percent_encoding::percent_decode_str(p).decode_utf8_lossy().into_owned())
            }
        };
        if !user.is_empty() {
            proxy = proxy.basic_auth(&user, password.as_deref().unwrap_or_default());
        }
        Ok(Some(proxy))
    }

    /// Sends `req` once: resolves and vets the host's addresses (§5.3), then
    /// posts to those addresses only. Never follows a redirect.
    pub async fn send(&self, req: Request<'_>) -> Attempt {
        let Some(tls) = self.tls.clone() else {
            return Attempt::Refused {
                reason: "webhooks_disabled".into(),
                message: "Webhooks are switched off on this server (WEBHOOKS_ALLOWED=false)".into(),
            };
        };
        let addrs = match ssrf::vet(req.url, &self.resolver, &self.cfg.allow_private).await {
            Ok(a) => a,
            Err(Refusal::Unresolved(m)) => return Attempt::Failed { message: format!("DNS: {m}") },
            Err(Refusal::Blocked(b)) => {
                return Attempt::Refused {
                    reason: format!("address_blocked:{}", b.address),
                    message: format!(
                        "{} resolves to {} ({}), which webhooks may not reach",
                        req.url.host_str().unwrap_or_default(),
                        b.address,
                        b.why
                    ),
                };
            }
        };
        let proxy = match self.proxy_for(req.url) {
            Ok(p) => p,
            Err(m) => return Attempt::Failed { message: m },
        };
        let mut builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls.as_ref().clone())
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(req.timeout)
            .pool_max_idle_per_host(0)
            .user_agent(concat!("ShadouCMDB/", env!("CARGO_PKG_VERSION")))
            .no_proxy();
        match (proxy, req.url.host()) {
            // The proxy resolves the name itself; it was vetted above all the same.
            (Some(p), _) => builder = builder.proxy(p),
            (None, Some(url::Host::Domain(d))) => builder = builder.resolve_to_addrs(d, &addrs),
            (None, _) => {}
        }
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => return Attempt::Failed { message: format!("HTTP client: {}", describe(&e)) },
        };
        let res = client.post(req.url.clone()).headers(req.headers).body(req.body).send().await;
        let mut res = match res {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Attempt::Failed { message: format!("No answer within {} ms", req.timeout.as_millis()) };
            }
            Err(e) => return Attempt::Failed { message: describe(&e) },
        };
        let status = res.status().as_u16();
        let location = res.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_owned);
        let retry_after = retry_after(res.headers());
        let mut kept = Vec::new();
        let mut read = 0usize;
        // Read at most 64 KiB, keep 1 KiB; the rest of the body is dropped with the connection.
        while read < MAX_BODY_READ {
            match res.chunk().await {
                Ok(Some(chunk)) => {
                    read += chunk.len();
                    let room = MAX_BODY_KEPT.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..chunk.len().min(room)]);
                }
                _ => break,
            }
        }
        Attempt::Answered { status, location, retry_after, excerpt: String::from_utf8_lossy(&kept).into_owned() }
    }
}

/// Header names the server sets itself; an endpoint's auth header may not be one of them.
pub const RESERVED_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "host",
    "user-agent",
    "idempotency-key",
    "transfer-encoding",
    "connection",
    "x-shadoucmdb-signature",
    "x-shadoucmdb-event",
    "x-shadoucmdb-delivery",
    "x-shadoucmdb-webhook-id",
];

/// The headers of a webhook request.
pub fn headers(
    event: &str,
    delivery: &str,
    endpoint_key: &str,
    signature: &str,
    auth: Option<(&str, &[u8])>,
) -> Result<HeaderMap, String> {
    let mut h = HeaderMap::new();
    let value = |v: &str| HeaderValue::from_str(v).map_err(|_| format!("invalid header value {v:?}"));
    h.insert(reqwest::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    h.insert(HeaderName::from_static("x-shadoucmdb-event"), value(event)?);
    h.insert(HeaderName::from_static("x-shadoucmdb-delivery"), value(delivery)?);
    h.insert(HeaderName::from_static("idempotency-key"), value(delivery)?);
    h.insert(HeaderName::from_static("x-shadoucmdb-webhook-id"), value(endpoint_key)?);
    h.insert(HeaderName::from_static("x-shadoucmdb-signature"), value(signature)?);
    if let Some((name, v)) = auth {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "invalid auth header name".to_owned())?;
        let mut v =
            HeaderValue::from_bytes(v).map_err(|_| "the auth header value is not a valid header value".to_owned())?;
        v.set_sensitive(true);
        h.insert(name, v);
    }
    Ok(h)
}

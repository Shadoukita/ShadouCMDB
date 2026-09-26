//! Session tokens and cookies.
//!
//! Login creates a row in `sessions` and sets two cookies:
//! - `shadoucmdb_session`: 32 random bytes (hex), HttpOnly, SameSite=Lax. The
//!   database stores only its SHA-256, so a leaked table does not leak sessions.
//! - `shadoucmdb_csrf`: the session's CSRF token, readable by the web UI, which
//!   echoes it in `X-CSRF-Token` on every state-changing request. A cross-site
//!   page can neither read the cookie nor set the header without a CORS grant.
//!
//! Both are `Secure` when the request reached the proxy over HTTPS (see
//! [`CookieSecure`]).

use std::net::{IpAddr, SocketAddr};

use axum::http::{HeaderMap, HeaderValue, header};
use password_hash::rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::config::{AuthConfig, CookieSecure};

pub const SESSION_COOKIE: &str = "shadoucmdb_session";
pub const CSRF_COOKIE: &str = "shadoucmdb_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// 256 random bits, hex-encoded.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Equality that takes the same time wherever the first difference is.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Value of a cookie in the request (the first one if repeated).
pub fn cookie<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim_matches('"'))
        .filter(|v| !v.is_empty())
}

/// Whether the client talked HTTPS to us or to the reverse proxy in front of us.
pub fn request_is_https(headers: &HeaderMap) -> bool {
    let forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"));
    let forwarded = headers
        .get(header::FORWARDED)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .is_some_and(|first| {
            first.split(';').any(|kv| {
                kv.trim().split_once('=').is_some_and(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("proto") && v.trim().trim_matches('"').eq_ignore_ascii_case("https")
                })
            })
        });
    forwarded_proto || forwarded
}

/// The client's IP address, for the audit trail: the first hop of
/// `X-Forwarded-For`, else the first `Forwarded: for=`, else the TCP peer.
///
/// Trusted-proxy assumption, as for [`request_is_https`]: the reverse proxy in
/// front of the API overwrites (not appends to) these headers. Without such a
/// proxy a client can put any address there, so the value is evidence for an
/// investigator, never an input to an access decision.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
    let x_forwarded_for = || {
        let first = headers.get("x-forwarded-for")?.to_str().ok()?.split(',').next()?;
        parse_node(first)
    };
    let forwarded = || {
        let first = headers.get(header::FORWARDED)?.to_str().ok()?.split(',').next()?;
        first.split(';').find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            if k.trim().eq_ignore_ascii_case("for") { parse_node(v) } else { None }
        })
    };
    x_forwarded_for().or_else(forwarded).or(peer)
}

/// `1.2.3.4`, `1.2.3.4:5678`, `2001:db8::1`, `"[2001:db8::1]:4711"`; None for
/// `unknown`, obfuscated identifiers and garbage.
fn parse_node(s: &str) -> Option<IpAddr> {
    let s = s.trim().trim_matches('"');
    if let Ok(ip) = s.parse() {
        return Some(ip);
    }
    if let Some(rest) = s.strip_prefix('[') {
        return rest.split_once(']')?.0.parse().ok();
    }
    s.parse::<SocketAddr>().ok().map(|a| a.ip())
}

/// The User-Agent header, truncated.
pub fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(|s| s.chars().take(400).collect())
}

/// Whether cookies set on this response get `Secure`. Issuing a *new* session
/// cookie must go through `AuthState::session_cookie_secure`, which warns under
/// `auto`; this raw form is for clearing cookies.
pub(crate) fn secure_cookies(cfg: &AuthConfig, headers: &HeaderMap) -> bool {
    match cfg.cookie_secure {
        CookieSecure::Always => true,
        CookieSecure::Never => false,
        CookieSecure::Auto => request_is_https(headers),
    }
}

fn build(name: &str, value: &str, max_age_secs: u64, http_only: bool, secure: bool) -> HeaderValue {
    let mut c = format!("{name}={value}; Path=/; Max-Age={max_age_secs}; SameSite=Lax");
    if http_only {
        c.push_str("; HttpOnly");
    }
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).expect("cookie values are hex")
}

/// Set-Cookie headers for a new session.
pub fn login_cookies(cfg: &AuthConfig, secure: bool, token: &str, csrf: &str) -> Vec<HeaderValue> {
    let max_age = cfg.session_max_age.as_secs();
    vec![build(SESSION_COOKIE, token, max_age, true, secure), build(CSRF_COOKIE, csrf, max_age, false, secure)]
}

/// Set-Cookie headers that delete both cookies.
pub fn logout_cookies(secure: bool) -> Vec<HeaderValue> {
    vec![build(SESSION_COOKIE, "", 0, true, secure), build(CSRF_COOKIE, "", 0, false, secure)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn tokens_are_random_hex_and_hash_to_32_bytes() {
        let a = new_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, new_token());
        assert_eq!(token_hash(&a).len(), 32);
    }

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn reads_cookies_from_one_or_more_headers() {
        let h = headers(&[("cookie", "a=1; shadoucmdb_session=tok"), ("cookie", "shadoucmdb_csrf=\"c\"")]);
        assert_eq!(cookie(&h, SESSION_COOKIE), Some("tok"));
        assert_eq!(cookie(&h, CSRF_COOKIE), Some("c"));
        assert_eq!(cookie(&h, "missing"), None);
        assert_eq!(cookie(&headers(&[("cookie", "shadoucmdb_session=")]), SESSION_COOKIE), None);
    }

    #[test]
    fn https_detection_through_proxies() {
        assert!(request_is_https(&headers(&[("x-forwarded-proto", "https")])));
        assert!(request_is_https(&headers(&[("x-forwarded-proto", "HTTPS, http")])));
        assert!(request_is_https(&headers(&[("forwarded", "for=1.2.3.4;proto=https;by=x")])));
        assert!(!request_is_https(&headers(&[("x-forwarded-proto", "http")])));
        assert!(!request_is_https(&HeaderMap::new()));
    }

    #[test]
    fn client_ip_prefers_forwarded_headers_then_the_peer() {
        let peer: Option<IpAddr> = Some("10.0.0.9".parse().unwrap());
        let ip = |pairs: &[(&'static str, &'static str)]| client_ip(&headers(pairs), peer).map(|a| a.to_string());
        assert_eq!(ip(&[("x-forwarded-for", "203.0.113.7, 10.0.0.1")]).as_deref(), Some("203.0.113.7"));
        assert_eq!(ip(&[("x-forwarded-for", "203.0.113.7:5123")]).as_deref(), Some("203.0.113.7"));
        assert_eq!(ip(&[("x-forwarded-for", "2001:db8::1")]).as_deref(), Some("2001:db8::1"));
        assert_eq!(
            ip(&[("forwarded", "For=\"[2001:db8::2]:4711\";proto=https, for=1.1.1.1")]).as_deref(),
            Some("2001:db8::2")
        );
        assert_eq!(ip(&[("forwarded", "proto=https;for=198.51.100.4")]).as_deref(), Some("198.51.100.4"));
        // X-Forwarded-For wins over Forwarded; unusable values fall through.
        assert_eq!(
            ip(&[("x-forwarded-for", "198.51.100.1"), ("forwarded", "for=198.51.100.2")]).as_deref(),
            Some("198.51.100.1")
        );
        assert_eq!(ip(&[("x-forwarded-for", "garbage"), ("forwarded", "for=unknown")]).as_deref(), Some("10.0.0.9"));
        assert_eq!(ip(&[]).as_deref(), Some("10.0.0.9"));
        assert_eq!(client_ip(&HeaderMap::new(), None), None);
    }

    #[test]
    fn cookie_attributes() {
        let cfg = AuthConfig {
            session_idle: Duration::from_secs(60),
            session_max_age: Duration::from_secs(3600),
            cookie_secure: CookieSecure::Auto,
        };
        let c = login_cookies(&cfg, true, "tok", "csrf");
        assert_eq!(c[0], "shadoucmdb_session=tok; Path=/; Max-Age=3600; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c[1], "shadoucmdb_csrf=csrf; Path=/; Max-Age=3600; SameSite=Lax; Secure");
        let c = logout_cookies(false);
        assert_eq!(c[0], "shadoucmdb_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly");
        assert!(secure_cookies(&AuthConfig { cookie_secure: CookieSecure::Always, ..cfg.clone() }, &HeaderMap::new()));
        assert!(!secure_cookies(&cfg, &HeaderMap::new()));
    }
}

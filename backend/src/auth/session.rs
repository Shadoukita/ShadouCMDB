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
//! [`CookieSecure`]), and then carry the `__Host-` prefix
//! (`__Host-shadoucmdb_session`, `__Host-shadoucmdb_csrf`): the browser keeps
//! such a cookie only if it is `Secure`, has `Path=/` and no `Domain`, so a
//! sibling subdomain cannot plant one for this host ("cookie tossing"). A
//! request carrying both names is always read by its `__Host-` cookie. Plain
//! HTTP setups (`COOKIE_SECURE=never`, or `auto` without HTTPS) keep the plain
//! names, which a browser would not accept with the prefix.
//!
//! Sessions opened before the prefix are still read by their plain name as
//! long as no `__Host-` cookie is present, and the first HTTPS answer moves
//! them over ([`upgrade_cookies`]).
//!
//! A sign-in whose password was right but whose second factor is due sets
//! `shadoucmdb_mfa` instead: a random token naming the pending challenge,
//! HttpOnly, sent only to `/api/v1/auth`, gone after a few minutes.
//!
//! An OIDC sign-in sets `shadoucmdb_oidc` (HttpOnly, sent only to
//! `/api/v1/auth/oidc`, 10 minutes): the pending sign-in, including the
//! `state` it sends the provider, sealed with a server key
//! ([`crate::auth::sso::login_state`]). Nothing is stored in the database.

use std::net::{IpAddr, SocketAddr};

use axum::http::{HeaderMap, HeaderValue, header};
use sha2::{Digest, Sha256};

use crate::config::{AuthConfig, CookieSecure};

pub const SESSION_COOKIE: &str = "shadoucmdb_session";
pub const CSRF_COOKIE: &str = "shadoucmdb_csrf";
/// [`SESSION_COOKIE`] under `Secure`.
pub const HOST_SESSION_COOKIE: &str = "__Host-shadoucmdb_session";
/// [`CSRF_COOKIE`] under `Secure`.
pub const HOST_CSRF_COOKIE: &str = "__Host-shadoucmdb_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";
pub const MFA_COOKIE: &str = "shadoucmdb_mfa";
const MFA_COOKIE_PATH: &str = "/api/v1/auth";
/// The sealed pending OIDC sign-in; its `state` is checked against the
/// callback's `state` parameter so a callback only completes in the browser
/// that started it.
pub const OIDC_COOKIE: &str = "shadoucmdb_oidc";
const OIDC_COOKIE_PATH: &str = "/api/v1/auth/oidc";

/// 256 random bits, hex-encoded.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator");
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
    raw_cookie(headers, name).filter(|v| !v.is_empty())
}

/// Like [`cookie`], but an empty value is `Some("")`: the cookie is present.
fn raw_cookie<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim_matches('"'))
}

/// The session cookie's name on a response with or without `Secure`.
pub fn session_cookie_name(secure: bool) -> &'static str {
    if secure { HOST_SESSION_COOKIE } else { SESSION_COOKIE }
}

/// The CSRF cookie's name on a response with or without `Secure`.
pub fn csrf_cookie_name(secure: bool) -> &'static str {
    if secure { HOST_CSRF_COOKIE } else { CSRF_COOKIE }
}

/// The session token the request carries, and whether it came under the
/// plain name. A `__Host-` cookie, when present, is the only one read (even
/// if empty, and wherever it stands among the cookies): a plain-named cookie
/// may have been planted by a sibling subdomain.
pub fn session_token(headers: &HeaderMap) -> Option<(&str, bool)> {
    match raw_cookie(headers, HOST_SESSION_COOKIE) {
        Some(v) => Some((v, false)).filter(|(v, _)| !v.is_empty()),
        None => cookie(headers, SESSION_COOKIE).map(|v| (v, true)),
    }
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
/// proxy a client can put any address there (as it can behind a proxy that
/// appends), so the value is evidence for an investigator, never an input to an
/// access decision. The audit trail therefore also keeps the TCP peer when it
/// differs (`ClientInfo::peer_ip`).
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
    build_at("/", name, value, max_age_secs, http_only, secure)
}

fn build_at(path: &str, name: &str, value: &str, max_age_secs: u64, http_only: bool, secure: bool) -> HeaderValue {
    let mut c = format!("{name}={value}; Path={path}; Max-Age={max_age_secs}; SameSite=Lax");
    if http_only {
        c.push_str("; HttpOnly");
    }
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).expect("cookie values are hex")
}

/// Set-Cookie headers for a new session: the session cookie, then the CSRF
/// cookie. Under `Secure` they carry the `__Host-` prefix and are followed by
/// headers deleting the plain-named ones a browser may still hold.
pub fn login_cookies(cfg: &AuthConfig, secure: bool, token: &str, csrf: &str) -> Vec<HeaderValue> {
    let max_age = cfg.session_max_age.as_secs();
    let mut cookies = vec![
        build(session_cookie_name(secure), token, max_age, true, secure),
        build(csrf_cookie_name(secure), csrf, max_age, false, secure),
    ];
    if secure {
        cookies.extend(clear_plain_cookies());
    }
    cookies
}

/// Set-Cookie headers moving a session that authenticated by the plain cookie
/// name to the `__Host-` names, on a response that gets `Secure`; None when
/// there is nothing to move. The new cookies get the full session lifetime:
/// the server's own expiry of the session still applies.
///
/// TODO(GH-192): remove, with the plain-name fallback in [`session_token`],
/// one release after the one that introduced `__Host-` cookies.
pub fn upgrade_cookies(cfg: &AuthConfig, headers: &HeaderMap, csrf: &str) -> Option<Vec<HeaderValue>> {
    if !secure_cookies(cfg, headers) {
        return None;
    }
    let (token, true) = session_token(headers)? else { return None };
    Some(login_cookies(cfg, true, token, csrf))
}

/// Deletes the plain-named session and CSRF cookies (same Path, so the
/// browser matches them).
fn clear_plain_cookies() -> [HeaderValue; 2] {
    [build(SESSION_COOKIE, "", 0, true, true), build(CSRF_COOKIE, "", 0, false, true)]
}

/// Set-Cookie header naming a pending second-factor challenge.
pub fn mfa_cookie(secure: bool, token: &str, ttl: std::time::Duration) -> HeaderValue {
    build_at(MFA_COOKIE_PATH, MFA_COOKIE, token, ttl.as_secs(), true, secure)
}

/// Set-Cookie header that deletes the challenge cookie.
pub fn clear_mfa_cookie(secure: bool) -> HeaderValue {
    build_at(MFA_COOKIE_PATH, MFA_COOKIE, "", 0, true, secure)
}

/// Set-Cookie header binding a pending OIDC sign-in to this browser. SameSite=Lax
/// still sends it on the provider's top-level redirect back to the callback.
pub fn oidc_cookie(secure: bool, sealed: &str, ttl: std::time::Duration) -> HeaderValue {
    build_at(OIDC_COOKIE_PATH, OIDC_COOKIE, sealed, ttl.as_secs(), true, secure)
}

pub fn clear_oidc_cookie(secure: bool) -> HeaderValue {
    build_at(OIDC_COOKIE_PATH, OIDC_COOKIE, "", 0, true, secure)
}

/// The value a Set-Cookie header built here sets for `name`.
pub fn cookie_value(set_cookie: &HeaderValue, name: &str) -> Option<String> {
    let text = set_cookie.to_str().ok()?;
    let (k, v) = text.split(';').next()?.split_once('=')?;
    (k == name).then(|| v.to_owned())
}

/// Set-Cookie headers that delete both cookies (under `Secure`, both names of each).
pub fn logout_cookies(secure: bool) -> Vec<HeaderValue> {
    let mut cookies = vec![
        build(session_cookie_name(secure), "", 0, true, secure),
        build(csrf_cookie_name(secure), "", 0, false, secure),
    ];
    if secure {
        cookies.extend(clear_plain_cookies());
    }
    cookies
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
    fn the_host_session_cookie_wins_in_either_order() {
        let host_first = headers(&[("cookie", "__Host-shadoucmdb_session=good; shadoucmdb_session=tossed")]);
        let plain_first = headers(&[("cookie", "shadoucmdb_session=tossed; __Host-shadoucmdb_session=good")]);
        let split = headers(&[("cookie", "shadoucmdb_session=tossed"), ("cookie", "__Host-shadoucmdb_session=good")]);
        for h in [&host_first, &plain_first, &split] {
            assert_eq!(session_token(h), Some(("good", false)));
        }
        // An empty __Host- cookie still shuts out the plain name.
        let empty = headers(&[("cookie", "shadoucmdb_session=tossed; __Host-shadoucmdb_session=")]);
        assert_eq!(session_token(&empty), None);
        // Without one, the plain name is read (sessions from before the prefix).
        assert_eq!(session_token(&headers(&[("cookie", "shadoucmdb_session=old")])), Some(("old", true)));
        assert_eq!(session_token(&HeaderMap::new()), None);
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
            public_url: None,
            oidc_allowed_hosts: None,
            setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
            setup_token_file: None,
        };
        let c = login_cookies(&cfg, true, "tok", "csrf");
        assert_eq!(c[0], "__Host-shadoucmdb_session=tok; Path=/; Max-Age=3600; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c[1], "__Host-shadoucmdb_csrf=csrf; Path=/; Max-Age=3600; SameSite=Lax; Secure");
        assert_eq!(c[2], "shadoucmdb_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c[3], "shadoucmdb_csrf=; Path=/; Max-Age=0; SameSite=Lax; Secure");
        assert_eq!(c.len(), 4);
        assert!(c.iter().all(|c| !c.to_str().unwrap().contains("Domain")), "__Host- cookies must not carry Domain");
        let c = login_cookies(&cfg, false, "tok", "csrf");
        assert_eq!(
            c,
            [
                "shadoucmdb_session=tok; Path=/; Max-Age=3600; SameSite=Lax; HttpOnly",
                "shadoucmdb_csrf=csrf; Path=/; Max-Age=3600; SameSite=Lax"
            ]
        );
        assert_eq!(
            mfa_cookie(false, "tok", Duration::from_secs(300)),
            "shadoucmdb_mfa=tok; Path=/api/v1/auth; Max-Age=300; SameSite=Lax; HttpOnly"
        );
        let c = logout_cookies(false);
        assert_eq!(c[0], "shadoucmdb_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly");
        assert_eq!(c.len(), 2);
        let c = logout_cookies(true);
        assert_eq!(c[0], "__Host-shadoucmdb_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c[2], "shadoucmdb_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c.len(), 4);
        assert!(secure_cookies(&AuthConfig { cookie_secure: CookieSecure::Always, ..cfg.clone() }, &HeaderMap::new()));
        assert!(!secure_cookies(&cfg, &HeaderMap::new()));
    }

    #[test]
    fn plain_cookie_sessions_move_to_host_cookies_over_https() {
        let cfg = AuthConfig {
            session_idle: Duration::from_secs(60),
            session_max_age: Duration::from_secs(3600),
            cookie_secure: CookieSecure::Auto,
            public_url: None,
            oidc_allowed_hosts: None,
            setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
            setup_token_file: None,
        };
        let plain = headers(&[("cookie", "shadoucmdb_session=old; shadoucmdb_csrf=c"), ("x-forwarded-proto", "https")]);
        let c = upgrade_cookies(&cfg, &plain, "c").unwrap();
        assert_eq!(c[0], "__Host-shadoucmdb_session=old; Path=/; Max-Age=3600; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c[1], "__Host-shadoucmdb_csrf=c; Path=/; Max-Age=3600; SameSite=Lax; Secure");
        assert_eq!(c[2], "shadoucmdb_session=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(c[3], "shadoucmdb_csrf=; Path=/; Max-Age=0; SameSite=Lax; Secure");
        // Already moved, or plain HTTP (the plain name is the current one): nothing to do.
        let host = headers(&[("cookie", "__Host-shadoucmdb_session=new"), ("x-forwarded-proto", "https")]);
        assert_eq!(upgrade_cookies(&cfg, &host, "c"), None);
        assert_eq!(upgrade_cookies(&cfg, &headers(&[("cookie", "shadoucmdb_session=old")]), "c"), None);
    }
}

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
//! sibling subdomain cannot plant one for this host ("cookie tossing"). Plain
//! HTTP setups (`COOKIE_SECURE=never`, or `auto` without HTTPS) keep the plain
//! names, which a browser would not accept with the prefix.
//!
//! A request is read by the names its answer would set: where cookies get
//! `Secure`, only the `__Host-` names, so a plain-named cookie a sibling
//! subdomain planted is never read (GH#285). Sessions a pre-release opened
//! under the plain names over HTTPS are not carried over; their users sign in
//! again.
//!
//! A sign-in whose password was right but whose second factor is due sets
//! `shadoucmdb_mfa` instead: a random token naming the pending challenge,
//! HttpOnly, gone after a few minutes.
//!
//! An OIDC sign-in sets `shadoucmdb_oidc` (HttpOnly, 10 minutes): the pending
//! sign-in, including the `state` it sends the provider, sealed with a server
//! key ([`crate::auth::sso::login_state`]). Nothing is stored in the database.
//!
//! Under `Secure` these two carry the `__Host-` prefix as well and are sent
//! to every path (the prefix requires `Path=/`); otherwise they are sent only
//! to `/api/v1/auth` and `/api/v1/auth/oidc`.

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
/// [`MFA_COOKIE`] under `Secure`.
pub const HOST_MFA_COOKIE: &str = "__Host-shadoucmdb_mfa";
const MFA_COOKIE_PATH: &str = "/api/v1/auth";
/// The sealed pending OIDC sign-in; its `state` is checked against the
/// callback's `state` parameter so a callback only completes in the browser
/// that started it.
pub const OIDC_COOKIE: &str = "shadoucmdb_oidc";
/// [`OIDC_COOKIE`] under `Secure`.
pub const HOST_OIDC_COOKIE: &str = "__Host-shadoucmdb_oidc";
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

/// Value of a cookie in the request (the first one if repeated); None if empty.
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

/// The session cookie's name on a response with or without `Secure`.
pub fn session_cookie_name(secure: bool) -> &'static str {
    if secure { HOST_SESSION_COOKIE } else { SESSION_COOKIE }
}

/// The CSRF cookie's name on a response with or without `Secure`.
pub fn csrf_cookie_name(secure: bool) -> &'static str {
    if secure { HOST_CSRF_COOKIE } else { CSRF_COOKIE }
}

/// The session token the request carries, under the name this answer would
/// set it: only `__Host-shadoucmdb_session` where cookies get `Secure`, since a
/// plain-named cookie there may have been planted by a sibling subdomain
/// (GH#285).
pub fn session_token<'h>(cfg: &AuthConfig, headers: &'h HeaderMap) -> Option<&'h str> {
    cookie(headers, session_cookie_name(secure_cookies(cfg, headers)))
}

/// The pending second-factor challenge the request names, read like [`session_token`].
pub fn second_factor_challenge<'h>(cfg: &AuthConfig, headers: &'h HeaderMap) -> Option<&'h str> {
    cookie(headers, if secure_cookies(cfg, headers) { HOST_MFA_COOKIE } else { MFA_COOKIE })
}

/// The sealed pending OIDC sign-in the request carries, read like [`session_token`].
pub fn oidc_state<'h>(cfg: &AuthConfig, headers: &'h HeaderMap) -> Option<&'h str> {
    cookie(headers, if secure_cookies(cfg, headers) { HOST_OIDC_COOKIE } else { OIDC_COOKIE })
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

/// The client's IP address as the request claims it, for the audit trail: the
/// first hop of `X-Forwarded-For`, else the first `Forwarded: for=`, else the
/// TCP peer.
///
/// Any client can put any address in these headers (directly, or through a
/// proxy that appends to them), so the value is evidence for an investigator,
/// never an input to an access decision. The audit trail records it only as
/// `claimedIpAddress`, next to the [`throttle_ip`] address (GH#282).
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
    let x_forwarded_for = || forwarded_for(headers)?.into_iter().next().flatten();
    let forwarded = || forwarded_nodes(headers)?.into_iter().next().flatten();
    x_forwarded_for().or_else(forwarded).or(peer)
}

/// The client address the sign-in throttle keys on (GH#215): the TCP peer,
/// unless the peer is one of the operator's [`TrustedProxies`]. Then the
/// forwarding header (`X-Forwarded-For`, else `Forwarded: for=`) is read from
/// right to left, skipping the trusted hops, and the first untrusted hop is
/// the client: every hop left of it was written by someone we do not trust.
/// An unusable hop ends the walk at the trusted hop that reported it.
///
/// The header is chosen by presence, not by whether it parses: with any
/// `X-Forwarded-For` present, `Forwarded` is never read. Otherwise a client
/// behind a proxy that appends to `X-Forwarded-For` could spoil that header
/// and have its own `Forwarded: for=` believed instead.
pub fn throttle_ip(headers: &HeaderMap, peer: Option<IpAddr>, trusted: &TrustedProxies) -> Option<IpAddr> {
    let mut client = peer?;
    if !trusted.contains(client) {
        return Some(client);
    }
    let hops = match forwarded_for(headers) {
        Some(hops) => hops,
        None => match forwarded_nodes(headers) {
            Some(hops) => hops,
            None => return Some(client),
        },
    };
    for hop in hops.into_iter().rev() {
        let Some(hop) = hop else { break };
        client = hop;
        if !trusted.contains(hop) {
            break;
        }
    }
    Some(client)
}

/// The hops of the `X-Forwarded-For` headers, left to right (None for an unusable one).
fn forwarded_for(headers: &HeaderMap) -> Option<Vec<Option<IpAddr>>> {
    let values = header_list(headers, "x-forwarded-for")?;
    Some(values.into_iter().map(|hop| hop.and_then(parse_node)).collect())
}

/// The `for=` of each `Forwarded` element, left to right (None for an unusable or missing one).
fn forwarded_nodes(headers: &HeaderMap) -> Option<Vec<Option<IpAddr>>> {
    let elements = header_list(headers, header::FORWARDED.as_str())?;
    Some(
        elements
            .into_iter()
            .map(|element| {
                element?.split(';').find_map(|kv| {
                    let (k, v) = kv.trim().split_once('=')?;
                    if k.trim().eq_ignore_ascii_case("for") { parse_node(v) } else { None }
                })
            })
            .collect(),
    )
}

/// The comma-separated entries of every `name` header, in order; None without
/// one. Each entry is decoded on its own, so bytes that are not UTF-8 spoil
/// only their entry (a None entry), not the hops a proxy appended after it.
fn header_list<'h>(headers: &'h HeaderMap, name: &str) -> Option<Vec<Option<&'h str>>> {
    let mut values = headers.get_all(name).iter().peekable();
    values.peek()?;
    let mut entries = Vec::new();
    for v in values {
        entries.extend(v.as_bytes().split(|b| *b == b',').map(|entry| std::str::from_utf8(entry).ok()));
    }
    Some(entries)
}

/// `TRUSTED_PROXIES`: the reverse proxies whose forwarding headers the sign-in
/// throttle believes. Empty (the default): none, so the throttle keys on the
/// TCP peer only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<ipnetwork::IpNetwork>);

impl TrustedProxies {
    /// A comma-separated list of addresses and CIDR ranges.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut nets = Vec::new();
        for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let net: ipnetwork::IpNetwork = entry.parse().map_err(|_| {
                format!("\"{entry}\" is not an IP address or CIDR range, e.g. 10.0.0.5 or 10.0.0.0/24 or fd00::/64")
            })?;
            // An IPv4-mapped range (`::ffff:10.0.0.0/104`) as the IPv4 range it
            // stands for: `contains` compares canonical addresses.
            let net = match net {
                ipnetwork::IpNetwork::V6(v6) if v6.prefix() >= 96 => match v6.ip().to_ipv4_mapped() {
                    Some(v4) => ipnetwork::IpNetwork::V4(
                        ipnetwork::Ipv4Network::new(v4, v6.prefix() - 96).expect("a prefix of at most 32"),
                    ),
                    None => net,
                },
                _ => net,
            };
            if net.prefix() == 0 {
                return Err(format!(
                    "\"{entry}\" would trust every address, so any client could choose the network it is \
                     throttled as; list only the addresses of your reverse proxies"
                ));
            }
            nets.push(net);
        }
        Ok(TrustedProxies(nets))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.0.iter().any(|net| net.contains(ip))
    }
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

/// Deletes the plain-named session and CSRF cookies (same Path, so the
/// browser matches them).
fn clear_plain_cookies() -> [HeaderValue; 2] {
    [build(SESSION_COOKIE, "", 0, true, true), build(CSRF_COOKIE, "", 0, false, true)]
}

/// A sign-in flow cookie: under `Secure`, the `__Host-` name at `Path=/`;
/// otherwise the plain name, sent only to `path`.
fn flow_cookie(secure: bool, names: (&str, &str), path: &str, value: &str, max_age_secs: u64) -> HeaderValue {
    if secure {
        build(names.0, value, max_age_secs, true, true)
    } else {
        build_at(path, names.1, value, max_age_secs, true, false)
    }
}

/// Set-Cookie header naming a pending second-factor challenge.
pub fn mfa_cookie(secure: bool, token: &str, ttl: std::time::Duration) -> HeaderValue {
    flow_cookie(secure, (HOST_MFA_COOKIE, MFA_COOKIE), MFA_COOKIE_PATH, token, ttl.as_secs())
}

/// Set-Cookie header that deletes the challenge cookie.
pub fn clear_mfa_cookie(secure: bool) -> HeaderValue {
    flow_cookie(secure, (HOST_MFA_COOKIE, MFA_COOKIE), MFA_COOKIE_PATH, "", 0)
}

/// Set-Cookie header binding a pending OIDC sign-in to this browser. SameSite=Lax
/// still sends it on the provider's top-level redirect back to the callback.
pub fn oidc_cookie(secure: bool, sealed: &str, ttl: std::time::Duration) -> HeaderValue {
    flow_cookie(secure, (HOST_OIDC_COOKIE, OIDC_COOKIE), OIDC_COOKIE_PATH, sealed, ttl.as_secs())
}

pub fn clear_oidc_cookie(secure: bool) -> HeaderValue {
    flow_cookie(secure, (HOST_OIDC_COOKIE, OIDC_COOKIE), OIDC_COOKIE_PATH, "", 0)
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

    fn test_config(cookie_secure: CookieSecure) -> AuthConfig {
        AuthConfig {
            session_idle: Duration::from_secs(60),
            session_max_age: Duration::from_secs(3600),
            cookie_secure,
            public_url: None,
            oidc_allowed_hosts: None,
            setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
            setup_token_file: None,
            trusted_proxies: Default::default(),
            sign_in_failure_floor: Duration::ZERO,
        }
    }

    /// GH#285: over HTTPS only the `__Host-` names are read, so a plain-named
    /// cookie a sibling subdomain planted (`Domain=.example.com`) is ignored.
    #[test]
    fn over_https_only_host_cookies_are_read() {
        let cfg = test_config(CookieSecure::Auto);
        let https = |cookie: &'static str| headers(&[("cookie", cookie), ("x-forwarded-proto", "https")]);
        for h in [
            https("__Host-shadoucmdb_session=good; shadoucmdb_session=tossed"),
            https("shadoucmdb_session=tossed; __Host-shadoucmdb_session=good"),
            headers(&[
                ("cookie", "shadoucmdb_session=tossed"),
                ("cookie", "__Host-shadoucmdb_session=good"),
                ("x-forwarded-proto", "https"),
            ]),
        ] {
            assert_eq!(session_token(&cfg, &h), Some("good"));
        }
        assert_eq!(session_token(&cfg, &https("shadoucmdb_session=tossed; __Host-shadoucmdb_session=")), None);
        assert_eq!(session_token(&cfg, &https("shadoucmdb_session=tossed")), None);
        assert_eq!(second_factor_challenge(&cfg, &https("shadoucmdb_mfa=tossed")), None);
        assert_eq!(oidc_state(&cfg, &https("shadoucmdb_oidc=tossed")), None);
        assert_eq!(second_factor_challenge(&cfg, &https("shadoucmdb_mfa=x; __Host-shadoucmdb_mfa=m")), Some("m"));
        assert_eq!(oidc_state(&cfg, &https("__Host-shadoucmdb_oidc=o; shadoucmdb_oidc=x")), Some("o"));
        // COOKIE_SECURE=always reads the __Host- names whatever the request says.
        let always = test_config(CookieSecure::Always);
        assert_eq!(session_token(&always, &headers(&[("cookie", "shadoucmdb_session=tossed")])), None);
        // Plain HTTP answers set, and so read, the plain names only.
        let http = headers(&[("cookie", "__Host-shadoucmdb_session=h; shadoucmdb_session=p; shadoucmdb_mfa=m")]);
        assert_eq!(session_token(&cfg, &http), Some("p"));
        assert_eq!(second_factor_challenge(&cfg, &http), Some("m"));
        assert_eq!(session_token(&test_config(CookieSecure::Never), &https("__Host-shadoucmdb_session=h")), None);
        assert_eq!(session_token(&cfg, &HeaderMap::new()), None);
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

    fn trusted(raw: &str) -> TrustedProxies {
        TrustedProxies::parse(raw).unwrap()
    }

    /// GH#215: forwarding headers from an untrusted peer cannot choose the throttled network.
    #[test]
    fn throttle_ip_ignores_forwarding_headers_from_an_untrusted_peer() {
        let peer: Option<IpAddr> = Some("198.51.100.9".parse().unwrap());
        let forged = headers(&[("x-forwarded-for", "203.0.113.10"), ("forwarded", "for=203.0.113.11")]);
        for proxies in [TrustedProxies::default(), trusted("10.0.0.0/8")] {
            assert_eq!(throttle_ip(&forged, peer, &proxies), peer);
            assert_eq!(
                crate::auth::throttle::Net::of(throttle_ip(&forged, peer, &proxies)),
                crate::auth::throttle::Net::of(peer),
                "a forged X-Forwarded-For does not change the network"
            );
        }
        // The audit trail still records the claim, next to the peer.
        assert_eq!(client_ip(&forged, peer), Some("203.0.113.10".parse().unwrap()));
        assert_eq!(throttle_ip(&forged, None, &TrustedProxies::default()), None);
    }

    #[test]
    fn throttle_ip_takes_the_rightmost_untrusted_hop_behind_a_trusted_peer() {
        let proxies = trusted("10.0.0.0/8, 192.0.2.7, fd00::/64");
        let ip = |peer: &str, pairs: &[(&'static str, &'static str)]| {
            throttle_ip(&headers(pairs), Some(peer.parse().unwrap()), &proxies).map(|a| a.to_string())
        };
        // An appending proxy: the client's forged hop is left of the one the proxy added.
        assert_eq!(
            ip("10.0.0.2", &[("x-forwarded-for", "203.0.113.10, 198.51.100.4")]).as_deref(),
            Some("198.51.100.4")
        );
        // Trusted hops are skipped, across repeated headers too.
        assert_eq!(
            ip("10.0.0.2", &[("x-forwarded-for", "203.0.113.10, 198.51.100.4"), ("x-forwarded-for", "192.0.2.7")])
                .as_deref(),
            Some("198.51.100.4")
        );
        assert_eq!(ip("::ffff:10.0.0.2", &[("x-forwarded-for", "198.51.100.4:5123")]).as_deref(), Some("198.51.100.4"));
        assert_eq!(
            ip("fd00::1", &[("forwarded", "for=203.0.113.10, for=\"[2001:db8::5]:443\";proto=https")]).as_deref(),
            Some("2001:db8::5")
        );
        // All hops trusted: the leftmost one. None, or an unusable hop: the trusted hop that reported it.
        assert_eq!(ip("10.0.0.2", &[("x-forwarded-for", "10.1.1.1, 192.0.2.7")]).as_deref(), Some("10.1.1.1"));
        assert_eq!(ip("10.0.0.2", &[]).as_deref(), Some("10.0.0.2"));
        assert_eq!(
            ip("10.0.0.2", &[("x-forwarded-for", "203.0.113.10, unknown, 10.3.3.3")]).as_deref(),
            Some("10.3.3.3")
        );
    }

    /// An `X-Forwarded-For` spoilt with bytes that are not visible ASCII still
    /// counts, hop by hop: the client's own `Forwarded` is never read instead.
    #[test]
    fn throttle_ip_never_falls_back_to_forwarded_when_x_forwarded_for_is_present() {
        let proxies = trusted("10.0.0.0/8");
        let peer: Option<IpAddr> = Some("10.0.0.2".parse().unwrap());
        let spoilt = |xff: &'static [u8]| {
            let mut h = HeaderMap::new();
            h.append("x-forwarded-for", HeaderValue::from_bytes(xff).unwrap());
            h.append("forwarded", HeaderValue::from_static("for=203.0.113.99"));
            h
        };
        // What nginx's `$proxy_add_x_forwarded_for` makes of a client's `\xff`.
        let appended = throttle_ip(&spoilt(b"\xff, 198.51.100.4"), peer, &proxies);
        assert_eq!(appended, Some("198.51.100.4".parse().unwrap()));
        assert_eq!(throttle_ip(&spoilt(b"\xff"), peer, &proxies), peer, "the trusted peer, not the forged Forwarded");
        assert_eq!(throttle_ip(&spoilt(b"garbage"), peer, &proxies), peer);
    }

    #[test]
    fn trusted_proxies_parse_addresses_and_ranges() {
        let p = trusted(" 10.0.0.0/8 ,, 192.0.2.7,2001:db8::/48");
        assert!(p.contains("10.200.0.1".parse().unwrap()));
        assert!(p.contains("::ffff:192.0.2.7".parse().unwrap()), "mapped IPv4 is IPv4");
        assert!(!p.contains("192.0.2.8".parse().unwrap()));
        let mapped = trusted("::ffff:172.16.0.0/108");
        assert!(mapped.contains("172.16.3.4".parse().unwrap()), "a mapped range is the IPv4 range");
        assert!(mapped.contains("::ffff:172.16.3.4".parse().unwrap()));
        assert!(p.contains("2001:db8:0:ffff::1".parse().unwrap()));
        assert!(trusted("").is_empty());
        for bad in ["10.0.0.0/33", "proxy.example.com", "0.0.0.0/0", "::/0", "::ffff:0.0.0.0/96"] {
            assert!(TrustedProxies::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cookie_attributes() {
        let cfg = test_config(CookieSecure::Auto);
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
        // The flow cookies take the __Host- prefix, and so Path=/, under Secure.
        assert_eq!(
            mfa_cookie(true, "tok", Duration::from_secs(300)),
            "__Host-shadoucmdb_mfa=tok; Path=/; Max-Age=300; SameSite=Lax; HttpOnly; Secure"
        );
        assert_eq!(clear_mfa_cookie(true), "__Host-shadoucmdb_mfa=; Path=/; Max-Age=0; SameSite=Lax; HttpOnly; Secure");
        assert_eq!(
            oidc_cookie(true, "sealed", Duration::from_secs(600)),
            "__Host-shadoucmdb_oidc=sealed; Path=/; Max-Age=600; SameSite=Lax; HttpOnly; Secure"
        );
        assert_eq!(
            clear_oidc_cookie(false),
            "shadoucmdb_oidc=; Path=/api/v1/auth/oidc; Max-Age=0; SameSite=Lax; HttpOnly"
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
}

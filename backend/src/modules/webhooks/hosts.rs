//! Which hosts a webhook may reach (design SHAA-2725 §5.1, §5.2): host
//! patterns, the operator's ceiling (`WEBHOOK_ALLOWED_HOSTS`) and the URL
//! rules an endpoint's URL must meet on save and before every attempt.

use std::fmt;
use std::net::IpAddr;

/// Longest endpoint URL.
pub const MAX_URL: usize = 2048;

/// The host part of a pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HostMatch {
    /// One host name, lower-case and in IDNA (punycode) form.
    Exact(String),
    /// `*.suffix`: any name one label or more below `suffix`, never `suffix` itself.
    Below(String),
    /// An IP address, matched only literally.
    Ip(IpAddr),
}

/// An allowlist entry: a host and, optionally, the only port allowed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPattern {
    pub host: HostMatch,
    pub port: Option<u16>,
}

impl fmt::Display for HostMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostMatch::Exact(h) => f.write_str(h),
            HostMatch::Below(s) => write!(f, "*.{s}"),
            HostMatch::Ip(ip) => write!(f, "{ip}"),
        }
    }
}

impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.host, self.port) {
            (HostMatch::Ip(IpAddr::V6(ip)), Some(p)) => write!(f, "[{ip}]:{p}"),
            (h, Some(p)) => write!(f, "{h}:{p}"),
            (h, None) => write!(f, "{h}"),
        }
    }
}

/// A host name as `url` normalises it (lower case, IDNA), or `None`.
fn domain(raw: &str) -> Option<String> {
    let url = url::Url::parse(&format!("https://{raw}/")).ok()?;
    match url.host()? {
        url::Host::Domain(d) if !d.is_empty() && !d.ends_with('.') => Some(d.to_owned()),
        _ => None,
    }
}

impl HostMatch {
    /// `itsm.corp.example`, `*.corp.example`, `10.1.2.3`, `fd00::1` or `[fd00::1]`.
    pub fn parse(raw: &str) -> Result<HostMatch, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("is empty".into());
        }
        let bare = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(raw);
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return Ok(HostMatch::Ip(ip));
        }
        if let Some(suffix) = raw.strip_prefix("*.") {
            if suffix.contains('*') {
                return Err(format!("{raw:?}: only a leading `*.` is allowed"));
            }
            return match domain(suffix) {
                Some(d) if d.parse::<IpAddr>().is_err() && !d.contains(':') => Ok(HostMatch::Below(d)),
                _ => Err(format!("{raw:?}: `*.` must be followed by a domain name")),
            };
        }
        if raw.contains('*') {
            return Err(format!("{raw:?}: a wildcard is only allowed as a leading `*.` (never a bare `*`)"));
        }
        if raw.contains(['/', '@', '?', '#', ':', ' ']) {
            return Err(format!("{raw:?} is not a host name or IP address"));
        }
        match domain(raw) {
            // `url` reads `1.2.3` and other short forms as an IPv4 address; only a name is a name here.
            Some(d) if d.parse::<IpAddr>().is_err() => Ok(HostMatch::Exact(d)),
            _ => Err(format!("{raw:?} is not a host name or IP address")),
        }
    }

    pub fn matches(&self, host: &url::Host<&str>) -> bool {
        match (self, host) {
            (HostMatch::Exact(h), url::Host::Domain(d)) => h == d,
            (HostMatch::Below(s), url::Host::Domain(d)) => {
                d.len() > s.len() + 1 && d.ends_with(s.as_str()) && d.as_bytes()[d.len() - s.len() - 1] == b'.'
            }
            (HostMatch::Ip(ip), url::Host::Ipv4(v4)) => *ip == IpAddr::V4(*v4),
            (HostMatch::Ip(ip), url::Host::Ipv6(v6)) => *ip == IpAddr::V6(*v6),
            _ => false,
        }
    }

    /// Whether everything `self` matches, `outer` matches too.
    fn within(&self, outer: &HostMatch) -> bool {
        match (self, outer) {
            (HostMatch::Exact(h), o) => o.matches(&url::Host::Domain(h)),
            (HostMatch::Below(s), HostMatch::Below(o)) => {
                s == o || HostMatch::Below(o.clone()).matches(&url::Host::Domain(s))
            }
            (HostMatch::Ip(a), HostMatch::Ip(b)) => a == b,
            _ => false,
        }
    }

    /// The form the database stores (`webhook_allowed_hosts.host_pattern`).
    pub fn stored(&self) -> String {
        self.to_string()
    }
}

impl HostPattern {
    /// `host[:port]`, an IPv6 address in brackets when it has a port.
    pub fn parse(raw: &str) -> Result<HostPattern, String> {
        let raw = raw.trim().to_ascii_lowercase();
        let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
            let (ip, after) = rest.split_once(']').ok_or_else(|| format!("{raw:?}: unclosed `[`"))?;
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':').ok_or_else(|| format!("{raw:?}: expected `]:port`"))?),
            };
            (format!("[{ip}]"), port.map(str::to_owned))
        } else if raw.matches(':').count() > 1 {
            (raw.clone(), None)
        } else {
            match raw.split_once(':') {
                Some((h, p)) => (h.to_owned(), Some(p.to_owned())),
                None => (raw.clone(), None),
            }
        };
        let port = port
            .map(|p| p.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| format!("{raw:?}: invalid port")))
            .transpose()?;
        Ok(HostPattern { host: HostMatch::parse(&host)?, port })
    }

    pub fn allows(&self, url: &url::Url) -> bool {
        url.host().is_some_and(|h| self.host.matches(&h))
            && self.port.is_none_or(|p| Some(p) == url.port_or_known_default())
    }

    /// Whether every URL `self` allows, `outer` allows as well.
    pub fn within(&self, outer: &HostPattern) -> bool {
        self.host.within(&outer.host)
            && match (self.port, outer.port) {
                (_, None) => true,
                (Some(a), Some(b)) => a == b,
                (None, Some(_)) => false,
            }
    }
}

/// `WEBHOOK_ALLOWED_HOSTS`: the operator's ceiling. Unset, the administrator's
/// allowlist alone decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCeiling(pub Vec<HostPattern>);

impl HostCeiling {
    /// A comma-separated list; an empty one is refused (unset the variable instead).
    pub fn parse(raw: &str) -> Result<HostCeiling, String> {
        let list: Vec<HostPattern> = raw
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(HostPattern::parse)
            .collect::<Result<_, _>>()?;
        if list.is_empty() {
            return Err("lists no host; leave it unset to let the administrator's allowlist decide".into());
        }
        Ok(HostCeiling(list))
    }

    pub fn allows(&self, url: &url::Url) -> bool {
        self.0.iter().any(|p| p.allows(url))
    }

    pub fn contains(&self, entry: &HostPattern) -> bool {
        self.0.iter().any(|p| entry.within(p))
    }
}

/// An entry of the administrator's allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedHost {
    pub pattern: HostPattern,
    pub allow_http: bool,
}

/// Why a URL may not be used: a validation code and a message for the administrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlRefusal {
    pub code: &'static str,
    pub message: String,
}

impl UrlRefusal {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        UrlRefusal { code, message: message.into() }
    }
}

/// The operator's switches that a URL is checked against.
#[derive(Debug, Clone, Copy)]
pub struct Ceiling<'a> {
    pub hosts: Option<&'a HostCeiling>,
    /// `WEBHOOK_ALLOW_HTTP`
    pub allow_http: bool,
}

/// The URL as it is stored and sent, if it meets the rules of §5.2: absolute
/// https (http only when both the operator and the matching entry allow it),
/// no user name or password, no fragment, at most 2,048 characters, and a host
/// (and port) on the administrator's allowlist and inside the operator's
/// ceiling. The host comes back in IDNA form.
pub fn check_url(raw: &str, ceiling: Ceiling<'_>, allowlist: &[AllowedHost]) -> Result<url::Url, UrlRefusal> {
    let url = parse_url(raw)?;
    let host = url.host().expect("parse_url checked the host");
    let shown = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    };
    if ceiling.hosts.is_some_and(|c| !c.allows(&url)) {
        return Err(UrlRefusal::new(
            "host_not_allowed",
            format!("{shown} is outside the hosts the server's operator allows (WEBHOOK_ALLOWED_HOSTS)"),
        ));
    }
    let entries: Vec<&AllowedHost> = allowlist.iter().filter(|e| e.pattern.allows(&url)).collect();
    if entries.is_empty() {
        return Err(UrlRefusal::new(
            "host_not_allowed",
            format!("{shown} is not on the webhook host allowlist (Administration > Webhooks)"),
        ));
    }
    if url.scheme() == "http" && !(ceiling.allow_http && entries.iter().any(|e| e.allow_http)) {
        return Err(UrlRefusal::new(
            "http_not_allowed",
            "Unencrypted http needs both WEBHOOK_ALLOW_HTTP=true on the server and an allowlist entry for this host \
             that allows http; use https",
        ));
    }
    Ok(url)
}

/// The URL's form alone (§5.2 without the allowlist): absolute http(s), a
/// host, no credentials, no fragment, at most 2,048 characters.
pub fn parse_url(raw: &str) -> Result<url::Url, UrlRefusal> {
    if raw.len() > MAX_URL {
        return Err(UrlRefusal::new("too_long", format!("At most {MAX_URL} characters")));
    }
    let url = url::Url::parse(raw.trim())
        .map_err(|e| UrlRefusal::new("invalid_url", format!("Not an absolute URL ({e})")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlRefusal::new(
            "url_userinfo",
            "Credentials in the URL are not allowed; give the receiver's credentials as the authentication header",
        ));
    }
    if url.fragment().is_some() {
        return Err(UrlRefusal::new("url_fragment", "A URL with a fragment (#...) is not allowed"));
    }
    if !matches!(url.scheme(), "https" | "http") {
        return Err(UrlRefusal::new("invalid_url", format!("Scheme {} is not allowed; use https", url.scheme())));
    }
    if url.host().is_none_or(|h| h.to_string().is_empty()) {
        return Err(UrlRefusal::new("invalid_url", "The URL has no host"));
    }
    if url.as_str().len() > MAX_URL {
        return Err(UrlRefusal::new("too_long", format!("At most {MAX_URL} characters once normalised")));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(p: &str, http: bool) -> AllowedHost {
        AllowedHost { pattern: HostPattern::parse(p).unwrap(), allow_http: http }
    }

    const OPEN: Ceiling<'static> = Ceiling { hosts: None, allow_http: false };

    #[test]
    fn patterns_parse_and_print_as_stored() {
        for (raw, stored) in [
            ("ITSM.corp.example", "itsm.corp.example"),
            ("*.corp.example", "*.corp.example"),
            ("10.1.2.3", "10.1.2.3"),
            ("[fd00::1]", "fd00::1"),
            ("fd00::1", "fd00::1"),
            ("bücher.example", "xn--bcher-kva.example"),
        ] {
            assert_eq!(HostMatch::parse(raw).unwrap().stored(), stored, "{raw}");
        }
        for bad in ["", "*", "*.", "a.*.example", "https://x", "u@x", "x/y", "*.10.0.0.1"] {
            assert!(HostMatch::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(HostPattern::parse("[fd00::1]:8443").unwrap().to_string(), "[fd00::1]:8443");
        assert_eq!(HostPattern::parse("a.example:8443").unwrap().port, Some(8443));
        assert!(HostPattern::parse("a.example:0").is_err());
    }

    #[test]
    fn wildcards_match_names_below_their_domain_only() {
        let p = HostMatch::parse("*.corp.example").unwrap();
        assert!(p.matches(&url::Host::Domain("a.corp.example")));
        assert!(p.matches(&url::Host::Domain("a.b.corp.example")));
        assert!(!p.matches(&url::Host::Domain("corp.example")));
        assert!(!p.matches(&url::Host::Domain("evilcorp.example")));
        assert!(!p.matches(&url::Host::Domain("a.corp.example.evil")));
        assert!(!p.matches(&url::Host::Ipv4("10.0.0.1".parse().unwrap())));
    }

    #[test]
    fn urls_follow_the_rules_of_5_2() {
        let list = [allow("itsm.corp.example", false), allow("*.hooks.example", true), allow("10.1.2.3:8443", false)];
        let ok = |u: &str| check_url(u, OPEN, &list).map(|u| u.to_string());
        let code = |u: &str, c: Ceiling<'_>| check_url(u, c, &list).unwrap_err().code;
        assert_eq!(ok("https://ITSM.corp.example/hook?x=1").unwrap(), "https://itsm.corp.example/hook?x=1");
        assert!(ok("https://a.hooks.example:9000/").is_ok());
        assert!(ok("https://10.1.2.3:8443/").is_ok());
        assert_eq!(code("https://10.1.2.3/", OPEN), "host_not_allowed", "the entry names port 8443");
        assert_eq!(code("https://itsm2.corp.example/", OPEN), "host_not_allowed");
        assert_eq!(code("https://user:pw@itsm.corp.example/", OPEN), "url_userinfo");
        assert_eq!(code("https://user@itsm.corp.example/", OPEN), "url_userinfo");
        assert_eq!(code("https://itsm.corp.example/#x", OPEN), "url_fragment");
        assert_eq!(code("ftp://itsm.corp.example/", OPEN), "invalid_url");
        assert_eq!(code("/relative", OPEN), "invalid_url");
        assert_eq!(code(&format!("https://itsm.corp.example/{}", "a".repeat(2048)), OPEN), "too_long");
        // http needs both switches.
        assert_eq!(code("http://itsm.corp.example/", Ceiling { hosts: None, allow_http: true }), "http_not_allowed");
        assert_eq!(code("http://a.hooks.example/", OPEN), "http_not_allowed");
        assert!(check_url("http://a.hooks.example/", Ceiling { hosts: None, allow_http: true }, &list).is_ok());
        // The ceiling applies on top of the list.
        let ceiling = HostCeiling::parse("*.hooks.example").unwrap();
        let c = Ceiling { hosts: Some(&ceiling), allow_http: false };
        assert_eq!(code("https://itsm.corp.example/", c), "host_not_allowed");
        assert!(check_url("https://a.hooks.example/", c, &list).is_ok());
        // An empty allowlist allows nothing.
        assert_eq!(check_url("https://a.hooks.example/", OPEN, &[]).unwrap_err().code, "host_not_allowed");
    }

    #[test]
    fn the_administrator_list_only_narrows_the_ceiling() {
        let ceiling = HostCeiling::parse("*.corp.example, hooks.example:443, 10.0.0.5").unwrap();
        let inside = |p: &str| ceiling.contains(&HostPattern::parse(p).unwrap());
        assert!(inside("itsm.corp.example"));
        assert!(inside("*.itsm.corp.example"));
        assert!(inside("*.corp.example"));
        assert!(inside("hooks.example:443"));
        assert!(inside("10.0.0.5:8443"));
        assert!(!inside("corp.example"));
        assert!(!inside("hooks.example"), "any port is wider than :443");
        assert!(!inside("*.example"));
        assert!(!inside("10.0.0.6"));
        assert!(HostCeiling::parse(" , ").is_err());
    }
}

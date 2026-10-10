//! SSRF protection (design SHAA-2725 §5.3): the addresses a webhook may be
//! sent to, judged on what the host name resolves to at send time.
//!
//! Every resolved address must be outside the blocked ranges (unless the
//! operator allows its network with `WEBHOOK_ALLOW_PRIVATE_CIDRS`), and never
//! a cloud metadata address, whatever the operator allows. An IPv6 address
//! that carries an IPv4 one (IPv4-mapped, SIIT, NAT64 well-known and
//! local-use, 6to4, Teredo, IPv4-compatible) is judged by the IPv4 address
//! too. The request then goes to the vetted
//! addresses only (the client's resolver is pinned to them), so a name that
//! resolves differently a moment later (DNS rebinding) changes nothing.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use ipnetwork::IpNetwork;

/// Ranges never reached unless `WEBHOOK_ALLOW_PRIVATE_CIDRS` covers the address.
const BLOCKED_V4: &[(&str, &str)] = &[
    ("0.0.0.0/8", "this network"),
    ("10.0.0.0/8", "private network"),
    ("100.64.0.0/10", "carrier-grade NAT"),
    ("127.0.0.0/8", "loopback"),
    ("169.254.0.0/16", "link-local"),
    ("172.16.0.0/12", "private network"),
    ("192.0.0.0/24", "IETF protocol assignments"),
    ("192.0.2.0/24", "documentation"),
    ("192.168.0.0/16", "private network"),
    ("198.18.0.0/15", "benchmarking"),
    ("198.51.100.0/24", "documentation"),
    ("203.0.113.0/24", "documentation"),
    ("224.0.0.0/4", "multicast"),
    ("240.0.0.0/4", "reserved"),
    ("255.255.255.255/32", "broadcast"),
];

const BLOCKED_V6: &[(&str, &str)] = &[
    ("::/128", "unspecified"),
    ("::1/128", "loopback"),
    ("64:ff9b:1::/48", "local-use NAT64"),
    ("100::/64", "discard-only"),
    ("2001::/23", "IETF protocol assignments"),
    ("fc00::/7", "unique local"),
    ("fe80::/10", "link-local"),
    ("fec0::/10", "site-local"),
    ("ff00::/8", "multicast"),
    ("2001:db8::/32", "documentation"),
];

/// Cloud metadata services: refused even inside an allowed private range.
const METADATA: &[&str] = &["169.254.169.254", "169.254.170.2", "fd00:ec2::254", "100.100.100.200"];

/// Why an address may not be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub address: IpAddr,
    /// `link-local`, `cloud metadata`, ...; with the IPv4 address it carries, if that is what is blocked.
    pub why: String,
}

/// The IPv4 address an IPv6 address carries, if it is of a form that does:
/// IPv4-mapped (`::ffff:a.b.c.d`), SIIT IPv4-translated (`::ffff:0:a.b.c.d`),
/// NAT64 well-known (`64:ff9b::/96`) or local-use (`64:ff9b:1::/48`, read as
/// the usual /96 layout), 6to4 (`2002::/16`), Teredo (`2001::/32`, the client
/// address: the low 32 bits inverted) or the deprecated IPv4-compatible form
/// (`::a.b.c.d`).
pub fn embedded_v4(ip: &Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let low = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
    match s {
        [0, 0, 0, 0, 0, 0xffff, _, _] => Some(low),
        [0, 0, 0, 0, 0xffff, 0, _, _] => Some(low),
        [0x64, 0xff9b, 0, 0, 0, 0, _, _] => Some(low),
        [0x64, 0xff9b, 1, ..] => Some(low),
        [0x2002, a, b, ..] => Some(Ipv4Addr::new((a >> 8) as u8, a as u8, (b >> 8) as u8, b as u8)),
        [0x2001, 0, ..] => Some(Ipv4Addr::from(!u32::from(low))),
        // `::` and `::1` are blocked as themselves first.
        [0, 0, 0, 0, 0, 0, _, _] => Some(low),
        _ => None,
    }
}

fn nets(list: &'static [(&'static str, &'static str)]) -> impl Iterator<Item = (IpNetwork, &'static str)> {
    list.iter().map(|(n, why)| (n.parse::<IpNetwork>().expect("a valid CIDR"), *why))
}

fn is_metadata(ip: IpAddr) -> bool {
    METADATA.iter().any(|m| m.parse::<IpAddr>().ok() == Some(ip))
}

/// Judges one address. `allowed`: `WEBHOOK_ALLOW_PRIVATE_CIDRS`.
pub fn check(ip: IpAddr, allowed: &[IpNetwork]) -> Result<(), Blocked> {
    let inner = match ip {
        IpAddr::V6(v6) => embedded_v4(&v6).map(IpAddr::V4),
        IpAddr::V4(_) => None,
    };
    let blocked = |why: String| Err(Blocked { address: ip, why });
    for candidate in [Some(ip), inner].into_iter().flatten() {
        if is_metadata(candidate) {
            let via = if candidate == ip { String::new() } else { format!(" ({candidate})") };
            return blocked(format!("cloud metadata service{via}"));
        }
    }
    let allowed_here = |a: IpAddr| allowed.iter().any(|n| n.contains(a));
    for candidate in [Some(ip), inner].into_iter().flatten() {
        if allowed_here(candidate) {
            continue;
        }
        let ranges = match candidate {
            IpAddr::V4(_) => nets(BLOCKED_V4).collect::<Vec<_>>(),
            IpAddr::V6(_) => nets(BLOCKED_V6).collect(),
        };
        if let Some((_, why)) = ranges.iter().find(|(n, _)| n.contains(candidate)) {
            let via = if candidate == ip { String::new() } else { format!(" ({candidate})") };
            return blocked(format!("{why}{via}"));
        }
    }
    Ok(())
}

/// Looks a host name up (A and AAAA). Replaceable for tests.
pub trait Resolve: Send + Sync {
    fn lookup(&self, host: &str, port: u16) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>>;
}

/// The operating system's resolver.
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn lookup(&self, host: &str, port: u16) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> {
        let host = host.to_owned();
        Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), port)).await?.map(|a| a.ip()).collect()) })
    }
}

/// Why a URL's host cannot be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The name did not resolve (transient: DNS may answer next time).
    Unresolved(String),
    /// An address is in a blocked range (permanent).
    Blocked(Blocked),
}

/// The vetted addresses of `url`'s host: the literal, or every address the
/// name resolves to now, all of which must pass [`check`].
pub async fn vet(
    url: &url::Url,
    resolver: &Arc<dyn Resolve>,
    allowed: &[IpNetwork],
) -> Result<Vec<SocketAddr>, Refusal> {
    let port = url.port_or_known_default().unwrap_or(443);
    let ips = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
        Some(url::Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
        Some(url::Host::Domain(d)) => {
            let ips = resolver.lookup(d, port).await.map_err(|e| Refusal::Unresolved(format!("{d}: {e}")))?;
            if ips.is_empty() {
                return Err(Refusal::Unresolved(format!("{d}: no address")));
            }
            ips
        }
        None => return Err(Refusal::Unresolved("no host".into())),
    };
    for ip in &ips {
        check(*ip, allowed).map_err(Refusal::Blocked)?;
    }
    let mut out: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// A resolver with fixed answers that a test can change between calls.
    #[derive(Default)]
    pub struct StaticResolver(pub Mutex<HashMap<String, Vec<IpAddr>>>);

    impl StaticResolver {
        pub fn set(&self, host: &str, ips: &[&str]) {
            self.0.lock().unwrap().insert(host.to_owned(), ips.iter().map(|i| i.parse().unwrap()).collect());
        }
    }

    impl Resolve for StaticResolver {
        fn lookup(&self, host: &str, _port: u16) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> {
            let found = self.0.lock().unwrap().get(host).cloned();
            Box::pin(
                async move { found.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such host")) },
            )
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn every_blocked_range_of_5_3_is_refused() {
        for a in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.5.4",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "fec0::1",
            "100::1",
            "2001:0:4136:e378:8000:63bf:f5ff:fffe",
            "2001:2::1",
            "64:ff9b:1::808:808",
            // Carriers of a blocked IPv4 address.
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:a00:1::1",
            "::127.0.0.1",
            "::ffff:0:a00:1",
            "::ffff:0:127.0.0.1",
        ] {
            assert!(check(ip(a), &[]).is_err(), "{a} must be refused");
        }
        for a in ["93.184.215.14", "8.8.8.8", "2606:4700::1111", "64:ff9b::808:808", "::ffff:8.8.8.8", "172.32.0.1"] {
            assert_eq!(check(ip(a), &[]), Ok(()), "{a} is public");
        }
    }

    #[test]
    fn metadata_addresses_are_refused_even_inside_an_allowed_range() {
        let allowed: Vec<IpNetwork> =
            ["169.254.0.0/16", "100.64.0.0/10", "fd00::/8", "0.0.0.0/0"].iter().map(|n| n.parse().unwrap()).collect();
        for a in [
            "169.254.169.254",
            "169.254.170.2",
            "fd00:ec2::254",
            "100.100.100.200",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::a9fe:a9fe",
            "::ffff:169.254.169.254",
            "::ffff:0:169.254.169.254",
            // Teredo, client 169.254.169.254 (inverted: 5601:5601).
            "2001:0:4136:e378:8000:63bf:5601:5601",
        ] {
            let refused = check(ip(a), &allowed).unwrap_err();
            assert!(refused.why.starts_with("cloud metadata"), "{a}: {refused:?}");
        }
        // Their neighbours are allowed there.
        assert_eq!(check(ip("169.254.169.253"), &allowed), Ok(()));
        let nat64 = check(ip("64:ff9b::a9fe:a9fe"), &[]).unwrap_err();
        assert_eq!(nat64.why, "cloud metadata service (169.254.169.254)");
    }

    #[test]
    fn ipv4_carriers_decode_the_address_they_reach() {
        let v6 = |s: &str| s.parse::<Ipv6Addr>().unwrap();
        let v4 = |s: &str| Some(s.parse::<Ipv4Addr>().unwrap());
        assert_eq!(embedded_v4(&v6("::ffff:0:a00:1")), v4("10.0.0.1"));
        assert_eq!(embedded_v4(&v6("64:ff9b:1::a00:1")), v4("10.0.0.1"));
        assert_eq!(embedded_v4(&v6("2001:0:4136:e378:8000:63bf:f5ff:fffe")), v4("10.0.0.1"));
        assert_eq!(embedded_v4(&v6("2001:db8::a00:1")), None);
        assert_eq!(embedded_v4(&v6("2606:4700::1111")), None);
    }

    #[test]
    fn an_allowed_local_nat64_prefix_still_judges_the_ipv4_address() {
        // An operator whose egress goes through a local-use NAT64 gateway opens
        // the prefix; the IPv4 address behind it is still judged.
        let allowed: Vec<IpNetwork> = vec!["64:ff9b:1::/48".parse().unwrap()];
        assert_eq!(check(ip("64:ff9b:1::808:808"), &allowed), Ok(()));
        let refused = check(ip("64:ff9b:1::a00:1"), &allowed).unwrap_err();
        assert_eq!(refused.why, "private network (10.0.0.1)");
        // The same for Teredo: the client address is judged.
        let allowed: Vec<IpNetwork> = vec!["2001::/32".parse().unwrap()];
        let refused = check(ip("2001:0:4136:e378:8000:63bf:f5ff:fffe"), &allowed).unwrap_err();
        assert_eq!(refused.why, "private network (10.0.0.1)");
    }

    #[test]
    fn an_allowed_range_opens_its_addresses_only() {
        let allowed: Vec<IpNetwork> = vec!["10.20.0.0/16".parse().unwrap()];
        assert_eq!(check(ip("10.20.3.4"), &allowed), Ok(()));
        assert_eq!(check(ip("::ffff:10.20.3.4"), &allowed), Ok(()));
        assert!(check(ip("10.21.0.1"), &allowed).is_err());
        assert!(check(ip("127.0.0.1"), &allowed).is_err());
    }

    #[tokio::test]
    async fn every_resolved_address_is_vetted() {
        let r = Arc::new(StaticResolver::default());
        r.set("public.example", &["93.184.215.14"]);
        r.set("internal.example", &["10.0.0.1"]);
        r.set("mixed.example", &["93.184.215.14", "127.0.0.1"]);
        let resolver: Arc<dyn Resolve> = r.clone();
        let url = |u: &str| url::Url::parse(u).unwrap();
        let addrs = vet(&url("https://public.example:8443/x"), &resolver, &[]).await.unwrap();
        assert_eq!(addrs, vec!["93.184.215.14:8443".parse().unwrap()]);
        for u in ["https://internal.example/", "https://mixed.example/", "https://[::ffff:127.0.0.1]/"] {
            assert!(matches!(vet(&url(u), &resolver, &[]).await, Err(Refusal::Blocked(_))), "{u}");
        }
        assert!(matches!(vet(&url("https://nowhere.example/"), &resolver, &[]).await, Err(Refusal::Unresolved(_))));
    }
}

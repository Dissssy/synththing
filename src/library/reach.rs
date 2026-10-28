//! Whether a hub's address works from the open internet: the hub asks the
//! server (`POST /api/v1/reach`, `server/reach.rs`), which fetches the
//! hub's `hello` from outside, since a hub can't test its own public
//! address from inside its network. This is what both sides agree on, and
//! what the hub makes of the answer (`hint`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// Check `url` (a hub's address): the hub at it should answer with
/// `hub_key`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Ask {
    pub url: String,
    pub hub_key: String,
}

/// What the server found at the address.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Reach {
    /// The host didn't resolve: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<String>,
    /// The host's first IPv4 and IPv6 address, and what each answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v4: Option<Probe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v6: Option<Probe>,
    /// The address the hub's own request came from.
    pub from: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Probe {
    pub addr: String,
    #[serde(flatten)]
    pub verdict: Verdict,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// The hub answered, in this many milliseconds.
    Ok { ms: u64 },
    /// It's a private address (not tried).
    Private,
    /// Nothing listens there.
    Refused,
    TimedOut,
    Tls { reason: String },
    /// It answered with this HTTP status.
    Status { status: u16 },
    /// Something answered, but not a hub (a router's page, say).
    NotAHub,
    /// A hub answered, but another one.
    OtherHub,
    Failed { reason: String },
}

impl Reach {
    /// Whether the hub answered at any of its addresses.
    pub fn reachable(&self) -> bool {
        [&self.v4, &self.v6].into_iter().flatten().any(|probe| matches!(probe.verdict, Verdict::Ok { .. }))
    }
}

/// Whether `ip` is reached across the internet (not a private, shared,
/// loopback, link-local, multicast or reserved address).
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => public_v6(ip),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // shared (CGNAT)
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || first == 0x2001 && ip.segments()[1] == 0x0db8 // documentation
        || first == 0x0064 && ip.segments()[1] == 0xff9b // NAT64
        || first == 0)
}

/// The host in `url` (without brackets around an IPv6 address).
pub fn host(url: &str) -> &str {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.rsplit_once(':').map_or(authority, |(host, _)| host),
    }
}

/// Whether `url` is only reached on a network of its own: a private
/// address, or a name only that network knows. The internet check skips
/// those.
pub fn local(url: &str) -> bool {
    let host = host(url);
    match host.parse::<IpAddr>() {
        Ok(ip) => !public(ip),
        Err(_) => host == "localhost" || host.ends_with(".local") || host.ends_with(".lan") || host.ends_with(".internal") || !host.contains('.'),
    }
}

/// What to do about `url`, as the server found it: nothing when it
/// answers everywhere it should.
pub fn hint(url: &str, reach: &Reach) -> Option<String> {
    let host = host(url);
    if let Some(dns) = &reach.dns {
        return Some(format!("{host} doesn't resolve ({dns}): check the name"));
    }
    let addrs: Vec<&str> = [&reach.v4, &reach.v6].into_iter().flatten().map(|probe| probe.addr.as_str()).collect();
    let stale = !reach.from.is_empty() && !addrs.contains(&reach.from.as_str());
    let failed = [&reach.v4, &reach.v6].into_iter().flatten().find(|probe| !matches!(probe.verdict, Verdict::Ok { .. }))?;
    let port = url.rsplit_once(':').and_then(|(_, port)| port.split('/').next()?.parse::<u16>().ok());
    let why = match &failed.verdict {
        Verdict::Ok { .. } => return None,
        Verdict::Private => format!("{host} points at a private address ({}): only devices on that network reach it", failed.addr),
        Verdict::Refused | Verdict::TimedOut if stale && host.parse::<IpAddr>().is_err() => format!(
            "nothing answers at {} from the internet, and {host} points there while this hub reaches the internet from {}: the name points at an old address, or your provider shares one between its customers (CGNAT)",
            failed.addr, reach.from
        ),
        Verdict::Refused | Verdict::TimedOut => match port {
            Some(port) => format!("nothing answers at {} from the internet: forward port {port} to this machine on your router, and open it in the firewall", failed.addr),
            None => format!("nothing answers at {} from the internet: check the port forward and the firewall", failed.addr),
        },
        Verdict::Tls { reason } => format!("HTTPS to {host} failed: {reason}"),
        Verdict::Status { status } => format!("{url} answered {status}: does the proxy in front pass /hub/v1 on to the hub?"),
        Verdict::NotAHub => format!("something else answers at {url} (a router's page?): the port forward points at the wrong place"),
        Verdict::OtherHub => format!("another hub answers at {url}: the port forward points at a different machine"),
        Verdict::Failed { reason } => format!("reaching {url} failed: {reason}"),
    };
    let family = if reach.reachable() { if failed.addr.contains(':') { " over IPv6" } else { " over IPv4" } } else { "" };
    Some(if family.is_empty() { why } else { format!("it answers, but not{family}: {why}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_internet_addresses_are_public() {
        for ip in ["8.8.8.8", "5.161.82.164", "2a01:4f8::1", "::ffff:8.8.8.8"] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.0.0.1",
            "192.168.1.20",
            "172.16.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "2001:db8::1",
            "64:ff9b::a00:1",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn hosts_and_local_addresses() {
        assert_eq!(host("http://[2a01:4f8::1]:7390/x"), "2a01:4f8::1");
        assert_eq!(host("https://hub.example.org"), "hub.example.org");
        assert_eq!(host("http://5.161.82.164:7390"), "5.161.82.164");
        assert!(local("http://192.168.1.20:7390"));
        assert!(local("http://nas.local:7390"));
        assert!(local("http://nas:7390"));
        assert!(!local("http://5.161.82.164:7390"));
        assert!(!local("https://hub.example.org"));
    }

    #[test]
    fn hints_say_what_to_fix() {
        let probe = |addr: &str, verdict| Some(Probe { addr: addr.to_string(), verdict });
        let ok = Reach { dns: None, v4: probe("5.161.82.164", Verdict::Ok { ms: 40 }), v6: None, from: "5.161.82.164".into() };
        assert_eq!(hint("http://5.161.82.164:7390", &ok), None);
        let closed = Reach { v4: probe("5.161.82.164", Verdict::TimedOut), ..ok.clone() };
        assert!(hint("http://5.161.82.164:7390", &closed).unwrap().contains("forward port 7390"));
        let stale = Reach { v4: probe("203.0.113.5", Verdict::Refused), ..ok.clone() };
        assert!(hint("http://hub.example.org:7390", &stale).unwrap().contains("old address"));
        let no_v6 = Reach { v6: probe("2a01:4f8::1", Verdict::Refused), ..ok.clone() };
        assert!(hint("http://hub.example.org:7390", &no_v6).unwrap().starts_with("it answers, but not over IPv6"));
        let other = Reach { v4: probe("5.161.82.164", Verdict::OtherHub), ..ok };
        assert!(hint("http://5.161.82.164:7390", &other).unwrap().contains("different machine"));
    }
}

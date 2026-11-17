//! SSRF guard for `web_fetch` (06 §8, 08 §2.1): private, loopback, link-local, CGNAT,
//! multicast, reserved and cloud-metadata addresses are blocked by default; every
//! address a host resolves to must pass, and the connection is pinned to the checked
//! addresses (no second lookup → no DNS rebinding).

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;

/// Which destinations `web_fetch` may reach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SsrfPolicy {
    /// Allow private/loopback/link-local/CGNAT ranges (a corporate setting). Metadata
    /// endpoints stay blocked.
    pub allow_private: bool,
    /// Exact addresses that are always allowed (tests: a loopback server).
    pub allow: Vec<IpAddr>,
}

/// Cloud metadata endpoints (AWS/GCP/Azure/OpenStack, Alibaba, AWS IPv6, Oracle).
const METADATA_V4: &[Ipv4Addr] = &[
    Ipv4Addr::new(169, 254, 169, 254),
    Ipv4Addr::new(169, 254, 170, 2),
    Ipv4Addr::new(100, 100, 100, 200),
    Ipv4Addr::new(192, 0, 0, 192),
];
const METADATA_V6: &[Ipv6Addr] = &[Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)];

/// Why an address is blocked, or `None` if it may be reached.
pub fn blocked_reason(ip: IpAddr, policy: &SsrfPolicy) -> Option<&'static str> {
    if policy.allow.contains(&ip) {
        return None;
    }
    let class = classify(ip)?;
    if class == "a cloud metadata address" || !policy.allow_private {
        return Some(class);
    }
    // Private ranges are allowed by policy, but never "unspecified"/multicast/reserved.
    match class {
        "a private address"
        | "a loopback address"
        | "a link-local address"
        | "a shared (CGNAT) address" => None,
        other => Some(other),
    }
}

fn classify(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let o = ip.octets();
    if METADATA_V4.contains(&ip) {
        return Some("a cloud metadata address");
    }
    if o[0] == 0 {
        return Some("an unspecified address");
    }
    if ip.is_loopback() {
        return Some("a loopback address");
    }
    if ip.is_private() {
        return Some("a private address");
    }
    if ip.is_link_local() {
        return Some("a link-local address");
    }
    if o[0] == 100 && (o[1] & 0xc0) == 64 {
        return Some("a shared (CGNAT) address");
    }
    if ip.is_multicast() || ip.is_broadcast() {
        return Some("a multicast or broadcast address");
    }
    if o[0] >= 240
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
    {
        return Some("a reserved address");
    }
    None
}

fn classify_v6(ip: Ipv6Addr) -> Option<&'static str> {
    if METADATA_V6.contains(&ip) {
        return Some("a cloud metadata address");
    }
    let s = ip.segments();
    // IPv4-mapped (::ffff:a.b.c.d), IPv4-compatible (::a.b.c.d), NAT64 (64:ff9b::/96).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return classify_v4(v4);
    }
    if s[..6] == [0, 0, 0, 0, 0, 0] && !ip.is_unspecified() && !ip.is_loopback() {
        return classify_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return classify_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    // 6to4 (2002:a.b.c.d::/48).
    if s[0] == 0x2002 {
        return classify_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    if ip.is_unspecified() {
        return Some("an unspecified address");
    }
    if ip.is_loopback() {
        return Some("a loopback address");
    }
    if (s[0] & 0xfe00) == 0xfc00 {
        return Some("a private address");
    }
    if (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xffc0) == 0xfec0 {
        return Some("a link-local address");
    }
    if ip.is_multicast() {
        return Some("a multicast or broadcast address");
    }
    None
}

/// DNS lookups (a port so tests and the engine can control resolution).
#[async_trait]
pub trait Resolver: Send + Sync {
    async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>>;
}

/// The system resolver (`getaddrinfo` via tokio).
pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let addrs = tokio::net::lookup_host((host, 0)).await?;
        let mut ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
        ips.dedup();
        Ok(ips)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn default_policy_table() {
        let p = SsrfPolicy::default();
        let blocked = [
            "127.0.0.1",
            "127.8.9.10",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.255",
            "100.100.100.200",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.0.192",
            "198.18.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
        ];
        for b in blocked {
            assert!(blocked_reason(ip(b), &p).is_some(), "{b} should be blocked");
        }
        let open = [
            "93.184.216.34",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ];
        for o in open {
            assert_eq!(blocked_reason(ip(o), &p), None, "{o} should be allowed");
        }
        assert_eq!(
            blocked_reason(ip("169.254.169.254"), &p),
            Some("a cloud metadata address")
        );
    }

    #[test]
    fn allow_private_keeps_metadata_blocked() {
        let p = SsrfPolicy {
            allow_private: true,
            allow: vec![],
        };
        assert_eq!(blocked_reason(ip("10.0.0.1"), &p), None);
        assert_eq!(blocked_reason(ip("127.0.0.1"), &p), None);
        assert!(blocked_reason(ip("169.254.169.254"), &p).is_some());
        assert!(blocked_reason(ip("0.0.0.0"), &p).is_some());
        let p = SsrfPolicy {
            allow_private: false,
            allow: vec![ip("127.0.0.1")],
        };
        assert_eq!(blocked_reason(ip("127.0.0.1"), &p), None);
        assert!(blocked_reason(ip("127.0.0.2"), &p).is_some());
    }
}

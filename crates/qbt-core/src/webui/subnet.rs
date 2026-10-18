//! IP subnets (qBittorrent `Utils::Net::Subnet`): parsing `addr/prefix`
//! (a bare address is a host route), normalisation to the network address
//! and membership tests that treat IPv4-mapped IPv6 addresses as IPv4.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subnet {
    network: IpAddr,
    prefix: u8,
}

impl Subnet {
    /// Parses `192.168.0.0/24`, `fd00::/8`, `10.0.0.1` (= /32) or `::1` (= /128).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a.trim(), Some(p.trim())),
            None => (s, None),
        };
        let addr = parse_ip(addr)?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => {
                if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 3 {
                    return None;
                }
                let v: u8 = p.parse().ok()?;
                if v > max {
                    return None;
                }
                v
            }
        };
        Some(Self { network: mask(addr, prefix), prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        match (self.network, ip) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => mask(ip, self.prefix) == self.network,
            _ => false,
        }
    }
}

impl std::fmt::Display for Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// Parses an IP address, accepting IPv6 in brackets. IPv4-mapped IPv6 is
/// canonicalised to IPv4.
pub fn parse_ip(s: &str) -> Option<IpAddr> {
    let s = s.trim();
    let s = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(s);
    s.parse::<IpAddr>().ok().map(|ip| ip.to_canonical())
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let m = if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix.min(32))) };
            IpAddr::V4(Ipv4Addr::from(bits & m))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let m = if prefix == 0 { 0 } else { u128::MAX << (128 - u32::from(prefix.min(128))) };
            IpAddr::V6(Ipv6Addr::from(bits & m))
        }
    }
}

/// Parses a list, dropping invalid entries.
pub fn parse_list<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<Subnet> {
    items.into_iter().filter_map(Subnet::parse).collect()
}

pub fn any_contains(subnets: &[Subnet], ip: IpAddr) -> bool {
    subnets.iter().any(|s| s.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_contains() {
        let s = Subnet::parse("192.168.1.77/24").unwrap();
        assert_eq!(s.to_string(), "192.168.1.0/24");
        assert!(s.contains("192.168.1.5".parse().unwrap()));
        assert!(!s.contains("192.168.2.5".parse().unwrap()));
        // IPv4-mapped IPv6 is matched as IPv4
        assert!(s.contains("::ffff:192.168.1.9".parse().unwrap()));
        let h = Subnet::parse("10.0.0.1").unwrap();
        assert_eq!(h.to_string(), "10.0.0.1/32");
        let v6 = Subnet::parse("fd00::1/8").unwrap();
        assert_eq!(v6.to_string(), "fd00::/8");
        assert!(v6.contains("fdab::1".parse().unwrap()));
        assert!(!v6.contains("10.0.0.1".parse().unwrap()));
        assert!(Subnet::parse("0.0.0.0/0").unwrap().contains("8.8.8.8".parse().unwrap()));
        assert!(Subnet::parse("[::1]").unwrap().contains("::1".parse().unwrap()));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "/", "1.2.3.4/33", "::/129", "abc", "1.2.3.4/-1", "1.2.3.4/ 8x", "1.2.3/24", "1.2.3.4/0008"] {
            assert!(Subnet::parse(bad).is_none(), "{bad}");
        }
    }
}

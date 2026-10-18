//! Network interfaces for the "Network interface" and "Optional IP address
//! to bind to" advanced settings (`app/networkInterfaceList`,
//! `app/networkInterfaceAddressList`), read with `getifaddrs(3)`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::model::NetworkInterface;

/// Every interface with at least one IPv4/IPv6 address, in system order.
/// `name` is the human-readable name and `value` the system name (the same
/// on macOS, e.g. `en0`); IPv6 addresses are canonical, without scope.
pub fn list() -> Vec<NetworkInterface> {
    let mut out: Vec<NetworkInterface> = Vec::new();
    for (name, addr) in raw_addresses() {
        let addr = addr.to_string();
        match out.iter_mut().find(|i| i.value == name) {
            Some(iface) => {
                if !iface.addresses.contains(&addr) {
                    iface.addresses.push(addr);
                }
            }
            None => out.push(NetworkInterface { name: name.clone(), value: name, addresses: vec![addr] }),
        }
    }
    out
}

/// Addresses of one interface ("" = all addresses of all interfaces).
pub fn addresses(interface: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (name, addr) in raw_addresses() {
        if interface.is_empty() || name == interface {
            let a = addr.to_string();
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// True when `interface` currently exists with at least one address.
pub fn exists(interface: &str) -> bool {
    raw_addresses().iter().any(|(n, _)| n == interface)
}

/// (interface name, address) pairs for every IPv4/IPv6 address.
fn raw_addresses() -> Vec<(String, IpAddr)> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a linked list we free below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list returned by getifaddrs.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
            continue;
        }
        // SAFETY: ifa_name is a valid NUL-terminated string for the list's lifetime.
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
        // SAFETY: ifa_addr points to a sockaddr whose family tells its real type.
        let family = unsafe { (*ifa.ifa_addr).sa_family } as i32;
        let addr = match family {
            libc::AF_INET => {
                // SAFETY: family AF_INET means a sockaddr_in.
                let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
            }
            libc::AF_INET6 => {
                // SAFETY: family AF_INET6 means a sockaddr_in6.
                let sin6 = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
                IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr))
            }
            _ => continue,
        };
        out.push((name, addr));
    }
    // SAFETY: `head` came from a successful getifaddrs call.
    unsafe { libc::freeifaddrs(head) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_loopback() {
        let all = list();
        let lo = all
            .iter()
            .find(|i| i.addresses.iter().any(|a| a == "127.0.0.1"))
            .expect("loopback interface with 127.0.0.1");
        assert_eq!(lo.name, lo.value);
        assert!(addresses(&lo.value).contains(&"127.0.0.1".to_string()));
        assert!(addresses("").len() >= lo.addresses.len());
        assert!(addresses("no-such-interface0").is_empty());
        assert!(exists(&lo.value));
        // IPv6 renders canonically without a scope id.
        assert!(addresses("").iter().all(|a| !a.contains('%')));
    }
}
